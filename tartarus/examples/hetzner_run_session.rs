//! Worked example: spin up an ephemeral session against a fixed
//! throwaway repo on Hetzner Cloud, then tear it down.
//!
//! Drives the Hetzner provider end-to-end through the workspace's
//! [`SessionProvider`] dispatch. Requires:
//!
//! - a populated `~/.config/tartarus/config.toml` with `provider = "hetzner"` plus a `[hetzner]` section (api_token,
//!   image, location, server_type),
//! - a populated `[github]` section so the in-guest agent can clone,
//! - working egress to `api.hetzner.cloud`.
//!
//! Usage: `cargo run --example hetzner_run_session -- [owner/repo]`.
//!
//! [`SessionProvider`]: tartarus_provider::SessionProvider

use std::process::ExitCode;

use tartarus::{
    config::{self, ConfigError},
    error::Error,
    logging,
    provider::Provider,
    refuse_root,
};
use tartarus_provider::{RunRequest, SessionProvider};

// Constants

/// Exit code returned when the example completes successfully.
const EXIT_SUCCESS: u8 = 0;

/// Exit code returned for any unexpected failure.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Hard-coded throwaway repo this example clones into the session.
const DEFAULT_REPO: &str = "octocat/Hello-World";

// Public API

/// Example entry point.
fn main() -> ExitCode {
    if let Err(err) = refuse_root() {
        eprintln!("hetzner_run_session example: {err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    let _ = logging::init(logging::Verbosity::Info);

    let repo = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_REPO.to_owned());

    let resolved = match config::load_and_resolve(config::CliOverrides::default()) {
        Ok(c) => c,
        Err(provider_err) => {
            let err = Error::from(provider_err);
            if let Error::Config(ConfigError::NotFound { path }) = &err {
                eprintln!(
                    "hetzner_run_session example: config not found at {p}; run `tartarus auth init` first",
                    p = path.display(),
                );
                return ExitCode::from(EXIT_GENERIC_FAILURE);
            }
            eprintln!("hetzner_run_session example: failed to load config: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    if !matches!(resolved.provider, config::ProviderKind::Hetzner) {
        eprintln!(
            "hetzner_run_session example: config has provider = {p:?}; this example expects `provider = \"hetzner\"`",
            p = resolved.provider,
        );
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    let request = RunRequest {
        background: false,
        default_repo: None,
        detach: false,
        ephemeral: true,
        gpu: None,
        name: None,
        privileged_libvirt: false,
        repos: vec![repo],
    };

    let provider = match Provider::from_config(resolved, &request) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("hetzner_run_session example: provider setup failed: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    let outcome = match provider.run(&request) {
        Ok(outcome) => outcome,
        Err(err) => {
            eprintln!("hetzner_run_session example: run failed: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    println!(
        "hetzner_run_session example: session {uuid} started; tearing down because --ephemeral",
        uuid = outcome.uuid,
    );

    if let Err(err) = provider.destroy(&outcome.uuid) {
        eprintln!("hetzner_run_session example: destroy failed: {err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    println!("hetzner_run_session example: destroyed cleanly");
    ExitCode::from(EXIT_SUCCESS)
}
