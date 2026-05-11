//! `tartarus rename`: create or move a session alias symlink.
//!
//! Refuses when the target alias name is already taken by a *different*
//! UUID. Idempotent: pointing the alias at the same UUID it already
//! resolves to is a success.

use crate::{
    error::Result,
    paths,
    session::{error::SessionError, identity},
};

// ---------------------------------------------------------------------------
// RenameOutcome
// ---------------------------------------------------------------------------

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameOutcome {
    /// Alias the session is now reachable as.
    pub alias: String,

    /// UUID the alias resolves to.
    pub uuid: String,
}

/// Run `tartarus rename <uuid> <name>`.
pub fn run(uuid: &str, alias: &str) -> Result<RenameOutcome> {
    let session_dir = paths::sessions_by_uuid_dir()?.join(uuid);
    if !session_dir.is_dir() {
        return Err(SessionError::NotFound {
            target: uuid.to_owned(),
        }
        .into());
    }

    identity::set_alias(alias, uuid)?;

    update_metadata_alias(&session_dir, alias)?;

    tracing::info!(uuid, alias, "alias updated");

    Ok(RenameOutcome {
        alias: alias.to_owned(),
        uuid: uuid.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Metadata Alias
// ---------------------------------------------------------------------------

/// Persist the new alias into `metadata.json`.
fn update_metadata_alias(session_dir: &std::path::Path, alias: &str) -> Result<()> {
    let path = session_dir.join(crate::session::metadata::METADATA_FILE_NAME);
    let mut metadata = crate::session::metadata::Metadata::load(&path)?;
    metadata.alias = Some(alias.to_owned());
    metadata.save(&path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::session::metadata::{self, METADATA_FILE_NAME};

    #[test]
    fn update_metadata_alias_persists_into_metadata_json() {
        let dir = tempdir();
        let metadata = metadata::fresh(metadata::FreshFields {
            alias: None,
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: Vec::new(),
            overlay_virtual_gib: 100,
            persist: true,
            repos: Vec::new(),
            uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
        });
        let path = dir.join(METADATA_FILE_NAME);
        metadata.save(&path).expect("initial save should succeed");

        update_metadata_alias(&dir, "fresh-alias").expect("update should succeed");

        let reloaded = metadata::Metadata::load(&path).expect("reload should succeed");
        assert_eq!(
            reloaded.alias.as_deref(),
            Some("fresh-alias"),
            "the new alias should be persisted into metadata.json",
        );
    }

    // ---------------------------------------------------------------------------
    // Test Utilities
    // ---------------------------------------------------------------------------

    fn tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-rename-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed");

        path
    }
}
