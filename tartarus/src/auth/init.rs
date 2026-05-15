//! `tartarus auth init`: interactive GitHub + Anthropic credential bootstrap.
//!
//! Prompts for a GitHub PAT (paste-only) and an Anthropic API key
//! (paste, or browser fallback via `xdg-open`), then writes an
//! `[claude] backend = "anthropic"` config atomically. Refuses to
//! overwrite unless `force` is true.

use std::io::{BufRead, Write};

use crate::{
    auth::{error::AuthError, prompt, write},
    config::{Backend, ClaudeAnthropicSection, ClaudeSection, FileConfig, GithubSection},
    error::Result,
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Anthropic console URL for the browser fallback.
const ANTHROPIC_CONSOLE_URL: &str = "https://console.anthropic.com/settings/keys";

/// Known GitHub PAT prefixes. A mismatch warns (token shapes evolve).
const KNOWN_GITHUB_PREFIXES: &[&str] = &["ghp_", "github_pat_"];

// -----------------------------------------------------------------------------
// Init Flow
// -----------------------------------------------------------------------------

/// I/O context for [`run`].
pub struct InitContext<'a, R, W>
where
    R: BufRead,
    W: Write,
{
    /// Allow overwriting an existing config.
    pub force: bool,

    /// Destination config path.
    pub path: &'a std::path::Path,

    /// Input source for prompt lines.
    pub reader: &'a mut R,

    /// Output sink for prompts and status messages.
    pub writer: &'a mut W,
}

/// Drive the interactive `auth init` flow.
///
/// Collects GitHub PAT and Anthropic API key, then writes the config
/// at mode `0600`. Aborts before prompting when the config already
/// exists and `force` is false.
pub fn run<R, W>(ctx: InitContext<'_, R, W>) -> Result<()>
where
    R: BufRead,
    W: Write,
{
    let InitContext {
        force,
        path,
        reader,
        writer,
    } = ctx;

    if !force && path.exists() {
        return Err(AuthError::ConfigAlreadyExists {
            path: path.to_path_buf(),
        }
        .into());
    }

    let github_token = collect_github_token(reader, writer)?;
    let anthropic_key = collect_anthropic_key(reader, writer)?;

    let config = build_anthropic_config(github_token, anthropic_key);

    write::write_config(path, &config, force)?;

    writeln!(writer, "wrote {} (mode 0600)", path.display()).map_err(AuthError::InteractiveWriteFailed)?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Credential Collection
// -----------------------------------------------------------------------------

/// Build the Anthropic-backend [`FileConfig`].
fn build_anthropic_config(github_token: String, anthropic_key: String) -> FileConfig {
    FileConfig {
        claude: ClaudeSection {
            anthropic: ClaudeAnthropicSection {
                api_key: Some(anthropic_key),
            },
            backend: Some(Backend::Anthropic),
            ..ClaudeSection::default()
        },
        github: GithubSection {
            token: Some(github_token),
        },
        ..FileConfig::default()
    }
}

/// Prompt for a GitHub PAT; reject empty or unsafe input.
fn collect_github_token<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> Result<String> {
    let pasted = prompt::read_line(reader, writer, "GitHub personal access token (paste): ")?;

    if pasted.is_empty() {
        return Err(AuthError::GithubTokenMissing.into());
    }

    if !tartarus_provider::seed::input::is_safe_single_line(&pasted) {
        return Err(AuthError::InteractiveReadFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "token contains control characters; paste a clean single-line value",
        ))
        .into());
    }

    if !KNOWN_GITHUB_PREFIXES.iter().any(|p| pasted.starts_with(p)) {
        tracing::warn!(
            "the supplied GitHub token does not match any known prefix ({:?}); accepting anyway",
            KNOWN_GITHUB_PREFIXES,
        );
    }

    Ok(pasted)
}

/// Prompt for an Anthropic API key. Empty input opens the console
/// URL and re-prompts.
fn collect_anthropic_key<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) -> Result<String> {
    let first = prompt::read_line(
        reader,
        writer,
        "Anthropic API key (paste, or press Enter for browser): ",
    )?;

    if !first.is_empty() {
        if !tartarus_provider::seed::input::is_safe_single_line(&first) {
            return Err(AuthError::InteractiveReadFailed(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "API key contains control characters; paste a clean single-line value",
            ))
            .into());
        }
        return Ok(first);
    }

    writeln!(
        writer,
        "Opening {ANTHROPIC_CONSOLE_URL} (Anthropic has no public OAuth flow for API keys)",
    )
    .map_err(AuthError::InteractiveWriteFailed)?;

    try_open_browser(ANTHROPIC_CONSOLE_URL);

    let pasted = prompt::read_line(reader, writer, "Anthropic API key (paste): ")?;

    if pasted.is_empty() {
        return Err(AuthError::AnthropicKeyMissing.into());
    }

    if !tartarus_provider::seed::input::is_safe_single_line(&pasted) {
        return Err(AuthError::InteractiveReadFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "API key contains control characters; paste a clean single-line value",
        ))
        .into());
    }

    Ok(pasted)
}

/// Best-effort `xdg-open` wrapper.
///
/// Accepts `&'static str` so only compile-time URLs reach `xdg-open`
/// (which honours `javascript:` / `file://` schemes). Spawn failures
/// are silently ignored; the URL was already printed.
fn try_open_browser(url: &'static str) {
    let result = std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Err(err) = result {
        tracing::debug!(?err, url, "xdg-open spawn failed; user must open the URL manually");
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{
        io::{BufReader, Cursor},
        path::PathBuf,
    };

    use super::*;
    use crate::error::Error;

    #[test]
    fn pasted_credentials_round_trip_into_a_0600_config() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        let input = b"ghp_pasted_pat\nsk-ant-pasted\n";
        let mut reader = BufReader::new(Cursor::new(input.as_slice()));
        let mut writer: Vec<u8> = Vec::new();

        run(InitContext {
            force: false,
            path: &path,
            reader: &mut reader,
            writer: &mut writer,
        })
        .expect("paste-only flow should succeed");

        let parsed = tartarus_provider::config::load_from(&path).expect("written config should load");

        assert_eq!(parsed.github.token.as_deref(), Some("ghp_pasted_pat"));
        assert_eq!(parsed.claude.anthropic.api_key.as_deref(), Some("sk-ant-pasted"));
        assert_eq!(parsed.claude.backend, Some(Backend::Anthropic));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "init should leave the config at mode 0600");
        }

        let captured = String::from_utf8(writer).expect("captured output should be UTF-8");

        assert!(
            captured.contains("wrote"),
            "init should narrate where it wrote, got {captured:?}",
        );
    }

    #[test]
    fn empty_github_input_is_rejected() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        let input = b"\nsk-ant-pasted\n";
        let mut reader = BufReader::new(Cursor::new(input.as_slice()));
        let mut writer: Vec<u8> = Vec::new();

        let err = run(InitContext {
            force: false,
            path: &path,
            reader: &mut reader,
            writer: &mut writer,
        })
        .expect_err("MVP rejects empty GitHub token (device-flow path is deferred)");

        match err {
            Error::Auth(AuthError::GithubTokenMissing) => {},
            other => panic!("expected GithubTokenMissing, got {other:?}"),
        }
    }

    #[test]
    fn refuses_to_overwrite_existing_file_without_force() {
        let dir = tempdir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "existing = true\n").expect("seed the file");

        let mut reader = BufReader::new(Cursor::new(b""));
        let mut writer: Vec<u8> = Vec::new();

        let err = run(InitContext {
            force: false,
            path: &path,
            reader: &mut reader,
            writer: &mut writer,
        })
        .expect_err("init should refuse to overwrite without --force");

        match err {
            Error::Auth(AuthError::ConfigAlreadyExists { .. }) => {},
            other => panic!("expected ConfigAlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn empty_anthropic_input_after_browser_fallback_errors() {
        let dir = tempdir();
        let path = dir.join("config.toml");

        let input = b"ghp_paste\n\n\n";
        let mut reader = BufReader::new(Cursor::new(input.as_slice()));
        let mut writer: Vec<u8> = Vec::new();

        let err = run(InitContext {
            force: false,
            path: &path,
            reader: &mut reader,
            writer: &mut writer,
        })
        .expect_err("dismissing both prompts should not silently write a key-less config");

        match err {
            Error::Auth(AuthError::AnthropicKeyMissing) => {},
            other => panic!("expected AnthropicKeyMissing, got {other:?}"),
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
        let path = std::env::temp_dir().join(format!("tartarus-auth-init-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir root");

        path
    }
}
