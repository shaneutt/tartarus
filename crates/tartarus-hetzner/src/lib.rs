//! Hetzner Cloud session provider for Tartarus.
//!
//! Implements [`SessionProvider`] against the Hetzner Cloud REST API
//! (`https://api.hetzner.cloud/v1`). Sessions map to Hetzner servers,
//! per-session `/home` lives on an attached volume, and the
//! Tartarus seed (`#cloud-config` YAML) ships as the server's
//! `user_data`.
//!
//! [`SessionProvider`]: tartarus_provider::SessionProvider

#![deny(unsafe_code)]

pub mod api;
pub mod config;
pub mod error;
pub mod provider;
pub mod session;

pub use error::{Error, Result};
pub use provider::HetznerProvider;
