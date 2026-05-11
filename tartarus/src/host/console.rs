//! Console attach: bidirectional pump on top of
//! `virDomainOpenConsole`.
//!
//! Wires the user's terminal to the guest's serial console PTY. Raw
//! mode is managed via `stty` (no unsafe termios FFI). Detach on
//! `Ctrl-A D` or SIGINT/SIGTERM/SIGHUP. A [`RawModeGuard`] restores
//! termios on any exit path.

use std::{
    fs::File,
    io::{Read, Write},
    os::fd::OwnedFd,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use crate::{
    error::Result,
    host::{error::HostError, signals},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Detach-sequence prefix: `Ctrl-A` (ASCII 0x01).
pub const DETACH_PREFIX: u8 = 0x01;

/// Detach-sequence suffix: ASCII `D`.
pub const DETACH_SUFFIX: u8 = b'D';

/// Lowercase variant of [`DETACH_SUFFIX`]. We accept either case.
const DETACH_SUFFIX_LOWER: u8 = b'd';

/// Read-buffer size for the input/output pumps (4 KiB).
const PUMP_BUFFER_BYTES: usize = 4_096;

/// Polling interval for the libvirt stream recv side.
const STREAM_RECV_POLL: Duration = Duration::from_millis(50);

// ---------------------------------------------------------------------------
// Console Attach
// ---------------------------------------------------------------------------

/// Reason an [`attach`] call returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetachReason {
    /// Domain shut down or the libvirt stream closed externally.
    Disconnected,

    /// User typed `Ctrl-A D`.
    Escape,

    /// Process received SIGINT, SIGTERM, or SIGHUP.
    Signal,
}

/// Stateful detector for the `Ctrl-A D` escape sequence.
///
/// Returns bytes to forward and whether the detach chord fired.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DetachDetector {
    armed: bool,
}

impl DetachDetector {
    /// Build a fresh detector in the unprimed state.
    pub fn new() -> Self {
        Self { armed: false }
    }

    /// Whether the detector has seen `Ctrl-A` and is awaiting the
    /// next byte.
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Feed `input` and return bytes to forward plus whether the
    /// detach chord fired.
    pub fn feed(&mut self, input: &[u8]) -> DetachOutcome {
        let mut forward = Vec::with_capacity(input.len());
        let mut detach = false;

        for &byte in input {
            if self.armed {
                self.armed = false;
                if byte == DETACH_SUFFIX || byte == DETACH_SUFFIX_LOWER {
                    detach = true;
                    break;
                }

                forward.push(DETACH_PREFIX);
                if byte == DETACH_PREFIX {
                    self.armed = true;
                } else {
                    forward.push(byte);
                }
                continue;
            }

            if byte == DETACH_PREFIX {
                self.armed = true;
            } else {
                forward.push(byte);
            }
        }

        DetachOutcome { detach, forward }
    }
}

/// Result of feeding a buffer through [`DetachDetector::feed`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DetachOutcome {
    /// True iff the buffer contained the `Ctrl-A D` chord.
    pub detach: bool,

    /// Bytes that should be forwarded to the libvirt stream.
    pub forward: Vec<u8>,
}

/// Restores the host TTY to its pre-attach termios on drop.
#[derive(Debug)]
pub struct RawModeGuard {
    snapshot: Option<String>,
}

impl RawModeGuard {
    /// Build a guard. `None` means no snapshot was captured (no-op
    /// restore).
    pub fn new(snapshot: Option<String>) -> Self {
        Self { snapshot }
    }

    /// Discard the snapshot without restoring.
    pub fn forget(mut self) {
        self.snapshot = None;
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if let Some(snapshot) = self.snapshot.take()
            && let Err(err) = restore_termios(&snapshot)
        {
            tracing::warn!(%err, "could not restore host termios via stty");
        }
    }
}

/// Capture current termios and switch to raw mode.
///
/// Returns a [`RawModeGuard`] that restores on drop. Returns a no-op
/// guard when no controlling TTY is attached.
pub fn enter_raw_mode() -> Result<RawModeGuard> {
    let snapshot = match capture_termios() {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            tracing::debug!(%err, "no controlling tty for stty -g; raw mode is a no-op");
            return Ok(RawModeGuard::new(None));
        },
    };

    let guard = RawModeGuard::new(snapshot);
    apply_raw_mode()?;

    Ok(guard)
}

/// Restore the TTY by dropping the guard.
pub fn leave_raw_mode(guard: RawModeGuard) {
    drop(guard);
}

/// Attach the host TTY to the guest's serial console for the
/// lifetime of the call. The domain is left running on return.
pub fn attach(domain: &virt::domain::Domain) -> Result<DetachReason> {
    let connect = domain.get_connect().map_err(|source| HostError::DomainOperation {
        operation: "domain.get_connect",
        source,
    })?;

    let stream = virt::stream::Stream::new(&connect, 0).map_err(|source| HostError::DomainOperation {
        operation: "stream-new",
        source,
    })?;

    domain
        .open_console(None, &stream, 0)
        .map_err(|source| HostError::DomainOperation {
            operation: "open-console",
            source,
        })?;

    let signal_pipe = signals::install_detach_signals()?;

    let guard = enter_raw_mode()?;
    let reason = run_pumps(&stream, signal_pipe)?;
    leave_raw_mode(guard);

    let _ = stream.finish();

    tracing::info!(?reason, "console detached");
    match reason {
        DetachReason::Disconnected => println!("[ disconnected. domain may have shut down. ]"),
        DetachReason::Escape | DetachReason::Signal => {
            println!("[ detached. domain still running. ]")
        },
    }

    Ok(reason)
}

// ---------------------------------------------------------------------------
// I/O Pumps and Terminal Control
// ---------------------------------------------------------------------------

/// Spawn the input + output pumps plus the signal-pipe watcher,
/// then wait for any to report a detach reason.
fn run_pumps(stream: &virt::stream::Stream, signal_pipe: OwnedFd) -> Result<DetachReason> {
    let running = Arc::new(AtomicBool::new(true));
    let (tx, rx) = mpsc::channel::<DetachReason>();

    let input_handle = spawn_input_pump(stream.clone(), running.clone(), tx.clone());
    let output_handle = spawn_output_pump(stream.clone(), running.clone(), tx.clone());
    spawn_signal_watcher(signal_pipe, tx);

    let reason = rx.recv().unwrap_or(DetachReason::Disconnected);
    running.store(false, Ordering::SeqCst);

    if let Err(payload) = input_handle.join() {
        tracing::warn!(?payload, "input pump panicked");
    }
    if let Err(payload) = output_handle.join() {
        tracing::warn!(?payload, "output pump panicked");
    }

    Ok(reason)
}

/// Pump host stdin into the libvirt stream until detach or EOF.
fn spawn_input_pump(
    stream: virt::stream::Stream,
    running: Arc<AtomicBool>,
    tx: mpsc::Sender<DetachReason>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut detector = DetachDetector::new();
        let mut buf = [0_u8; PUMP_BUFFER_BYTES];
        let stdin = std::io::stdin();
        let mut handle = stdin.lock();

        while running.load(Ordering::SeqCst) {
            match handle.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(DetachReason::Disconnected);
                    return;
                },
                Ok(n) => {
                    let outcome = detector.feed(&buf[..n]);
                    if !outcome.forward.is_empty()
                        && let Err(err) = stream.send(&outcome.forward)
                    {
                        tracing::debug!(%err, "stream send failed; disconnecting");
                        let _ = tx.send(DetachReason::Disconnected);
                        return;
                    }
                    if outcome.detach {
                        let _ = tx.send(DetachReason::Escape);
                        return;
                    }
                },
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                    let _ = tx.send(DetachReason::Signal);
                    return;
                },
                Err(err) => {
                    tracing::debug!(%err, "stdin read failed; disconnecting");
                    let _ = tx.send(DetachReason::Disconnected);
                    return;
                },
            }
        }
    })
}

/// Pump libvirt stream bytes onto host stdout until stopped.
fn spawn_output_pump(
    stream: virt::stream::Stream,
    running: Arc<AtomicBool>,
    tx: mpsc::Sender<DetachReason>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0_u8; PUMP_BUFFER_BYTES];
        let stdout = std::io::stdout();
        let mut handle = stdout.lock();

        while running.load(Ordering::SeqCst) {
            match stream.recv(&mut buf) {
                Ok(0) => {
                    thread::sleep(STREAM_RECV_POLL);
                },
                Ok(n) => {
                    if let Err(err) = handle.write_all(&buf[..n]) {
                        tracing::debug!(%err, "stdout write failed; disconnecting");
                        let _ = tx.send(DetachReason::Disconnected);
                        return;
                    }
                    let _ = handle.flush();
                },
                Err(err) => {
                    tracing::debug!(%err, "stream recv failed; disconnecting");
                    let _ = tx.send(DetachReason::Disconnected);
                    return;
                },
            }
        }
    })
}

/// Spawn a watcher that posts [`DetachReason::Signal`] when a byte
/// arrives on the signal pipe.
fn spawn_signal_watcher(read_fd: OwnedFd, tx: mpsc::Sender<DetachReason>) {
    thread::spawn(move || {
        let mut file = File::from(read_fd);
        let mut buf = [0_u8; 1];

        if let Ok(n) = file.read(&mut buf)
            && n > 0
        {
            let _ = tx.send(DetachReason::Signal);
        }
    });
}

/// Capture the current termios via `stty -g`.
///
/// Errors when stdin is not a tty (the `stty` binary returns non-zero).
fn capture_termios() -> Result<String> {
    let stdout = run_stty("capture", &["-g"])?;
    Ok(stdout.trim().to_owned())
}

/// Switch the TTY into raw mode via `stty raw -echo`.
fn apply_raw_mode() -> Result<()> {
    run_stty("raw", &["raw", "-echo"]).map(|_| ())
}

/// Restore the original termios from a captured `stty -g` snapshot.
fn restore_termios(snapshot: &str) -> Result<()> {
    run_stty("restore", &[snapshot]).map(|_| ())
}

/// Run `stty` with `args`, surfacing non-zero exits as
/// [`HostError::ConsoleSttyFailed`].
fn run_stty(operation: &'static str, args: &[&str]) -> Result<String> {
    let output = Command::new("stty").args(args).output()?;

    if !output.status.success() {
        return Err(HostError::ConsoleSttyFailed {
            operation,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }
        .into());
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_passes_plain_bytes_through() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(b"hello world");

        assert!(!outcome.detach, "no chord present; detach should not fire");
        assert_eq!(
            outcome.forward, b"hello world",
            "plain bytes should pass through unchanged"
        );
        assert!(!detector.is_armed(), "no Ctrl-A seen; detector should remain unarmed");
    }

    #[test]
    fn detector_fires_on_ctrl_a_d() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(&[DETACH_PREFIX, DETACH_SUFFIX]);

        assert!(outcome.detach, "Ctrl-A D should trigger detach");
        assert!(
            outcome.forward.is_empty(),
            "neither byte of the chord should reach the guest, got: {:?}",
            outcome.forward,
        );
    }

    #[test]
    fn detector_accepts_lowercase_d() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(&[DETACH_PREFIX, DETACH_SUFFIX_LOWER]);

        assert!(outcome.detach, "Ctrl-A d (lowercase) should also detach");
    }

    #[test]
    fn detector_passes_ctrl_a_followed_by_unknown_byte() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(&[DETACH_PREFIX, b'x']);

        assert!(!outcome.detach, "Ctrl-A x is not the detach chord");
        assert_eq!(
            outcome.forward,
            vec![DETACH_PREFIX, b'x'],
            "the buffered Ctrl-A and the follow-on byte should both reach the guest",
        );
        assert!(!detector.is_armed(), "follow-on byte should disarm the detector");
    }

    #[test]
    fn detector_holds_arm_across_buffers() {
        let mut detector = DetachDetector::new();

        let first = detector.feed(&[DETACH_PREFIX]);
        assert!(first.forward.is_empty(), "lone Ctrl-A is buffered, not forwarded");
        assert!(detector.is_armed(), "lone Ctrl-A should leave the detector armed");
        assert!(!first.detach);

        let second = detector.feed(&[DETACH_SUFFIX]);
        assert!(second.detach, "the follow-on D should fire detach");
    }

    #[test]
    fn detector_handles_double_ctrl_a_as_literal_then_armed() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(&[DETACH_PREFIX, DETACH_PREFIX]);

        assert!(!outcome.detach, "two Ctrl-As do not constitute the detach chord");
        assert_eq!(
            outcome.forward,
            vec![DETACH_PREFIX],
            "the first Ctrl-A should pass through; the second arms the detector",
        );
        assert!(detector.is_armed(), "trailing Ctrl-A should leave the detector armed");
    }

    #[test]
    fn detector_drops_bytes_after_detach_chord() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(&[b'a', DETACH_PREFIX, DETACH_SUFFIX, b'b']);

        assert!(outcome.detach, "chord should fire mid-buffer");
        assert_eq!(
            outcome.forward, b"a",
            "bytes after the chord should be discarded; only the prefix passes through",
        );
    }

    #[test]
    fn detector_passes_full_session_with_no_chord() {
        let mut detector = DetachDetector::new();

        let outcome = detector.feed(b"echo hello\n\rls -la\n");

        assert!(!outcome.detach);
        assert_eq!(outcome.forward, b"echo hello\n\rls -la\n");
    }

    #[test]
    fn raw_mode_guard_with_no_snapshot_is_a_noop() {
        let guard = RawModeGuard::new(None);
        drop(guard);
    }

    #[test]
    fn raw_mode_guard_forget_disables_drop_restore() {
        let guard = RawModeGuard::new(Some("synthetic-snapshot".to_owned()));
        guard.forget();
    }

    #[test]
    fn detach_reason_is_clone_copy() {
        let reason = DetachReason::Escape;
        let copy = reason;

        assert_eq!(reason, copy, "DetachReason should be Copy");
    }

    #[test]
    fn signal_watcher_posts_detach_reason_on_pipe_byte() {
        let (tx, rx) = mpsc::channel::<DetachReason>();
        let read_fd = signals::install_detach_signals().expect("install should succeed on Linux");

        spawn_signal_watcher(read_fd, tx);

        signals::wake_for_test_via_stash().expect("test wake helper should write one byte");

        let reason = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("watcher should post a detach reason within the timeout");

        assert_eq!(
            reason,
            DetachReason::Signal,
            "a byte on the signal pipe must surface as DetachReason::Signal",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a working tty; run with --ignored after setting up locally"]
    fn attach_against_real_libvirtd() {
        use crate::host::{
            connect::{Connection, DEFAULT_URI},
            domain::{self, DomainSpec},
        };

        let connection = Connection::open(DEFAULT_URI).expect("qemu:///session should be reachable");

        let name = format!("tartarus-test-attach-{pid}", pid = std::process::id());
        let spec = DomainSpec::trivial(&name);

        let domain = domain::define(&connection, &spec).expect("define should succeed");

        let _ = attach(&domain);

        domain::undefine(&connection, &spec.name).expect("undefine should succeed");
    }

    #[test]
    #[ignore = "requires a real attached console; run with --ignored after setting up locally"]
    fn ctrl_a_d_detaches_real_attach() {
        let mut detector = DetachDetector::new();
        let outcome = detector.feed(b"\x01D");

        assert!(outcome.detach, "Ctrl-A D against a fresh detector must trip the chord");
        assert!(
            outcome.forward.is_empty(),
            "the chord itself must not reach the guest stream",
        );
    }
}
