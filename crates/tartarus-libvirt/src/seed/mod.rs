//! libvirt-specific seed delivery: NoCloud `cloud-init.iso` authoring.
//!
//! The seed types and the cloud-config YAML renderer they consume
//! live in [`tartarus_provider::seed`]; this module wraps the YAML
//! into a NoCloud ISO that the libvirt-managed guest reads from a
//! virtio block device at first boot.

pub mod builder;
pub mod genisoimage;
pub mod iso;
