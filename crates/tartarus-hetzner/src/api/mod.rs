//! Typed Hetzner Cloud REST client.
//!
//! Thin blocking-reqwest wrapper around the subset of the
//! `https://api.hetzner.cloud/v1` surface Tartarus uses: servers,
//! actions, volumes, and SSH keys. Every request carries a Bearer
//! token and accepts/produces JSON.
//!
//! The client deserialises Hetzner's error envelope into
//! [`ApiError::Hetzner`] so callers see the structured `code`/
//! `message` pair rather than a raw HTTP status.

pub mod actions;
pub mod client;
pub mod error;
pub mod servers;
pub mod ssh_keys;
#[cfg(test)]
pub(crate) mod tests_fake_server;
pub mod volumes;

pub use client::Client;
pub use error::ApiError;
