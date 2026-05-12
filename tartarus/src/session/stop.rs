//! `tartarus stop`: graceful shutdown with timeout, falling back to
//! forced destroy. Metadata and overlay survive on disk.

use std::{path::Path, time::Duration};

use crate::{
    config::Config,
    error::{Error, Result},
    gpu::driver::{KernelSysfs, release_with_receipt},
    host::{connect::Connection, domain, error::HostError},
    paths,
    session::{
        identity,
        metadata::{self, Metadata},
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// How long to wait for graceful shutdown before force-destroying.
const GRACEFUL_TIMEOUT_SECS: u64 = 60;

// -----------------------------------------------------------------------------
// StopOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopOutcome {
    /// True when graceful shutdown timed out and force-destroy was used.
    pub force_stopped: bool,

    /// Session identifier for the success message.
    pub name: String,
}

/// Run `tartarus stop <alias|uuid>`.
pub fn run(config: &Config, target: &str) -> Result<StopOutcome> {
    let resolved = identity::resolve(target)?;

    tracing::info!(uuid = %resolved.uuid, "stopping session (graceful shutdown)");

    let connection = Connection::open(&config.network_uri)?;
    let outcome = match domain::shutdown(&connection, &resolved.uuid, Duration::from_secs(GRACEFUL_TIMEOUT_SECS)) {
        Ok(()) => {
            tracing::info!(uuid = %resolved.uuid, "session shut down gracefully");
            Ok(StopOutcome {
                force_stopped: false,
                name: resolved.uuid.clone(),
            })
        },
        Err(Error::Host(HostError::ShutdownTimeout { name, .. })) => {
            tracing::warn!(uuid = %name, "graceful shutdown timed out; forcing destroy");
            domain::destroy(&connection, &name)?;
            Ok(StopOutcome {
                force_stopped: true,
                name,
            })
        },
        Err(other) => return Err(other),
    };

    if let Err(release_err) = release_gpu_borrow(&resolved.uuid) {
        tracing::warn!(
            uuid = %resolved.uuid,
            err = %release_err,
            "session stopped but GPU borrow release failed; run `tartarus host gpu release <BDF>` to retry",
        );
    }

    outcome
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

    let receipt = record.into_receipt()?;
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
}
