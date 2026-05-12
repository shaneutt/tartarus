//! `tartarus ssh <alias|uuid>`: attach to a running session over SSH.
//!
//! Captures the guest host key on first attach, then execs `ssh`
//! with strict host-key checking against the per-session
//! `known_hosts`.

use std::{
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};

use crate::{
    config::Config,
    error::Result,
    host::{agent::Agent, connect::Connection},
    paths,
    session::{
        error::SessionError,
        identity,
        metadata::{self, Metadata},
        ssh::{self, SessionSshLayout},
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Guest-side ed25519 host key path.
const GUEST_HOST_KEY_PATH: &str = "/etc/ssh/ssh_host_ed25519_key.pub";

/// Maximum wait for the guest to expose its host key.
const HOST_KEY_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

/// Per-call timeout for the guest-agent file_read probe.
const HOST_KEY_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

// -----------------------------------------------------------------------------
// AttachRequest
// -----------------------------------------------------------------------------

/// Caller-supplied parameters for [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachRequest {
    /// Alias or UUID identifying the session to attach to.
    pub target: String,

    /// Arguments forwarded to `ssh` (after `--`).
    pub trailing_ssh_args: Vec<String>,
}

/// Result of a successful attach setup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttachOutcome {
    /// Loopback host port the session forwards.
    pub host_port: u16,

    /// Resolved session UUID.
    pub uuid: String,
}

/// Run `tartarus ssh <target>`.
pub fn run(config: &Config, request: &AttachRequest) -> Result<AttachOutcome> {
    let resolved = identity::resolve(&request.target)?;

    let metadata_path = resolved.directory.join(metadata::METADATA_FILE_NAME);
    let metadata = Metadata::load(&metadata_path)?;
    let port = metadata.ssh_port.ok_or_else(|| SessionError::SshHostKeyUnavailable {
        attempts: 0,
        detail: "session metadata has no `ssh_port`; the session predates M2 SSH attach".to_owned(),
    })?;

    let layout = SessionSshLayout::for_session(&resolved.directory);

    if !layout.known_hosts.exists() {
        capture_host_key(config, &resolved.uuid, &layout, port)?;
    }

    exec_ssh(config, &layout, port, &resolved.uuid, &request.trailing_ssh_args)
}

// -----------------------------------------------------------------------------
// Host Key Capture
// -----------------------------------------------------------------------------

/// Read the guest host key via `qemu-guest-agent` and persist it.
fn capture_host_key(config: &Config, uuid: &str, layout: &SessionSshLayout, port: u16) -> Result<()> {
    let connection = Connection::open(&config.network_uri)?;
    let domain = virt::domain::Domain::lookup_by_name(connection.inner(), uuid).map_err(|source| {
        crate::host::error::HostError::DomainOperation {
            operation: "lookup_by_name",
            source,
        }
    })?;
    let agent = Agent::new(domain);

    let deadline = Instant::now() + HOST_KEY_TOTAL_TIMEOUT;
    let mut attempts: u32 = 0;
    let mut last_detail = String::new();

    while Instant::now() < deadline {
        attempts += 1;
        match agent.file_read(GUEST_HOST_KEY_PATH, HOST_KEY_PROBE_TIMEOUT) {
            Ok(bytes) => {
                let line = std::str::from_utf8(&bytes)
                    .map_err(|err| SessionError::SshHostKeyUnavailable {
                        attempts,
                        detail: format!("guest host key was not valid utf-8: {err}"),
                    })?
                    .trim_end_matches('\n');

                let entry = ssh::known_hosts_entry(port, line);
                ssh::write_known_hosts(layout, entry.trim_end_matches('\n'))?;
                tracing::info!(uuid, port, attempts, "captured guest host key");
                return Ok(());
            },
            Err(err) => {
                last_detail = format!("{err}");
                std::thread::sleep(Duration::from_secs(2));
            },
        }
    }

    Err(SessionError::SshHostKeyUnavailable {
        attempts,
        detail: last_detail,
    }
    .into())
}

/// Exec `ssh` with per-session credentials. Returns only on failure.
fn exec_ssh(
    config: &Config,
    layout: &SessionSshLayout,
    port: u16,
    uuid: &str,
    trailing: &[String],
) -> Result<AttachOutcome> {
    let mut cmd = Command::new("ssh");
    cmd.arg("-p")
        .arg(port.to_string())
        .arg("-o")
        .arg({
            let known_hosts = layout.known_hosts.display();
            format!("UserKnownHostsFile={known_hosts}")
        })
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg({
            let identity = layout.private_key.display();
            format!("IdentityFile={identity}")
        })
        .arg("-o")
        .arg("IdentitiesOnly=yes");

    let username = invoking_username(config)?;
    let target = format!("{username}@127.0.0.1");
    cmd.arg(target);

    for arg in trailing {
        cmd.arg(arg);
    }

    tracing::info!(uuid, port, "exec ssh");
    let status = cmd.status()?;

    let outcome = AttachOutcome {
        host_port: port,
        uuid: uuid.to_owned(),
    };

    if status.success() {
        Ok(outcome)
    } else {
        Err(SessionError::SshHostKeyUnavailable {
            attempts: 0,
            detail: format!("ssh exited with {status}"),
        }
        .into())
    }
}

/// Derive the in-guest username from the host invoker's identity.
fn invoking_username(_config: &Config) -> Result<String> {
    let user = crate::host_user::current()?;
    Ok(user.username)
}

/// Shorthand for [`paths::sessions_by_uuid_dir`].
#[allow(dead_code)]
fn sessions_root() -> Result<PathBuf> {
    paths::sessions_by_uuid_dir()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_request_round_trips_target_and_trailing_args() {
        let request = AttachRequest {
            target: "fix-bug".to_owned(),
            trailing_ssh_args: vec!["-L".to_owned(), "8080:localhost:8080".to_owned()],
        };

        assert_eq!(request.target, "fix-bug");
        assert_eq!(request.trailing_ssh_args.len(), 2);
    }

    #[test]
    fn attach_outcome_round_trips_uuid_and_port() {
        let outcome = AttachOutcome {
            host_port: 32000,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
        };

        assert_eq!(outcome.host_port, 32000);
        assert!(outcome.uuid.starts_with("11111111"));
    }
}
