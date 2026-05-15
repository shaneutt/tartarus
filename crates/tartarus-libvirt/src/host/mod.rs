//! Host-side libvirt surface.
//!
//! Owns the [`connect::Connection`] to libvirtd, the domain XML
//! templates, and the `qemu-guest-agent` client.

pub mod agent;
pub mod connect;
pub mod domain;
pub mod error;
