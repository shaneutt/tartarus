//! `tartarus auth init google`: bootstrap the Vertex (Google Cloud) backend.
//!
//! Prompts for project ID, region (default `us-east5`), and
//! service-account JSON path, then merges into any existing
//! `config.toml` (preserving `[github]` and `[claude.anthropic]`).

use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use crate::{
    auth::{error::AuthError, prompt, vertex, write},
    config::Backend,
    error::Result,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default Vertex region when the user accepts the prompt's default.
const DEFAULT_REGION: &str = "us-east5";

/// Maximum credential-path attempts before giving up.
const MAX_CREDENTIAL_PATH_ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------------------
// Vertex Init
// ---------------------------------------------------------------------------

/// Drive the interactive `auth init google` flow.
///
/// Merges into the existing config at `path` rather than overwriting,
/// preserving any prior GitHub and Anthropic credentials.
pub fn run<R, W>(path: &Path, reader: &mut R, writer: &mut W) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let project_id = prompt::read_line(reader, writer, "GCP project ID: ")?;
    if project_id.trim().is_empty() {
        return Err(AuthError::InteractiveReadFailed(std::io::Error::other("project ID was empty")).into());
    }
    if !crate::seed::input::is_safe_single_line(&project_id) {
        return Err(
            AuthError::InteractiveReadFailed(std::io::Error::other("project ID contains unsafe characters")).into(),
        );
    }

    let region =
        prompt::read_line_with_default(reader, writer, &format!("Region [{DEFAULT_REGION}]: "), DEFAULT_REGION)?;
    if !crate::seed::input::is_safe_single_line(&region) {
        return Err(
            AuthError::InteractiveReadFailed(std::io::Error::other("region contains unsafe characters")).into(),
        );
    }

    let credentials_file = collect_credentials_path(reader, writer)?;

    write::merge_and_write_config(path, |cfg| {
        cfg.claude.backend = Some(Backend::Vertex);
        cfg.claude.vertex.project_id = Some(project_id);
        cfg.claude.vertex.region = Some(region);
        cfg.claude.vertex.credentials_file = Some(credentials_file);
    })?;

    writeln!(writer, "wrote {} (mode 0600)", path.display()).map_err(AuthError::InteractiveWriteFailed)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Credential Path Collection
// ---------------------------------------------------------------------------

/// Prompt for the service-account JSON path, re-prompting on
/// validation failure up to [`MAX_CREDENTIAL_PATH_ATTEMPTS`] times.
fn collect_credentials_path<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> Result<PathBuf> {
    let mut last_err: Option<crate::error::Error> = None;

    for _ in 0..MAX_CREDENTIAL_PATH_ATTEMPTS {
        let raw = prompt::read_line(reader, writer, "Service-account JSON file (absolute path): ")?;

        if raw.trim().is_empty() {
            writeln!(writer, "  (path is required)").map_err(AuthError::InteractiveWriteFailed)?;
            continue;
        }

        let candidate = PathBuf::from(raw.trim());

        match vertex::validate_service_account_file(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) => {
                writeln!(writer, "  {err}").map_err(AuthError::InteractiveWriteFailed)?;
                last_err = Some(err);
            },
        }
    }

    Err(last_err.unwrap_or_else(|| {
        AuthError::InteractiveReadFailed(std::io::Error::other("no valid service-account path supplied")).into()
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        io::{BufReader, Cursor},
        path::PathBuf,
    };

    use super::*;
    use crate::error::Error;

    #[test]
    fn merges_vertex_into_existing_config() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        crate::auth::write::write_config(
            &path,
            &crate::config::FileConfig {
                claude: crate::config::ClaudeSection {
                    anthropic: crate::config::ClaudeAnthropicSection {
                        api_key: Some("sk-ant-existing".to_owned()),
                    },
                    backend: Some(Backend::Anthropic),
                    ..crate::config::ClaudeSection::default()
                },
                github: crate::config::GithubSection {
                    token: Some("ghp_existing".to_owned()),
                },
                ..crate::config::FileConfig::default()
            },
            false,
        )
        .expect("seed should write");

        let sa_path = dir.join("sa.json");
        std::fs::write(&sa_path, br#"{"type":"service_account"}"#).expect("write sa.json");

        let canonical_sa = sa_path.canonicalize().expect("canonicalize sa.json");

        let sa_display = canonical_sa.display();
        let input = format!("my-project\n\n{sa_display}\n");
        let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
        let mut writer: Vec<u8> = Vec::new();

        run(&path, &mut reader, &mut writer).expect("vertex flow should succeed");

        let parsed = crate::config::load_from(&path).expect("merged config should load");

        assert_eq!(
            parsed.claude.backend,
            Some(Backend::Vertex),
            "vertex flow should switch the backend selector",
        );
        assert_eq!(parsed.claude.vertex.project_id.as_deref(), Some("my-project"));
        assert_eq!(
            parsed.claude.vertex.region.as_deref(),
            Some("us-east5"),
            "default region should apply",
        );
        assert_eq!(
            parsed.claude.vertex.credentials_file.as_deref(),
            Some(canonical_sa.as_path()),
        );
        assert_eq!(
            parsed.github.token.as_deref(),
            Some("ghp_existing"),
            "merging should preserve the existing GitHub token",
        );
        assert_eq!(
            parsed.claude.anthropic.api_key.as_deref(),
            Some("sk-ant-existing"),
            "merging should preserve the existing Anthropic key",
        );
    }

    #[test]
    fn invalid_json_is_rejected_at_init_time() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        let bad_path = dir.join("bad.json");
        std::fs::write(&bad_path, b"definitely not json").expect("write bad json");

        let canonical_bad = bad_path.canonicalize().expect("canonicalize bad.json");

        let input = format!("proj\n\n{0}\n{0}\n{0}\n", canonical_bad.display());
        let mut reader = BufReader::new(Cursor::new(input.into_bytes()));
        let mut writer: Vec<u8> = Vec::new();

        let err = run(&path, &mut reader, &mut writer).expect_err("invalid JSON should bail out after the retries");

        match err {
            Error::Auth(AuthError::VertexFileParse { .. }) => {},
            other => panic!("expected VertexFileParse, got {other:?}"),
        }
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
        let path = std::env::temp_dir().join(format!("tartarus-auth-google-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir root");

        path
    }
}
