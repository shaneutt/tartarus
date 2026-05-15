//! Hetzner Cloud action polling.
//!
//! Server create, delete, power-on/off, and volume attach return an
//! [`Action`] with `status = "running"`. Callers poll the action
//! endpoint until status flips to `success` or `error`.

use serde::Deserialize;

use crate::api::{Client, error::ApiError};

// -----------------------------------------------------------------------------
// Action
// -----------------------------------------------------------------------------

/// Long-running operation handle from Hetzner.
#[derive(Clone, Debug, Deserialize)]
pub struct Action {
    /// Numeric action ID.
    pub id: u64,

    /// One of `running`, `success`, `error`.
    pub status: String,

    /// 0..=100 progress percentage.
    #[serde(default)]
    pub progress: u8,

    /// Hetzner error envelope on `status == "error"`.
    #[serde(default)]
    pub error: Option<ActionError>,
}

/// Inner error shape on a failed action.
#[derive(Clone, Debug, Deserialize)]
pub struct ActionError {
    /// Stable error code.
    pub code: String,

    /// Human-readable detail.
    pub message: String,
}

/// `GET /actions/{id}` envelope.
#[derive(Debug, Deserialize)]
struct ActionResponse {
    action: Action,
}

/// `GET /actions/{id}`.
pub fn get(client: &Client, id: u64) -> Result<Action, ApiError> {
    let response: ActionResponse = client.get("GET /actions/{id}", &format!("/actions/{id}"))?;
    Ok(response.action)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_response_deserialises_running_action() {
        let body = r#"{"action":{"id":7,"status":"running","progress":42}}"#;
        let response: ActionResponse = serde_json::from_str(body).expect("running action");
        assert_eq!(response.action.id, 7);
        assert_eq!(response.action.status, "running");
        assert_eq!(response.action.progress, 42);
        assert!(response.action.error.is_none());
    }

    #[test]
    fn action_response_deserialises_failed_action() {
        let body = r#"{"action":{"id":8,"status":"error","progress":100,"error":{"code":"limit_reached","message":"server limit exceeded"}}}"#;
        let response: ActionResponse = serde_json::from_str(body).expect("failed action");
        let inner = response.action.error.expect("error body present");
        assert_eq!(inner.code, "limit_reached");
        assert_eq!(inner.message, "server limit exceeded");
    }

    #[test]
    fn action_progress_defaults_to_zero_when_absent() {
        let body = r#"{"action":{"id":9,"status":"success"}}"#;
        let response: ActionResponse = serde_json::from_str(body).expect("progress-less action");
        assert_eq!(response.action.progress, 0);
    }
}
