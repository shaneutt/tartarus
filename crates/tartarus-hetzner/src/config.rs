//! Hetzner-specific configuration consumed by [`HetznerProvider`].
//!
//! The binary's TOML config gains a `[hetzner]` section the binary
//! parses; this crate is handed the resulting [`HetznerConfig`].

use serde::{Deserialize, Serialize};

// -----------------------------------------------------------------------------
// HetznerConfig
// -----------------------------------------------------------------------------

/// Resolved Hetzner provider settings.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct HetznerConfig {
    /// Hetzner Cloud API bearer token.
    pub api_token: String,

    /// Server image slug (e.g. `ubuntu-22.04`, `fedora-39`).
    pub image: String,

    /// Hetzner location code (e.g. `nbg1`, `fsn1`, `hel1`).
    pub location: String,

    /// Server type slug (e.g. `cx11`, `cx21`, `cpx31`).
    pub server_type: String,

    /// Existing SSH key name in the Hetzner project, used as a
    /// fallback access path alongside cloud-init's `authorized_keys`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_name: Option<String>,

    /// Per-session `/home` volume size in GiB.
    ///
    /// `0` means do not attach a volume.
    #[serde(default)]
    pub volume_gib: u32,
}

impl HetznerConfig {
    /// Whether this config attaches a persistent `/home` volume.
    pub fn attaches_volume(&self) -> bool {
        self.volume_gib > 0
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaches_volume_is_false_when_size_is_zero() {
        assert!(!sample(0).attaches_volume(), "volume_gib = 0 should disable the volume");
    }

    #[test]
    fn attaches_volume_is_true_for_any_positive_size() {
        assert!(sample(10).attaches_volume(), "volume_gib > 0 should enable the volume");
        assert!(
            sample(500).attaches_volume(),
            "large volume_gib should still enable the volume"
        );
    }

    #[test]
    fn config_round_trips_through_serde() {
        let config = sample(64);
        let toml_value = serde_json::to_string(&config).expect("serialize");
        let reparsed: HetznerConfig = serde_json::from_str(&toml_value).expect("deserialize");
        assert_eq!(config, reparsed);
    }

    fn sample(volume_gib: u32) -> HetznerConfig {
        HetznerConfig {
            api_token: "tok".to_owned(),
            image: "ubuntu-22.04".to_owned(),
            location: "fsn1".to_owned(),
            server_type: "cx21".to_owned(),
            ssh_key_name: Some("desktop".to_owned()),
            volume_gib,
        }
    }
}
