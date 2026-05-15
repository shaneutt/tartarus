//! Crate-wide error type and [`Result`] alias.

// -----------------------------------------------------------------------------
// Result
// -----------------------------------------------------------------------------

/// Crate-wide [`Result`][std::result::Result] alias.
pub type Result<T> = std::result::Result<T, Error>;

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

/// All error conditions Tartarus surfaces to its callers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Credential acquisition or storage failed.
    #[error(transparent)]
    Auth(#[from] crate::auth::error::AuthError),

    /// A base-library operation (pull, list, prune) failed.
    #[error(transparent)]
    Base(#[from] tartarus_libvirt::disk::base::BaseError),

    /// A configuration source (file, env, CLI) is missing, malformed, or
    /// fails semantic validation.
    #[error(transparent)]
    Config(#[from] tartarus_provider::config::ConfigError),

    /// `tartarus doctor` reported failing checks.
    #[error("tartarus doctor: {0} check(s) failed")]
    DoctorFailures(u8),

    /// A GPU passthrough operation (host pre-check, device probe,
    /// driver detach) failed.
    #[error(transparent)]
    Gpu(#[from] tartarus_libvirt::gpu::GpuError),

    /// A per-session online-grow operation failed.
    #[error(transparent)]
    Grow(#[from] tartarus_libvirt::disk::grow::GrowError),

    /// A libvirt or in-guest agent operation failed.
    #[error(transparent)]
    Host(#[from] tartarus_libvirt::host::error::HostError),

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
    Overlay(#[from] tartarus_libvirt::disk::overlay::OverlayError),

    /// Tartarus refuses to run as root.
    #[error("tartarus refuses to run as root. invoke as your unprivileged user.")]
    RunningAsRoot,

    /// A seed authoring operation (genisoimage, write_files staging) failed.
    #[error(transparent)]
    Seed(#[from] tartarus_libvirt::seed::iso::SeedError),

    /// Materialising a session seed (e.g. reading the Vertex SA file) failed.
    #[error(transparent)]
    SeedBuilder(#[from] tartarus_libvirt::seed::builder::SeedBuilderError),

    /// A session lifecycle, identity, or metadata operation failed.
    #[error(transparent)]
    Session(#[from] tartarus_provider::session::SessionError),

    /// A Hetzner Cloud API call failed.
    #[error(transparent)]
    HetznerApi(#[from] tartarus_hetzner::api::ApiError),

    /// A Hetzner lifecycle step (action polling, server lookup) failed.
    #[error(transparent)]
    HetznerLifecycle(#[from] tartarus_hetzner::session::lifecycle::LifecycleError),
}

// -----------------------------------------------------------------------------
// Provider Error Conversion
// -----------------------------------------------------------------------------

/// Flatten a [`tartarus_provider::Error`] into a binary-side [`Error`]
/// without nesting. The provider's error variants are 1:1 with
/// variants on the binary's enum; this conversion picks the matching
/// one rather than wrapping the whole provider error.
impl From<tartarus_provider::Error> for Error {
    fn from(err: tartarus_provider::Error) -> Self {
        match err {
            tartarus_provider::Error::Config(cfg) => Error::Config(cfg),
            tartarus_provider::Error::Io(io) => Error::Io(io),
            tartarus_provider::Error::NoProjectDirs => Error::NoProjectDirs,
            tartarus_provider::Error::Session(session) => Error::Session(session),
        }
    }
}

/// Flatten a [`tartarus_libvirt::Error`] into a binary-side [`Error`]
/// the same way; each libvirt variant is 1:1 with one of ours.
impl From<tartarus_libvirt::Error> for Error {
    fn from(err: tartarus_libvirt::Error) -> Self {
        match err {
            tartarus_libvirt::Error::Base(base) => Error::Base(base),
            tartarus_libvirt::Error::Gpu(gpu) => Error::Gpu(gpu),
            tartarus_libvirt::Error::Grow(grow) => Error::Grow(grow),
            tartarus_libvirt::Error::Host(host) => Error::Host(host),
            tartarus_libvirt::Error::NotImplemented(s) => Error::NotImplemented(s),
            tartarus_libvirt::Error::Overlay(overlay) => Error::Overlay(overlay),
            tartarus_libvirt::Error::Provider(provider) => Error::from(provider),
            tartarus_libvirt::Error::Seed(seed) => Error::Seed(seed),
            tartarus_libvirt::Error::SeedBuilder(sb) => Error::SeedBuilder(sb),
        }
    }
}

/// Same shape for [`tartarus_hetzner::Error`].
impl From<tartarus_hetzner::Error> for Error {
    fn from(err: tartarus_hetzner::Error) -> Self {
        match err {
            tartarus_hetzner::Error::Api(api) => Error::HetznerApi(api),
            tartarus_hetzner::Error::Lifecycle(lc) => Error::HetznerLifecycle(lc),
            tartarus_hetzner::Error::Provider(provider) => Error::from(provider),
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_hetzner::session::lifecycle::LifecycleError;
    use tartarus_provider::{config::ConfigError, session::SessionError};

    use super::*;

    #[test]
    fn from_provider_error_routes_each_variant() {
        let io = std::io::Error::other("boom");
        match Error::from(tartarus_provider::Error::Io(io)) {
            Error::Io(_) => {},
            other => panic!("expected Io, got {other:?}"),
        }

        match Error::from(tartarus_provider::Error::NoProjectDirs) {
            Error::NoProjectDirs => {},
            other => panic!("expected NoProjectDirs, got {other:?}"),
        }

        let cfg = ConfigError::Invalid("bad".to_owned());
        match Error::from(tartarus_provider::Error::Config(cfg)) {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert_eq!(msg, "bad", "the inner ConfigError detail should round-trip");
            },
            other => panic!("expected Config(Invalid), got {other:?}"),
        }

        let session = SessionError::MissingCredentials;
        match Error::from(tartarus_provider::Error::Session(session)) {
            Error::Session(SessionError::MissingCredentials) => {},
            other => panic!("expected Session(MissingCredentials), got {other:?}"),
        }
    }

    #[test]
    fn from_libvirt_error_flattens_not_implemented() {
        match Error::from(tartarus_libvirt::Error::NotImplemented("hetzner mock")) {
            Error::NotImplemented(s) => assert_eq!(s, "hetzner mock"),
            other => panic!("expected NotImplemented, got {other:?}"),
        }
    }

    #[test]
    fn from_libvirt_error_flattens_provider_wrapper() {
        let provider_io = tartarus_provider::Error::Io(std::io::Error::other("nested"));
        match Error::from(tartarus_libvirt::Error::Provider(provider_io)) {
            Error::Io(_) => {},
            other => panic!("expected Io (flattened from libvirt::Provider(Io)), got {other:?}"),
        }
    }

    #[test]
    fn from_hetzner_error_routes_each_variant() {
        let api = tartarus_hetzner::api::ApiError::Hetzner {
            code: "rate_limited".to_owned(),
            message: "too many requests".to_owned(),
        };
        match Error::from(tartarus_hetzner::Error::Api(api)) {
            Error::HetznerApi(_) => {},
            other => panic!("expected HetznerApi, got {other:?}"),
        }

        let lc = LifecycleError::SessionNotFound {
            target: "foo".to_owned(),
        };
        match Error::from(tartarus_hetzner::Error::Lifecycle(lc)) {
            Error::HetznerLifecycle(LifecycleError::SessionNotFound { target }) => assert_eq!(target, "foo"),
            other => panic!("expected HetznerLifecycle(SessionNotFound), got {other:?}"),
        }
    }

    #[test]
    fn from_hetzner_error_flattens_provider_wrapper() {
        let inner = tartarus_provider::Error::NoProjectDirs;
        match Error::from(tartarus_hetzner::Error::Provider(inner)) {
            Error::NoProjectDirs => {},
            other => panic!("expected NoProjectDirs, got {other:?}"),
        }
    }

    #[test]
    fn doctor_failures_renders_count() {
        let err = Error::DoctorFailures(3);
        assert!(
            err.to_string().contains("3 check(s)"),
            "DoctorFailures message should include the count, got: {err}",
        );
    }

    #[test]
    fn running_as_root_renders_helpful_message() {
        assert!(
            Error::RunningAsRoot.to_string().contains("refuses to run as root"),
            "RunningAsRoot message should explain the refusal",
        );
    }

    #[test]
    fn not_implemented_renders_the_label() {
        let err = Error::NotImplemented("widget");
        assert!(
            err.to_string().contains("widget"),
            "NotImplemented message should include the label, got: {err}",
        );
    }

    #[test]
    fn io_error_propagates_via_question_mark() {
        fn faulty() -> Result<()> {
            Err(std::io::Error::other("disk full"))?;
            Ok(())
        }
        match faulty() {
            Err(Error::Io(_)) => {},
            other => panic!("expected Err(Io), got {other:?}"),
        }
    }
}
