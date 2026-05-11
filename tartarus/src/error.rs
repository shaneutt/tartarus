//! Crate-wide error type and [`Result`] alias.

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Crate-wide [`Result`][std::result::Result] alias.
pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// All error conditions Tartarus surfaces to its callers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Credential acquisition or storage failed.
    #[error(transparent)]
    Auth(#[from] crate::auth::error::AuthError),

    /// A base-library operation (pull, list, prune) failed.
    #[error(transparent)]
    Base(#[from] crate::disk::base::BaseError),

    /// A configuration source (file, env, CLI) is missing, malformed, or
    /// fails semantic validation.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),

    /// `tartarus doctor` reported failing checks.
    #[error("tartarus doctor: {0} check(s) failed")]
    DoctorFailures(u8),

    /// A GPU passthrough operation (host pre-check, device probe,
    /// driver detach) failed.
    #[error(transparent)]
    Gpu(#[from] crate::gpu::GpuError),

    /// A per-session online-grow operation failed.
    #[error(transparent)]
    Grow(#[from] crate::disk::grow::GrowError),

    /// A libvirt or in-guest agent operation failed.
    #[error(transparent)]
    Host(#[from] crate::host::error::HostError),

    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Could not derive XDG paths from the current environment.
    #[error("could not determine XDG project directories for tartarus")]
    NoProjectDirs,

    /// Subcommand not yet implemented.
    #[error("`{0}` is not yet implemented")]
    NotImplemented(&'static str),

    /// A per-session overlay operation failed.
    #[error(transparent)]
    Overlay(#[from] crate::disk::overlay::OverlayError),

    /// Tartarus refuses to run as root.
    #[error("tartarus refuses to run as root. invoke as your unprivileged user.")]
    RunningAsRoot,

    /// A seed authoring operation (genisoimage, write_files staging) failed.
    #[error(transparent)]
    Seed(#[from] crate::seed::iso::SeedError),

    /// A session lifecycle, identity, or metadata operation failed.
    #[error(transparent)]
    Session(#[from] crate::session::error::SessionError),
}
