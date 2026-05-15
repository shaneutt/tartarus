//! Author the per-session NoCloud `cloud-init.iso` via [`Genisoimage`].
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
};

use tartarus_provider::seed::{input::Seed, render};

use crate::{
    Result,
    seed::genisoimage::{Genisoimage, KernelGenisoimage},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Seed ISO file name within the session directory.
pub const SEED_ISO_FILE_NAME: &str = "cloud-init.iso";

/// Mode for seed artefacts (contains credentials).
#[cfg(unix)]
const SEED_FILE_MODE: u32 = 0o600;

// -----------------------------------------------------------------------------
// ISO Authoring
// -----------------------------------------------------------------------------

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
    write_iso_with(&KernelGenisoimage, session_dir, seed)
}

/// [`write_iso`] parameterised by the [`Genisoimage`] runner.
/// Tests pass a recorder; production calls [`write_iso`] which
/// installs the default [`KernelGenisoimage`].
pub fn write_iso_with<G: Genisoimage + ?Sized>(genisoimage: &G, session_dir: &Path, seed: &Seed) -> Result<PathBuf> {
    let docs = render::render(seed);

    let user_data = session_dir.join("user-data");
    let meta_data = session_dir.join("meta-data");
    write_secret_file(&user_data, docs.user_data.as_bytes())?;
    write_secret_file(&meta_data, docs.meta_data.as_bytes())?;

    let iso = session_dir.join(SEED_ISO_FILE_NAME);
    run_genisoimage(genisoimage, session_dir, &iso)?;
    enforce_owner_only_mode(&iso)?;

    tracing::info!(iso = %iso.display(), "authored session seed ISO");

    Ok(iso)
}

// -----------------------------------------------------------------------------
// File Operations
// -----------------------------------------------------------------------------

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

/// Shell out via [`Genisoimage`].
pub(crate) fn run_genisoimage<G: Genisoimage + ?Sized>(genisoimage: &G, workdir: &Path, iso: &Path) -> Result<()> {
    let output = genisoimage
        .write_iso(workdir, iso)
        .map_err(|err| SeedError::Genisoimage {
            detail: err.to_string(),
            iso: iso.to_path_buf(),
        })?;

    if !output.success {
        return Err(SeedError::Genisoimage {
            detail: output.stderr_trim(),
            iso: iso.to_path_buf(),
        }
        .into());
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use tartarus_provider::{
        host_user::HostUser,
        seed::input::{ClaudeCredentials, ClaudeDefaults, CredentialBundle, RepoSpec, Seed},
    };

    use super::*;

    #[test]
    fn write_iso_produces_iso_and_source_files() {
        if !tool_on_path("genisoimage") {
            eprintln!("skipping write_iso_produces_iso_and_source_files: genisoimage not on PATH");
            return;
        }
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

        if !tool_on_path("genisoimage") {
            eprintln!("skipping write_iso_seed_files_land_at_mode_0600: genisoimage not on PATH");
            return;
        }
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
            claude: Some(ClaudeCredentials {
                backend: CredentialBundle::Anthropic {
                    api_key: "sk-ant-test".to_owned(),
                },
                defaults: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
            }),
            envs: vec!["rust".to_owned()],
            github_token: "ghp_test".to_owned(),
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

    /// Probe whether a command exists on `PATH` so tests can early-skip
    /// when the host lacks the binary they shell out to.
    fn tool_on_path(name: &str) -> bool {
        std::process::Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }

    // -----------------------------------------------------------------------
    // Pure-helper Tests
    // -----------------------------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn write_secret_file_lands_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tempdir();
        let path = dir.join("seed-token");
        write_secret_file(&path, b"sk-ant-test").expect("write_secret_file should succeed");

        assert_eq!(
            std::fs::read(&path).expect("readback"),
            b"sk-ant-test",
            "file body should round-trip",
        );
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "file should land at mode 0600");
    }

    #[cfg(unix)]
    #[test]
    fn enforce_owner_only_mode_tightens_loose_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tempdir();
        let path = dir.join("loose-token");
        std::fs::write(&path, b"x").expect("create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");

        enforce_owner_only_mode(&path).expect("enforce should succeed");
        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "enforce should tighten 0o644 to 0o600");
    }

    #[test]
    fn write_iso_propagates_genisoimage_not_found_as_error() {
        if tool_on_path("genisoimage") {
            eprintln!("skipping write_iso_propagates_genisoimage_not_found_as_error: genisoimage is on PATH");
            return;
        }
        // When the binary is missing, write_secret_file still
        // succeeds (the source files land), then run_genisoimage
        // surfaces a Genisoimage error.
        let dir = unique_tempdir();
        let seed = anthropic_seed();
        let err = write_iso(&dir, &seed).expect_err("missing genisoimage should error");
        match err {
            crate::Error::Seed(SeedError::Genisoimage { iso, .. }) => {
                assert_eq!(
                    iso,
                    dir.join(SEED_ISO_FILE_NAME),
                    "ISO path should round-trip in the error"
                );
            },
            other => panic!("expected Seed(Genisoimage), got {other:?}"),
        }
    }

    #[test]
    fn seed_error_renders_iso_path_and_detail() {
        let err = SeedError::Genisoimage {
            detail: "stderr: bad option".to_owned(),
            iso: PathBuf::from("/tmp/missing.iso"),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("/tmp/missing.iso"));
        assert!(rendered.contains("stderr: bad option"));
    }

    // -----------------------------------------------------------------------
    // Genisoimage-Recorder Driven Tests
    // -----------------------------------------------------------------------

    #[test]
    fn write_iso_with_recorder_runs_genisoimage_in_session_dir() {
        use crate::seed::genisoimage::recorder::Recorder;

        let dir = unique_tempdir();
        let seed = anthropic_seed();
        let recorder = Recorder::with_file_writes();

        let iso = write_iso_with(&recorder, &dir, &seed).expect("write_iso_with should succeed");

        assert_eq!(iso, dir.join(SEED_ISO_FILE_NAME), "iso path should round-trip");
        let calls = recorder.calls.borrow();
        assert_eq!(calls.len(), 1, "exactly one genisoimage call should be made");
        assert_eq!(calls[0].workdir, dir, "workdir should be the session dir");
        assert_eq!(calls[0].iso, iso, "iso path should match the rendered handle");
        assert!(dir.join("user-data").exists(), "user-data should land on disk");
        assert!(dir.join("meta-data").exists(), "meta-data should land on disk");
        assert!(iso.exists(), "iso should land on disk");
    }

    #[test]
    fn write_iso_with_recorder_surfaces_genisoimage_failure() {
        use crate::seed::genisoimage::recorder::Recorder;

        let dir = unique_tempdir();
        let seed = anthropic_seed();
        let recorder = Recorder::new();
        recorder.enqueue_err("genisoimage: bad volid");

        let err = write_iso_with(&recorder, &dir, &seed).expect_err("scripted failure should propagate");
        match err {
            crate::Error::Seed(SeedError::Genisoimage { detail, .. }) => {
                assert!(detail.contains("bad volid"), "stderr should land in detail: {detail}");
            },
            other => panic!("expected Seed(Genisoimage), got {other:?}"),
        }
    }
}
