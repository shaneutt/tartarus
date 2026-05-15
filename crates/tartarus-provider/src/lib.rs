//! Provider-agnostic interface and shared types for Tartarus
//! session backends.
//!
//! This crate defines the [`SessionProvider`] trait, the
//! provider-agnostic request/response and metadata types that flow
//! through every backend, the seed renderer that produces
//! cloud-init `#cloud-config` YAML, and the small utilities (paths,
//! host user lookup, time) shared by the binary and every provider
//! implementation.
//!
//! Provider implementations (`tartarus-libvirt`, `tartarus-hetzner`)
//! depend on this crate and never on each other; the binary is the
//! only place that holds references to more than one provider at a
//! time.

#![deny(unsafe_code)]

pub mod config;
pub mod error;
pub mod host_user;
pub mod paths;
pub mod provider;
pub mod seed;
pub mod session;
pub mod time;

pub use error::{Error, Result};
pub use provider::{
    DestroyOutcome, ListEntry, RenameOutcome, ResumeOutcome, RunOutcome, RunRequest, SessionProvider, StopOutcome,
};
