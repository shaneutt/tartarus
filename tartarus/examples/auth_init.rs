//! Worked example: drive `tartarus auth init` against the live device-flow
//! and Anthropic console fallback paths.
//!
//! This is the same code path the `tartarus auth init` subcommand runs.
//! The example exists so a user can `cargo run --example auth_init` to
//! exercise the bootstrap flow end-to-end without invoking the binary,
//! and so the public API (`tartarus::auth::run_init`) is exercised the
//! way the architecture spec promises in the
//! [Doctor / examples][doctor-examples] section.
//!
//! The example writes (or refuses to overwrite) the user's real
//! `~/.config/tartarus/config.toml`, so re-running on an already-bootstrapped
//! host requires `--force`.
//!
//! [doctor-examples]: https://github.com/the-lost-art-of-programming/tartarus/blob/main/docs/architecture.md#doctor-subsystem

use std::process::ExitCode;

use tartarus::{auth, error::Error, logging, refuse_root};

// Constants

/// Exit code returned when the example completes successfully.
const EXIT_SUCCESS: u8 = 0;

/// Exit code returned when the bootstrap flow declined to overwrite an
/// existing config and the user did not pass `--force`.
const EXIT_REFUSED_OVERWRITE: u8 = 4;

/// Exit code returned for any other unexpected failure.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Example entry point.
fn main() -> ExitCode {
    let force = std::env::args().any(|a| a == "--force");

    if let Err(err) = logging::init(logging::Verbosity::Info) {
        eprintln!("failed to install tracing subscriber: {err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    if let Err(err) = refuse_root() {
        eprintln!("{err}");
        return ExitCode::from(EXIT_GENERIC_FAILURE);
    }

    match auth::run_init(force) {
        Ok(()) => {
            println!("auth_init example: completed");
            ExitCode::from(EXIT_SUCCESS)
        },
        Err(Error::Auth(auth::error::AuthError::ConfigAlreadyExists { path })) => {
            eprintln!(
                "auth_init example: config already exists at {p}; pass `--force` to overwrite",
                p = path.display()
            );
            ExitCode::from(EXIT_REFUSED_OVERWRITE)
        },
        Err(err) => {
            eprintln!("auth_init example: failed: {err}");
            ExitCode::from(EXIT_GENERIC_FAILURE)
        },
    }
}
