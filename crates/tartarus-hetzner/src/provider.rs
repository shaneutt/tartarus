//! [`HetznerProvider`]: Hetzner Cloud [`SessionProvider`] impl.

use tartarus_provider::{
    DestroyOutcome, ListEntry, RenameOutcome, ResumeOutcome, RunOutcome, RunRequest, SessionProvider, StopOutcome,
    seed::{input::Seed, render},
};

use crate::{Error, api::Client, config::HetznerConfig, session};

// -----------------------------------------------------------------------------
// HetznerProvider
// -----------------------------------------------------------------------------

/// Hetzner-backed [`SessionProvider`].
#[derive(Clone, Debug)]
pub struct HetznerProvider {
    /// Configured API client.
    client: Client,

    /// Resolved Hetzner settings.
    config: HetznerConfig,

    /// Pre-built [`Seed`] the run path injects as `user_data`. Set
    /// by the binary via [`HetznerProvider::with_seed`] before
    /// calling [`SessionProvider::run`]; other lifecycle methods do
    /// not consult it.
    seed: Option<Seed>,
}

impl HetznerProvider {
    /// Construct a provider from a resolved Hetzner config.
    pub fn new(config: HetznerConfig) -> Self {
        let client = Client::new(config.api_token.clone());
        Self {
            client,
            config,
            seed: None,
        }
    }

    /// Attach the [`Seed`] the next [`SessionProvider::run`] call
    /// will inject as `user_data`.
    pub fn with_seed(mut self, seed: Seed) -> Self {
        self.seed = Some(seed);
        self
    }
}

impl SessionProvider for HetznerProvider {
    type Error = Error;

    fn run(&self, request: &RunRequest) -> std::result::Result<RunOutcome, Self::Error> {
        let seed = self.seed.as_ref().ok_or_else(|| {
            Error::Provider(tartarus_provider::Error::Session(
                tartarus_provider::session::SessionError::MissingCredentials,
            ))
        })?;
        let yaml = render::render(seed).user_data;
        session::run::run(&self.client, &self.config, request, seed, &yaml)
    }

    fn resume(&self, target: &str) -> std::result::Result<ResumeOutcome, Self::Error> {
        session::resume::run(&self.client, target)
    }

    fn stop(&self, target: &str) -> std::result::Result<StopOutcome, Self::Error> {
        session::stop::run(&self.client, target)
    }

    fn destroy(&self, target: &str) -> std::result::Result<DestroyOutcome, Self::Error> {
        session::destroy::run(&self.client, target)
    }

    fn list(&self) -> std::result::Result<Vec<ListEntry>, Self::Error> {
        session::list::collect(&self.client)
    }

    fn rename(&self, uuid: &str, alias: &str) -> std::result::Result<RenameOutcome, Self::Error> {
        session::rename::run(&self.client, uuid, alias)
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_provider::{
        host_user::HostUser,
        seed::input::{ClaudeCredentials, ClaudeDefaults, CredentialBundle, RepoSpec},
    };

    use super::*;
    use crate::api::tests_fake_server::{CannedResponse, Server};

    fn sample_seed() -> Seed {
        Seed {
            name: "x".to_owned(),
            claude: Some(ClaudeCredentials {
                backend: CredentialBundle::Anthropic {
                    api_key: "sk-ant-test".to_owned(),
                },
                defaults: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
            }),
            envs: vec![],
            github_token: "ghp_test".to_owned(),
            remote_connect: false,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_pubkey: None,
            uuid: "abcd".to_owned(),
            user: HostUser {
                gid: 1000,
                uid: 1000,
                username: "alice".to_owned(),
            },
        }
    }

    fn cfg() -> HetznerConfig {
        HetznerConfig {
            api_token: "tok".to_owned(),
            image: "ubuntu-22.04".to_owned(),
            location: "fsn1".to_owned(),
            server_type: "cx21".to_owned(),
            ssh_key_name: None,
            volume_gib: 0,
        }
    }

    /// Build a HetznerProvider that points at a fake server.
    fn provider_pointed_at(server: &Server) -> HetznerProvider {
        let client = crate::api::Client::with_base_url(&cfg().api_token, &server.base_url);
        HetznerProvider {
            client,
            config: cfg(),
            seed: None,
        }
    }

    #[test]
    fn run_without_seed_returns_missing_credentials() {
        let server = Server::start(vec![]);
        let provider = provider_pointed_at(&server);

        let request = tartarus_provider::RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };
        let err = SessionProvider::run(&provider, &request).expect_err("missing seed should error");
        match err {
            crate::Error::Provider(tartarus_provider::Error::Session(
                tartarus_provider::session::SessionError::MissingCredentials,
            )) => {},
            other => panic!("expected MissingCredentials, got {other:?}"),
        }
    }

    #[test]
    fn list_returns_each_tartarus_server_as_a_list_entry() {
        let body = r#"{"servers":[
            {"id":1,"name":"a","status":"running","labels":{"tartarus.owned":"true","tartarus.uuid":"abcdefgh","tartarus.alias":"first","tartarus.persist":"true"}},
            {"id":2,"name":"b","status":"off","labels":{"tartarus.owned":"true","tartarus.uuid":"zyxw","tartarus.persist":"false"}}
        ]}"#;
        let server = Server::start(vec![CannedResponse::ok(body)]);
        let provider = provider_pointed_at(&server);

        let entries = provider.list().expect("list should succeed");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].alias, "first");
        assert_eq!(entries[0].persist, "yes");
        assert_eq!(entries[1].alias, "(unnamed)");
        assert_eq!(entries[1].persist, "no");
    }

    #[test]
    fn with_seed_attaches_the_seed_for_subsequent_run_calls() {
        let server = Server::start(vec![
            CannedResponse::created(
                r#"{
                "server": {"id": 200, "name": "tartarus-abcd", "status": "initializing", "labels": {}},
                "action": {"id": 300, "status": "running"}
            }"#,
            ),
            CannedResponse::ok(r#"{"action":{"id":300,"status":"success"}}"#),
        ]);
        let provider = provider_pointed_at(&server).with_seed(sample_seed());

        let request = tartarus_provider::RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: true,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };
        let outcome = SessionProvider::run(&provider, &request).expect("run should succeed");
        assert_eq!(outcome.uuid, "abcd");
    }
}
