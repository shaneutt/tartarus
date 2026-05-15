//! `tartarus resume`: re-attach to an existing session.
//!
//! Resolves the target, starts the domain if shut off, attaches the
//! serial console, and updates `last_attached_at`.

use tartarus_provider::{
    ResumeOutcome,
    session::{
        identity,
        metadata::{self, Metadata},
    },
};

use crate::{
    config::Config,
    error::Result,
    host::{connect::Connection, console, domain, error::HostError},
};

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

/// Branch the `is_active` / `start` decision tree onto pluggable
/// callbacks so tests can substitute scripted closures without
/// needing a libvirtd. Returns `Ok(true)` iff the domain had to be
/// started (i.e. it was shut off on entry).
pub(crate) fn ensure_running_with<A, S>(uuid: &str, is_active_fn: A, start_fn: S) -> Result<bool>
where
    A: FnOnce(&str) -> Result<bool>,
    S: FnOnce(&str) -> Result<()>,
{
    if is_active_fn(uuid)? {
        tracing::debug!(uuid, "session already running; attaching to live console");
        return Ok(false);
    }

    tracing::info!(uuid, "session is shut off; starting before attach");
    start_fn(uuid)?;
    Ok(true)
}

// -----------------------------------------------------------------------------
// Domain State
// -----------------------------------------------------------------------------

/// Start the domain if shut off; no-op if already running.
fn ensure_running(connection: &Connection, uuid: &str) -> Result<bool> {
    ensure_running_with(
        uuid,
        |id| {
            let domain = domain::lookup(connection, id)?;
            domain.is_active().map_err(|source| {
                HostError::DomainOperation {
                    operation: "is_active",
                    source,
                }
                .into()
            })
        },
        |id| domain::start(connection, id),
    )
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

    meta.last_attached_at = Some(tartarus_provider::time::now_iso());
    if let Err(err) = meta.save(&path) {
        tracing::warn!(?path, %err, "could not persist last_attached_at");
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use tartarus_provider::{seed::input::RepoSpec, session::metadata::FreshFields};

    use super::*;

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

    // -----------------------------------------------------------------------
    // ensure_running_with Branching
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_running_with_returns_false_when_already_active() {
        use std::cell::Cell;

        let start_called = Cell::new(false);
        let started = ensure_running_with(
            "abc",
            |_| Ok(true),
            |_| {
                start_called.set(true);
                Ok(())
            },
        )
        .expect("active path should succeed");
        assert!(!started, "already-active should report not-started");
        assert!(!start_called.get(), "start callback must not run on the active path");
    }

    #[test]
    fn ensure_running_with_starts_when_shut_off() {
        use std::cell::Cell;

        let start_called = Cell::new(false);
        let started = ensure_running_with(
            "abc",
            |_| Ok(false),
            |_| {
                start_called.set(true);
                Ok(())
            },
        )
        .expect("shut-off path should succeed");
        assert!(started, "shut-off should report started=true");
        assert!(start_called.get(), "start callback must run when shut off");
    }

    #[test]
    fn ensure_running_with_propagates_is_active_failure() {
        let err = ensure_running_with(
            "abc",
            |_| Err(crate::Error::Host(crate::host::error::HostError::AgentChannelMissing)),
            |_| panic!("start must not run when is_active errors"),
        )
        .expect_err("is_active failure should propagate");
        match err {
            crate::Error::Host(crate::host::error::HostError::AgentChannelMissing) => {},
            other => panic!("expected AgentChannelMissing, got {other:?}"),
        }
    }

    #[test]
    fn ensure_running_with_propagates_start_failure() {
        let err = ensure_running_with(
            "abc",
            |_| Ok(false),
            |_| Err(crate::Error::Host(crate::host::error::HostError::AgentChannelMissing)),
        )
        .expect_err("start failure should propagate");
        match err {
            crate::Error::Host(crate::host::error::HostError::AgentChannelMissing) => {},
            other => panic!("expected AgentChannelMissing, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn sample_metadata() -> Metadata {
        metadata::fresh(FreshFields {
            alias: Some("alpha".to_owned()),
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: vec!["rust".to_owned()],
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            vcpus: 2,
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
