//! libvirt-backed session provider for Tartarus.
//!
//! Owns the libvirt connection wrapper, the QEMU domain XML
//! templating + lifecycle, the qcow2 overlay and base-image library,
//! GPU passthrough, the cloud-init NoCloud ISO authoring, and the
//! libvirt-specific halves of `tartarus run`, `resume`, `stop`,
//! `destroy`, etc.
//!
//! The binary (`tartarus`) consumes the [`SessionProvider`] impl
//! defined here when the user did not select a cloud profile.
//!
//! [`SessionProvider`]: tartarus_provider::SessionProvider

#![deny(unsafe_code)]

pub mod disk;
pub mod error;
pub mod gpu;
pub mod host;
pub mod provider;
pub mod seed;
pub mod session;

pub use error::{Error, Result};
pub use provider::LibvirtProvider;
// Configuration is provider-agnostic; re-exported here so this
// crate's modules keep resolving `crate::config::...` after the
// workspace split.
pub use tartarus_provider::config;
