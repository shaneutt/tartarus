//! Lifecycle wrapper around [`virt::connect::Connect`].

use crate::{error::Result, host::error::HostError};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default libvirt URI Tartarus targets.
pub const DEFAULT_URI: &str = "qemu:///session";

// -----------------------------------------------------------------------------
// Connection
// -----------------------------------------------------------------------------

/// Owning wrapper around [`virt::connect::Connect`].
///
/// Closes the connection on drop.
#[derive(Debug)]
pub struct Connection {
    /// Underlying libvirt connection handle.
    inner: virt::connect::Connect,

    /// URI the connection was opened against.
    uri: String,
}

impl Connection {
    /// Open a libvirt connection at `uri`.
    ///
    /// Returns [`HostError::Connect`] on any libvirt-side open failure.
    pub fn open(uri: &str) -> Result<Self> {
        tracing::debug!(uri, "opening libvirt connection");

        let inner = virt::connect::Connect::open(Some(uri)).map_err(|source| HostError::Connect {
            source,
            uri: uri.to_owned(),
        })?;

        Ok(Self {
            inner,
            uri: uri.to_owned(),
        })
    }

    /// Borrow the underlying [`virt::connect::Connect`].
    pub fn inner(&self) -> &virt::connect::Connect {
        &self.inner
    }

    /// True iff the connection is still alive.
    pub fn is_alive(&self) -> bool {
        self.inner.is_alive().unwrap_or(false)
    }

    /// URI this connection was opened against.
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Err(err) = self.inner.close() {
            tracing::warn!(uri = %self.uri, %err, "failed to close libvirt connection cleanly");
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uri_matches_session_bus() {
        assert_eq!(
            DEFAULT_URI, "qemu:///session",
            "default URI should match the architecture spec",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
    fn open_succeeds_against_real_session_libvirtd() {
        let conn = Connection::open(DEFAULT_URI).expect("opening qemu:///session should succeed when libvirtd runs");

        assert!(conn.is_alive(), "freshly opened connection should report alive");
        assert_eq!(conn.uri(), DEFAULT_URI, "uri() should round-trip the input");
    }

    #[test]
    fn open_returns_typed_error_for_unreachable_uri() {
        let result = Connection::open("qemu+tcp://127.0.0.1:1/system");

        match result {
            Ok(_) => panic!("opening an unreachable URI should not succeed"),
            Err(crate::error::Error::Host(HostError::Connect { uri, .. })) => {
                assert_eq!(
                    uri, "qemu+tcp://127.0.0.1:1/system",
                    "uri should round-trip into the error",
                );
            },
            Err(other) => panic!("expected HostError::Connect, got {other:?}"),
        }
    }
}
