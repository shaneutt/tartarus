//! Errors raised by the libvirt provider.
//!
//! The libvirt crate's internal modules (host, disk, gpu, seed,
//! session) each carry their own focused error enum; this top-level
//! [`Error`] wraps them via `#[from]` and also re-exports the
//! provider-shared errors (I/O, config, session-shape) coming up
//! from [`tartarus_provider::Error`].
//!
//! The binary's top-level [`tartarus::error::Error`] in turn wraps
//! `Error` from this crate (via a manual `From` that flattens) so
//! the user-visible message stays one transparent variant deep.

use crate::{
    disk::{base::BaseError, grow::GrowError, overlay::OverlayError},
    gpu::GpuError,
    host::error::HostError,
    seed::{builder::SeedBuilderError, iso::SeedError},
};

// -----------------------------------------------------------------------------
// Result
// -----------------------------------------------------------------------------

/// Crate-wide [`Result`][std::result::Result] alias.
pub type Result<T> = std::result::Result<T, Error>;

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

/// All error conditions the libvirt provider surfaces to its callers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A base-library operation (pull, list, prune) failed.
    #[error(transparent)]
    Base(#[from] BaseError),

    /// A GPU passthrough operation failed.
    #[error(transparent)]
    Gpu(#[from] GpuError),

    /// A per-session online-grow operation failed.
    #[error(transparent)]
    Grow(#[from] GrowError),

    /// A libvirt or in-guest agent operation failed.
    #[error(transparent)]
    Host(#[from] HostError),

    /// Subcommand not yet implemented.
    #[error("`{0}` is not yet implemented")]
    NotImplemented(&'static str),

    /// A per-session overlay operation failed.
    #[error(transparent)]
    Overlay(#[from] OverlayError),

    /// An error surfaced by `tartarus-provider` (I/O, config,
    /// session-shape).
    #[error(transparent)]
    Provider(#[from] tartarus_provider::Error),

    /// A seed authoring operation (genisoimage, write_files staging) failed.
    #[error(transparent)]
    Seed(#[from] SeedError),

    /// Materialising a session [`Seed`] failed (e.g. Vertex SA file).
    ///
    /// [`Seed`]: tartarus_provider::seed::input::Seed
    #[error(transparent)]
    SeedBuilder(#[from] SeedBuilderError),
}

// -----------------------------------------------------------------------------
// Convenience Conversions
// -----------------------------------------------------------------------------

/// Forward `std::io::Error` via the provider's `Io` variant rather
/// than a separate one on this enum, keeping the error tree linear.
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Provider(tartarus_provider::Error::Io(err))
    }
}

/// Forward `tartarus_provider::session::SessionError` through the
/// provider's wrapper for the same reason.
impl From<tartarus_provider::session::SessionError> for Error {
    fn from(err: tartarus_provider::session::SessionError) -> Self {
        Error::Provider(tartarus_provider::Error::Session(err))
    }
}

/// Forward `tartarus_provider::config::ConfigError` through the
/// provider's wrapper.
impl From<tartarus_provider::config::ConfigError> for Error {
    fn from(err: tartarus_provider::config::ConfigError) -> Self {
        Error::Provider(tartarus_provider::Error::Config(err))
    }
}
