//! Validation for the Vertex service-account credential file.
//!
//! Paths are taken literally (no `~` expansion, no env-var
//! interpolation); the user must supply an absolute path.

use std::path::Path;

use crate::{auth::error::AuthError, error::Result};

// -----------------------------------------------------------------------------
// Validation
// -----------------------------------------------------------------------------

/// Verify `path` is absolute, readable, and parses as JSON.
///
/// Does not validate service-account-shaped fields; the Anthropic
/// CLI validates those at token-exchange time.
pub fn validate_service_account_file(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(AuthError::PathNotAbsolute {
            path: path.to_path_buf(),
        }
        .into());
    }

    let bytes = std::fs::read(path).map_err(|source| AuthError::VertexFileRead {
        path: path.to_path_buf(),
        source,
    })?;

    serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|source| AuthError::VertexFileParse {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::error::Error;

    #[test]
    fn relative_path_is_rejected() {
        let err = validate_service_account_file(Path::new("relative/path.json"))
            .expect_err("relative paths should be rejected up front");

        match err {
            Error::Auth(AuthError::PathNotAbsolute { .. }) => {},
            other => panic!("expected PathNotAbsolute, got {other:?}"),
        }
    }

    #[test]
    fn missing_file_is_rejected() {
        let dir = tempdir();
        let path = dir.join("does-not-exist.json");

        let err = validate_service_account_file(&path).expect_err("missing file should be rejected");

        match err {
            Error::Auth(AuthError::VertexFileRead { path: reported, .. }) => {
                assert_eq!(reported, path, "error should report the path we tried");
            },
            other => panic!("expected VertexFileRead, got {other:?}"),
        }
    }

    #[test]
    fn invalid_json_is_rejected() {
        let dir = tempdir();
        let path = dir.join("garbage.json");
        std::fs::write(&path, b"this is not JSON {{{{}}").expect("write garbage");

        let err = validate_service_account_file(&path).expect_err("invalid JSON should be rejected");

        match err {
            Error::Auth(AuthError::VertexFileParse { path: reported, .. }) => {
                assert_eq!(reported, path, "error should report the path we tried");
            },
            other => panic!("expected VertexFileParse, got {other:?}"),
        }
    }

    #[test]
    fn valid_json_is_accepted() {
        let dir = tempdir();
        let path = dir.join("good.json");
        std::fs::write(&path, br#"{"type":"service_account","project_id":"x"}"#).expect("write good json");

        validate_service_account_file(&path).expect("a parseable JSON file should validate");
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    /// Unique per-process temp directory.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-auth-vertex-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir root");

        path
    }
}
