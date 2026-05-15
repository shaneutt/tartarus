//! libvirt-specific session lifecycle: run, resume, stop, destroy,
//! set, env, update, ssh, list, rename, gpu_index.
//!
//! The provider-agnostic primitives (identity, metadata, run-mode,
//! SessionError) live in [`tartarus_provider::session`]; this
//! module's contents are the libvirt-flavored orchestrators.

pub mod destroy;
pub mod env;
pub mod gpu_index;
pub mod list;
pub mod rename;
pub mod resume;
pub mod run;
pub mod set;
pub mod ssh;
pub mod ssh_attach;
pub mod ssh_port;
pub mod stop;
pub mod update;
