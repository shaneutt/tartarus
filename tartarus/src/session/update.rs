//! `tartarus update <session>`: drive the in-guest update orchestrator.
//!
//! Auto-detects running vs. stopped sessions. Running sessions
//! dispatch the orchestrator in place; stopped sessions are booted,
//! updated, and shut down.

use std::time::{Duration, Instant};

use crate::{
    config::Config,
    error::{Error, Result},
    host::{
        agent::Agent,
        connect::Connection,
        domain::{self},
        error::HostError,
    },
    session::{
        error::SessionError,
        identity::{self, ResolvedSession},
    },
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// In-guest update orchestrator path.
pub const UPDATE_SCRIPT_PATH: &str = "/usr/local/bin/tartarus-update.sh";

/// Per-call timeout for agent operations (five minutes).
const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum wait for the in-guest agent after starting a stopped domain.
const AGENT_BOOT_GRACE: Duration = Duration::from_secs(120);

/// Polling interval while waiting for `tartarus-update.sh` to exit.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Hard ceiling on orchestrator total runtime (30 minutes).
const UPDATE_TOTAL_RUNTIME_LIMIT: Duration = Duration::from_secs(30 * 60);

/// Polling interval while waiting for the agent to come online after
/// a stopped-mode boot.
const AGENT_PING_INTERVAL: Duration = Duration::from_secs(2);

/// Graceful shutdown timeout used for the stopped-mode shutdown step.
const STOPPED_MODE_SHUTDOWN_SECS: u64 = 60;

// ---------------------------------------------------------------------------
// UpdateOutcome
// ---------------------------------------------------------------------------

/// Whether the session was running at update time (and the orchestrator
/// was dispatched directly) or stopped (and the host first booted it).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateMode {
    /// Session was already running. Update dispatched in place.
    Running,

    /// Session was stopped. Host booted it, ran the update, shut it down.
    Stopped,
}

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateOutcome {
    /// Whether the session was already running or had to be booted.
    pub mode: UpdateMode,

    /// Per-step results, in dispatch order.
    pub steps: Vec<UpdateStep>,

    /// Captured stdout from the orchestrator.
    pub stdout: Vec<u8>,

    /// Session UUID that was updated.
    pub uuid: String,
}

/// One sub-step's result in a [`UpdateOutcome`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateStep {
    /// Short identifier for the step (e.g. `"orchestrator"`).
    pub name: String,

    /// Exit code, or `None` if the process exited via signal.
    pub exit_code: Option<i64>,

    /// Captured stderr (raw bytes).
    pub stderr: Vec<u8>,

    /// Captured stdout (raw bytes).
    pub stdout: Vec<u8>,
}

/// Run `tartarus update <alias|uuid>`.
///
/// Auto-detects running vs. stopped and routes accordingly.
pub fn run(config: &Config, target: &str) -> Result<UpdateOutcome> {
    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, "update: resolving session");

    let connection = Connection::open(&config.network_uri)?;
    let was_running = is_active(&connection, &resolved.uuid)?;

    let mode = if was_running {
        UpdateMode::Running
    } else {
        UpdateMode::Stopped
    };

    tracing::info!(uuid = %resolved.uuid, ?mode, "update: routing");

    match mode {
        UpdateMode::Running => run_running(&connection, &resolved),
        UpdateMode::Stopped => run_stopped(&connection, &resolved),
    }
}

/// Drive the running-session update path.
pub fn run_running(connection: &Connection, resolved: &ResolvedSession) -> Result<UpdateOutcome> {
    let domain = domain::lookup(connection, &resolved.uuid)?;
    let agent = Agent::new(domain);

    let step = dispatch_and_wait(&agent)?;
    let stdout = step.stdout.clone();

    Ok(UpdateOutcome {
        mode: UpdateMode::Running,
        steps: vec![step],
        stdout,
        uuid: resolved.uuid.clone(),
    })
}

/// Drive the stopped-session update path: boot, update, shut down.
pub fn run_stopped(connection: &Connection, resolved: &ResolvedSession) -> Result<UpdateOutcome> {
    domain::start(connection, &resolved.uuid)?;
    tracing::info!(uuid = %resolved.uuid, "update: domain started for stopped-mode update");

    let result = (|| -> Result<UpdateStep> {
        let domain = domain::lookup(connection, &resolved.uuid)?;
        let agent = Agent::new(domain);

        wait_for_agent(&agent, AGENT_BOOT_GRACE)?;
        quiesce_claude_unit(&agent);
        dispatch_and_wait(&agent)
    })();

    let shutdown_result = domain::shutdown(
        connection,
        &resolved.uuid,
        Duration::from_secs(STOPPED_MODE_SHUTDOWN_SECS),
    );

    match (&result, &shutdown_result) {
        (Ok(_), Ok(())) => {},
        (Err(_), _) => tracing::warn!("update body failed; attempted post-update shutdown anyway"),
        (Ok(_), Err(err)) => tracing::warn!(%err, "update succeeded but shutdown timed out; force-destroying"),
    }

    if shutdown_result.is_err()
        && let Err(err) = domain::destroy(connection, &resolved.uuid)
    {
        tracing::warn!(%err, "force-destroy after timed-out shutdown reported a libvirt failure");
    }

    let step = result?;
    let stdout = step.stdout.clone();

    Ok(UpdateOutcome {
        mode: UpdateMode::Stopped,
        steps: vec![step],
        stdout,
        uuid: resolved.uuid.clone(),
    })
}

// ---------------------------------------------------------------------------
// Orchestration
// ---------------------------------------------------------------------------

/// Stop `tartarus-claude@*.service` before a stopped-mode update.
/// Failure is logged but not fatal.
fn quiesce_claude_unit(agent: &Agent) {
    let outcome = (|| -> Result<()> {
        let handle = agent.exec(
            "/usr/bin/systemctl",
            &["stop", "tartarus-claude@*.service"],
            false,
            AGENT_CALL_TIMEOUT,
        )?;

        let deadline = std::time::Instant::now() + AGENT_CALL_TIMEOUT;
        loop {
            let status = agent.exec_status(&handle, AGENT_CALL_TIMEOUT)?;
            if status.exited {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    })();

    match outcome {
        Ok(()) => tracing::info!("update: quiesced tartarus-claude unit before orchestrator"),
        Err(err) => tracing::warn!(%err, "update: could not quiesce tartarus-claude unit; continuing"),
    }
}

/// Dispatch `tartarus-update.sh` via the agent and poll to completion.
fn dispatch_and_wait(agent: &Agent) -> Result<UpdateStep> {
    tracing::info!(script = UPDATE_SCRIPT_PATH, "update: dispatching orchestrator");

    let handle = agent.exec(UPDATE_SCRIPT_PATH, &[], true, AGENT_CALL_TIMEOUT)?;
    let deadline = Instant::now() + UPDATE_TOTAL_RUNTIME_LIMIT;

    loop {
        let status = agent.exec_status(&handle, AGENT_CALL_TIMEOUT)?;
        if status.exited {
            let stdout = status.stdout.unwrap_or_default();
            let stderr = status.stderr.unwrap_or_default();
            let step = UpdateStep {
                name: "orchestrator".to_owned(),
                exit_code: status.exit_code,
                stderr,
                stdout,
            };
            match status.exit_code.unwrap_or(0) {
                0 => {
                    tracing::info!("update: orchestrator exited cleanly");
                    return Ok(step);
                },
                code => {
                    return Err(HostError::AgentExecFailed {
                        code,
                        detail: "tartarus-update.sh exited non-zero",
                    }
                    .into());
                },
            }
        }
        if Instant::now() >= deadline {
            return Err(HostError::AgentExecFailed {
                code: -1,
                detail: "tartarus-update.sh did not exit within the runtime limit",
            }
            .into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// True iff `name`'s libvirt domain is currently active.
fn is_active(connection: &Connection, name: &str) -> Result<bool> {
    let domain = domain::lookup(connection, name)?;
    domain.is_active().map_err(|source| {
        HostError::DomainOperation {
            operation: "is_active",
            source,
        }
        .into()
    })
}

/// Block until [`Agent::ping`] succeeds, or `grace` elapses.
fn wait_for_agent(agent: &Agent, grace: Duration) -> Result<()> {
    let deadline = Instant::now() + grace;
    let mut last_err: Option<Error> = None;

    while Instant::now() < deadline {
        match agent.ping(AGENT_CALL_TIMEOUT) {
            Ok(()) => {
                tracing::debug!("update: qemu-ga responded; ready to dispatch");
                return Ok(());
            },
            Err(err) => {
                tracing::trace!(%err, "update: qemu-ga not yet responsive; retrying");
                last_err = Some(err);
                std::thread::sleep(AGENT_PING_INTERVAL);
            },
        }
    }

    Err(last_err.unwrap_or_else(|| {
        Error::from(SessionError::NotFound {
            target: "qemu-guest-agent did not come online within the boot grace".to_owned(),
        })
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_mode_round_trips_through_clone() {
        let mode = UpdateMode::Running;
        let cloned = mode;
        assert_eq!(mode, cloned, "UpdateMode should be Copy + Eq",);
    }

    #[test]
    fn update_mode_running_and_stopped_are_distinct() {
        assert_ne!(
            UpdateMode::Running,
            UpdateMode::Stopped,
            "Running and Stopped must be distinguishable",
        );
    }

    #[test]
    fn update_outcome_round_trips_through_struct() {
        let step = UpdateStep {
            name: "orchestrator".to_owned(),
            exit_code: Some(0_i64),
            stderr: Vec::new(),
            stdout: b"ok".to_vec(),
        };
        let outcome = UpdateOutcome {
            mode: UpdateMode::Stopped,
            steps: vec![step.clone()],
            stdout: step.stdout.clone(),
            uuid: "abcd".to_owned(),
        };

        assert_eq!(outcome.uuid, "abcd", "uuid should round-trip");
        assert_eq!(outcome.mode, UpdateMode::Stopped, "mode should round-trip");
        assert_eq!(outcome.stdout, b"ok".to_vec(), "stdout should round-trip");
        assert_eq!(outcome.steps.len(), 1, "steps should carry the orchestrator entry");
        assert_eq!(outcome.steps[0].name, "orchestrator", "step name should round-trip");
        assert_eq!(
            outcome.steps[0].exit_code,
            Some(0_i64),
            "step exit_code should round-trip"
        );
    }

    #[test]
    fn update_script_path_matches_in_guest_layout() {
        assert_eq!(
            UPDATE_SCRIPT_PATH, "/usr/local/bin/tartarus-update.sh",
            "the host-side path constant must match what the layering step installs",
        );
    }

    #[test]
    fn agent_call_timeout_is_under_an_hour() {
        assert!(
            AGENT_CALL_TIMEOUT < Duration::from_secs(3_600),
            "per-call timeout should bound a misbehaving guest from hanging the host indefinitely",
        );
    }

    #[test]
    fn agent_boot_grace_is_at_least_thirty_seconds() {
        assert!(
            AGENT_BOOT_GRACE >= Duration::from_secs(30),
            "boot grace must be long enough for a fresh cloud-init + agent start (>=30s)",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a session with qemu-ga responding; run with --ignored after setting up locally"]
    fn end_to_end_running_path_against_real_session() {}

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a stopped session with qemu-ga responding; run with --ignored after setting up locally"]
    fn end_to_end_stopped_path_against_real_session() {}
}
