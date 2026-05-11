//! Worked example: fetch the latest Fedora cloud base, GPG-verify it,
//! apply the Tartarus layer, and update the `current` symlink.
//!
//! Drives [`tartarus::disk::base::pull`] directly, the same code path the
//! `tartarus base pull` subcommand runs. The layering boot requires a
//! working `qemu:///session` libvirtd plus `/dev/kvm`; on a host that
//! lacks either, the example surfaces the libvirt error verbatim and
//! exits with [`EXIT_NEEDS_LIBVIRTD`].

use std::process::ExitCode;

use tartarus::{
    disk::base::{self, DEFAULT_FEDORA_RELEASE},
    error::Error,
    logging, refuse_root,
};

// Constants

/// Exit code returned when the example completes successfully.
const EXIT_SUCCESS: u8 = 0;

/// Exit code returned for any other unexpected failure.
const EXIT_GENERIC_FAILURE: u8 = 1;

/// Exit code returned when the host lacks a working `qemu:///session`
/// libvirtd. Distinct from [`EXIT_GENERIC_FAILURE`] so a CI matrix can
/// distinguish "infra missing" from "logic broken."
const EXIT_NEEDS_LIBVIRTD: u8 = 5;

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

    let release = std::env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_FEDORA_RELEASE.to_owned());

    match base::pull(&release) {
        Ok(base) => {
            println!("base_pull example: completed; current -> {name}", name = base.name);
            ExitCode::from(EXIT_SUCCESS)
        },
        Err(Error::Host(err)) => {
            eprintln!(
                "base_pull example: needs libvirtd; run on a Fedora workstation with `qemu:///session` available: {err}",
            );
            ExitCode::from(EXIT_NEEDS_LIBVIRTD)
        },
        Err(err) => {
            eprintln!("base_pull example: failed: {err}");
            ExitCode::from(EXIT_GENERIC_FAILURE)
        },
    }
}
