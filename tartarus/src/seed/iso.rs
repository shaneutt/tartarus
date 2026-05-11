//! Author the per-session NoCloud `cloud-init.iso` via `genisoimage`.
//!
//! Regenerated on every invocation (never cached). Shelling out to
//! `genisoimage` is the sanctioned exception to the no-shell-out rule
//! (ISO authoring is not a libvirt domain).

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    error::Result,
    seed::{input::Seed, render},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Seed ISO file name within the session directory.
pub const SEED_ISO_FILE_NAME: &str = "cloud-init.iso";

/// Mode for seed artefacts (contains credentials).
#[cfg(unix)]
const SEED_FILE_MODE: u32 = 0o600;

// ---------------------------------------------------------------------------
// ISO Authoring
// ---------------------------------------------------------------------------

/// Seed ISO authoring errors.
#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    /// `genisoimage` failed.
    #[error("`genisoimage` failed authoring {iso}: {detail}")]
    Genisoimage {
        /// Detail from stderr or exit status.
        detail: String,

        /// ISO path.
        iso: PathBuf,
    },
}

/// Build `<session_dir>/cloud-init.iso` from `seed`. Source files
/// (`user-data`, `meta-data`) remain for audit.
pub fn write_iso(session_dir: &Path, seed: &Seed) -> Result<PathBuf> {
    let docs = render::render(seed);

    let user_data = session_dir.join("user-data");
    let meta_data = session_dir.join("meta-data");
    write_secret_file(&user_data, docs.user_data.as_bytes())?;
    write_secret_file(&meta_data, docs.meta_data.as_bytes())?;

    let iso = session_dir.join(SEED_ISO_FILE_NAME);
    run_genisoimage(session_dir, &iso)?;
    enforce_owner_only_mode(&iso)?;

    tracing::info!(iso = %iso.display(), "authored session seed ISO");

    Ok(iso)
}

// ---------------------------------------------------------------------------
// File Operations
// ---------------------------------------------------------------------------

/// Write `bytes` to `path` at mode `0600`.
#[cfg(unix)]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(SEED_FILE_MODE)
        .open(path)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    enforce_owner_only_mode(path)
}

/// Non-Unix shim for build portability.
#[cfg(not(unix))]
fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(())
}

/// Re-check mode `0600` after write; fix if needed.
#[cfg(unix)]
fn enforce_owner_only_mode(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;

    if mode != SEED_FILE_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(SEED_FILE_MODE))?;
    }

    Ok(())
}

/// Non-Unix shim for build portability.
#[cfg(not(unix))]
fn enforce_owner_only_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Shell out to `genisoimage`. `pub(crate)` so the layering seed can
/// reuse it.
pub(crate) fn run_genisoimage(workdir: &Path, iso: &Path) -> Result<()> {
    let output = Command::new("genisoimage")
        .arg("-output")
        .arg(iso)
        .arg("-volid")
        .arg("cidata")
        .arg("-joliet")
        .arg("-rock")
        .arg("user-data")
        .arg("meta-data")
        .current_dir(workdir)
        .output()
        .map_err(|err| SeedError::Genisoimage {
            detail: err.to_string(),
            iso: iso.to_path_buf(),
        })?;

    if !output.status.success() {
        return Err(SeedError::Genisoimage {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            iso: iso.to_path_buf(),
        }
        .into());
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
    use crate::{
        host_user::HostUser,
        seed::input::{ClaudeDefaults, CredentialBundle, Credentials, RepoSpec, Seed},
    };

    #[test]
    fn write_iso_produces_iso_and_source_files() {
        let dir = unique_tempdir();
        let seed = anthropic_seed();

        let iso = write_iso(&dir, &seed).expect("write_iso should succeed against real genisoimage");

        assert_eq!(iso, dir.join(SEED_ISO_FILE_NAME), "iso path should round-trip");
        assert!(iso.exists(), "iso file should exist on disk after write_iso");
        assert!(
            dir.join("user-data").exists(),
            "user-data should remain in the session dir for audit",
        );
        assert!(
            dir.join("meta-data").exists(),
            "meta-data should remain in the session dir for audit",
        );

        let bytes = std::fs::read(&iso).expect("iso should be readable");
        assert!(
            bytes.windows(5).any(|w| w == b"CD001"),
            "ISO 9660 magic `CD001` should appear in the rendered ISO",
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_iso_seed_files_land_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tempdir();
        let seed = anthropic_seed();

        let iso = write_iso(&dir, &seed).expect("write_iso should succeed against real genisoimage");

        for path in [&dir.join("user-data"), &dir.join("meta-data"), &iso] {
            let mode = std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o600,
                "seed artefact at {path} must be world-unreadable",
                path = path.display(),
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    fn anthropic_seed() -> Seed {
        Seed {
            name: "(unnamed)".to_owned(),
            credentials: Credentials {
                backend: CredentialBundle::Anthropic {
                    api_key: "sk-ant-test".to_owned(),
                },
                claude: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
                github_token: "ghp_test".to_owned(),
            },
            envs: vec!["rust".to_owned()],
            remote_connect: false,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_pubkey: None,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            user: HostUser {
                gid: 1000,
                uid: 1000,
                username: "alice".to_owned(),
            },
        }
    }

    fn unique_tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-seed-iso-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
