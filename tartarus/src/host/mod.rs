//! Host-side libvirt surface.
//!
//! Owns the [`connect::Connection`] to libvirtd, the domain XML
//! templates, the serial console attach pump, and the
//! `qemu-guest-agent` client.

pub mod agent;
pub mod connect;
pub mod console;
pub mod domain;
pub mod error;
mod signals;
