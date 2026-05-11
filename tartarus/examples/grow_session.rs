//! Worked example: drive `tartarus grow <session>` against a running
//! session passed as a CLI argument.
//!
//! Drives [`tartarus::disk::grow::run`] which performs the four-step
//! online grow: `qemu-img info` → `qemu-img resize` →
//! `virDomainBlockResize` → in-guest `growpart` + `resize2fs`. The
//! session must be running.
//!
//! Usage: `cargo run --example grow_session -- <alias-or-uuid>`.

use std::process::ExitCode;

use tartarus::{config, disk::grow, error::Error, logging, refuse_root};

// Constants

/// Exit code returned when the example completes successfully.
const EXIT_SUCCESS: u8 = 0;

/// Exit code returned for any other unexpected failure.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Exit code returned when the host lacks a working `qemu:///session`
/// libvirtd. Distinct from [`EXIT_GENERIC_FAILURE`] so a CI matrix can
/// distinguish "infra missing" from "logic broken."
const EXIT_NEEDS_LIBVIRTD: u8 = 5;

/// Exit code returned when the user did not pass a target alias / UUID.
const EXIT_USAGE: u8 = 64;

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

    let target = match std::env::args().nth(1) {
        Some(t) => t,
        None => {
            eprintln!("usage: cargo run --example grow_session -- <alias-or-uuid>");
            return ExitCode::from(EXIT_USAGE);
        },
    };

    let resolved = match config::load_and_resolve(config::CliOverrides::default()) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("grow_session example: failed to load config: {err}");
            return ExitCode::from(EXIT_GENERIC_FAILURE);
        },
    };

    match grow::run(&resolved, &target) {
        Ok(outcome) => {
            println!(
                "grow_session example: session {uuid} grown {before}G -> {after}G",
                uuid = outcome.uuid,
                before = outcome.before_gib,
                after = outcome.after_gib,
            );
            ExitCode::from(EXIT_SUCCESS)
        },
        Err(Error::Host(err)) => {
            eprintln!("grow_session example: needs libvirtd: {err}");
            ExitCode::from(EXIT_NEEDS_LIBVIRTD)
        },
        Err(err) => {
            eprintln!("grow_session example: failed: {err}");
            ExitCode::from(EXIT_GENERIC_FAILURE)
        },
    }
}
