//! Tartarus: a security sandbox for running AI coding agents inside disposable
//! QEMU/KVM virtual machines on local or remote Linux hosts.
//!
//! The crate is a thin Rust layer over libvirt (via the [`virt`] crate). Local
//! and remote hosts are both reached through libvirt's native transports —
//! Tartarus does not implement its own remote-execution layer.

pub mod auth;
pub mod cli;
pub mod config;
pub mod disk;
pub mod doctor;
pub mod error;
pub mod gpu;
pub mod host;
pub mod host_user;
pub mod logging;
pub mod paths;
pub mod seed;
pub mod session;
pub mod time;

// ---------------------------------------------------------------------------
// Root Guard
// ---------------------------------------------------------------------------

/// Refuse to proceed when the invoking user has `euid == 0`.
pub fn refuse_root() -> error::Result<()> {
    #[cfg(unix)]
    if effective_uid_is_zero() {
        return Err(error::Error::RunningAsRoot);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// UID Inspection
// ---------------------------------------------------------------------------

/// True iff the process is running with `euid == 0`.
#[cfg(unix)]
pub(crate) fn effective_uid_is_zero() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };

    status
        .lines()
        .filter_map(|line| line.strip_prefix("Uid:"))
        .filter_map(|rest| rest.split_whitespace().nth(1).map(str::to_owned))
        .next()
        .is_some_and(|effective| effective == "0")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuse_root_passes_for_non_root_user() {
        if cfg!(unix) && effective_uid_is_zero() {
            return;
        }

        refuse_root().expect("a non-root caller should pass the root refusal check");
    }

    #[test]
    #[cfg(unix)]
    fn effective_uid_helper_agrees_with_proc_status() {
        let observed = effective_uid_is_zero();

        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let truth = status.lines().any(|line| {
            line.strip_prefix("Uid:")
                .and_then(|rest| rest.split_whitespace().nth(1))
                .is_some_and(|effective| effective == "0")
        });

        assert_eq!(observed, truth, "helper should agree with /proc/self/status");
    }
}
