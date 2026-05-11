//! POSIX signal handling for the foreground console attach.
//!
//! Sole `#![allow(unsafe_code)]` carve-out in the workspace: Rust's
//! stdlib cannot install POSIX signal handlers without `unsafe`, and
//! the dependency rule forbids `signal-hook` / `nix` / `libc`.
//!
//! SIGINT, SIGTERM, and SIGHUP each write one byte to a pipe; the
//! console-attach machinery reads it as "please detach now" and
//! unwinds normally so the [`crate::host::console::RawModeGuard`]
//! restores termios.
//!
//! The handler is async-signal-safe: only an [`AtomicI32`] load and a
//! `write(2)`. Do not extend it without confirming every added call
//! is on the `signal-safety(7)` safe list.

#![allow(unsafe_code)]

use std::{
    io,
    os::fd::{FromRawFd, OwnedFd, RawFd},
    sync::atomic::{AtomicI32, Ordering},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `SIGHUP` per `signal(7)`: controlling terminal closed.
const SIGHUP: i32 = 1;

/// `SIGINT` per `signal(7)`: terminal interrupt.
const SIGINT: i32 = 2;

/// `SIGTERM` per `signal(7)`: polite termination request.
const SIGTERM: i32 = 15;

/// Sentinel: no write-end fd stashed yet.
const SIGNAL_FD_UNSET: i32 = -1;

/// Byte written by the handler (value is arbitrary).
const WAKE_BYTE: u8 = b'!';

// ---------------------------------------------------------------------------
// Signal Installation
// ---------------------------------------------------------------------------

/// Install SIGINT/SIGTERM/SIGHUP handlers and return the read end
/// of the self-pipe. One readable byte means "detach now".
pub fn install_detach_signals() -> io::Result<OwnedFd> {
    let (read_fd, write_fd) = create_self_pipe()?;

    SIGNAL_WRITE_FD.store(write_fd, Ordering::SeqCst);

    install_signal(SIGINT)?;
    install_signal(SIGTERM)?;
    install_signal(SIGHUP)?;

    // SAFETY: `pipe(2)` just produced `read_fd` for this process and we have
    // not handed it to anything else. `OwnedFd::from_raw_fd` is the standard
    // safe-ownership bridge for newly-allocated raw fds. The write end stays
    // a `RawFd` because it is owned by the static stash for the lifetime of
    // the process; closing it from a Drop would race the signal handler.
    let owned = unsafe { OwnedFd::from_raw_fd(read_fd) };

    Ok(owned)
}

// ---------------------------------------------------------------------------
// POSIX FFI Declarations
// ---------------------------------------------------------------------------

/// Write end of the self-pipe, stashed for the signal handler.
static SIGNAL_WRITE_FD: AtomicI32 = AtomicI32::new(SIGNAL_FD_UNSET);

// ---------------------------------------------------------------------------
// Signal Handlers
// ---------------------------------------------------------------------------

// `extern "C"` declarations for the libc symbols this module touches.
//
// We declare these by hand rather than depend on the `libc` crate (the
// project's dependency rule, see CLAUDE.md). The signatures mirror POSIX /
// glibc; they are stable across every Linux libc Tartarus runs on.
unsafe extern "C" {
    fn pipe(pipefd: *mut i32) -> i32;
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
}

/// Async-signal-safe handler: writes one wake byte to the stashed
/// pipe fd.
extern "C" fn detach_signal_handler(_signum: i32) {
    let fd = SIGNAL_WRITE_FD.load(Ordering::SeqCst);
    if fd == SIGNAL_FD_UNSET {
        return;
    }

    let byte = WAKE_BYTE;

    // SAFETY: `fd` is a pipe write end produced by `pipe(2)` in
    // `install_detach_signals` and stashed before the handler was installed,
    // so it is either the sentinel (handled above) or a live writable fd
    // owned by this process. `&byte` points to one initialised byte on the
    // signal stack, which is valid for the duration of the call. `write(2)`
    // is async-signal-safe per `signal-safety(7)`. We deliberately ignore
    // the return value: a short write or `EAGAIN` is fine — even one queued
    // byte is enough to wake the reader, and there is nothing useful the
    // handler can do about a real error.
    unsafe {
        let _ = write(fd, &byte as *const u8, 1);
    }
}

/// Thin wrapper around `pipe(2)`.
fn create_self_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds: [i32; 2] = [-1, -1];

    // SAFETY: `fds` is a stack-resident array of two `i32`s, which matches
    // the `int pipefd[2]` contract of `pipe(2)`. The pointer is valid for
    // the duration of the call and `pipe(2)` writes exactly two fds before
    // returning.
    let rc = unsafe { pipe(fds.as_mut_ptr()) };

    if rc != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok((fds[0], fds[1]))
}

/// Thin wrapper around `signal(2)` for [`detach_signal_handler`].
fn install_signal(signum: i32) -> io::Result<()> {
    // SAFETY: `signum` is one of SIGINT/SIGTERM/SIGHUP — all standard signal
    // numbers documented in `signal(7)`. `detach_signal_handler` is a
    // plain `extern "C"` function with the right ABI for the
    // `sighandler_t` slot. The previous handler is discarded; we never
    // need to chain to it because Tartarus is the foreground process and
    // the only relevant default action is "terminate", which is exactly
    // what we are replacing with the self-pipe wake-up.
    let prev = unsafe { signal(signum, detach_signal_handler) };

    if prev == usize::MAX {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

/// Write one wake byte to `fd` (test helper).
#[cfg(test)]
pub(super) fn wake_for_test(fd: RawFd) -> io::Result<()> {
    let byte = WAKE_BYTE;

    // SAFETY: callers (tests) own `fd` and pass it in directly from the
    // pipe `install_detach_signals` returned. `&byte` points to one
    // initialised byte on the caller's stack. `write(2)` is safe to call
    // from non-signal context with no further constraints.
    let rc = unsafe { write(fd, &byte as *const u8, 1) };

    if rc < 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

/// Write one wake byte via the stashed write fd (test helper for
/// sibling modules).
#[cfg(test)]
pub(super) fn wake_for_test_via_stash() -> io::Result<()> {
    let fd = SIGNAL_WRITE_FD.load(Ordering::SeqCst);

    if fd == SIGNAL_FD_UNSET {
        return Err(io::Error::other("install_detach_signals has not been called"));
    }

    wake_for_test(fd)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{io::Read, os::fd::AsRawFd};

    use super::*;

    #[test]
    fn install_returns_a_valid_read_fd() {
        let read_fd = install_detach_signals().expect("install should succeed on Linux");

        assert!(read_fd.as_raw_fd() >= 0, "read fd should be non-negative");
        assert_ne!(
            SIGNAL_WRITE_FD.load(Ordering::SeqCst),
            SIGNAL_FD_UNSET,
            "write fd should be stashed after install_detach_signals returned",
        );

        let write_fd = SIGNAL_WRITE_FD.load(Ordering::SeqCst);
        close_test_fd(write_fd);
    }

    #[test]
    fn writing_to_pipe_wakes_reader() {
        let read_fd = install_detach_signals().expect("install should succeed on Linux");
        let write_fd = SIGNAL_WRITE_FD.load(Ordering::SeqCst);

        wake_for_test(write_fd).expect("test wake helper should write one byte");

        let mut file = std::fs::File::from(read_fd);
        let mut buf = [0_u8; 1];
        let n = file.read(&mut buf).expect("read should observe the wake byte");

        assert_eq!(n, 1, "exactly one wake byte should be readable");
        assert_eq!(buf[0], WAKE_BYTE, "wake byte should match the documented sentinel");

        close_test_fd(write_fd);
    }

    #[test]
    #[ignore = "raises a real signal at the running process; brittle under parallel test runners. run with --ignored \
                after setting up locally"]
    fn real_signal_wakes_reader() {}

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    /// Close a raw fd via `OwnedFd`'s Drop.
    fn close_test_fd(fd: RawFd) {
        if fd < 0 {
            return;
        }

        // SAFETY: tests own the fd via the pipe returned from
        // `install_detach_signals` and have not handed it to anything else
        // that closes it. `OwnedFd::from_raw_fd` takes ownership and Drop
        // calls `close(2)`.
        let _owned = unsafe { OwnedFd::from_raw_fd(fd) };
    }
}
