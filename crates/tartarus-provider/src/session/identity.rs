//! Session UUID generation and alias-symlink resolution.
//!
//! The on-disk layout under [`crate::paths::sessions_dir`] is:
//!
//! ```text
//! sessions/
//!   by-uuid/<uuid>/{overlay.qcow2, cloud-init.iso, domain.xml, metadata.json}
//!   by-name/<alias> -> ../by-uuid/<uuid>      # relative symlink
//! ```
//!
//! Two aliases can point at the same UUID. Renaming an alias is a
//! `mv`; deleting one is `unlink`. Tartarus never tells libvirt about
//! aliases — `virsh` only ever sees the UUID.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{error::Result, paths, session::error::SessionError};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Maximum alias length, in chars.
const MAX_ALIAS_LEN: usize = 64;

/// Counter feeding the per-process suffix on temp alias links.
static ALIAS_COUNTER: AtomicU64 = AtomicU64::new(0);

// -----------------------------------------------------------------------------
// ResolvedSession
// -----------------------------------------------------------------------------

/// Resolved session location: the canonical `by-uuid/<uuid>/` directory
/// plus the alias (if the input was an alias).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedSession {
    /// Alias the input matched, when the input was an alias name.
    pub alias: Option<String>,

    /// Canonical session directory under `by-uuid/<uuid>/`.
    pub directory: PathBuf,

    /// Session UUID (file-name of [`Self::directory`]).
    pub uuid: String,
}

/// Generate a fresh v4 UUID for a new session.
pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Resolve `target` (alias or UUID) into the canonical session
/// directory.
///
/// Tries `by-uuid/<target>/` first, then `by-name/<target>`.
/// Rejects inputs outside the UUID/alias charset before any
/// filesystem call to prevent path traversal.
pub fn resolve(target: &str) -> Result<ResolvedSession> {
    let by_uuid = paths::sessions_by_uuid_dir()?;
    let by_name = paths::sessions_by_name_dir()?;

    if !is_valid_uuid(target) && !is_valid_alias(target) {
        return Err(SessionError::InvalidAlias {
            alias: target.to_owned(),
        }
        .into());
    }

    if is_valid_uuid(target)
        && let Some(resolved) = resolve_as_uuid(&by_uuid, target)
    {
        return Ok(resolved);
    }

    if is_valid_alias(target) {
        return resolve_as_alias(&by_name, target);
    }

    Err(SessionError::NotFound {
        target: target.to_owned(),
    }
    .into())
}

/// Create or replace the alias symlink pointing at `uuid`.
///
/// Idempotent when the alias already points at the same UUID.
/// Refuses with [`SessionError::AliasInUse`] when the alias
/// points at a different UUID.
pub fn set_alias(alias: &str, uuid: &str) -> Result<()> {
    if !is_valid_alias(alias) {
        return Err(SessionError::InvalidAlias {
            alias: alias.to_owned(),
        }
        .into());
    }
    if !is_valid_uuid(uuid) {
        return Err(SessionError::InvalidUuid { uuid: uuid.to_owned() }.into());
    }

    let by_name = paths::sessions_by_name_dir()?;
    std::fs::create_dir_all(&by_name)?;

    let link = by_name.join(alias);

    if let Some(existing) = read_alias_target(&link)?
        && existing != uuid
    {
        return Err(SessionError::AliasInUse {
            alias: alias.to_owned(),
            existing_uuid: existing,
        }
        .into());
    }

    create_alias_atomically(&link, uuid)
}

/// Remove the alias symlink at `alias`. Idempotent on `NotFound`.
pub fn unlink_alias(alias: &str) -> Result<()> {
    if !is_valid_alias(alias) {
        return Ok(());
    }

    let by_name = paths::sessions_by_name_dir()?;
    let link = by_name.join(alias);

    match std::fs::remove_file(&link) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

/// Remove the alias symlink only when it points at `expected_uuid`.
///
/// Race-safe variant of [`unlink_alias`] that avoids clobbering an
/// alias concurrently re-pointed at a different session.
pub fn unlink_alias_if_points_at(alias: &str, expected_uuid: &str) -> Result<()> {
    if !is_valid_alias(alias) {
        return Ok(());
    }

    let by_name = paths::sessions_by_name_dir()?;
    let link = by_name.join(alias);

    match read_alias_target(&link)? {
        Some(target) if target == expected_uuid => match std::fs::remove_file(&link) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        },
        Some(target) => {
            tracing::warn!(
                %alias,
                observed = %target,
                expected = %expected_uuid,
                "alias was re-pointed since resolve; leaving the symlink in place",
            );
            Ok(())
        },
        None => Ok(()),
    }
}

/// List every alias symlink in `by-name/` and the UUID it points at.
pub fn list_aliases() -> Result<Vec<(String, String)>> {
    let by_name = paths::sessions_by_name_dir()?;
    if !by_name.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(&by_name)? {
        let entry = entry?;
        let alias = match entry.file_name().to_str() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        if let Some(target) = read_alias_target(&entry.path())? {
            out.push((alias, target));
        }
    }

    Ok(out)
}

/// Test whether `alias` matches `[A-Za-z0-9][A-Za-z0-9._-]{0,63}`.
pub fn is_valid_alias(alias: &str) -> bool {
    let mut chars = alias.chars();
    let Some(first) = chars.next() else { return false };

    if !first.is_ascii_alphanumeric() {
        return false;
    }

    let mut len = 1usize;
    for c in chars {
        if !is_alias_char(c) {
            return false;
        }
        len += 1;
        if len > MAX_ALIAS_LEN {
            return false;
        }
    }

    true
}

/// Test whether `uuid` matches the canonical v4 hex form
/// (`xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx`).
pub fn is_valid_uuid(uuid: &str) -> bool {
    let bytes = uuid.as_bytes();
    if bytes.len() != 36 {
        return false;
    }

    for (i, &b) in bytes.iter().enumerate() {
        let ok = match i {
            8 | 13 | 18 | 23 => b == b'-',
            14 => b == b'4',
            19 => matches!(b, b'8' | b'9' | b'a' | b'b'),
            _ => b.is_ascii_hexdigit() && (b.is_ascii_digit() || b.is_ascii_lowercase()),
        };
        if !ok {
            return false;
        }
    }

    true
}

// -----------------------------------------------------------------------------
// Symlink Management
// -----------------------------------------------------------------------------

/// Whether `c` is a valid non-leading alias character.
fn is_alias_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Try `target` as a UUID directory under `by-uuid/`.
fn resolve_as_uuid(by_uuid: &Path, target: &str) -> Option<ResolvedSession> {
    let candidate = by_uuid.join(target);
    if !candidate.is_dir() {
        return None;
    }

    Some(ResolvedSession {
        alias: None,
        directory: candidate,
        uuid: target.to_owned(),
    })
}

/// Try `target` as an alias, following the `by-name/<target>` symlink.
fn resolve_as_alias(by_name: &Path, target: &str) -> Result<ResolvedSession> {
    let link = by_name.join(target);
    let uuid = read_alias_target(&link)?.ok_or_else(|| SessionError::NotFound {
        target: target.to_owned(),
    })?;

    let directory = paths::sessions_by_uuid_dir()?.join(&uuid);
    if !directory.is_dir() {
        return Err(SessionError::DanglingAlias {
            alias: target.to_owned(),
            target: directory,
        }
        .into());
    }

    Ok(ResolvedSession {
        alias: Some(target.to_owned()),
        directory,
        uuid,
    })
}

/// Read the file-name component of an alias symlink target.
///
/// Returns `Ok(None)` when the symlink does not exist or the
/// target's file-name is not a valid UUID.
fn read_alias_target(link: &Path) -> Result<Option<String>> {
    match std::fs::read_link(link) {
        Ok(path) => {
            let name = path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
            Ok(name.filter(|s| is_valid_uuid(s)))
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

/// Atomically replace `link` with a relative symlink to
/// `../by-uuid/<uuid>` via write-then-rename.
fn create_alias_atomically(link: &Path, uuid: &str) -> Result<()> {
    let parent = link.parent().expect("alias symlink path always has a parent");
    let pid = std::process::id();
    let n = ALIAS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".alias.tartarus-{pid}-{n}",));

    if tmp.symlink_metadata().is_ok() {
        let _ = std::fs::remove_file(&tmp);
    }

    let target = PathBuf::from("..").join("by-uuid").join(uuid);
    create_symlink(&target, &tmp)?;

    if let Err(err) = std::fs::rename(&tmp, link) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err.into());
    }

    Ok(())
}

/// Create a relative symlink (Unix only).
#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(Into::into)
}

/// Non-Unix shim.
#[cfg(not(unix))]
fn create_symlink(_target: &Path, link: &Path) -> Result<()> {
    Err(std::io::Error::other(format!(
        "symlinks are only supported on Unix; cannot create {}",
        link.display(),
    ))
    .into())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uuid_round_trips_through_uuid_parser() {
        let uuid = new_uuid();
        let parsed = uuid::Uuid::parse_str(&uuid).expect("new_uuid output should parse");
        assert_eq!(parsed.get_version_num(), 4, "Tartarus uses v4 UUIDs");
    }

    #[test]
    fn is_valid_uuid_accepts_new_uuid_output() {
        for _ in 0..16 {
            let uuid = new_uuid();
            assert!(is_valid_uuid(&uuid), "new_uuid output should validate: {uuid}");
        }
    }

    #[test]
    fn is_valid_uuid_rejects_path_traversal_shapes() {
        for bad in [
            "",
            "../passwd",
            "../../etc/passwd",
            "deadbeef",
            "11111111-2222-3333-4444-555555555555/extra",
            "11111111-2222-1333-4444-555555555555",
            "11111111-2222-3333-c444-555555555555",
            "11111111-2222-3333-4444-55555555555X",
            "11111111-2222-3333-4444-55555555555",
            "11111111_2222_3333_4444_555555555555",
            "11111111-2222-3333-4444-555555555555 ",
            "11111111-2222-3333-4444-5555555555555",
            "1A111111-2222-4333-8444-555555555555",
        ] {
            assert!(!is_valid_uuid(bad), "{bad} must not validate as a uuid");
        }
    }

    #[test]
    fn is_valid_alias_accepts_typical_names() {
        for ok in ["alpha", "fix-bug", "fix.bug", "a", "Alpha_1", "x".repeat(64).as_str()] {
            assert!(is_valid_alias(ok), "{ok} should validate");
        }
    }

    #[test]
    fn is_valid_alias_rejects_path_traversal_shapes() {
        for bad in [
            "",
            ".",
            "..",
            "../passwd",
            "/abs",
            "a/b",
            "a b",
            ".hidden",
            "-leading-dash",
            "alpha\nbeta",
            "alpha\x00beta",
            "x".repeat(65).as_str(),
        ] {
            assert!(!is_valid_alias(bad), "{bad:?} must not validate as an alias");
        }
    }

    #[test]
    fn set_alias_rejects_path_traversal() {
        let err = set_alias("../passwd", "11111111-2222-4333-8444-555555555555")
            .expect_err("alias with path traversal must be rejected");
        match err {
            crate::error::Error::Session(SessionError::InvalidAlias { alias }) => {
                assert_eq!(alias, "../passwd", "rejected alias should round-trip into the error");
            },
            other => panic!("expected InvalidAlias, got {other:?}"),
        }
    }

    #[test]
    fn set_alias_rejects_non_canonical_uuid() {
        let err = set_alias("alpha", "../../etc/passwd").expect_err("uuid with path traversal must be rejected");
        match err {
            crate::error::Error::Session(SessionError::InvalidUuid { uuid }) => {
                assert_eq!(
                    uuid, "../../etc/passwd",
                    "rejected uuid should round-trip into the error"
                );
            },
            other => panic!("expected InvalidUuid, got {other:?}"),
        }
    }

    #[test]
    fn read_alias_target_returns_none_for_missing_link() {
        let dir = unique_tempdir();
        let result = read_alias_target(&dir.join("missing")).expect("missing link should not error");
        assert!(result.is_none(), "missing link should map to None");
    }

    #[cfg(unix)]
    #[test]
    fn create_alias_atomically_writes_relative_symlink() {
        let dir = unique_tempdir();
        let by_name = dir.join("by-name");
        std::fs::create_dir_all(&by_name).expect("by-name dir");
        let link = by_name.join("alpha");

        create_alias_atomically(&link, "deadbeef-uuid").expect("create_alias_atomically should succeed");

        let target = std::fs::read_link(&link).expect("symlink should exist");
        assert_eq!(
            target,
            PathBuf::from("..").join("by-uuid").join("deadbeef-uuid"),
            "alias target must be relative ../by-uuid/<uuid>",
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_as_uuid_finds_existing_directory() {
        let dir = unique_tempdir();
        let by_uuid = dir.join("by-uuid");
        std::fs::create_dir_all(by_uuid.join("deadbeef")).expect("create by-uuid/<uuid>");

        let resolved = resolve_as_uuid(&by_uuid, "deadbeef").expect("by-uuid lookup should succeed");

        assert_eq!(resolved.uuid, "deadbeef", "uuid should round-trip");
        assert!(resolved.alias.is_none(), "direct UUID lookup should not carry an alias");
    }

    #[test]
    fn resolve_as_uuid_returns_none_for_missing_directory() {
        let dir = unique_tempdir();
        let by_uuid = dir.join("by-uuid");
        std::fs::create_dir_all(&by_uuid).expect("create by-uuid");

        let resolved = resolve_as_uuid(&by_uuid, "missing");

        assert!(resolved.is_none(), "missing directory should return None");
    }

    #[cfg(unix)]
    #[test]
    fn create_alias_atomically_replaces_existing_symlink() {
        let dir = unique_tempdir();
        let by_name = dir.join("by-name");
        std::fs::create_dir_all(&by_name).expect("by-name dir");
        let link = by_name.join("alpha");

        create_alias_atomically(&link, "first-uuid").expect("first create");
        create_alias_atomically(&link, "second-uuid").expect("second create");

        let target = std::fs::read_link(&link).expect("symlink should exist");
        assert_eq!(
            target.file_name().and_then(|n| n.to_str()),
            Some("second-uuid"),
            "atomic replace should leave the new uuid in place",
        );
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn unique_tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-session-id-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
