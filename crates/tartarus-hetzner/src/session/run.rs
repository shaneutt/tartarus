//! `tartarus run` against Hetzner Cloud.
//!
//! Optionally creates a volume, then creates a server with the
//! Tartarus seed (`#cloud-config`) shipped as `user_data`, waits for
//! the create action to finish, and returns the rendered
//! [`RunOutcome`]. Background mode (`--background`) is not yet
//! supported on the Hetzner path: Hetzner has no `qemu-guest-agent`
//! channel to capture the remote-connect URL through, so the
//! provider returns [`tartarus_provider::Error`]'s `NotImplemented`
//! analogue when asked.

use tartarus_provider::{
    RunOutcome, RunRequest,
    seed::input::Seed,
    session::{identity, run_mode::RunMode},
};

use crate::{
    Result,
    api::{Client, servers},
    config::HetznerConfig,
    session::{labels, lifecycle, metadata::HetznerState},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Hetzner server name template (the user-visible Hetzner display name).
const SERVER_NAME_PREFIX: &str = "tartarus-";

/// Number of UUID characters preserved in the Hetzner display name.
const UUID_SUFFIX_LEN: usize = 8;

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run the Hetzner session-start flow.
///
/// `seed_yaml` is the rendered `#cloud-config` document the binary
/// already produced via `tartarus_provider::seed::render::render`.
/// `seed` is provided so we can capture the session UUID/alias
/// without round-tripping the YAML.
pub fn run(
    client: &Client,
    config: &HetznerConfig,
    request: &RunRequest,
    seed: &Seed,
    seed_yaml: &str,
) -> Result<RunOutcome> {
    if matches!(request.run_mode(), RunMode::Background) {
        return Err(
            tartarus_provider::Error::Session(tartarus_provider::session::SessionError::NotFound {
                target: "background mode is not yet supported on the Hetzner provider".to_owned(),
            })
            .into(),
        );
    }

    let uuid = if seed.uuid.is_empty() {
        identity::new_uuid()
    } else {
        seed.uuid.clone()
    };
    let alias = request.name.as_deref();
    let persist = !request.ephemeral;

    // Optional /home volume.
    let volume_id = if config.attaches_volume() {
        Some(create_home_volume(client, config, &uuid)?)
    } else {
        None
    };

    let server_response = create_server(
        client,
        CreateServerInputs {
            config,
            request,
            uuid: &uuid,
            user_data: seed_yaml,
            volume_id,
        },
    )?;

    lifecycle::wait_for(client, server_response.action.id, "server create")?;

    if let Some(vid) = volume_id {
        attach_home_volume(client, vid, server_response.server.id)?;
    }

    let public_ipv4 = server_response
        .server
        .public_net
        .as_ref()
        .and_then(|n| n.ipv4.as_ref())
        .map(|ip| ip.ip.clone());

    let _state = HetznerState {
        server_id: server_response.server.id,
        public_ipv4: public_ipv4.clone(),
        volume_id,
    };

    tracing::info!(
        uuid = %uuid,
        alias = ?alias,
        server_id = server_response.server.id,
        persist,
        "Hetzner session created",
    );

    Ok(RunOutcome {
        alias: alias.map(str::to_owned),
        mode: request.run_mode(),
        remote_url: None,
        uuid,
    })
}

// -----------------------------------------------------------------------------
// Server Creation
// -----------------------------------------------------------------------------

/// Inputs to [`create_server`].
struct CreateServerInputs<'a> {
    /// Resolved Hetzner config.
    config: &'a HetznerConfig,

    /// Caller's run request (alias, ephemeral, ...).
    request: &'a RunRequest,

    /// Session UUID.
    uuid: &'a str,

    /// Pre-rendered cloud-config YAML.
    user_data: &'a str,

    /// Volume to attach at create time, if any.
    volume_id: Option<u64>,
}

/// Issue `POST /servers`.
fn create_server(client: &Client, inputs: CreateServerInputs<'_>) -> Result<servers::CreateServerResponse> {
    let name = hetzner_display_name(inputs.uuid);
    let alias = inputs.request.name.as_deref();
    let persist = !inputs.request.ephemeral;

    let ssh_keys: Vec<&str> = inputs.config.ssh_key_name.iter().map(String::as_str).collect();

    let payload = servers::CreateServerRequest {
        name: &name,
        server_type: &inputs.config.server_type,
        image: &inputs.config.image,
        location: &inputs.config.location,
        user_data: Some(inputs.user_data),
        ssh_keys,
        start_after_create: true,
        labels: labels::fresh_session(inputs.uuid, alias, persist),
        volumes: inputs.volume_id.into_iter().collect(),
    };

    Ok(servers::create(client, &payload)?)
}

// -----------------------------------------------------------------------------
// Volume Management
// -----------------------------------------------------------------------------

/// Create + format the per-session `/home` volume. Returns the
/// numeric volume ID for later attach.
fn create_home_volume(client: &Client, config: &HetznerConfig, uuid: &str) -> Result<u64> {
    let request = crate::api::volumes::CreateVolumeRequest {
        name: &format!("tartarus-home-{uuid_short}", uuid_short = uuid_short(uuid)),
        size: config.volume_gib,
        location: &config.location,
        format: Some("ext4"),
    };
    let response = crate::api::volumes::create(client, &request)?;
    lifecycle::wait_for(client, response.action.id, "volume create")?;
    Ok(response.volume.id)
}

/// Attach an existing volume to the freshly-created server.
fn attach_home_volume(client: &Client, volume_id: u64, server_id: u64) -> Result<()> {
    let request = crate::api::volumes::AttachVolumeRequest {
        server: server_id,
        automount: false,
    };
    let response = crate::api::volumes::attach(client, volume_id, &request)?;
    lifecycle::wait_for(client, response.action.id, "volume attach")?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Name Helpers
// -----------------------------------------------------------------------------

/// Hetzner display name: `tartarus-<short uuid>`.
fn hetzner_display_name(uuid: &str) -> String {
    format!(
        "{prefix}{suffix}",
        prefix = SERVER_NAME_PREFIX,
        suffix = uuid_short(uuid)
    )
}

/// First [`UUID_SUFFIX_LEN`] characters of the UUID.
fn uuid_short(uuid: &str) -> &str {
    if uuid.len() <= UUID_SUFFIX_LEN {
        uuid
    } else {
        &uuid[..UUID_SUFFIX_LEN]
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hetzner_display_name_uses_short_uuid_suffix() {
        let name = hetzner_display_name("11111111-2222-3333-4444-555555555555");
        assert_eq!(name, "tartarus-11111111");
    }

    #[test]
    fn hetzner_display_name_tolerates_short_uuid() {
        let name = hetzner_display_name("abc");
        assert_eq!(name, "tartarus-abc");
    }

    // -----------------------------------------------------------------------
    // End-to-end Lifecycle Tests
    // -----------------------------------------------------------------------

    use tartarus_provider::{
        host_user::HostUser,
        seed::input::{ClaudeDefaults, CredentialBundle, Credentials, RepoSpec},
    };

    use crate::api::tests_fake_server::{CannedResponse, Server as FakeServer};

    fn sample_seed() -> Seed {
        Seed {
            name: "fix-bug".to_owned(),
            credentials: Credentials {
                backend: CredentialBundle::Anthropic {
                    api_key: "sk-ant-test".to_owned(),
                },
                claude: ClaudeDefaults {
                    effort: "high".to_owned(),
                    model: "claude-opus-4-7".to_owned(),
                },
                github_token: "ghp_test".to_owned(),
            },
            envs: vec!["rust".to_owned()],
            remote_connect: false,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_pubkey: None,
            uuid: "abcd1234-uuid-fixture".to_owned(),
            user: HostUser {
                gid: 1000,
                uid: 1000,
                username: "alice".to_owned(),
            },
        }
    }

    fn sample_hetzner_config(volume_gib: u32) -> HetznerConfig {
        HetznerConfig {
            api_token: "stub".to_owned(),
            image: "ubuntu-22.04".to_owned(),
            location: "fsn1".to_owned(),
            server_type: "cx21".to_owned(),
            ssh_key_name: None,
            volume_gib,
        }
    }

    fn sample_run_request() -> RunRequest {
        RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: Some("fix-bug".to_owned()),
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        }
    }

    #[test]
    fn run_no_volume_creates_server_and_polls_action() {
        let fake = FakeServer::start(vec![
            CannedResponse::created(
                r#"{
                "server": {"id": 100, "name": "tartarus-abcd1234", "status": "initializing", "labels": {}},
                "action": {"id": 200, "status": "running"}
            }"#,
            ),
            CannedResponse::ok(r#"{"action":{"id":200,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);
        let config = sample_hetzner_config(0);
        let request = sample_run_request();
        let seed = sample_seed();

        let outcome = run(&client, &config, &request, &seed, "#cloud-config\n").expect("run should succeed");
        assert_eq!(outcome.uuid, "abcd1234-uuid-fixture");
        assert_eq!(outcome.alias.as_deref(), Some("fix-bug"));

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(
            paths[0].contains("POST /servers"),
            "first call should be the server create: {paths:?}",
        );
    }

    #[test]
    fn run_with_volume_creates_then_attaches() {
        let fake = FakeServer::start(vec![
            // POST /volumes
            CannedResponse::created(
                r#"{
                "volume": {"id": 11, "name": "tartarus-home-abcd1234", "size": 64, "server": null},
                "action": {"id": 201, "status": "running"}
            }"#,
            ),
            // GET /actions/201 (volume create poll)
            CannedResponse::ok(r#"{"action":{"id":201,"status":"success"}}"#),
            // POST /servers
            CannedResponse::created(
                r#"{
                "server": {"id": 101, "name": "tartarus-abcd1234", "status": "initializing", "labels": {}},
                "action": {"id": 202, "status": "running"}
            }"#,
            ),
            // GET /actions/202 (server create poll)
            CannedResponse::ok(r#"{"action":{"id":202,"status":"success"}}"#),
            // POST /volumes/11/actions/attach
            CannedResponse::ok(r#"{"action":{"id":203,"status":"running"}}"#),
            // GET /actions/203 (attach poll)
            CannedResponse::ok(r#"{"action":{"id":203,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);
        let config = sample_hetzner_config(64);
        let request = sample_run_request();
        let seed = sample_seed();

        let outcome = run(&client, &config, &request, &seed, "#cloud-config\n").expect("run should succeed");
        assert_eq!(outcome.uuid, "abcd1234-uuid-fixture");

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(
            paths[0].contains("POST /volumes"),
            "first call should create the volume"
        );
        assert!(
            paths[2].contains("POST /servers"),
            "third call should create the server"
        );
        assert!(
            paths[4].contains("POST /volumes/11/actions/attach"),
            "fifth call should attach the volume",
        );
    }

    #[test]
    fn run_rejects_background_mode() {
        let fake = FakeServer::start(vec![]);
        let client = Client::with_base_url("tok", &fake.base_url);
        let config = sample_hetzner_config(0);
        let mut request = sample_run_request();
        request.background = true;
        let seed = sample_seed();

        let err = run(&client, &config, &request, &seed, "").expect_err("background mode should error");
        match err {
            crate::Error::Provider(tartarus_provider::Error::Session(_)) => {},
            other => panic!("expected Provider(Session(...)), got {other:?}"),
        }
    }
}
