//! Per-session qcow2 overlay creation and removal via `qemu-img`.
//!
//! A post-create `qemu-img info` sanity check confirms the file is
//! valid qcow2 with the expected backing pointer.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::Deserialize;

use crate::error::{Error, Result};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// File name of the per-session overlay inside its session directory.
pub const OVERLAY_FILE_NAME: &str = "overlay.qcow2";

/// Argument name for `qemu-img`'s overlay format.
const OVERLAY_FORMAT: &str = "qcow2";

// -----------------------------------------------------------------------------
// Overlay
// -----------------------------------------------------------------------------

/// Failure modes specific to overlay creation and destruction.
#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    /// The configured backing base image does not exist.
    #[error("overlay base {path} does not exist; hint: run `tartarus base pull` first")]
    BaseMissing {
        /// Path that was probed.
        path: PathBuf,
    },

    /// A qcow2 file written by `qemu-img create` failed the post-write
    /// `qemu-img info` sanity check.
    #[error(
        "overlay {path} failed `qemu-img info` validation: {detail}. \
         hint: re-run `tartarus run` after deleting the session dir."
    )]
    InvalidOverlay {
        /// Short detail extracted from the validator output.
        detail: String,

        /// Path that was checked.
        path: PathBuf,
    },

    /// `qemu-img` reported a failure while creating or inspecting an
    /// overlay.
    #[error("`qemu-img {operation}` failed for {path}: {detail}")]
    QemuImg {
        /// Short detail extracted from stderr or the exit status.
        detail: String,

        /// Static label identifying the operation.
        operation: &'static str,

        /// Path of the overlay being created or inspected.
        path: PathBuf,
    },
}

/// Per-session overlay handle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Overlay {
    /// Path to the read-only base image that backs the overlay.
    pub base_path: PathBuf,

    /// Absolute path to the overlay file.
    pub path: PathBuf,

    /// Configured virtual size in GiB.
    pub virtual_size_gib: u32,
}

impl Overlay {
    /// Create a fresh qcow2 overlay at `<session_dir>/overlay.qcow2`,
    /// validated via `qemu-img info` post-create.
    pub fn create(session_dir: &Path, base_path: &Path, virtual_size_gib: u32) -> Result<Self> {
        if !base_path.exists() {
            return Err(OverlayError::BaseMissing {
                path: base_path.to_path_buf(),
            }
            .into());
        }

        let path = session_dir.join(OVERLAY_FILE_NAME);

        run_create(&path, base_path, virtual_size_gib)?;

        validate_overlay(&path, base_path)?;

        tracing::info!(
            overlay = %path.display(),
            base = %base_path.display(),
            virtual_size_gib,
            "created session overlay",
        );

        Ok(Self {
            base_path: base_path.to_path_buf(),
            path,
            virtual_size_gib,
        })
    }

    /// Delete the overlay file, ignoring `NotFound`.
    pub fn destroy(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {
                tracing::debug!(overlay = %self.path.display(), "removed session overlay");
                Ok(())
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(Error::Io(err)),
        }
    }
}

/// Build the `qemu-img create` argument vector.
pub fn create_args(path: &Path, base_path: &Path, virtual_size_gib: u32) -> Vec<String> {
    vec![
        "create".to_owned(),
        "-f".to_owned(),
        OVERLAY_FORMAT.to_owned(),
        "-b".to_owned(),
        base_path.display().to_string(),
        "-F".to_owned(),
        OVERLAY_FORMAT.to_owned(),
        path.display().to_string(),
        format!("{virtual_size_gib}G"),
    ]
}

/// Subset of `qemu-img info --output=json` we validate post-create.
#[derive(Debug, Deserialize)]
struct OverlayInfo {
    /// `qemu-img info` reports the resolved backing path here.
    #[serde(rename = "backing-filename")]
    backing_filename: Option<String>,

    /// `qemu-img info`'s `format` field; we require `qcow2`.
    format: Option<String>,
}

/// Validate `qemu-img info` JSON: format must be qcow2 and
/// backing-filename must match `expected_base`.
pub fn validate_info_json(json: &[u8], expected_base: &Path, overlay: &Path) -> Result<()> {
    let info: OverlayInfo = serde_json::from_slice(json).map_err(|err| OverlayError::InvalidOverlay {
        detail: err.to_string(),
        path: overlay.to_path_buf(),
    })?;

    if info.format.as_deref() != Some(OVERLAY_FORMAT) {
        return Err(OverlayError::InvalidOverlay {
            detail: format!("expected format=qcow2, got {:?}", info.format),
            path: overlay.to_path_buf(),
        }
        .into());
    }

    if info.backing_filename.as_deref() != Some(expected_base.display().to_string().as_str()) {
        return Err(OverlayError::InvalidOverlay {
            detail: format!(
                "expected backing-filename={}, got {:?}",
                expected_base.display(),
                info.backing_filename,
            ),
            path: overlay.to_path_buf(),
        }
        .into());
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Overlay Validation
// -----------------------------------------------------------------------------

/// Execute the `qemu-img create` shell-out.
fn run_create(path: &Path, base_path: &Path, virtual_size_gib: u32) -> Result<()> {
    let args = create_args(path, base_path, virtual_size_gib);
    let output = Command::new("qemu-img")
        .args(&args)
        .output()
        .map_err(|err| OverlayError::QemuImg {
            detail: err.to_string(),
            operation: "create",
            path: path.to_path_buf(),
        })?;

    if !output.status.success() {
        return Err(OverlayError::QemuImg {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            operation: "create",
            path: path.to_path_buf(),
        }
        .into());
    }

    Ok(())
}

/// Run `qemu-img info --output=json` and pipe the JSON through
/// [`validate_info_json`].
fn validate_overlay(path: &Path, base_path: &Path) -> Result<()> {
    let output = Command::new("qemu-img")
        .arg("info")
        .arg("--output=json")
        .arg(path)
        .output()
        .map_err(|err| OverlayError::QemuImg {
            detail: err.to_string(),
            operation: "info",
            path: path.to_path_buf(),
        })?;

    if !output.status.success() {
        return Err(OverlayError::QemuImg {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            operation: "info",
            path: path.to_path_buf(),
        }
        .into());
    }

    validate_info_json(&output.stdout, base_path, path)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn create_args_match_documented_invocation() {
        let path = PathBuf::from("/data/sessions/by-uuid/abc/overlay.qcow2");
        let base = PathBuf::from("/data/base/fedora-41-2026-05-01.qcow2");

        let args = create_args(&path, &base, 100);

        assert_eq!(
            args,
            vec![
                "create".to_owned(),
                "-f".to_owned(),
                "qcow2".to_owned(),
                "-b".to_owned(),
                "/data/base/fedora-41-2026-05-01.qcow2".to_owned(),
                "-F".to_owned(),
                "qcow2".to_owned(),
                "/data/sessions/by-uuid/abc/overlay.qcow2".to_owned(),
                "100G".to_owned(),
            ],
            "qemu-img create args should match the documented spec.md invocation",
        );
    }

    #[test]
    fn validate_info_json_accepts_matching_backing() {
        let base = PathBuf::from("/base/current.qcow2");
        let overlay = PathBuf::from("/overlay.qcow2");
        let json = br#"{"format":"qcow2","backing-filename":"/base/current.qcow2","virtual-size":107374182400}"#;

        validate_info_json(json, &base, &overlay).expect("matching backing should validate");
    }

    #[test]
    fn validate_info_json_rejects_format_mismatch() {
        let base = PathBuf::from("/base/current.qcow2");
        let overlay = PathBuf::from("/overlay.raw");
        let json = br#"{"format":"raw","backing-filename":"/base/current.qcow2"}"#;

        let err = validate_info_json(json, &base, &overlay).expect_err("non-qcow2 format should fail");

        match err {
            Error::Overlay(OverlayError::InvalidOverlay { detail, .. }) => {
                assert!(
                    detail.contains("qcow2"),
                    "detail should mention the format mismatch, got: {detail}",
                );
            },
            other => panic!("expected OverlayError::InvalidOverlay, got {other:?}"),
        }
    }

    #[test]
    fn validate_info_json_rejects_backing_mismatch() {
        let base = PathBuf::from("/base/current.qcow2");
        let overlay = PathBuf::from("/overlay.qcow2");
        let json = br#"{"format":"qcow2","backing-filename":"/elsewhere/old.qcow2"}"#;

        let err = validate_info_json(json, &base, &overlay).expect_err("backing mismatch should fail");

        match err {
            Error::Overlay(OverlayError::InvalidOverlay { detail, .. }) => {
                assert!(
                    detail.contains("backing-filename"),
                    "detail should mention backing-filename, got: {detail}",
                );
            },
            other => panic!("expected OverlayError::InvalidOverlay, got {other:?}"),
        }
    }

    #[test]
    fn create_rejects_missing_base() {
        let session = unique_tempdir();
        let base = session.join("does-not-exist.qcow2");

        let err = Overlay::create(&session, &base, 100).expect_err("missing base should be rejected");

        match err {
            Error::Overlay(OverlayError::BaseMissing { path }) => {
                assert_eq!(path, base, "reported path should match the missing base");
            },
            other => panic!("expected OverlayError::BaseMissing, got {other:?}"),
        }
    }

    #[test]
    fn create_round_trips_against_real_qemu_img() {
        let session = unique_tempdir();
        let base = session.join("base.qcow2");
        std::process::Command::new("qemu-img")
            .args(["create", "-f", "qcow2", base.to_str().unwrap(), "16M"])
            .output()
            .expect("qemu-img create base should succeed");

        let overlay = Overlay::create(&session, &base, 8).expect("overlay create should succeed against real qemu-img");

        assert!(
            overlay.path.exists(),
            "overlay file should exist after create, got: {}",
            overlay.path.display(),
        );
        assert_eq!(
            overlay.base_path, base,
            "overlay base_path should round-trip into the handle",
        );

        overlay.destroy().expect("overlay destroy should succeed");
        assert!(
            !overlay.path.exists(),
            "overlay file should be deleted after destroy, got: {}",
            overlay.path.display(),
        );
    }

    #[test]
    fn destroy_is_idempotent_for_missing_file() {
        let session = unique_tempdir();
        let overlay = Overlay {
            base_path: session.join("base.qcow2"),
            path: session.join("never-existed.qcow2"),
            virtual_size_gib: 8,
        };

        overlay.destroy().expect("destroy on missing file should be a no-op");
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn unique_tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-overlay-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in tempdir root");

        path
    }
}
