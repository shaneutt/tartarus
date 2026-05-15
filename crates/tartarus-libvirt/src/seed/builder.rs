//! Build a [`Seed`] from a resolved [`Config`] plus per-run
//! [`SeedInputs`].
//!
//! Lives in `tartarus-libvirt` rather than `tartarus-provider`
//! because materialising the Vertex service-account file is a
//! libvirt-flavoured I/O step (the renderer + seed types themselves
//! are provider-agnostic and stay in `tartarus-provider::seed`).

use std::path::{Path, PathBuf};

use tartarus_provider::seed::input::{ClaudeCredentials, ClaudeDefaults, CredentialBundle, RepoSpec, Seed, SeedInputs};

use crate::{
    config::{Backend, Config},
    error::Result,
};

// -----------------------------------------------------------------------------
// SeedBuilderError
// -----------------------------------------------------------------------------

/// Failure modes specific to materialising a [`Seed`].
#[derive(Debug, thiserror::Error)]
pub enum SeedBuilderError {
    /// Could not read the Vertex service-account JSON file from disk.
    #[error("failed to read service-account file at {path}: {source}")]
    VertexFileRead {
        /// Path that failed to read.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The Vertex service-account file did not parse as JSON.
    #[error("service-account file at {path} is not valid JSON: {source}")]
    VertexFileParse {
        /// Path that failed to parse.
        path: PathBuf,

        /// Underlying serde_json error.
        source: serde_json::Error,
    },
}

// -----------------------------------------------------------------------------
// Seed Assembly
// -----------------------------------------------------------------------------

/// Build a [`Seed`] from a resolved config, per-run inputs, and an
/// optional alias.
///
/// Returns `Ok(None)` when the GitHub token or repos are missing.
/// Returns `Err` when the Vertex SA file cannot be read.
pub fn build_seed(config: &Config, alias: Option<&str>, inputs: SeedInputs) -> Result<Option<Seed>> {
    let claude = if config.claude_enabled {
        let Some(creds) = build_claude_credentials(config)? else {
            return Ok(None);
        };
        Some(creds)
    } else {
        None
    };

    let Some(github_token) = config.github_token.clone() else {
        return Ok(None);
    };

    let name = alias.map_or_else(|| "(unnamed)".to_owned(), str::to_owned);

    let Some(repos) = resolve_repos(&inputs, config) else {
        return Ok(None);
    };

    Ok(Some(Seed {
        name,
        claude,
        envs: config.base_envs.clone(),
        github_token,
        remote_connect: inputs.remote_connect,
        repos,
        ssh_pubkey: inputs.ssh_pubkey,
        uuid: inputs.uuid,
        user: inputs.user,
    }))
}

/// Resolve the ordered repo list. Returns `None` when empty.
fn resolve_repos(inputs: &SeedInputs, config: &Config) -> Option<Vec<RepoSpec>> {
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

/// Assemble [`ClaudeCredentials`] from the resolved config. Returns
/// `Ok(None)` when required Claude fields are missing.
fn build_claude_credentials(config: &Config) -> Result<Option<ClaudeCredentials>> {
    let backend = match config.claude_backend {
        Backend::Anthropic => {
            let Some(api_key) = config.claude_anthropic_api_key.clone() else {
                return Ok(None);
            };
            CredentialBundle::Anthropic { api_key }
        },
        Backend::Bedrock => return Ok(None),
        Backend::Vertex => {
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

    Ok(Some(ClaudeCredentials {
        backend,
        defaults: ClaudeDefaults {
            effort: config.claude_effort.clone(),
            model: config.claude_model.clone(),
        },
    }))
}

/// Read and re-serialise a Vertex SA file to canonical compact JSON.
///
/// Re-serialising guarantees no unescaped newlines outside JSON-string
/// contexts, closing the cloud-init block-scalar breaker.
fn read_vertex_credentials(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|err| SeedBuilderError::VertexFileRead {
        path: path.to_path_buf(),
        source: err,
    })?;

    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|source| SeedBuilderError::VertexFileParse {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(value.to_string())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_provider::host_user::HostUser;

    use super::*;

    #[test]
    fn build_seed_populates_anthropic_bundle() {
        let config = anthropic_config();
        let seed = build_seed(&config, Some("fix-bug"), sample_inputs("abcd-1234", false))
            .expect("anthropic config should not error")
            .expect("anthropic config should yield a populated Seed");

        assert_eq!(seed.uuid, "abcd-1234", "uuid should round-trip");
        assert_eq!(seed.name, "fix-bug", "alias should round-trip into name");
        assert!(!seed.remote_connect, "remote_connect should default off");
        assert_eq!(seed.repos.len(), 1, "single-repo invocation should yield one RepoSpec",);
        assert!(seed.repos[0].default, "the only repo should be marked default");
        let claude = seed.claude.expect("claude should be populated");
        match claude.backend {
            CredentialBundle::Anthropic { api_key } => {
                assert_eq!(api_key, "sk-ant-test", "anthropic key should round-trip");
            },
            other => panic!("expected anthropic bundle, got {other:?}"),
        }
    }

    #[test]
    fn build_seed_promotes_explicit_default_repo() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: Some("owner/two".to_owned()),
            remote_connect: false,
            repos: vec!["owner/one".to_owned(), "owner/two".to_owned(), "owner/three".to_owned()],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = build_seed(&config, None, inputs)
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
    fn build_seed_falls_back_to_first_listed_when_no_default_set() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: None,
            remote_connect: false,
            repos: vec!["owner/alpha".to_owned(), "owner/beta".to_owned()],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = build_seed(&config, None, inputs)
            .expect("multi-repo input should not error")
            .expect("multi-repo input should yield a Seed");

        assert!(
            seed.repos.first().is_some_and(|r| r.default && r.slug == "owner/alpha"),
            "first listed slug should win when no explicit default is supplied",
        );
    }

    #[test]
    fn build_seed_uses_config_repos_when_cli_has_none() {
        let mut config = anthropic_config();
        config.base_repos = vec![
            tartarus_provider::config::RepoEntry {
                default: false,
                slug: "owner/alpha".to_owned(),
            },
            tartarus_provider::config::RepoEntry {
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

        let seed = build_seed(&config, None, inputs)
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
    fn build_seed_returns_none_when_no_repos_anywhere() {
        let config = anthropic_config();
        let inputs = SeedInputs {
            default_repo: None,
            remote_connect: false,
            repos: vec![],
            ssh_pubkey: None,
            user: sample_user(),
            uuid: "abc".to_owned(),
        };

        let seed = build_seed(&config, None, inputs).expect("repo-empty path should not error");

        assert!(
            seed.is_none(),
            "no CLI repos and no config repos should yield None so the caller surfaces the helpful failure",
        );
    }

    #[test]
    fn build_seed_returns_none_when_credentials_missing() {
        let mut config = anthropic_config();
        config.github_token = None;

        let seed = build_seed(&config, None, sample_inputs("abc", false)).expect("missing-token path should not error");

        assert!(
            seed.is_none(),
            "missing GitHub token should return None so the caller renders the helpful failure",
        );
    }

    #[test]
    fn build_seed_uses_unnamed_when_no_alias() {
        let seed = build_seed(&anthropic_config(), None, sample_inputs("abc", false))
            .expect("config path should not error")
            .expect("config should yield Seed");

        assert_eq!(
            seed.name, "(unnamed)",
            "absent alias should surface the documented `(unnamed)` placeholder",
        );
    }

    #[test]
    fn build_seed_propagates_remote_connect_flag() {
        let seed = build_seed(&anthropic_config(), None, sample_inputs("abc", true))
            .expect("config path should not error")
            .expect("config should yield Seed");

        assert!(
            seed.remote_connect,
            "background-mode flag should round-trip into the Seed",
        );
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
            claude_enabled: true,
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
            provider: tartarus_provider::config::ProviderKind::Libvirt,
            hetzner: None,
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
