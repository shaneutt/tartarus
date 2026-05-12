//! `tartarus auth status`: print configured credentials, redacted.

use std::io::Write;

use crate::{
    auth::{error::AuthError, redact},
    config::{self, FileConfig},
    error::Result,
};

// -----------------------------------------------------------------------------
// Status Reporting
// -----------------------------------------------------------------------------

/// Render the credential status report to `writer`.
///
/// `file` recovers individual credential values for redaction (the
/// resolved view drops the unselected backend's bundle).
pub fn print<W: Write>(writer: &mut W, resolved: Option<&config::Config>, file: Option<&FileConfig>) -> Result<()> {
    let backend_label = backend_label(resolved, file);

    writeln!(writer, "claude backend: {backend_label}").map_err(AuthError::InteractiveWriteFailed)?;

    let github_token = file.and_then(|f| f.github.token.as_deref());
    write_credential_line(writer, "github token", github_token)?;

    let anthropic_key = file.and_then(|f| f.claude.anthropic.api_key.as_deref());
    write_credential_line(writer, "anthropic api key", anthropic_key)?;

    let vertex_project = file.and_then(|f| f.claude.vertex.project_id.as_deref());
    let vertex_region = file.and_then(|f| f.claude.vertex.region.as_deref());
    let vertex_creds = file.and_then(|f| f.claude.vertex.credentials_file.as_deref());

    write_plain_line(writer, "vertex project id", vertex_project)?;
    write_plain_line(writer, "vertex region", vertex_region)?;

    let creds_label = vertex_creds.map(|p| p.display().to_string());
    write_plain_line(writer, "vertex credentials file", creds_label.as_deref())?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

/// Human-readable label for the active backend, preferring the resolved
/// view (which applies the default) over the raw file.
fn backend_label(resolved: Option<&config::Config>, file: Option<&FileConfig>) -> String {
    if let Some(cfg) = resolved {
        let backend = cfg.claude_backend;
        return format!("{backend:?}").to_lowercase();
    }

    if let Some(f) = file
        && let Some(b) = f.claude.backend
    {
        return format!("{b:?}").to_lowercase();
    }

    "not configured".to_owned()
}

/// Write a redacted credential line.
fn write_credential_line<W: Write>(writer: &mut W, label: &str, value: Option<&str>) -> Result<()> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => writeln!(writer, "{label}: configured (last 4: {})", redact::last_four(v))
            .map_err(AuthError::InteractiveWriteFailed)?,
        None => writeln!(writer, "{label}: not configured").map_err(AuthError::InteractiveWriteFailed)?,
    }

    Ok(())
}

/// Write a non-secret plain-text line (e.g. project ID, region).
fn write_plain_line<W: Write>(writer: &mut W, label: &str, value: Option<&str>) -> Result<()> {
    match value.filter(|v| !v.is_empty()) {
        Some(v) => writeln!(writer, "{label}: {v}").map_err(AuthError::InteractiveWriteFailed)?,
        None => writeln!(writer, "{label}: not configured").map_err(AuthError::InteractiveWriteFailed)?,
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::{
        Backend, ClaudeAnthropicSection, ClaudeSection, ClaudeVertexSection, FileConfig, GithubSection,
    };

    #[test]
    fn missing_file_reports_everything_unconfigured() {
        let mut writer: Vec<u8> = Vec::new();

        print(&mut writer, None, None).expect("status should never fail on empty input");

        let captured = String::from_utf8(writer).expect("captured output should be UTF-8");

        assert!(captured.contains("github token: not configured"));
        assert!(captured.contains("anthropic api key: not configured"));
        assert!(captured.contains("vertex project id: not configured"));
    }

    #[test]
    fn present_credentials_are_redacted_to_last_four() {
        let file = FileConfig {
            claude: ClaudeSection {
                anthropic: ClaudeAnthropicSection {
                    api_key: Some("sk-ant-secretsecret1234".to_owned()),
                },
                backend: Some(Backend::Anthropic),
                vertex: ClaudeVertexSection {
                    project_id: Some("my-project".to_owned()),
                    region: Some("us-east5".to_owned()),
                    credentials_file: Some(PathBuf::from("/abs/path/sa.json")),
                },
                ..ClaudeSection::default()
            },
            github: GithubSection {
                token: Some("ghp_supersecrettokenABCD".to_owned()),
            },
            ..FileConfig::default()
        };

        let mut writer: Vec<u8> = Vec::new();

        print(&mut writer, None, Some(&file)).expect("status should succeed");

        let captured = String::from_utf8(writer).expect("captured output should be UTF-8");

        assert!(
            captured.contains("github token: configured (last 4: …ABCD)"),
            "github token line should be redacted to last 4: {captured:?}",
        );
        assert!(
            captured.contains("anthropic api key: configured (last 4: …1234)"),
            "anthropic api key line should be redacted to last 4: {captured:?}",
        );
        assert!(
            !captured.contains("ghp_supersecrettoken"),
            "the unredacted github token must NEVER appear in status output: {captured:?}",
        );
        assert!(
            !captured.contains("sk-ant-secretsecret"),
            "the unredacted anthropic key must NEVER appear in status output: {captured:?}",
        );
        assert!(
            captured.contains("vertex project id: my-project"),
            "non-secret vertex project id should be printed verbatim: {captured:?}",
        );
        assert!(
            captured.contains("vertex region: us-east5"),
            "non-secret vertex region should be printed verbatim: {captured:?}",
        );
        assert!(
            captured.contains("vertex credentials file: /abs/path/sa.json"),
            "vertex credentials file path should be printed verbatim: {captured:?}",
        );
        assert!(
            captured.contains("claude backend: anthropic"),
            "backend selector should be printed: {captured:?}",
        );
    }
}
