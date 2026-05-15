//! Structured input for [`crate::seed::render`].

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::host_user::HostUser;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Maximum repo slug length in chars.
const MAX_SLUG_LEN: usize = 100;

/// Maximum single-line credential length in bytes (4 KiB).
const MAX_SINGLE_LINE_CREDENTIAL_LEN: usize = 4096;

// -----------------------------------------------------------------------------
// Seed Types
// -----------------------------------------------------------------------------

/// Repository spec for the in-guest clone. Exactly one per session
/// is flagged default.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct RepoSpec {
    /// Whether this is the session's default working directory.
    pub default: bool,

    /// `owner/name` slug, e.g. `the-lost-art-of-programming/tartarus`.
    pub slug: String,
}

/// Claude backend bundle injected into the seed.
///
/// The Vertex variant caches the canonical-JSON SA body (read once
/// before the renderer runs) so the renderer stays pure.
#[derive(Clone, Eq, PartialEq)]
pub enum CredentialBundle {
    /// Direct Anthropic API key.
    Anthropic {
        /// Anthropic API key (`sk-ant-...`).
        api_key: String,
    },

    /// Vertex AI bundle.
    Vertex {
        /// Canonical-JSON SA body, embedded verbatim into the seed ISO.
        credentials_json: String,

        /// GCP project ID.
        project_id: String,

        /// GCP region.
        region: String,
    },
}

impl fmt::Debug for CredentialBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic { .. } => f.debug_struct("Anthropic").field("api_key", &"[REDACTED]").finish(),
            Self::Vertex { project_id, region, .. } => f
                .debug_struct("Vertex")
                .field("credentials_json", &"[REDACTED]")
                .field("project_id", project_id)
                .field("region", region)
                .finish(),
        }
    }
}

/// Claude runtime defaults (env files).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeDefaults {
    /// Effort tier (e.g. `high`).
    pub effort: String,

    /// Model identifier (e.g. `claude-opus-4-7`).
    pub model: String,
}

/// Seed credentials bundle.
#[derive(Clone, Eq, PartialEq)]
pub struct Credentials {
    /// Active Claude backend bundle.
    pub backend: CredentialBundle,

    /// Claude runtime defaults (model, effort).
    pub claude: ClaudeDefaults,

    /// GitHub PAT for `gh` and `git clone`.
    pub github_token: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("backend", &self.backend)
            .field("claude", &self.claude)
            .field("github_token", &"[REDACTED]")
            .finish()
    }
}

/// Full structured input to [`crate::seed::render::render`].
#[derive(Clone, Eq, PartialEq)]
pub struct Seed {
    /// Display name (alias or `(unnamed)`). Surfaces in
    /// `local-hostname`.
    pub name: String,

    /// Credentials bundle.
    pub credentials: Credentials,

    /// Programming envs to activate at first boot.
    pub envs: Vec<String>,

    /// Enable Claude remote-connect mode (background sessions).
    pub remote_connect: bool,

    /// Repos to clone at first boot.
    pub repos: Vec<RepoSpec>,

    /// SSH public key for `authorized_keys`, or `None`.
    pub ssh_pubkey: Option<String>,

    /// Session UUID (cloud-init `instance-id`).
    pub uuid: String,

    /// Invoking user identity, mirrored inside the guest.
    pub user: HostUser,
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Seed")
            .field("name", &self.name)
            .field("credentials", &self.credentials)
            .field("envs", &self.envs)
            .field("remote_connect", &self.remote_connect)
            .field("repos", &self.repos)
            .field("ssh_pubkey", &self.ssh_pubkey.as_deref().map(|_| "[REDACTED]"))
            .field("uuid", &self.uuid)
            .field("user", &self.user)
            .finish()
    }
}

/// Non-config inputs to the seed builder run by the binary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeedInputs {
    /// CLI override for the default repo slug.
    pub default_repo: Option<String>,

    /// Background mode (enables remote-connect).
    pub remote_connect: bool,

    /// Repos to clone at first boot (order preserved).
    pub repos: Vec<String>,

    /// SSH public key for `authorized_keys`, or `None`.
    pub ssh_pubkey: Option<String>,

    /// User identity mirrored inside the guest.
    pub user: HostUser,

    /// Session UUID — also the cloud-init `instance-id`.
    pub uuid: String,
}

// -----------------------------------------------------------------------------
// Validation
// -----------------------------------------------------------------------------

/// Whether `slug` matches `[A-Za-z0-9._-]+/[A-Za-z0-9._-]+` (≤ 100
/// chars).
pub fn is_valid_repo_slug(slug: &str) -> bool {
    if slug.is_empty() || slug.len() > MAX_SLUG_LEN {
        return false;
    }

    let parts: Vec<&str> = slug.split('/').collect();
    if parts.len() != 2 {
        return false;
    }

    parts
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(is_slug_component_char))
}

/// Whether `value` is safe as a single-line credential: no ASCII
/// control characters, at most 4 KiB.
pub fn is_safe_single_line(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_SINGLE_LINE_CREDENTIAL_LEN {
        return false;
    }

    !value.chars().any(|c| c.is_ascii_control())
}

/// Valid character in one half of an `owner/name` slug.
fn is_slug_component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_repo_slug_accepts_canonical_slugs() {
        for slug in [
            "owner/name",
            "the-lost-art-of-programming/tartarus",
            "Owner.Name/repo_name",
            "a/b",
            "a-1/b_2.x",
        ] {
            assert!(is_valid_repo_slug(slug), "{slug} should validate");
        }
    }

    #[test]
    fn is_valid_repo_slug_rejects_metacharacters_and_traversal() {
        for slug in [
            "",
            "owner",
            "/owner/name",
            "owner/",
            "/owner",
            "owner/name/extra",
            "../etc/passwd",
            "owner; rm -rf /",
            "owner name/x",
            "owner/x y",
            "owner/x\nrm",
            &"x".repeat(101),
        ] {
            assert!(!is_valid_repo_slug(slug), "{slug:?} must not validate");
        }
    }

    #[test]
    fn is_safe_single_line_accepts_typical_credentials() {
        for ok in [
            "ghp_xxxxxxxxxxxxxxxxxxxx",
            "sk-ant-xxxxxxxxxxxxxxxxxxxxxxxx",
            "us-east5",
            "claude-opus-4-7",
            "high",
            "my-gcp-project",
        ] {
            assert!(is_safe_single_line(ok), "{ok} should validate");
        }
    }

    #[test]
    fn is_safe_single_line_rejects_control_characters() {
        for bad in ["", "ghp_xxx\n", "ghp_xxx\rgh", "ghp_xxx\0", "ghp\tabc", "line1\nline2"] {
            assert!(!is_safe_single_line(bad), "{bad:?} must not validate");
        }
    }
}
