//! Errors raised by the Hetzner provider.

use crate::{api::ApiError, session::lifecycle::LifecycleError};

/// Crate-wide [`Result`][std::result::Result] alias.
pub type Result<T> = std::result::Result<T, Error>;

// -----------------------------------------------------------------------------
// Error
// -----------------------------------------------------------------------------

/// All error conditions the Hetzner provider surfaces to its callers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A Hetzner Cloud API call failed (network, HTTP status, or
    /// error envelope).
    #[error(transparent)]
    Api(#[from] ApiError),

    /// A session lifecycle step (volume attach, user_data assembly,
    /// action poll timeout) failed.
    #[error(transparent)]
    Lifecycle(#[from] LifecycleError),

    /// An error surfaced by `tartarus-provider` (I/O, config,
    /// session-shape).
    #[error(transparent)]
    Provider(#[from] tartarus_provider::Error),
}

/// Forward `std::io::Error` via the provider's `Io` variant.
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Provider(tartarus_provider::Error::Io(err))
    }
}

/// Forward [`tartarus_provider::session::SessionError`] through the
/// provider wrapper.
impl From<tartarus_provider::session::SessionError> for Error {
    fn from(err: tartarus_provider::session::SessionError) -> Self {
        Error::Provider(tartarus_provider::Error::Session(err))
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_provider::session::SessionError;

    use super::*;

    #[test]
    fn from_io_error_lands_under_provider() {
        let err: Error = std::io::Error::other("boom").into();
        match err {
            Error::Provider(tartarus_provider::Error::Io(_)) => {},
            other => panic!("expected Provider(Io), got {other:?}"),
        }
    }

    #[test]
    fn from_session_error_lands_under_provider() {
        let err: Error = SessionError::MissingCredentials.into();
        match err {
            Error::Provider(tartarus_provider::Error::Session(SessionError::MissingCredentials)) => {},
            other => panic!("expected Provider(Session(MissingCredentials)), got {other:?}"),
        }
    }

    #[test]
    fn api_variant_is_transparent_in_display() {
        let api_err = crate::api::ApiError::Hetzner {
            code: "invalid_input".to_owned(),
            message: "bad".to_owned(),
        };
        let err: Error = api_err.into();
        let rendered = err.to_string();
        assert!(rendered.contains("invalid_input"));
        assert!(rendered.contains("bad"));
    }
}
