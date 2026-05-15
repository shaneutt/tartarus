//! Worked example: spin up an ephemeral session against a fixed throwaway
//! repo, attach the console for the lifetime of the session, then clean
//! up the overlay + libvirt domain.
//!
//! Drives [`tartarus_libvirt::session::run::run`] and [`tartarus_libvirt::session::destroy::run`]
//! end-to-end. The example needs:
//!
//! - a populated `~/.config/tartarus/config.toml` with a GitHub PAT and Anthropic key (run `tartarus auth init` first),
//! - a Fedora base produced by `tartarus base pull`,
//! - a working `qemu:///session` libvirtd with `/dev/kvm` access.
//!
//! On any of those preconditions failing, the example surfaces the
//! offending error verbatim and exits non-zero.

use std::process::ExitCode;

use tartarus::{
    config::{self, ConfigError},
    error::Error,
    logging, refuse_root,
};
use tartarus_libvirt::session::{destroy, run};
use tartarus_provider::RunRequest;

// Constants

/// Exit code returned when the example completes successfully.
const EXIT_SUCCESS: u8 = 0;

/// Exit code returned for any other unexpected failure.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Exit code returned when the host lacks a working `qemu:///session`
/// libvirtd. Distinct from [`EXIT_GENERIC_FAILURE`] so a CI matrix can
/// distinguish "infra missing" from "logic broken."
const EXIT_NEEDS_LIBVIRTD: u8 = 5;

/// Hard-coded throwaway repo this example clones into the session.
///
/// Public, tiny, and stable so the example does not race against repo
/// rewrites. Override by passing a slug as the first argument.
const DEFAULT_REPO: &str = "octocat/Hello-World";

// Public API

/// Example entry point.
fn main() -> ExitCode {
    if let Err(err) = logging::init(logging::Verbosity::Info) {
        eprintln!("failed to install tracing subscriber: {err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    if let Err(err) = refuse_root() {
        eprintln!("{err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    let repo = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_REPO.to_owned());

    let resolved = match config::load_and_resolve(config::CliOverrides::default()) {
        Ok(c) => c,
        Err(provider_err) => {
            let err = Error::from(provider_err);
            if let Error::Config(ConfigError::NotFound { path }) = &err {
                eprintln!(
                    "run_session example: config not found at {p}; run `tartarus auth init` first",
                    p = path.display(),
                );
                return ExitCode::from(EXIT_GENERIC_FAILURE);
            }
            eprintln!("run_session example: failed to load config: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    if let Err(err) = resolved.validate_for_run() {
        eprintln!("run_session example: config not ready for run: {err}");
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

    let outcome = match run::run(&resolved, &request).map_err(Error::from) {
        Ok(outcome) => outcome,
        Err(Error::Host(err)) => {
            eprintln!(
                "run_session example: needs libvirtd; run on a Fedora workstation with `qemu:///session` available: \
                 {err}",
            );
            return ExitCode::from(EXIT_NEEDS_LIBVIRTD);
        },
        Err(err) => {
            eprintln!("run_session example: failed: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    println!(
        "run_session example: session {uuid} attached and detached",
        uuid = outcome.uuid
    );

    if let Err(err) = destroy::run(&resolved, &outcome.uuid) {
        eprintln!("run_session example: cleanup failed: {err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    println!("run_session example: completed");
    ExitCode::from(EXIT_SUCCESS)
}
