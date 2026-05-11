//! Session lifecycle: identity, metadata, run, list, rename, stop, destroy.
//!
//! A Tartarus session is the running (or stopped) VM holding one user's
//! work-in-progress. The session subsystem owns:
//!
//! - **Identity**: UUID generation and the alias-symlink layout under [`crate::paths::sessions_dir`]. See [`identity`].
//! - **Metadata**: the `metadata.json` v1 schema and IO. See [`metadata`].
//! - **Lifecycle**: [`run`], [`resume`], [`list`], [`rename`], [`stop`], [`destroy`].

pub mod destroy;
pub mod env;
pub mod error;
pub mod gpu_index;
pub mod identity;
pub mod list;
pub mod metadata;
pub mod rename;
pub mod resume;
pub mod run;
pub mod run_mode;
pub mod ssh;
pub mod ssh_attach;
pub mod ssh_port;
pub mod stop;
pub mod update;
