//! `tartarus resume`: re-attach to an existing session.
//!
//! Resolves the target, starts the domain if shut off, attaches the
//! serial console, and updates `last_attached_at`.

use crate::{
    config::Config,
    error::Result,
    host::{connect::Connection, console, domain, error::HostError},
    session::{
        identity,
        metadata::{self, Metadata},
    },
};

// ---------------------------------------------------------------------------
// ResumeOutcome
// ---------------------------------------------------------------------------

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeOutcome {
    /// Whether the domain was started (vs. already running).
    pub started_from_shutoff: bool,

    /// Session UUID that was resumed.
    pub uuid: String,
}

/// Run `tartarus resume <alias|uuid>`.
pub fn run(config: &Config, target: &str) -> Result<ResumeOutcome> {
    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, "resuming session");

    let connection = Connection::open(&config.network_uri)?;
    let started_from_shutoff = ensure_running(&connection, &resolved.uuid)?;

    let domain = domain::lookup(&connection, &resolved.uuid)?;
    let _reason = console::attach(&domain)?;

    update_last_attached(&resolved.directory)?;

    Ok(ResumeOutcome {
        started_from_shutoff,
        uuid: resolved.uuid,
    })
}

// ---------------------------------------------------------------------------
// Domain State
// ---------------------------------------------------------------------------

/// Start the domain if shut off; no-op if already running.
fn ensure_running(connection: &Connection, uuid: &str) -> Result<bool> {
    let domain = domain::lookup(connection, uuid)?;

    let active = domain.is_active().map_err(|source| HostError::DomainOperation {
        operation: "is_active",
        source,
    })?;

    if active {
        tracing::debug!(uuid, "session already running; attaching to live console");
        return Ok(false);
    }

    tracing::info!(uuid, "session is shut off; starting before attach");
    domain::start(connection, uuid)?;
    Ok(true)
}

/// Refresh `last_attached_at` in `metadata.json`. Tolerates missing
/// or unreadable metadata.
fn update_last_attached(session_dir: &std::path::Path) -> Result<()> {
    let path = session_dir.join(metadata::METADATA_FILE_NAME);
    let mut meta = match Metadata::load(&path) {
        Ok(meta) => meta,
        Err(err) => {
            tracing::warn!(?path, %err, "could not load metadata.json; skipping last_attached_at update");
            return Ok(());
        },
    };

    meta.last_attached_at = Some(metadata::now_iso());
    if let Err(err) = meta.save(&path) {
        tracing::warn!(?path, %err, "could not persist last_attached_at");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::{seed::input::RepoSpec, session::metadata::FreshFields};

    #[test]
    fn update_last_attached_writes_iso_timestamp() {
        let dir = unique_tempdir();
        let path = dir.join(metadata::METADATA_FILE_NAME);
        sample_metadata().save(&path).expect("save");

        update_last_attached(&dir).expect("update should succeed");

        let reloaded = Metadata::load(&path).expect("reload");
        let stamp = reloaded.last_attached_at.expect("last_attached_at should be populated");
        assert!(
            stamp.ends_with('Z') && stamp.len() == 20,
            "last_attached_at should be a UTC RFC 3339 timestamp, got: {stamp}",
        );
    }

    #[test]
    fn update_last_attached_tolerates_missing_metadata() {
        let dir = unique_tempdir();

        let result = update_last_attached(&dir);

        assert!(
            result.is_ok(),
            "missing metadata should not fail resume; got: {result:?}",
        );
    }

    #[test]
    fn resume_outcome_round_trips_through_struct() {
        let outcome = ResumeOutcome {
            started_from_shutoff: true,
            uuid: "abcd".to_owned(),
        };

        assert_eq!(outcome.uuid, "abcd");
        assert!(outcome.started_from_shutoff);
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus /dev/kvm and a defined session; run with --ignored after setting up locally"]
    fn resume_attaches_to_real_session() {}

    // ---------------------------------------------------------------------------
    // Test Utilities
    // ---------------------------------------------------------------------------

    fn sample_metadata() -> Metadata {
        metadata::fresh(FreshFields {
            alias: Some("alpha".to_owned()),
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: vec!["rust".to_owned()],
            overlay_virtual_gib: 100,
            persist: true,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
        })
    }

    fn unique_tempdir() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-session-resume-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
