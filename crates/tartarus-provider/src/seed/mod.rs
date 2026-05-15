//! Cloud-init seed data structures and the pure renderer that
//! converts them into `#cloud-config` YAML.
//!
//! Provider implementations consume [`input::Seed`] and turn the
//! rendered YAML into whatever their backend expects: a NoCloud ISO
//! for libvirt, or the cloud-image `user_data` field for cloud
//! providers like Hetzner.

pub mod input;
pub mod render;

pub use input::{ClaudeCredentials, ClaudeDefaults, CredentialBundle, RepoSpec, Seed, SeedInputs};
