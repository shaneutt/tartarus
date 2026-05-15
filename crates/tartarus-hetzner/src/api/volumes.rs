//! Hetzner Cloud `/volumes` endpoint.

use serde::{Deserialize, Serialize};

use crate::api::{Client, error::ApiError, servers::Action};

// -----------------------------------------------------------------------------
// Volume
// -----------------------------------------------------------------------------

/// One volume resource as Hetzner returns it.
#[derive(Clone, Debug, Deserialize)]
pub struct Volume {
    /// Numeric volume ID.
    pub id: u64,

    /// Display name.
    pub name: String,

    /// Size in GiB.
    pub size: u32,

    /// Currently attached server ID, or `None` when detached.
    #[serde(default)]
    pub server: Option<u64>,
}

/// Payload for `POST /volumes`.
#[derive(Clone, Debug, Serialize)]
pub struct CreateVolumeRequest<'a> {
    /// Volume display name.
    pub name: &'a str,

    /// Size in GiB.
    pub size: u32,

    /// Location code (e.g. `nbg1`).
    pub location: &'a str,

    /// Optional ext4/xfs format request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<&'a str>,
}

/// Response envelope for `POST /volumes`.
#[derive(Debug, Deserialize)]
pub struct CreateVolumeResponse {
    /// The created volume.
    pub volume: Volume,

    /// Async creation action (especially when `format` is set).
    pub action: Action,
}

/// Payload for `POST /volumes/{id}/actions/attach`.
#[derive(Clone, Debug, Serialize)]
pub struct AttachVolumeRequest {
    /// Server ID to attach to.
    pub server: u64,

    /// Whether Hetzner should automount via fstab. We bring our own
    /// mount unit from cloud-init, so this defaults to false.
    pub automount: bool,
}

/// Response envelope for `POST /volumes/{id}/actions/attach`.
#[derive(Debug, Deserialize)]
pub struct AttachVolumeResponse {
    /// Async attach action.
    pub action: Action,
}

// -----------------------------------------------------------------------------
// Volume Operations
// -----------------------------------------------------------------------------

/// `POST /volumes`.
pub fn create(client: &Client, request: &CreateVolumeRequest<'_>) -> Result<CreateVolumeResponse, ApiError> {
    client.post("POST /volumes", "/volumes", request)
}

/// `POST /volumes/{id}/actions/attach`.
pub fn attach(client: &Client, id: u64, request: &AttachVolumeRequest) -> Result<AttachVolumeResponse, ApiError> {
    client.post(
        "POST /volumes/{id}/actions/attach",
        &format!("/volumes/{id}/actions/attach"),
        request,
    )
}

/// `DELETE /volumes/{id}` (Hetzner returns 204).
pub fn delete(client: &Client, id: u64) -> Result<(), ApiError> {
    let _: Option<serde_json::Value> = client.delete("DELETE /volumes/{id}", &format!("/volumes/{id}"))?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_deserialises_attached_payload() {
        let body = r#"{"id":11,"name":"tartarus-home-abcd1234","size":64,"server":42}"#;
        let volume: Volume = serde_json::from_str(body).expect("attached volume");
        assert_eq!(volume.id, 11);
        assert_eq!(volume.size, 64);
        assert_eq!(volume.server, Some(42));
    }

    #[test]
    fn volume_deserialises_detached_payload() {
        let body = r#"{"id":12,"name":"orphan","size":32}"#;
        let volume: Volume = serde_json::from_str(body).expect("detached volume");
        assert!(volume.server.is_none(), "server should default to None when absent");
    }

    #[test]
    fn create_volume_request_serialises_with_format() {
        let request = CreateVolumeRequest {
            name: "tartarus-home-abc",
            size: 64,
            location: "fsn1",
            format: Some("ext4"),
        };
        let json = serde_json::to_string(&request).expect("serialise");
        assert!(json.contains(r#""format":"ext4""#));
    }

    #[test]
    fn create_volume_request_skips_format_when_absent() {
        let request = CreateVolumeRequest {
            name: "raw",
            size: 32,
            location: "fsn1",
            format: None,
        };
        let json = serde_json::to_string(&request).expect("serialise");
        assert!(!json.contains("format"), "format: None should be skipped, got: {json}");
    }

    #[test]
    fn attach_volume_request_carries_automount_flag() {
        let request = AttachVolumeRequest {
            server: 7,
            automount: false,
        };
        let json = serde_json::to_string(&request).expect("serialise");
        assert!(json.contains(r#""server":7"#));
        assert!(json.contains(r#""automount":false"#));
    }
}
