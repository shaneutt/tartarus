//! Hetzner-side session lifecycle.
//!
//! Each module mirrors one [`SessionProvider`][trait] method:
//! [`run`] creates a server (and optional volume), [`resume`] /
//! [`stop`] flip the server power state, [`destroy`] tears the
//! server and its labels down, and [`list`] folds the project's
//! servers into the same [`ListEntry`] shape libvirt emits.
//!
//! [trait]: tartarus_provider::SessionProvider
//! [`ListEntry`]: tartarus_provider::ListEntry

pub mod destroy;
pub mod labels;
pub mod lifecycle;
pub mod list;
pub mod metadata;
pub mod rename;
pub mod resume;
pub mod run;
pub mod stop;
