//! Loopback-only port allocation for the per-session SSH hostfwd.
//!
//! Skips ports already claimed by other sessions (metadata-side
//! dedup), then probes via `TcpListener::bind`. Loopback only.

use std::{collections::HashSet, net::TcpListener};

use crate::{
    error::Result,
    paths,
    session::{
        error::SessionError,
        metadata::{self, Metadata},
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default port range Tartarus searches for a free SSH hostfwd port.
pub const DEFAULT_PORT_START: u16 = 32000;

/// Inclusive upper bound for the default range.
pub const DEFAULT_PORT_END: u16 = 32999;

// -----------------------------------------------------------------------------
// Port Allocation
// -----------------------------------------------------------------------------

/// Allocate an unused loopback port in the default range.
pub fn allocate_loopback_port() -> Result<u16> {
    allocate_loopback_port_in_range(DEFAULT_PORT_START, DEFAULT_PORT_END)
}

/// [`allocate_loopback_port`] with a custom range (for testing).
pub fn allocate_loopback_port_in_range(start: u16, end: u16) -> Result<u16> {
    let in_use = ports_in_use_by_other_sessions()?;
    for candidate in start..=end {
        if in_use.contains(&candidate) {
            continue;
        }
        if probe_port_is_free(candidate) {
            return Ok(candidate);
        }
    }
    Err(SessionError::SshPortExhausted { start, end }.into())
}

// -----------------------------------------------------------------------------
// Port Scanning
// -----------------------------------------------------------------------------

/// Collect every `ssh_port` from existing session metadata.
fn ports_in_use_by_other_sessions() -> Result<HashSet<u16>> {
    let root = paths::sessions_by_uuid_dir()?;
    if !root.exists() {
        return Ok(HashSet::new());
    }

    let mut found: HashSet<u16> = HashSet::new();
    let entries = std::fs::read_dir(&root)?;
    for entry in entries.flatten() {
        let path = entry.path().join(metadata::METADATA_FILE_NAME);
        if let Ok(m) = Metadata::load(&path)
            && let Some(port) = m.ssh_port
        {
            found.insert(port);
        }
    }
    Ok(found)
}

/// True iff `127.0.0.1:<port>` can be bound (listener dropped
/// immediately).
fn probe_port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn allocate_loopback_port_returns_a_port_in_the_requested_range() {
        let _g = serial();
        // Use a tiny user-space-friendly window to keep the test fast.
        let port = allocate_loopback_port_in_range(45100, 45110).expect("range should have a free port");
        assert!(
            (45100..=45110).contains(&port),
            "allocated port {port} should land in the requested range",
        );
    }

    #[test]
    fn allocate_loopback_port_skips_a_taken_port() {
        let _g = serial();
        let blocker = TcpListener::bind(("127.0.0.1", 45200)).expect("bind blocker");
        let port = allocate_loopback_port_in_range(45200, 45205).expect("range still has free ports");
        assert_ne!(port, 45200, "must skip the port held by the blocker");
        drop(blocker);
    }

    #[test]
    fn allocate_loopback_port_errors_when_range_is_fully_taken() {
        let _g = serial();
        let _hold0 = TcpListener::bind(("127.0.0.1", 45300)).expect("bind 1");
        let _hold1 = TcpListener::bind(("127.0.0.1", 45301)).expect("bind 2");

        let err = allocate_loopback_port_in_range(45300, 45301).expect_err("range fully blocked");

        match err {
            crate::error::Error::Session(SessionError::SshPortExhausted { start, end }) => {
                assert_eq!(start, 45300);
                assert_eq!(end, 45301);
            },
            other => panic!("expected SshPortExhausted, got {other:?}"),
        }
    }
}
