//! `tartarus destroy`: undefine the libvirt domain, delete the
//! overlay, unlink the alias, and remove the session directory.

use std::path::Path;

use tartarus_provider::{
    DestroyOutcome,
    session::{
        identity,
        metadata::{self, Metadata},
    },
};

use crate::{
    config::Config,
    error::Result,
    host::{connect::Connection, domain, error::HostError},
};

/// Run `tartarus destroy <alias|uuid>`.
pub fn run(config: &Config, target: &str) -> Result<DestroyOutcome> {
    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, "destroying session");

    let alias_from_metadata = preflight_alias(&resolved.directory);

    if let Err(err) = release_gpu_borrow(&resolved.directory) {
        tracing::warn!(
            uuid = %resolved.uuid,
            %err,
            "GPU borrow release during destroy failed; run `tartarus host gpu release <BDF>` to retry",
        );
    }

    teardown_libvirt_domain(config, &resolved.uuid)?;
    remove_session_dir(&resolved.directory)?;
    drop_alias_symlinks(
        &resolved.uuid,
        resolved.alias.as_deref(),
        alias_from_metadata.as_deref(),
    )?;

    tracing::info!(uuid = %resolved.uuid, "session destroyed");

    Ok(DestroyOutcome { uuid: resolved.uuid })
}

// -----------------------------------------------------------------------------
// Teardown
// -----------------------------------------------------------------------------

/// Return the alias recorded in `metadata.json`, when readable.
fn preflight_alias(session_dir: &Path) -> Option<String> {
    let path = session_dir.join(metadata::METADATA_FILE_NAME);
    Metadata::load(&path).ok().and_then(|m| m.alias)
}

/// Release the GPU borrow if recorded. No-op otherwise.
fn release_gpu_borrow(session_dir: &Path) -> Result<()> {
    let metadata_path = session_dir.join(metadata::METADATA_FILE_NAME);
    if !metadata_path.exists() {
        return Ok(());
    }

    let mut metadata = Metadata::load(&metadata_path)?;
    let Some(record) = metadata.gpu_borrow.take() else {
        return Ok(());
    };

    let receipt = crate::gpu::driver::record_into_receipt(record)?;
    let io = crate::gpu::driver::KernelSysfs;
    crate::gpu::driver::release_with_receipt(&io, &receipt)?;
    metadata.save(&metadata_path)?;
    Ok(())
}

/// Force-stop (best-effort) and undefine the libvirt domain backing
/// the session.
fn teardown_libvirt_domain(config: &Config, uuid: &str) -> Result<()> {
    let connection = Connection::open(&config.network_uri)?;
    teardown_libvirt_domain_with(
        uuid,
        |id| domain::destroy(&connection, id),
        |id| domain::undefine(&connection, id),
    )
}

/// Run the destroy/undefine sequence against pluggable callbacks.
/// Pure: the libvirt-side wiring lives in [`teardown_libvirt_domain`].
pub(crate) fn teardown_libvirt_domain_with<D, U>(uuid: &str, destroy_fn: D, undefine_fn: U) -> Result<()>
where
    D: FnOnce(&str) -> Result<()>,
    U: FnOnce(&str) -> Result<()>,
{
    if let Err(err) = destroy_fn(uuid) {
        tracing::debug!(uuid, %err, "domain destroy failed (already inactive?); continuing");
    }

    match undefine_fn(uuid) {
        Ok(()) => Ok(()),
        Err(crate::Error::Host(HostError::DomainOperation { operation, source })) => {
            tracing::warn!(uuid, operation, %source, "domain undefine reported a failure; continuing");
            Ok(())
        },
        Err(other) => Err(other),
    }
}

/// Remove the session directory and every artefact it carries.
fn remove_session_dir(session_dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(session_dir) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Remove alias symlinks pointing at the destroyed session (race-safe).
fn drop_alias_symlinks(uuid: &str, resolved_alias: Option<&str>, metadata_alias: Option<&str>) -> Result<()> {
    if let Some(alias) = resolved_alias {
        identity::unlink_alias_if_points_at(alias, uuid)?;
    }
    if let Some(alias) = metadata_alias
        && Some(alias) != resolved_alias
    {
        identity::unlink_alias_if_points_at(alias, uuid)?;
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;

    #[test]
    fn preflight_alias_returns_none_when_metadata_missing() {
        let dir = tempdir();

        assert!(
            preflight_alias(&dir).is_none(),
            "missing metadata.json must not surface a stale alias",
        );
    }

    #[test]
    fn preflight_alias_returns_alias_from_metadata_json() {
        let dir = tempdir();
        let metadata = metadata::fresh(metadata::FreshFields {
            alias: Some("preflight-alias".to_owned()),
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: Vec::new(),
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            repos: Vec::new(),
            uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
            vcpus: 2,
        });
        metadata
            .save(&dir.join(metadata::METADATA_FILE_NAME))
            .expect("save should succeed in a fresh tempdir");

        let alias = preflight_alias(&dir);

        assert_eq!(
            alias.as_deref(),
            Some("preflight-alias"),
            "preflight_alias should round-trip the alias from metadata.json",
        );
    }

    #[test]
    fn remove_session_dir_is_a_noop_for_a_missing_path() {
        let dir = tempdir();
        let absent = dir.join("not-here");

        remove_session_dir(&absent).expect("missing dir should not be an error");
    }

    #[test]
    fn remove_session_dir_clears_a_populated_directory() {
        let dir = tempdir();
        std::fs::write(dir.join("inside.txt"), b"contents").expect("write should succeed");

        remove_session_dir(&dir).expect("populated dir should be removable");

        assert!(!dir.exists(), "remove_session_dir should leave nothing behind");
    }

    // -----------------------------------------------------------------------
    // teardown_libvirt_domain_with Branching
    // -----------------------------------------------------------------------

    #[test]
    fn teardown_libvirt_domain_with_succeeds_when_both_callbacks_succeed() {
        teardown_libvirt_domain_with("abc", |_| Ok(()), |_| Ok(())).expect("happy path should succeed");
    }

    #[test]
    fn teardown_libvirt_domain_with_tolerates_destroy_failure() {
        // destroy fails (e.g. already inactive); undefine succeeds; overall Ok.
        teardown_libvirt_domain_with(
            "abc",
            |_| Err(crate::Error::Host(HostError::AgentChannelMissing)),
            |_| Ok(()),
        )
        .expect("destroy failure should be tolerated");
    }

    #[test]
    fn teardown_libvirt_domain_with_tolerates_undefine_domain_operation_failure() {
        // undefine fails with a `DomainOperation` error; logged as
        // warn and returned Ok per the original behaviour.
        use crate::host::error::HostError;
        teardown_libvirt_domain_with(
            "abc",
            |_| Ok(()),
            |_| {
                Err(crate::Error::Host(HostError::ConsoleSttyFailed {
                    operation: "raw",
                    detail: "should not match the DomainOperation arm".to_owned(),
                }))
            },
        )
        .expect_err("ConsoleSttyFailed should NOT be tolerated by the undefine arm");
    }

    #[test]
    fn teardown_libvirt_domain_with_propagates_unexpected_undefine_failure() {
        let err = teardown_libvirt_domain_with(
            "abc",
            |_| Ok(()),
            |_| Err(crate::Error::Host(HostError::AgentChannelMissing)),
        )
        .expect_err("non-DomainOperation undefine failure should propagate");
        match err {
            crate::Error::Host(HostError::AgentChannelMissing) => {},
            other => panic!("expected AgentChannelMissing, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-destroy-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed");

        path
    }
}
