//! Hetzner Cloud `/servers` endpoint.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api::{Client, error::ApiError};

// -----------------------------------------------------------------------------
// Server
// -----------------------------------------------------------------------------

/// One server resource as Hetzner returns it.
#[derive(Clone, Debug, Deserialize)]
pub struct Server {
    /// Numeric server ID.
    pub id: u64,

    /// Server name (Hetzner's display name, not Tartarus's alias).
    pub name: String,

    /// Lifecycle status (`initializing`, `running`, `off`, ...).
    pub status: String,

    /// Public networking block; carries the public IPv4/IPv6.
    #[serde(default)]
    pub public_net: Option<PublicNet>,

    /// Tartarus labels round-trip via this map.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// Public networking block on a server.
#[derive(Clone, Debug, Deserialize)]
pub struct PublicNet {
    /// IPv4 leg.
    #[serde(default)]
    pub ipv4: Option<Ipv4>,
}

/// IPv4 leg of [`PublicNet`].
#[derive(Clone, Debug, Deserialize)]
pub struct Ipv4 {
    /// Public IPv4 address.
    pub ip: String,
}

// -----------------------------------------------------------------------------
// CreateServer
// -----------------------------------------------------------------------------

/// Payload for `POST /servers`.
#[derive(Clone, Debug, Serialize)]
pub struct CreateServerRequest<'a> {
    /// Hetzner server name (we always pass `tartarus-<short uuid>`).
    pub name: &'a str,

    /// Server type slug (e.g. `cx21`).
    pub server_type: &'a str,

    /// OS image slug (e.g. `ubuntu-22.04`).
    pub image: &'a str,

    /// Location code (e.g. `nbg1`).
    pub location: &'a str,

    /// Cloud-init `#cloud-config` body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data: Option<&'a str>,

    /// Existing SSH-key names to inject (also covered by cloud-init).
    #[serde(default)]
    pub ssh_keys: Vec<&'a str>,

    /// Whether Hetzner should start the server immediately after create.
    pub start_after_create: bool,

    /// Round-trip labels (filters via `label_selector`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,

    /// Volumes to attach at create time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<u64>,
}

/// Response envelope for `POST /servers`.
#[derive(Debug, Deserialize)]
pub struct CreateServerResponse {
    /// The created server resource.
    pub server: Server,

    /// Async create action (poll for `progress`/`status`).
    pub action: Action,
}

/// Response envelope for `GET /servers/{id}`.
#[derive(Debug, Deserialize)]
pub struct ServerResponse {
    /// The server resource.
    pub server: Server,
}

/// Response envelope for `GET /servers`.
#[derive(Debug, Deserialize)]
pub struct ServersResponse {
    /// All servers Hetzner returned (this client does not paginate
    /// yet; per-page defaults are large enough for Tartarus's usage).
    pub servers: Vec<Server>,
}

/// Response envelope for `DELETE /servers/{id}`.
#[derive(Debug, Deserialize)]
pub struct DeleteServerResponse {
    /// Async deletion action.
    pub action: Action,
}

/// Minimal action shape; full polling lives in [`crate::api::actions`].
#[derive(Debug, Deserialize)]
pub struct Action {
    /// Action ID for polling.
    pub id: u64,

    /// Final or in-progress status (`running`, `success`, `error`).
    pub status: String,
}

// -----------------------------------------------------------------------------
// Server Operations
// -----------------------------------------------------------------------------

/// `POST /servers`.
pub fn create(client: &Client, request: &CreateServerRequest<'_>) -> Result<CreateServerResponse, ApiError> {
    client.post("POST /servers", "/servers", request)
}

/// `GET /servers/{id}`.
pub fn get(client: &Client, id: u64) -> Result<ServerResponse, ApiError> {
    client.get("GET /servers/{id}", &format!("/servers/{id}"))
}

/// `GET /servers`, optionally filtered by a label selector.
pub fn list(client: &Client, label_selector: Option<&str>) -> Result<ServersResponse, ApiError> {
    let path = match label_selector {
        Some(sel) => format!("/servers?label_selector={}", urlencode(sel)),
        None => "/servers".to_owned(),
    };
    client.get("GET /servers", &path)
}

/// `DELETE /servers/{id}` (async).
pub fn delete(client: &Client, id: u64) -> Result<Option<DeleteServerResponse>, ApiError> {
    client.delete("DELETE /servers/{id}", &format!("/servers/{id}"))
}

/// Minimal percent-encoding for label-selector segments.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b'=' | b',' => {
                (b as char).to_string()
            },
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_preserves_safe_chars() {
        assert_eq!(urlencode("tartarus.uuid=abc-123"), "tartarus.uuid=abc-123");
    }

    #[test]
    fn urlencode_escapes_spaces_and_braces() {
        let encoded = urlencode("x y{z}");
        assert!(encoded.contains("%20"), "space should be %20: {encoded}");
        assert!(encoded.contains("%7B") && encoded.contains("%7D"), "braces escaped");
    }

    #[test]
    fn server_deserialises_from_canonical_payload() {
        let body = r#"{
            "id": 42,
            "name": "tartarus-abcd1234",
            "status": "running",
            "public_net": {"ipv4": {"ip": "5.6.7.8"}},
            "labels": {"tartarus.uuid": "abcd1234"}
        }"#;
        let server: Server = serde_json::from_str(body).expect("server JSON should deserialise");
        assert_eq!(server.id, 42);
        assert_eq!(server.status, "running");
        assert_eq!(
            server.public_net.expect("public_net present").ipv4.expect("ipv4").ip,
            "5.6.7.8",
        );
        assert_eq!(server.labels.get("tartarus.uuid").map(String::as_str), Some("abcd1234"));
    }

    #[test]
    fn server_tolerates_missing_public_net() {
        let body = r#"{"id":1,"name":"x","status":"off","labels":{}}"#;
        let server: Server = serde_json::from_str(body).expect("server JSON should deserialise");
        assert!(
            server.public_net.is_none(),
            "public_net should default to None when absent"
        );
    }

    #[test]
    fn create_server_request_serialises_compact_payload() {
        let labels: BTreeMap<String, String> = BTreeMap::new();
        let payload = CreateServerRequest {
            name: "tartarus-abc",
            server_type: "cx21",
            image: "ubuntu-22.04",
            location: "fsn1",
            user_data: Some("#cloud-config\n"),
            ssh_keys: vec!["desktop"],
            start_after_create: true,
            labels,
            volumes: vec![],
        };
        let json = serde_json::to_string(&payload).expect("serialise payload");
        assert!(json.contains(r#""name":"tartarus-abc""#));
        assert!(json.contains(r#""server_type":"cx21""#));
        assert!(json.contains(r#""image":"ubuntu-22.04""#));
        assert!(json.contains(r#""start_after_create":true"#));
    }

    #[test]
    fn create_server_request_omits_user_data_when_none() {
        let payload = CreateServerRequest {
            name: "n",
            server_type: "cx21",
            image: "ubuntu-22.04",
            location: "fsn1",
            user_data: None,
            ssh_keys: vec![],
            start_after_create: false,
            labels: BTreeMap::new(),
            volumes: vec![],
        };
        let json = serde_json::to_string(&payload).expect("serialise");
        assert!(
            !json.contains("user_data"),
            "user_data: None should be skipped, got: {json}",
        );
    }

    #[test]
    fn create_server_response_envelope_round_trips() {
        let body = r#"{
            "server": {"id": 1, "name": "tartarus-x", "status": "initializing", "labels": {}},
            "action": {"id": 99, "status": "running"}
        }"#;
        let response: CreateServerResponse = serde_json::from_str(body).expect("envelope JSON");
        assert_eq!(response.server.id, 1);
        assert_eq!(response.action.id, 99);
        assert_eq!(response.action.status, "running");
    }

    #[test]
    fn delete_server_response_envelope_round_trips() {
        let body = r#"{"action":{"id":200,"status":"running"}}"#;
        let response: DeleteServerResponse = serde_json::from_str(body).expect("delete envelope");
        assert_eq!(response.action.id, 200);
    }
}
