//! Hetzner Cloud API error surface.

use serde::Deserialize;

// -----------------------------------------------------------------------------
// ApiError
// -----------------------------------------------------------------------------

/// Failure modes raised by the Hetzner Cloud client.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Could not assemble the request URL.
    #[error("could not build request URL for {endpoint}: {source}")]
    BuildUrl {
        /// API endpoint label.
        endpoint: &'static str,

        /// Underlying URL-parse error.
        source: reqwest::Error,
    },

    /// The Hetzner API returned a structured error envelope.
    #[error("Hetzner Cloud API error: {code} — {message}")]
    Hetzner {
        /// Stable error code (e.g. `not_found`, `invalid_input`).
        code: String,

        /// Human-readable detail from the API.
        message: String,
    },

    /// Transport failed (DNS, TLS, connect, read).
    #[error("Hetzner Cloud API transport error: {0}")]
    Transport(#[from] reqwest::Error),

    /// Response body could not be parsed as the expected JSON shape.
    #[error("could not parse Hetzner Cloud API response for {endpoint}: {source}")]
    ParseBody {
        /// API endpoint label.
        endpoint: &'static str,

        /// Underlying serde error.
        source: serde_json::Error,
    },

    /// HTTP request returned a status the client did not expect and
    /// the body was not a recognisable error envelope.
    #[error("Hetzner Cloud API returned unexpected status {status} for {endpoint}: {body}")]
    UnexpectedStatus {
        /// Truncated response body for the operator.
        body: String,

        /// API endpoint label.
        endpoint: &'static str,

        /// Numeric HTTP status code (e.g. 502).
        status: u16,
    },
}

/// Hetzner's standard error envelope.
#[derive(Debug, Deserialize)]
pub(crate) struct ErrorEnvelope {
    pub(crate) error: ErrorBody,
}

/// Inner half of [`ErrorEnvelope`].
#[derive(Debug, Deserialize)]
pub(crate) struct ErrorBody {
    pub(crate) code: String,
    pub(crate) message: String,
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hetzner_variant_renders_code_and_message() {
        let err = ApiError::Hetzner {
            code: "not_found".to_owned(),
            message: "no such server".to_owned(),
        };
        let rendered = err.to_string();
        assert!(
            rendered.contains("not_found"),
            "code should appear in message: {rendered}"
        );
        assert!(
            rendered.contains("no such server"),
            "message should appear in display: {rendered}",
        );
    }

    #[test]
    fn unexpected_status_carries_endpoint_status_and_body() {
        let err = ApiError::UnexpectedStatus {
            body: "<html>nginx 502</html>".to_owned(),
            endpoint: "GET /servers",
            status: 502,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("502"), "status should appear in message");
        assert!(
            rendered.contains("GET /servers"),
            "endpoint label should appear in message"
        );
    }

    #[test]
    fn parse_body_error_decodes_round_trip() {
        let invalid: serde_json::Error = serde_json::from_str::<u32>("not a number").unwrap_err();
        let err = ApiError::ParseBody {
            endpoint: "GET /actions/1",
            source: invalid,
        };
        let rendered = err.to_string();
        assert!(rendered.contains("GET /actions/1"));
    }

    #[test]
    fn error_envelope_deserialises_from_canonical_payload() {
        let body = r#"{"error":{"code":"invalid_input","message":"servers.name has wrong format"}}"#;
        let envelope: ErrorEnvelope = serde_json::from_str(body).expect("canonical envelope should parse");
        assert_eq!(envelope.error.code, "invalid_input");
        assert_eq!(envelope.error.message, "servers.name has wrong format");
    }
}
