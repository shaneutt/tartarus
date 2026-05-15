//! Shared lifecycle helpers: action polling, server lookup,
//! errors that span more than one entry point.

use std::{thread, time::Duration};

use crate::api::{self, Client, actions, servers};

// -----------------------------------------------------------------------------
// LifecycleError
// -----------------------------------------------------------------------------

/// Failure modes shared by every Hetzner lifecycle step.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// Asynchronous Hetzner action did not finish before the
    /// configured timeout.
    #[error("Hetzner action {action_id} ({label}) did not finish within {seconds}s; last status: {status}")]
    ActionTimeout {
        /// Hetzner action ID.
        action_id: u64,

        /// What we were doing (`server create`, `volume attach`, ...).
        label: &'static str,

        /// Last seen `status` field.
        status: String,

        /// Timeout in seconds.
        seconds: u64,
    },

    /// Asynchronous Hetzner action came back with `status = "error"`.
    #[error("Hetzner action {action_id} ({label}) failed: {code} — {message}")]
    ActionFailed {
        /// Hetzner action ID.
        action_id: u64,

        /// Stable error code from Hetzner.
        code: String,

        /// What we were doing.
        label: &'static str,

        /// Human-readable detail.
        message: String,
    },

    /// `tartarus stop foo` / `tartarus resume foo` / `tartarus
    /// destroy foo` was given a `foo` that no Hetzner server
    /// (matching the Tartarus label selector) carries.
    #[error("no Hetzner-side session matches `{target}`")]
    SessionNotFound {
        /// Alias or UUID the user supplied.
        target: String,
    },
}

// -----------------------------------------------------------------------------
// Action Polling
// -----------------------------------------------------------------------------

/// Default per-poll sleep when waiting for a Hetzner action.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Default total timeout for a Hetzner action.
const POLL_TIMEOUT_SECS: u64 = 300;

/// Wait until `action_id` reports `success` (Ok) or `error` (Err).
pub fn wait_for(client: &Client, action_id: u64, label: &'static str) -> Result<(), crate::Error> {
    wait_for_with_timeout(client, action_id, label, POLL_TIMEOUT_SECS)
}

/// [`wait_for`] with an explicit total timeout, in seconds.
pub fn wait_for_with_timeout(
    client: &Client,
    action_id: u64,
    label: &'static str,
    timeout_secs: u64,
) -> Result<(), crate::Error> {
    let start = std::time::Instant::now();
    loop {
        let action = actions::get(client, action_id)?;
        match action.status.as_str() {
            "success" => return Ok(()),
            "error" => {
                let (code, message) = action
                    .error
                    .map(|e| (e.code, e.message))
                    .unwrap_or_else(|| ("unknown".to_owned(), "no error body".to_owned()));
                return Err(LifecycleError::ActionFailed {
                    action_id,
                    code,
                    label,
                    message,
                }
                .into());
            },
            _ => {
                if start.elapsed().as_secs() >= timeout_secs {
                    return Err(LifecycleError::ActionTimeout {
                        action_id,
                        label,
                        seconds: timeout_secs,
                        status: action.status,
                    }
                    .into());
                }
                thread::sleep(POLL_INTERVAL);
            },
        }
    }
}

// -----------------------------------------------------------------------------
// Server Lookup
// -----------------------------------------------------------------------------

/// Find the Tartarus-owned server whose `tartarus.alias` or
/// `tartarus.uuid` label matches `target`.
pub fn find_server(client: &Client, target: &str) -> Result<servers::Server, crate::Error> {
    let response = servers::list(client, Some(crate::session::labels::SELECTOR_ALL))?;
    for srv in response.servers {
        if matches_target(&srv, target) {
            return Ok(srv);
        }
    }
    Err(LifecycleError::SessionNotFound {
        target: target.to_owned(),
    }
    .into())
}

/// True iff `target` matches the server's alias label, UUID label,
/// or its full UUID prefix.
fn matches_target(server: &servers::Server, target: &str) -> bool {
    if let Some(alias) = server.labels.get(crate::session::labels::LABEL_ALIAS)
        && alias == target
    {
        return true;
    }
    if let Some(uuid) = server.labels.get(crate::session::labels::LABEL_UUID)
        && (uuid == target || uuid.starts_with(target))
    {
        return true;
    }
    false
}

// -----------------------------------------------------------------------------
// Re-exports
// -----------------------------------------------------------------------------

// Re-export ApiError on the lifecycle surface so callers using
// `Result<_, lifecycle::Error>` can match a single import.
pub use api::ApiError;

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::session::labels;

    #[test]
    fn matches_target_hits_alias_label() {
        let server = make_server(Some("fix-bug"), "abc-12345");
        assert!(matches_target(&server, "fix-bug"));
        assert!(!matches_target(&server, "other-alias"));
    }

    #[test]
    fn matches_target_hits_uuid_prefix() {
        let server = make_server(None, "abc-12345");
        assert!(matches_target(&server, "abc-12345"));
        assert!(matches_target(&server, "abc"));
        assert!(!matches_target(&server, "xyz"));
    }

    fn make_server(alias: Option<&str>, uuid: &str) -> servers::Server {
        let mut srv_labels: BTreeMap<String, String> = BTreeMap::new();
        srv_labels.insert(labels::LABEL_OWNED.to_owned(), "true".to_owned());
        srv_labels.insert(labels::LABEL_UUID.to_owned(), uuid.to_owned());
        if let Some(alias) = alias {
            srv_labels.insert(labels::LABEL_ALIAS.to_owned(), alias.to_owned());
        }
        servers::Server {
            id: 1,
            name: "tartarus-1".to_owned(),
            status: "running".to_owned(),
            public_net: None,
            labels: srv_labels,
        }
    }
}
