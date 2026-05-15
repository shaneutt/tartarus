//! Atomic, mode-`0600`-enforced writer for `config.toml`.
//!
//! Writes to a same-directory temp file at mode `0600`, then
//! `rename(2)`s into place. Post-rename mode is re-checked as
//! defense-in-depth.

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    auth::error::AuthError,
    config::FileConfig,
    error::{Error, Result},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Required mode for config and temp files.
#[cfg(unix)]
const SECRET_FILE_MODE: u32 = 0o600;

/// Monotonic counter for unique temp file names within a process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// -----------------------------------------------------------------------------
// Config Writing
// -----------------------------------------------------------------------------

/// Atomically write `config` to `path` at mode `0600`.
///
/// Returns [`AuthError::ConfigAlreadyExists`] when `force` is false
/// and `path` exists. Creates the parent directory if needed.
pub fn write_config(path: &Path, config: &FileConfig, force: bool) -> Result<()> {
    if !force && path.exists() {
        return Err(AuthError::ConfigAlreadyExists {
            path: path.to_path_buf(),
        }
        .into());
    }

    let parent = path.parent().ok_or_else(|| {
        Error::from(AuthError::PathNotAbsolute {
            path: path.to_path_buf(),
        })
    })?;

    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent)?;
    }

    let body = toml::to_string(config).map_err(AuthError::Serialize)?;

    let temp_path = temp_path_for(path);

    write_temp_secret(&temp_path, body.as_bytes())?;

    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err.into());
    }

    enforce_owner_only_mode(path)?;

    tracing::debug!(?path, "wrote config (mode 0600)");

    Ok(())
}

/// Load the [`FileConfig`] at `path` (or a default), apply `mutator`,
/// and write the result back with `force = true`.
pub fn merge_and_write_config<F>(path: &Path, mutator: F) -> Result<()>
where
    F: FnOnce(&mut FileConfig),
{
    let mut current = if path.exists() {
        tartarus_provider::config::load_from(path)?
    } else {
        FileConfig::default()
    };

    mutator(&mut current);

    write_config(path, &current, true)
}

// -----------------------------------------------------------------------------
// Atomic File Operations
// -----------------------------------------------------------------------------

/// Temp file path adjacent to `path` for atomic `rename(2)`.
fn temp_path_for(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_owned());
    let temp_name = format!(".{name}.tartarus-{pid}-{n}");

    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(temp_name),
        _ => PathBuf::from(temp_name),
    }
}

/// Write `bytes` to `temp_path` at mode `0600` from creation.
#[cfg(unix)]
fn write_temp_secret(temp_path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(SECRET_FILE_MODE)
        .open(temp_path)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(())
}

/// Non-Unix shim for build portability.
#[cfg(not(unix))]
fn write_temp_secret(temp_path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(temp_path)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(())
}

/// Re-check mode `0600` after rename; fix if a filesystem layer
/// dropped it.
#[cfg(unix)]
fn enforce_owner_only_mode(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;

    if mode != SECRET_FILE_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(SECRET_FILE_MODE))?;
    }

    Ok(())
}

/// Non-Unix shim for build portability.
#[cfg(not(unix))]
fn enforce_owner_only_mode(_path: &Path) -> Result<()> {
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use tartarus_provider::config::{ClaudeAnthropicSection, ClaudeSection, GithubSection};

    use super::*;

    #[cfg(unix)]
    #[test]
    fn write_lands_at_mode_0600() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        let cfg = sample_anthropic_config();

        write_config(&path, &cfg, false).expect("first write should succeed");

        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(mode, 0o600, "write_config should land at 0600");
    }

    #[test]
    fn refuse_overwrite_without_force() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        let cfg = sample_anthropic_config();

        write_config(&path, &cfg, false).expect("first write should succeed");

        let err = write_config(&path, &cfg, false).expect_err("second write should refuse");

        match err {
            Error::Auth(AuthError::ConfigAlreadyExists { path: reported }) => {
                assert_eq!(reported, path, "error should report the conflicting path");
            },
            other => panic!("expected ConfigAlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn force_overwrites_existing_config() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        write_config(&path, &sample_anthropic_config(), false).expect("first write should succeed");

        let mut updated = sample_anthropic_config();
        updated.github.token = Some("ghp_updated".to_owned());

        write_config(&path, &updated, true).expect("forced overwrite should succeed");

        let parsed = tartarus_provider::config::load_from(&path).expect("forced write should round-trip");

        assert_eq!(
            parsed.github.token.as_deref(),
            Some("ghp_updated"),
            "forced write should land the new token",
        );
    }

    #[test]
    fn round_trip_preserves_fields() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        let cfg = sample_anthropic_config();

        write_config(&path, &cfg, false).expect("write should succeed");

        let parsed = tartarus_provider::config::load_from(&path).expect("written config should be loadable");

        assert_eq!(
            parsed.github.token.as_deref(),
            Some("ghp_test"),
            "github token should round-trip"
        );
        assert_eq!(
            parsed.claude.anthropic.api_key.as_deref(),
            Some("sk-ant-test"),
            "anthropic api key should round-trip",
        );
    }

    #[test]
    fn merge_preserves_existing_fields() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        write_config(&path, &sample_anthropic_config(), false).expect("initial write should succeed");

        merge_and_write_config(&path, |cfg| {
            cfg.claude.vertex.project_id = Some("my-project".to_owned());
            cfg.claude.vertex.region = Some("us-east5".to_owned());
            cfg.claude.vertex.credentials_file = Some(PathBuf::from("/tmp/sa.json"));
        })
        .expect("merge should succeed");

        let parsed = tartarus_provider::config::load_from(&path).expect("merged config should be loadable");

        assert_eq!(
            parsed.github.token.as_deref(),
            Some("ghp_test"),
            "merging should preserve the github token",
        );
        assert_eq!(
            parsed.claude.anthropic.api_key.as_deref(),
            Some("sk-ant-test"),
            "merging should preserve the anthropic api key",
        );
        assert_eq!(
            parsed.claude.vertex.project_id.as_deref(),
            Some("my-project"),
            "merging should add the vertex project id",
        );
    }

    #[test]
    fn merge_creates_file_when_missing() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        merge_and_write_config(&path, |cfg| {
            cfg.claude.vertex.project_id = Some("solo-project".to_owned());
        })
        .expect("merge into missing file should succeed");

        let parsed = tartarus_provider::config::load_from(&path).expect("written config should be loadable");

        assert_eq!(
            parsed.claude.vertex.project_id.as_deref(),
            Some("solo-project"),
            "merge into a missing file should still write the new value",
        );
    }

    #[test]
    fn failed_write_to_unrelated_path_leaves_other_file_intact() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        write_config(&path, &sample_anthropic_config(), false).expect("initial write should succeed");

        let original = std::fs::read_to_string(&path).expect("original should be readable");

        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").expect("blocker file");
        let bad_destination = blocker.join("config.toml");

        let err = write_config(&bad_destination, &sample_anthropic_config(), true)
            .expect_err("writing into a child of a non-directory should fail");

        let still_there = std::fs::read_to_string(&path).expect("original should still be readable");

        assert_eq!(
            still_there, original,
            "the original file should be untouched on a failed write to another path",
        );

        drop(err);
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_carries_mode_0600_before_rename() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let temp_path = dir.join(".captured-config.toml.tartarus-stub");

        write_temp_secret(&temp_path, b"some bytes\n").expect("temp write should succeed");

        let mode = std::fs::metadata(&temp_path)
            .expect("temp metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(
            mode, 0o600,
            "the in-flight temp file must be world-unreadable from the very first byte",
        );

        std::fs::remove_file(&temp_path).expect("clean up temp file");
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    /// Representative Anthropic-backend [`FileConfig`].
    fn sample_anthropic_config() -> FileConfig {
        FileConfig {
            claude: ClaudeSection {
                anthropic: ClaudeAnthropicSection {
                    api_key: Some("sk-ant-test".to_owned()),
                },
                backend: Some(tartarus_provider::config::Backend::Anthropic),
                ..ClaudeSection::default()
            },
            github: GithubSection {
                token: Some("ghp_test".to_owned()),
            },
            ..FileConfig::default()
        }
    }

    /// Unique per-process temp directory.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-auth-write-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir root");

        path
    }
}
