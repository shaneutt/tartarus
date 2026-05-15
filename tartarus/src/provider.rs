//! Binary-side provider selection and dispatch.
//!
//! Wraps the concrete [`SessionProvider`] impls in an enum so the
//! CLI dispatch layer in `cli.rs` only has to know "which provider"
//! once per invocation and otherwise calls trait methods uniformly.

use tartarus_hetzner::{HetznerProvider, config::HetznerConfig};
use tartarus_libvirt::{LibvirtProvider, seed};
use tartarus_provider::{
    DestroyOutcome, ListEntry, RenameOutcome, ResumeOutcome, RunOutcome, RunRequest, SessionProvider, StopOutcome,
    config::{Config, ProviderKind},
    host_user,
    seed::input::{Seed, SeedInputs},
    session::{SessionError, identity},
};

use crate::error::{Error, Result};

// -----------------------------------------------------------------------------
// Provider
// -----------------------------------------------------------------------------

/// Concrete provider picked at dispatch time.
#[derive(Debug)]
pub enum Provider {
    /// libvirt + QEMU/KVM.
    Libvirt(LibvirtProvider),

    /// Hetzner Cloud.
    Hetzner(HetznerProvider),
}

impl Provider {
    /// Construct the provider [`Config::provider`] selects.
    ///
    /// For Hetzner, [`tartarus_provider::seed::Seed`] is pre-built
    /// here from the resolved config (Hetzner has no in-provider seed
    /// builder); for libvirt, the inline builder inside
    /// `session::run::run` still runs.
    pub fn from_config(config: Config, request: &RunRequest) -> Result<Self> {
        match config.provider {
            ProviderKind::Libvirt => Ok(Provider::Libvirt(LibvirtProvider::new(config))),
            ProviderKind::Hetzner => {
                let hetz_section = config.hetzner.clone().ok_or_else(|| {
                    Error::Config(tartarus_provider::config::ConfigError::Invalid(
                        "[hetzner] section is required when provider = \"hetzner\"".to_owned(),
                    ))
                })?;
                let seed = build_seed_for_hetzner(&config, request)?;
                let hetzner_config = HetznerConfig {
                    api_token: hetz_section.api_token,
                    image: hetz_section.image,
                    location: hetz_section.location,
                    server_type: hetz_section.server_type,
                    ssh_key_name: hetz_section.ssh_key_name,
                    volume_gib: hetz_section.volume_gib,
                };
                Ok(Provider::Hetzner(HetznerProvider::new(hetzner_config).with_seed(seed)))
            },
        }
    }

    /// Lifecycle ops that do not need a [`RunRequest`] up front
    /// build a provider from `config` alone.
    pub fn from_config_for_lifecycle(config: Config) -> Result<Self> {
        match config.provider {
            ProviderKind::Libvirt => Ok(Provider::Libvirt(LibvirtProvider::new(config))),
            ProviderKind::Hetzner => {
                let hetz_section = config.hetzner.clone().ok_or_else(|| {
                    Error::Config(tartarus_provider::config::ConfigError::Invalid(
                        "[hetzner] section is required when provider = \"hetzner\"".to_owned(),
                    ))
                })?;
                let hetzner_config = HetznerConfig {
                    api_token: hetz_section.api_token,
                    image: hetz_section.image,
                    location: hetz_section.location,
                    server_type: hetz_section.server_type,
                    ssh_key_name: hetz_section.ssh_key_name,
                    volume_gib: hetz_section.volume_gib,
                };
                Ok(Provider::Hetzner(HetznerProvider::new(hetzner_config)))
            },
        }
    }
}

impl SessionProvider for Provider {
    type Error = Error;

    fn run(&self, request: &RunRequest) -> std::result::Result<RunOutcome, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.run(request).map_err(Into::into),
            Provider::Hetzner(p) => p.run(request).map_err(Into::into),
        }
    }

    fn resume(&self, target: &str) -> std::result::Result<ResumeOutcome, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.resume(target).map_err(Into::into),
            Provider::Hetzner(p) => p.resume(target).map_err(Into::into),
        }
    }

    fn stop(&self, target: &str) -> std::result::Result<StopOutcome, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.stop(target).map_err(Into::into),
            Provider::Hetzner(p) => p.stop(target).map_err(Into::into),
        }
    }

    fn destroy(&self, target: &str) -> std::result::Result<DestroyOutcome, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.destroy(target).map_err(Into::into),
            Provider::Hetzner(p) => p.destroy(target).map_err(Into::into),
        }
    }

    fn list(&self) -> std::result::Result<Vec<ListEntry>, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.list().map_err(Into::into),
            Provider::Hetzner(p) => p.list().map_err(Into::into),
        }
    }

    fn rename(&self, uuid: &str, alias: &str) -> std::result::Result<RenameOutcome, Self::Error> {
        match self {
            Provider::Libvirt(p) => p.rename(uuid, alias).map_err(Into::into),
            Provider::Hetzner(p) => p.rename(uuid, alias).map_err(Into::into),
        }
    }
}

// -----------------------------------------------------------------------------
// Seed Materialisation
// -----------------------------------------------------------------------------

/// Build a [`Seed`] for the Hetzner provider's `run` path.
///
/// Hetzner has no in-provider seed builder, so the binary calls the
/// libvirt-side builder (which is provider-agnostic at the data
/// level) and threads the result in via `HetznerProvider::with_seed`.
fn build_seed_for_hetzner(config: &Config, request: &RunRequest) -> Result<Seed> {
    let user = host_user::current()?;
    let uuid = identity::new_uuid();
    let inputs = SeedInputs {
        default_repo: request.default_repo.clone(),
        remote_connect: request.run_mode().enables_remote_connect(),
        repos: request.repos.clone(),
        ssh_pubkey: None,
        user,
        uuid: uuid.clone(),
    };

    let seed = seed::builder::build_seed(config, request.name.as_deref(), inputs)?
        .ok_or_else(|| Error::Session(SessionError::MissingCredentials))?;

    Ok(seed)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_provider::config::{Backend, ConfigError, HetznerSection};

    use super::*;

    #[test]
    fn from_config_for_lifecycle_yields_libvirt_when_provider_is_libvirt() {
        let config = libvirt_config();
        let provider = Provider::from_config_for_lifecycle(config).expect("libvirt provider should construct");
        match provider {
            Provider::Libvirt(_) => {},
            Provider::Hetzner(_) => panic!("libvirt config should not yield a Hetzner provider"),
        }
    }

    #[test]
    fn from_config_for_lifecycle_yields_hetzner_when_section_present() {
        let mut config = libvirt_config();
        config.provider = ProviderKind::Hetzner;
        config.hetzner = Some(sample_hetzner_section());
        let provider = Provider::from_config_for_lifecycle(config).expect("hetzner provider should construct");
        match provider {
            Provider::Hetzner(_) => {},
            Provider::Libvirt(_) => panic!("hetzner config should not yield a Libvirt provider"),
        }
    }

    #[test]
    fn from_config_for_lifecycle_errors_when_section_missing() {
        let mut config = libvirt_config();
        config.provider = ProviderKind::Hetzner;
        config.hetzner = None;
        let err = Provider::from_config_for_lifecycle(config).expect_err("missing [hetzner] section should error");
        match err {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("[hetzner]"),
                    "error should mention the missing section, got: {msg}",
                );
            },
            other => panic!("expected Config(Invalid), got {other:?}"),
        }
    }

    #[test]
    fn from_config_errors_when_hetzner_selected_but_section_missing() {
        let mut config = libvirt_config();
        config.provider = ProviderKind::Hetzner;
        config.hetzner = None;
        let request = sample_request();
        let err = Provider::from_config(config, &request).expect_err("missing [hetzner] section should error");
        match err {
            Error::Config(ConfigError::Invalid(_)) => {},
            other => panic!("expected Config(Invalid), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    fn libvirt_config() -> Config {
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
            provider: ProviderKind::Libvirt,
            hetzner: None,
        }
    }

    fn sample_hetzner_section() -> HetznerSection {
        HetznerSection {
            api_token: "stub-token".to_owned(),
            image: "ubuntu-22.04".to_owned(),
            location: "fsn1".to_owned(),
            server_type: "cx21".to_owned(),
            ssh_key_name: None,
            volume_gib: 0,
        }
    }

    fn sample_request() -> RunRequest {
        RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        }
    }

    #[test]
    fn from_config_threads_hetzner_section_into_provider() {
        let mut config = libvirt_config();
        config.provider = ProviderKind::Hetzner;
        config.hetzner = Some(sample_hetzner_section());
        let request = sample_request();
        let provider = Provider::from_config(config, &request).expect("hetzner provider should construct");
        match provider {
            Provider::Hetzner(_) => {},
            Provider::Libvirt(_) => panic!("hetzner config should not yield Libvirt"),
        }
    }

    #[test]
    fn build_seed_for_hetzner_propagates_remote_connect_flag_under_background() {
        // Background mode flips RunMode::Background which the trait
        // method will reject downstream; this test just exercises the
        // builder side that fills in `remote_connect` ahead of that
        // rejection, so the seed contents carry the bit through.
        let config = libvirt_config();
        let mut request = sample_request();
        request.background = true;
        // We intentionally don't call Provider::from_config (it would
        // dispatch into Hetzner's run, which rejects background). The
        // pure seed-build path is what we want here.
        let seed = build_seed_for_hetzner(&config, &request).expect("seed should build under libvirt-shaped config");
        assert!(
            seed.remote_connect,
            "background mode should flip the remote-connect bit on the built seed",
        );
    }

    #[test]
    fn build_seed_for_hetzner_returns_missing_credentials_when_github_token_absent() {
        let mut config = libvirt_config();
        config.github_token = None;
        let request = sample_request();
        let err = build_seed_for_hetzner(&config, &request).expect_err("missing token should surface");
        match err {
            Error::Session(tartarus_provider::session::SessionError::MissingCredentials) => {},
            other => panic!("expected MissingCredentials, got {other:?}"),
        }
    }
}
