//! Errors surfaced by the provider crate.
//!
//! Provider-side errors are the small set of failure modes that
//! every backend can hit before any provider-specific work begins:
//! file I/O, project-directory resolution, and the session-shape
//! errors raised by UUID + alias + metadata handling. Backend-
//! specific errors (libvirt domain operations, Hetzner API
//! responses) live inside their own crates and are wrapped at the
//! binary boundary.

use crate::{config::ConfigError, session::SessionError};

// -----------------------------------------------------------------------------
// Result
// -----------------------------------------------------------------------------

/// Crate-wide [`Result`][std::result::Result] alias.
pub type Result<T> = std::result::Result<T, Error>;

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

/// Error conditions surfaced by `tartarus-provider`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration source (file, env, CLI) is missing, malformed, or
    /// fails semantic validation.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Could not derive XDG paths from the current environment.
    #[error("could not determine XDG project directories for tartarus")]
    NoProjectDirs,

    /// A session lifecycle, identity, or metadata operation failed.
    #[error(transparent)]
    Session(#[from] SessionError),
}
