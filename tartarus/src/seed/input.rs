//! Structured input for [`crate::seed::render`].

use std::{fmt, path::Path};

use serde::{Deserialize, Serialize};

use crate::{auth::error::AuthError, error::Result, host_user::HostUser};

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
/// The Vertex variant caches the canonical-JSON SA body (read once at
/// [`Seed::from_config`] time) so the renderer stays pure.
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

/// Non-config inputs to [`Seed::from_config`].
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

impl Seed {
    /// Build a [`Seed`] from a resolved config, per-run inputs, and
    /// an optional alias.
    ///
    /// Returns `Ok(None)` when credentials or repos are missing.
    /// Returns `Err` when the Vertex SA file cannot be read.
    pub fn from_config(
        config: &crate::config::Config,
        alias: Option<&str>,
        inputs: SeedInputs,
    ) -> Result<Option<Self>> {
        let Some(credentials) = build_credentials(config)? else {
            return Ok(None);
        };
        let name = alias.map_or_else(|| "(unnamed)".to_owned(), str::to_owned);

        let Some(repos) = resolve_repos(&inputs, config) else {
            return Ok(None);
        };

        Ok(Some(Self {
            name,
            credentials,
            envs: config.base_envs.clone(),
            remote_connect: inputs.remote_connect,
            repos,
            ssh_pubkey: inputs.ssh_pubkey,
            uuid: inputs.uuid,
            user: inputs.user,
        }))
    }
}

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

// -----------------------------------------------------------------------------
// Seed Assembly
// -----------------------------------------------------------------------------

/// Valid character in one half of an `owner/name` slug.
fn is_slug_component_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

/// Resolve the ordered repo list. Returns `None` when empty.
fn resolve_repos(inputs: &SeedInputs, config: &crate::config::Config) -> Option<Vec<RepoSpec>> {
    let (slugs, config_flagged_default) = if inputs.repos.is_empty() {
        if config.base_repos.is_empty() {
            return None;
        }
        let slugs: Vec<String> = config.base_repos.iter().map(|r| r.slug.clone()).collect();
        let flagged = config.base_repos.iter().find(|r| r.default).map(|r| r.slug.clone());
        (slugs, flagged)
    } else {
        (inputs.repos.clone(), None)
    };

    let chosen_default = inputs
        .default_repo
        .clone()
        .or_else(|| config.base_default_repo.clone())
        .or(config_flagged_default);

    let default_slug = chosen_default
        .filter(|slug| slugs.iter().any(|s| s == slug))
        .unwrap_or_else(|| slugs[0].clone());

    Some(
        slugs
            .into_iter()
            .map(|slug| RepoSpec {
                default: slug == default_slug,
                slug,
            })
            .collect(),
    )
}

/// Assemble [`Credentials`] from the resolved config. Returns
/// `Ok(None)` when required fields are missing.
fn build_credentials(config: &crate::config::Config) -> Result<Option<Credentials>> {
    let backend = match config.claude_backend {
        crate::config::Backend::Anthropic => {
            let Some(api_key) = config.claude_anthropic_api_key.clone() else {
                return Ok(None);
            };
            CredentialBundle::Anthropic { api_key }
        },
        crate::config::Backend::Bedrock => return Ok(None),
        crate::config::Backend::Vertex => {
            let (Some(file), Some(project_id), Some(region)) = (
                config.claude_vertex_credentials_file.as_deref(),
                config.claude_vertex_project_id.clone(),
                config.claude_vertex_region.clone(),
            ) else {
                return Ok(None);
            };
            CredentialBundle::Vertex {
                credentials_json: read_vertex_credentials(file)?,
                project_id,
                region,
            }
        },
    };

    let Some(github_token) = config.github_token.clone() else {
        return Ok(None);
    };

    Ok(Some(Credentials {
        backend,
        claude: ClaudeDefaults {
            effort: config.claude_effort.clone(),
            model: config.claude_model.clone(),
        },
        github_token,
    }))
}

/// Read and re-serialise a Vertex SA file to canonical compact JSON.
///
/// Re-serialising guarantees no unescaped newlines outside JSON-string
/// contexts, closing the cloud-init block-scalar breaker.
fn read_vertex_credentials(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|err| AuthError::VertexFileRead {
        path: path.to_path_buf(),
        source: err,
    })?;

    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|err| AuthError::VertexFileRead {
        path: path.to_path_buf(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
    })?;

    Ok(value.to_string())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Backend, Config};

    #[test]
    fn from_config_populates_anthropic_bundle() {
        let config = anthropic_config();
        let seed = Seed::from_config(&config, Some("fix-bug"), sample_inputs("abcd-1234", false))
            .expect("anthropic config should not error")
            .expect("anthropic config should yield a populated Seed");

        assert_eq!(seed.uuid, "abcd-1234", "uuid should round-trip");
        assert_eq!(seed.name, "fix-bug", "alias should round-trip into name");
        assert!(!seed.remote_connect, "remote_connect should default off");
        assert_eq!(seed.repos.len(), 1, "single-repo invocation should yield one RepoSpec",);
        assert!(seed.repos[0].default, "the only repo should be marked default");
        match seed.credentials.backend {
            CredentialBundle::Anthropic { api_key } => {
                assert_eq!(api_key, "sk-ant-test", "anthropic key should round-trip");
            },
            other => panic!("expected anthropic bundle, got {other:?}"),
        }
    }

    #[test]
    fn from_config_promotes_explicit_default_repo() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: Some("owner/two".to_owned()),
            remote_connect: false,
            repos: vec!["owner/one".to_owned(), "owner/two".to_owned(), "owner/three".to_owned()],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = Seed::from_config(&config, None, inputs)
            .expect("multi-repo config should not error")
            .expect("multi-repo config should yield a populated Seed");

        assert_eq!(
            seed.repos.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
            vec!["owner/one", "owner/two", "owner/three"],
            "repo order should be preserved from inputs",
        );
        assert_eq!(
            seed.repos.iter().filter(|r| r.default).count(),
            1,
            "exactly one repo should be marked default",
        );
        assert!(
            seed.repos
                .iter()
                .find(|r| r.slug == "owner/two")
                .is_some_and(|r| r.default),
            "the slug named in default_repo should win",
        );
    }

    #[test]
    fn from_config_falls_back_to_first_listed_when_no_default_set() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: None,
            remote_connect: false,
            repos: vec!["owner/alpha".to_owned(), "owner/beta".to_owned()],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = Seed::from_config(&config, None, inputs)
            .expect("multi-repo input should not error")
            .expect("multi-repo input should yield a Seed");

        assert!(
            seed.repos.first().is_some_and(|r| r.default && r.slug == "owner/alpha"),
            "first listed slug should win when no explicit default is supplied",
        );
    }

    #[test]
    fn from_config_uses_config_repos_when_cli_has_none() {
        let mut config = anthropic_config();
        config.base_repos = vec![
            crate::config::RepoEntry {
                default: false,
                slug: "owner/alpha".to_owned(),
            },
            crate::config::RepoEntry {
                default: true,
                slug: "owner/beta".to_owned(),
            },
        ];

        let inputs = SeedInputs {
            default_repo: None,
            remote_connect: false,
            repos: vec![],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = Seed::from_config(&config, None, inputs)
            .expect("config-side repos should not error")
            .expect("config-side repos should yield a Seed");

        assert_eq!(
            seed.repos.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
            vec!["owner/alpha", "owner/beta"],
            "config repo order should be preserved",
        );
        assert!(
            seed.repos
                .iter()
                .find(|r| r.slug == "owner/beta")
                .is_some_and(|r| r.default),
            "the config-flagged default should win",
        );
    }

    #[test]
    fn from_config_returns_none_when_no_repos_anywhere() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: None,
            remote_connect: false,
            repos: vec![],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = Seed::from_config(&config, None, inputs).expect("repo-empty path should not error");

        assert!(
            seed.is_none(),
            "no CLI repos and no config repos should yield None so the caller surfaces the helpful failure",
        );
    }

    #[test]
    fn from_config_returns_none_when_credentials_missing() {
        let mut config = anthropic_config();
        config.github_token = None;

        let seed =
            Seed::from_config(&config, None, sample_inputs("abc", false)).expect("missing-token path should not error");

        assert!(
            seed.is_none(),
            "missing GitHub token should return None so the caller renders the helpful failure",
        );
    }

    #[test]
    fn from_config_uses_unnamed_when_no_alias() {
        let seed = Seed::from_config(&anthropic_config(), None, sample_inputs("abc", false))
            .expect("config path should not error")
            .expect("config should yield Seed");

        assert_eq!(
            seed.name, "(unnamed)",
            "absent alias should surface the documented `(unnamed)` placeholder",
        );
    }

    #[test]
    fn from_config_propagates_remote_connect_flag() {
        let seed = Seed::from_config(&anthropic_config(), None, sample_inputs("abc", true))
            .expect("config path should not error")
            .expect("config should yield Seed");

        assert!(
            seed.remote_connect,
            "background-mode flag should round-trip into the Seed",
        );
    }

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

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    fn anthropic_config() -> Config {
        Config {
            base_default_repo: None,
            base_envs: vec!["rust".to_owned()],
            base_repos: vec![],
            claude_backend: Backend::Anthropic,
            claude_anthropic_api_key: Some("sk-ant-test".to_owned()),
            claude_effort: "high".to_owned(),
            claude_model: "claude-opus-4-7".to_owned(),
            claude_vertex_credentials_file: None,
            claude_vertex_project_id: None,
            claude_vertex_region: None,
            disk_grow_increment_gib: 100,
            disk_grow_threshold_pct: 85,
            disk_virtual_size_gib: 100,
            github_token: Some("ghp_test".to_owned()),
            network_uri: "qemu:///session".to_owned(),
            rust_cargo_tools: vec![],
            rust_components: vec![],
            rust_toolchains: vec![],
            user_gid: None,
            user_uid: None,
            user_username: None,
            vm_memory_mib: 4_096,
            vm_vcpus: 2,
        }
    }

    fn sample_user() -> HostUser {
        HostUser {
            gid: 1000,
            uid: 1000,
            username: "alice".to_owned(),
        }
    }

    fn sample_inputs(uuid: &str, remote_connect: bool) -> SeedInputs {
        SeedInputs {
            default_repo: None,
            remote_connect,
            repos: vec!["owner/name".to_owned()],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: uuid.to_owned(),
        }
    }
}
