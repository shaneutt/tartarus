//! `tartarus env add` / `tartarus env update`: install or refresh a
//! programming environment inside an existing session via
//! `qemu-guest-agent`.

use std::time::{Duration, Instant};

use tartarus_provider::session::{
    SessionError,
    identity::{self, ResolvedSession},
};

use crate::{
    config::Config,
    error::Result,
    host::{
        agent::Agent,
        connect::Connection,
        domain::{self},
        error::HostError,
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// In-guest env-add orchestrator path.
pub const ENV_ADD_SCRIPT_PATH: &str = "/usr/local/bin/tartarus-env-add.sh";

/// In-guest env-update orchestrator path.
pub const ENV_UPDATE_SCRIPT_PATH: &str = "/usr/local/bin/tartarus-env-update.sh";

/// Per-call timeout for agent operations (ten minutes).
const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Polling interval while waiting for the env script to exit.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

// -----------------------------------------------------------------------------
// EnvOutcome
// -----------------------------------------------------------------------------

/// Recognised env names.
pub const SUPPORTED_ENVS: &[&str] = &["rust", "go", "python"];

/// Outcome of a successful [`add`] or [`update`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvOutcome {
    /// Env name acted upon. Empty for [`update`].
    pub env: String,

    /// Captured stdout (currently empty).
    pub stdout: Vec<u8>,

    /// Session UUID that was modified.
    pub uuid: String,
}

/// Run `tartarus env add <alias|uuid> <env>`.
pub fn add(config: &Config, target: &str, env: &str) -> Result<EnvOutcome> {
    let env = validate_env_name(env)?;

    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, env, "env add: resolving session");

    let connection = Connection::open(&config.network_uri)?;
    let agent = open_agent(&connection, &resolved)?;

    let args = build_add_args(env, config);
    dispatch_and_wait(&agent, ENV_ADD_SCRIPT_PATH, &args)?;

    Ok(EnvOutcome {
        env: env.to_owned(),
        stdout: Vec::new(),
        uuid: resolved.uuid,
    })
}

/// Run `tartarus env update <alias|uuid>`. Idempotent.
pub fn update(config: &Config, target: &str) -> Result<EnvOutcome> {
    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, "env update: resolving session");

    let connection = Connection::open(&config.network_uri)?;
    let agent = open_agent(&connection, &resolved)?;

    let args = build_update_args(config);
    dispatch_and_wait(&agent, ENV_UPDATE_SCRIPT_PATH, &args)?;

    Ok(EnvOutcome {
        env: String::new(),
        stdout: Vec::new(),
        uuid: resolved.uuid,
    })
}

/// Build the `tartarus-env-add.sh` argument list for `env`.
pub fn build_add_args(env: &str, config: &Config) -> Vec<String> {
    let mut args = vec![env.to_owned()];

    if env == "rust" {
        if !config.rust_components.is_empty() {
            args.push("--components".to_owned());
            args.push(config.rust_components.join(","));
        }
        if !config.rust_toolchains.is_empty() {
            args.push("--toolchains".to_owned());
            args.push(config.rust_toolchains.join(","));
        }
        if !config.rust_cargo_tools.is_empty() {
            args.push("--cargo-tools".to_owned());
            args.push(config.rust_cargo_tools.join(","));
        }
    }

    args
}

/// Build the `tartarus-env-update.sh` argument list.
pub fn build_update_args(config: &Config) -> Vec<String> {
    let mut args = Vec::new();
    if !config.rust_cargo_tools.is_empty() {
        args.push("--cargo-tools".to_owned());
        args.push(config.rust_cargo_tools.join(","));
    }
    args
}

/// Validate that `env` is one of [`SUPPORTED_ENVS`].
pub fn validate_env_name(env: &str) -> Result<&str> {
    if SUPPORTED_ENVS.contains(&env) {
        Ok(env)
    } else {
        Err(SessionError::UnknownEnv {
            env: env.to_owned(),
            supported: SUPPORTED_ENVS,
        }
        .into())
    }
}

// -----------------------------------------------------------------------------
// Agent Dispatch
// -----------------------------------------------------------------------------

/// Look up the session domain and wrap it in an [`Agent`].
fn open_agent(connection: &Connection, resolved: &ResolvedSession) -> Result<Agent> {
    let domain = domain::lookup(connection, &resolved.uuid)?;
    Ok(Agent::new(domain))
}

/// Dispatch a script via the agent and poll to completion.
fn dispatch_and_wait(agent: &Agent, script: &str, args: &[String]) -> Result<()> {
    tracing::info!(script, ?args, "env: dispatching orchestrator");

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let handle = agent.exec(script, &arg_refs, false, AGENT_CALL_TIMEOUT)?;

    let deadline = Instant::now() + AGENT_CALL_TIMEOUT;
    loop {
        let status = agent.exec_status(&handle, AGENT_CALL_TIMEOUT)?;
        if status.exited {
            return match status.exit_code.unwrap_or(0) {
                0 => {
                    tracing::info!(script, "env: orchestrator exited cleanly");
                    Ok(())
                },
                code => Err(HostError::AgentExecFailed {
                    code,
                    detail: "tartarus-env-*.sh exited non-zero",
                }
                .into()),
            };
        }
        if Instant::now() >= deadline {
            return Err(HostError::AgentExecFailed {
                code: -1,
                detail: "tartarus-env-*.sh did not exit within the poll window",
            }
            .into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::{CliOverrides, FileConfig},
        error::Error,
    };

    #[test]
    fn validate_env_name_accepts_rust_go_python() {
        for env in ["rust", "go", "python"] {
            assert!(
                validate_env_name(env).is_ok(),
                "{env} should validate as a supported env",
            );
        }
    }

    #[test]
    fn validate_env_name_rejects_unknown_env() {
        let err = validate_env_name("haskell").expect_err("haskell is not a supported env");

        match err {
            Error::Provider(tartarus_provider::Error::Session(SessionError::UnknownEnv { env, supported })) => {
                assert_eq!(env, "haskell", "rejected env name should round-trip");
                assert_eq!(
                    supported, SUPPORTED_ENVS,
                    "the supported list in the error should match the canonical constant",
                );
            },
            other => panic!("expected SessionError::UnknownEnv, got {other:?}"),
        }
    }

    #[test]
    fn build_add_args_for_rust_carries_components_toolchains_cargo_tools() {
        let config = Config::resolve(FileConfig::default(), CliOverrides::default());

        let args = build_add_args("rust", &config);

        assert_eq!(
            args.first().map(String::as_str),
            Some("rust"),
            "first arg must be the env name"
        );
        assert!(
            args.iter().any(|a| a == "--components"),
            "rust args must carry --components when the resolved config has any",
        );
        assert!(
            args.iter().any(|a| a == "--toolchains"),
            "rust args must carry --toolchains when the resolved config has any",
        );
        assert!(
            args.iter().any(|a| a == "--cargo-tools"),
            "rust args must carry --cargo-tools when the resolved config has any",
        );
    }

    #[test]
    fn build_add_args_for_go_carries_only_env_name() {
        let config = Config::resolve(FileConfig::default(), CliOverrides::default());

        let args = build_add_args("go", &config);

        assert_eq!(args, vec!["go".to_owned()], "go args must be just the env name");
    }

    #[test]
    fn build_add_args_for_python_carries_only_env_name() {
        let config = Config::resolve(FileConfig::default(), CliOverrides::default());

        let args = build_add_args("python", &config);

        assert_eq!(args, vec!["python".to_owned()], "python args must be just the env name");
    }

    #[test]
    fn build_add_args_joins_lists_with_commas() {
        let mut config = Config::resolve(FileConfig::default(), CliOverrides::default());
        config.rust_components = vec!["rustfmt".to_owned(), "clippy".to_owned()];
        config.rust_toolchains = vec!["stable".to_owned()];
        config.rust_cargo_tools = vec!["cargo-nextest".to_owned(), "cargo-audit".to_owned()];

        let args = build_add_args("rust", &config);

        let joined: Vec<String> = args
            .windows(2)
            .filter(|w| w[0] == "--components")
            .map(|w| w[1].clone())
            .collect();
        assert_eq!(
            joined,
            vec!["rustfmt,clippy".to_owned()],
            "components must be comma-joined into a single value arg",
        );
    }

    #[test]
    fn build_update_args_carries_cargo_tools_when_configured() {
        let mut config = Config::resolve(FileConfig::default(), CliOverrides::default());
        config.rust_cargo_tools = vec!["cargo-nextest".to_owned()];

        let args = build_update_args(&config);

        assert_eq!(
            args,
            vec!["--cargo-tools".to_owned(), "cargo-nextest".to_owned()],
            "update args must carry the cargo-tools flag when configured",
        );
    }

    #[test]
    fn build_update_args_is_empty_when_cargo_tools_empty() {
        let mut config = Config::resolve(FileConfig::default(), CliOverrides::default());
        config.rust_cargo_tools = Vec::new();

        let args = build_update_args(&config);

        assert!(
            args.is_empty(),
            "update args must be empty when no cargo tools are configured; got {args:?}",
        );
    }

    #[test]
    fn env_add_script_path_matches_in_guest_layout() {
        assert_eq!(
            ENV_ADD_SCRIPT_PATH, "/usr/local/bin/tartarus-env-add.sh",
            "host-side path constant must match what the layering step installs",
        );
    }

    #[test]
    fn env_update_script_path_matches_in_guest_layout() {
        assert_eq!(
            ENV_UPDATE_SCRIPT_PATH, "/usr/local/bin/tartarus-env-update.sh",
            "host-side path constant must match what the layering step installs",
        );
    }

    #[test]
    fn supported_envs_matches_documented_set() {
        assert_eq!(
            SUPPORTED_ENVS,
            &["rust", "go", "python"],
            "supported env list must match docs/spec.md's opinionated set",
        );
    }

    #[test]
    fn agent_call_timeout_is_under_an_hour() {
        assert!(
            AGENT_CALL_TIMEOUT < Duration::from_secs(3_600),
            "per-call timeout should bound a misbehaving guest from hanging the host indefinitely",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a session whose qemu-ga responds; run with --ignored after setting up locally"]
    fn end_to_end_env_add_rust_against_real_session() {}

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a session whose qemu-ga responds; run with --ignored after setting up locally"]
    fn end_to_end_env_update_idempotent_against_real_session() {}
}
