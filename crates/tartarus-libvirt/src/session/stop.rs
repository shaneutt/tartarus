//! `tartarus stop`: graceful shutdown with timeout, falling back to
//! forced destroy. Metadata and overlay survive on disk.

use std::{path::Path, time::Duration};

use tartarus_provider::{
    StopOutcome, paths,
    session::{
        identity,
        metadata::{self, Metadata},
    },
};

use crate::{
    config::Config,
    error::{Error, Result},
    gpu::driver::{KernelSysfs, release_with_receipt},
    host::{connect::Connection, domain, error::HostError},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// How long to wait for graceful shutdown before force-destroying.
const GRACEFUL_TIMEOUT_SECS: u64 = 60;

/// Run `tartarus stop <alias|uuid>`.
pub fn run(config: &Config, target: &str) -> Result<StopOutcome> {
    let resolved = identity::resolve(target)?;

    tracing::info!(uuid = %resolved.uuid, "stopping session (graceful shutdown)");

    let connection = Connection::open(&config.network_uri)?;
    let outcome = dispatch_shutdown(
        &resolved.uuid,
        |uuid| domain::shutdown(&connection, uuid, Duration::from_secs(GRACEFUL_TIMEOUT_SECS)),
        |uuid| domain::destroy(&connection, uuid),
    );

    if let Err(release_err) = release_gpu_borrow(&resolved.uuid) {
        tracing::warn!(
            uuid = %resolved.uuid,
            err = %release_err,
            "session stopped but GPU borrow release failed; run `tartarus host gpu release <BDF>` to retry",
        );
    }

    outcome
}

/// Branch the graceful-shutdown vs force-destroy logic onto the
/// supplied callbacks. Pure: the libvirt-side wiring lives in
/// [`run`], so tests substitute scripted closures.
fn dispatch_shutdown<S, D>(uuid: &str, shutdown: S, destroy: D) -> Result<StopOutcome>
where
    S: FnOnce(&str) -> Result<()>,
    D: FnOnce(&str) -> Result<()>,
{
    match shutdown(uuid) {
        Ok(()) => {
            tracing::info!(%uuid, "session shut down gracefully");
            Ok(StopOutcome {
                force_stopped: false,
                name: uuid.to_owned(),
            })
        },
        Err(Error::Host(HostError::ShutdownTimeout { name, .. })) => {
            tracing::warn!(uuid = %name, "graceful shutdown timed out; forcing destroy");
            destroy(&name)?;
            Ok(StopOutcome {
                force_stopped: true,
                name,
            })
        },
        Err(other) => Err(other),
    }
}

// -----------------------------------------------------------------------------
// GPU Borrow Release
// -----------------------------------------------------------------------------

/// Release the GPU borrow if one was recorded. No-op otherwise.
/// Errors are non-fatal.
fn release_gpu_borrow(uuid: &str) -> Result<()> {
    let metadata_path: std::path::PathBuf = paths::sessions_by_uuid_dir()?
        .join(uuid)
        .join(metadata::METADATA_FILE_NAME);
    if !metadata_path.exists() {
        return Ok(());
    }

    let mut metadata = Metadata::load(&metadata_path)?;
    let Some(record) = metadata.gpu_borrow.take() else {
        return Ok(());
    };

    let receipt = crate::gpu::driver::record_into_receipt(record)?;
    let io = KernelSysfs;
    release_with_receipt(&io, &receipt)?;
    metadata.save(Path::new(&metadata_path))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_outcome_force_stopped_round_trips() {
        let outcome = StopOutcome {
            force_stopped: true,
            name: "alpha".to_owned(),
        };

        assert!(outcome.force_stopped, "the force-stopped flag should round-trip");
        assert_eq!(outcome.name, "alpha", "the identifier should round-trip");
    }

    #[test]
    fn stop_outcome_graceful_round_trips() {
        let outcome = StopOutcome {
            force_stopped: false,
            name: "beta".to_owned(),
        };

        assert!(
            !outcome.force_stopped,
            "graceful shutdown should leave force_stopped unset",
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_shutdown Branching
    // -----------------------------------------------------------------------

    #[test]
    fn dispatch_shutdown_returns_graceful_outcome_when_shutdown_succeeds() {
        let outcome = dispatch_shutdown(
            "abc-123",
            |_| Ok(()),
            |_| panic!("destroy must not run on the graceful path"),
        )
        .expect("graceful path should succeed");
        assert!(!outcome.force_stopped);
        assert_eq!(outcome.name, "abc-123");
    }

    #[test]
    fn dispatch_shutdown_falls_through_to_destroy_on_timeout() {
        use std::cell::Cell;

        let destroy_called = Cell::new(false);
        let outcome = dispatch_shutdown(
            "abc-456",
            |name| {
                Err(Error::Host(HostError::ShutdownTimeout {
                    name: name.to_owned(),
                    seconds: 60,
                }))
            },
            |_name| {
                destroy_called.set(true);
                Ok(())
            },
        )
        .expect("force-destroy path should succeed");
        assert!(outcome.force_stopped, "timeout fallback should flip force_stopped");
        assert_eq!(outcome.name, "abc-456");
        assert!(
            destroy_called.get(),
            "destroy callback should fire when shutdown times out"
        );
    }

    #[test]
    fn dispatch_shutdown_propagates_non_timeout_errors() {
        let err = dispatch_shutdown(
            "abc-789",
            |_| Err(Error::Host(HostError::AgentChannelMissing)),
            |_| panic!("destroy must not fire when shutdown errors with a non-timeout"),
        )
        .expect_err("non-timeout error should propagate");
        match err {
            Error::Host(HostError::AgentChannelMissing) => {},
            other => panic!("expected AgentChannelMissing, got {other:?}"),
        }
    }

    #[test]
    fn dispatch_shutdown_propagates_destroy_failure() {
        let err = dispatch_shutdown(
            "abc-000",
            |name| {
                Err(Error::Host(HostError::ShutdownTimeout {
                    name: name.to_owned(),
                    seconds: 60,
                }))
            },
            |_| {
                Err(Error::Host(HostError::AgentProtocol {
                    detail: "synthetic destroy failure".to_owned(),
                }))
            },
        )
        .expect_err("destroy failure should propagate");
        match err {
            Error::Host(HostError::AgentProtocol { detail }) => {
                assert_eq!(detail, "synthetic destroy failure");
            },
            other => panic!("expected AgentProtocol, got {other:?}"),
        }
    }
}
