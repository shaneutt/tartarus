//! Hetzner Cloud HTTP client.
//!
//! Wraps a [`reqwest::blocking::Client`] preconfigured with the
//! bearer token and base URL. Endpoint modules call into the helper
//! methods on this struct rather than constructing requests by hand.

use reqwest::{
    StatusCode,
    blocking::{Client as Http, RequestBuilder, Response},
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE},
};
use serde::de::DeserializeOwned;

use crate::api::error::{ApiError, ErrorEnvelope};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default API base URL.
pub const DEFAULT_BASE_URL: &str = "https://api.hetzner.cloud/v1";

// -----------------------------------------------------------------------------
// Client
// -----------------------------------------------------------------------------

/// Typed Hetzner Cloud HTTP client.
#[derive(Clone, Debug)]
pub struct Client {
    /// API base URL (defaults to [`DEFAULT_BASE_URL`]).
    base_url: String,

    /// Underlying reqwest blocking client.
    http: Http,

    /// Bearer token (kept owned so reqwest's request builder can
    /// borrow it without us juggling lifetimes).
    token: String,
}

impl Client {
    /// Construct a client against the production API.
    pub fn new(api_token: impl Into<String>) -> Self {
        Self::with_base_url(api_token, DEFAULT_BASE_URL)
    }

    /// Construct a client against a custom base URL (testing).
    pub fn with_base_url(api_token: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            http: Http::new(),
            token: api_token.into(),
        }
    }

    /// GET `path`, decoding the response body as `T`.
    pub fn get<T: DeserializeOwned>(&self, endpoint: &'static str, path: &str) -> Result<T, ApiError> {
        let req = self.http.get(self.url(path)).headers(self.auth_headers());
        self.send(endpoint, req)
    }

    /// POST `path` with `body`, decoding the response as `T`.
    pub fn post<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        endpoint: &'static str,
        path: &str,
        body: &B,
    ) -> Result<T, ApiError> {
        let req = self.http.post(self.url(path)).headers(self.auth_headers()).json(body);
        self.send(endpoint, req)
    }

    /// DELETE `path`. Hetzner returns 204 for a successful delete or
    /// a 201 + action body for async deletes (servers).
    pub fn delete<T: DeserializeOwned>(&self, endpoint: &'static str, path: &str) -> Result<Option<T>, ApiError> {
        let req = self.http.delete(self.url(path)).headers(self.auth_headers());
        let response = req.send().map_err(ApiError::from)?;

        let status = response.status();
        if status == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let body = response.bytes().map_err(ApiError::from)?;
        if status.is_success() {
            let decoded: T =
                serde_json::from_slice(&body).map_err(|source| ApiError::ParseBody { endpoint, source })?;
            return Ok(Some(decoded));
        }
        Err(decode_error_body(endpoint, status, &body))
    }

    // ---- Internals ----------------------------------------------------------

    /// Build the fully-qualified URL for a given path fragment.
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Standard headers carried on every request.
    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = format!("Bearer {}", self.token).parse() {
            headers.insert(AUTHORIZATION, value);
        }
        headers.insert(ACCEPT, "application/json".parse().expect("static value parses"));
        headers.insert(CONTENT_TYPE, "application/json".parse().expect("static value parses"));
        headers
    }

    /// Send `req`, decode the body as `T` on 2xx, or surface a
    /// typed [`ApiError`] otherwise.
    fn send<T: DeserializeOwned>(&self, endpoint: &'static str, req: RequestBuilder) -> Result<T, ApiError> {
        let response: Response = req.send().map_err(ApiError::from)?;
        let status = response.status();
        let body = response.bytes().map_err(ApiError::from)?;

        if status.is_success() {
            return serde_json::from_slice(&body).map_err(|source| ApiError::ParseBody { endpoint, source });
        }

        Err(decode_error_body(endpoint, status, &body))
    }
}

/// Try to decode `body` as a Hetzner [`ErrorEnvelope`]; fall back to
/// the raw bytes when that fails.
fn decode_error_body(endpoint: &'static str, status: StatusCode, body: &[u8]) -> ApiError {
    if let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(body) {
        return ApiError::Hetzner {
            code: envelope.error.code,
            message: envelope.error.message,
        };
    }

    ApiError::UnexpectedStatus {
        body: String::from_utf8_lossy(body).chars().take(512).collect(),
        endpoint,
        status: status.as_u16(),
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::api::tests_fake_server::{CannedResponse, Server};

    #[derive(Debug, Deserialize, PartialEq)]
    struct Echo {
        ok: bool,
    }

    #[test]
    fn decode_error_body_prefers_typed_envelope() {
        let body = br#"{"error":{"code":"not_found","message":"x"}}"#;
        let err = decode_error_body("GET /x", StatusCode::NOT_FOUND, body);
        match err {
            ApiError::Hetzner { code, message } => {
                assert_eq!(code, "not_found");
                assert_eq!(message, "x");
            },
            other => panic!("expected Hetzner variant, got {other:?}"),
        }
    }

    #[test]
    fn decode_error_body_falls_back_to_unexpected_status() {
        let body = b"<html>upstream 502</html>";
        let err = decode_error_body("GET /x", StatusCode::BAD_GATEWAY, body);
        match err {
            ApiError::UnexpectedStatus { status, endpoint, body } => {
                assert_eq!(status, 502);
                assert_eq!(endpoint, "GET /x");
                assert!(body.contains("upstream 502"));
            },
            other => panic!("expected UnexpectedStatus, got {other:?}"),
        }
    }

    #[test]
    fn get_round_trips_through_fake_server() {
        let server = Server::start(vec![CannedResponse::ok(r#"{"ok":true}"#)]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let body: Echo = client.get("GET /probe", "/probe").expect("GET should succeed");
        assert_eq!(body, Echo { ok: true });

        let paths = server.seen_paths.lock().expect("seen_paths lock");
        assert!(
            paths[0].contains("GET /probe"),
            "request line should be captured: {paths:?}"
        );
    }

    #[test]
    fn post_sends_json_body_and_decodes_response() {
        let server = Server::start(vec![CannedResponse::created(r#"{"ok":true}"#)]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let body: Echo = client
            .post("POST /thing", "/thing", &serde_json::json!({"name":"hello"}))
            .expect("POST should succeed");
        assert_eq!(body, Echo { ok: true });

        let bodies = server.seen_bodies.lock().expect("seen_bodies lock");
        assert!(
            bodies[0].contains("\"name\":\"hello\""),
            "request body should be JSON-serialised: {bodies:?}",
        );
    }

    #[test]
    fn delete_decodes_no_content_to_none() {
        let server = Server::start(vec![CannedResponse::no_content()]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let result: Option<Echo> = client.delete("DELETE /thing", "/thing").expect("DELETE should succeed");
        assert!(result.is_none(), "204 No Content should map to Ok(None)");
    }

    #[test]
    fn delete_decodes_body_when_status_is_201() {
        let server = Server::start(vec![CannedResponse::created(r#"{"ok":false}"#)]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let result: Option<Echo> = client.delete("DELETE /thing", "/thing").expect("DELETE should succeed");
        assert_eq!(result, Some(Echo { ok: false }));
    }

    #[test]
    fn get_surfaces_typed_hetzner_error_on_4xx() {
        let server = Server::start(vec![CannedResponse::error(404, "not_found", "missing")]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let err = client
            .get::<Echo>("GET /thing", "/thing")
            .expect_err("404 should surface as ApiError");
        match err {
            ApiError::Hetzner { code, message } => {
                assert_eq!(code, "not_found");
                assert_eq!(message, "missing");
            },
            other => panic!("expected Hetzner variant, got {other:?}"),
        }
    }

    #[test]
    fn get_surfaces_unexpected_status_when_body_is_unparsable() {
        let server = Server::start(vec![CannedResponse {
            status: 502,
            body: "upstream down".to_owned(),
        }]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let err = client
            .get::<Echo>("GET /thing", "/thing")
            .expect_err("502 should error");
        match err {
            ApiError::UnexpectedStatus { status, .. } => assert_eq!(status, 502),
            other => panic!("expected UnexpectedStatus, got {other:?}"),
        }
    }

    #[test]
    fn delete_surfaces_typed_hetzner_error_on_409() {
        let server = Server::start(vec![CannedResponse::error(409, "in_use", "still attached")]);
        let client = Client::with_base_url("test-token", &server.base_url);
        let err = client
            .delete::<Echo>("DELETE /thing", "/thing")
            .expect_err("409 should error");
        match err {
            ApiError::Hetzner { code, .. } => assert_eq!(code, "in_use"),
            other => panic!("expected Hetzner variant, got {other:?}"),
        }
    }
}
