//! Hetzner-specific metadata: which server / volume backs a session.
//!
//! Carried as `hetzner_state: Option<HetznerState>` inside
//! [`tartarus_provider::session::metadata::Metadata`] (a forward
//! addition to the schema; libvirt-only sessions leave it `None`).
//! Today this module just holds the shape; the host-side
//! `metadata.json` field is added in a follow-up so libvirt /
//! Hetzner can coexist on disk.

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// HetznerState
// -----------------------------------------------------------------------------

/// Per-session Hetzner identifiers persisted into `metadata.json`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct HetznerState {
    /// Hetzner server ID (assigned at create time).
    pub server_id: u64,

    /// Public IPv4 address Hetzner picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ipv4: Option<String>,

    /// Volume ID when `[hetzner] volume_gib > 0`, else `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_id: Option<u64>,
}
