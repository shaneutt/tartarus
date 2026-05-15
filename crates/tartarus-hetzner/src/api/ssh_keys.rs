//! Hetzner Cloud `/ssh_keys` endpoint.

use serde::Deserialize;

use crate::api::{Client, error::ApiError};

/// One SSH key resource.
#[derive(Clone, Debug, Deserialize)]
pub struct SshKey {
    /// Numeric key ID.
    pub id: u64,

    /// Display name.
    pub name: String,

    /// SHA256 fingerprint.
    pub fingerprint: String,
}

/// Response envelope for `GET /ssh_keys`.
#[derive(Debug, Deserialize)]
pub struct SshKeysResponse {
    /// All keys in the project (no pagination handling — Tartarus
    /// projects keep a small SSH key set).
    pub ssh_keys: Vec<SshKey>,
}

/// `GET /ssh_keys`.
pub fn list(client: &Client) -> Result<SshKeysResponse, ApiError> {
    client.get("GET /ssh_keys", "/ssh_keys")
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_keys_response_deserialises() {
        let body = r#"{"ssh_keys":[
            {"id":1,"name":"desktop","fingerprint":"ab:cd"},
            {"id":2,"name":"laptop","fingerprint":"ef:01"}
        ]}"#;
        let response: SshKeysResponse = serde_json::from_str(body).expect("ssh_keys");
        assert_eq!(response.ssh_keys.len(), 2);
        assert_eq!(response.ssh_keys[0].name, "desktop");
        assert_eq!(response.ssh_keys[1].fingerprint, "ef:01");
    }
}
