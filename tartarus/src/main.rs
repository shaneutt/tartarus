//! Entry point for the `tartarus` CLI binary.
//!
//! Parse args, install the `tracing` subscriber, refuse to run as root, then
//! dispatch the subcommand. Any error is translated into a small, stable set
//! of process exit codes.

use std::process::ExitCode;

use clap::Parser;
use tartarus::{
    cli::{self, Cli},
    config::{self, ConfigError},
    error::{Error, Result},
    logging, refuse_root,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Exit code returned for [`Error::RunningAsRoot`] at startup.
const EXIT_RUNNING_AS_ROOT: u8 = 2;

/// Exit code returned when a subcommand stub is dispatched.
const EXIT_NOT_IMPLEMENTED: u8 = 3;

/// Exit code returned for any other [`Error`] the CLI surfaces.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Floor for the exit code returned when [`Error::DoctorFailures`] is
/// surfaced with a count of zero. The error itself only constructs
/// non-zero counts, but the floor keeps "doctor failed" distinguishable
/// from a clean run if a future caller miswires it.
const EXIT_DOCTOR_FLOOR: u8 = 1;

// ---------------------------------------------------------------------------
// Entry Point
// ---------------------------------------------------------------------------

/// Process entry point.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => exit_for(&err),
    }
}

// ---------------------------------------------------------------------------
// Exit Codes
// ---------------------------------------------------------------------------

/// Run the CLI from start to finish.
fn run() -> Result<()> {
    let cli = Cli::parse();

    logging::init(cli.verbosity())?;

    refuse_root()?;

    let config = resolve_config(&cli)?;

    cli::run(cli, config)
}

/// Resolve the configuration. Returns `None` when the file is missing
/// and the subcommand tolerates it.
fn resolve_config(cli: &Cli) -> Result<Option<tartarus::config::Config>> {
    let overrides = cli::cli_overrides(cli);

    match config::load_and_resolve(overrides) {
        Ok(config) => Ok(Some(config)),
        Err(Error::Config(ConfigError::NotFound { path })) if cli::tolerates_missing_config(cli) => {
            tracing::debug!(?path, "no config file present; subcommand tolerates this");
            Ok(None)
        },
        Err(other) => Err(other),
    }
}

/// Translate an [`Error`] into the matching [`ExitCode`].
fn exit_for(err: &Error) -> ExitCode {
    match err {
        Error::RunningAsRoot => {
            tracing::error!(%err, "tartarus refuses to run as root");
            ExitCode::from(EXIT_RUNNING_AS_ROOT)
        },
        Error::NotImplemented(label) => {
            tracing::error!(command = %label, "command not yet implemented");
            ExitCode::from(EXIT_NOT_IMPLEMENTED)
        },
        Error::DoctorFailures(failures) => {
            tracing::error!(%err, "doctor reported failing checks");
            ExitCode::from((*failures).max(EXIT_DOCTOR_FLOOR))
        },
        _ => {
            tracing::error!(%err, "command failed");
            ExitCode::from(EXIT_GENERIC_FAILURE)
        },
    }
}
