//! `pci.ids` lookup: vendor and device strings keyed by 16-bit IDs.
//!
//! Searches well-known distro paths for the PCI ID database. Degrades
//! to [`PciNames::default()`] when no database is installed.

use std::{
    fs,
    path::{Path, PathBuf},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Well-known paths for the PCI ID database, searched in order.
const PCI_IDS_SEARCH_PATHS: &[&str] = &[
    "/usr/share/hwdata/pci.ids",
    "/usr/share/misc/pci.ids",
    "/var/lib/pciutils/pci.ids",
    "/usr/share/pci.ids",
];

// ---------------------------------------------------------------------------
// PCI Name Lookup
// ---------------------------------------------------------------------------

/// Resolved human-readable names for a (vendor, device) pair.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PciNames {
    /// Device-name string, if found.
    pub device: Option<String>,

    /// Vendor-name string, if found.
    pub vendor: Option<String>,
}

impl PciNames {
    /// Render a human-friendly label, falling back to hex for
    /// missing fields.
    pub fn label(&self, vendor_id: u16, device_id: u16) -> String {
        match (self.vendor.as_deref(), self.device.as_deref()) {
            (Some(v), Some(d)) => format!("{v} {d}"),
            (Some(v), None) => format!("{v} [device 0x{device_id:04x}]"),
            (None, Some(d)) => format!("[vendor 0x{vendor_id:04x}] {d}"),
            (None, None) => format!("[vendor 0x{vendor_id:04x} device 0x{device_id:04x}]"),
        }
    }
}

/// Look up a (vendor, device) pair in the host's `pci.ids` file.
pub fn lookup(vendor_id: u16, device_id: u16) -> PciNames {
    let Some(path) = locate_database() else {
        return PciNames::default();
    };

    let Ok(contents) = fs::read_to_string(&path) else {
        return PciNames::default();
    };

    parse(&contents, vendor_id, device_id)
}

// ---------------------------------------------------------------------------
// Database Parsing
// ---------------------------------------------------------------------------

/// First readable database from [`PCI_IDS_SEARCH_PATHS`].
fn locate_database() -> Option<PathBuf> {
    PCI_IDS_SEARCH_PATHS
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .map(Path::to_path_buf)
}

/// Pure: scan the database text for a (vendor, device) match.
fn parse(contents: &str, vendor_id: u16, device_id: u16) -> PciNames {
    let mut result = PciNames::default();
    let mut in_target_vendor = false;

    for line in contents.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with("\t\t") {
            continue;
        }

        if let Some(rest) = line.strip_prefix('\t') {
            if !in_target_vendor {
                continue;
            }
            if let Some((id, name)) = split_id_and_name(rest)
                && id == device_id
            {
                result.device = Some(name.to_owned());
                if result.vendor.is_some() {
                    return result;
                }
            }
        } else {
            if result.vendor.is_some() {
                return result;
            }
            if let Some((id, name)) = split_id_and_name(line) {
                if id == vendor_id {
                    result.vendor = Some(name.to_owned());
                    in_target_vendor = true;
                } else {
                    in_target_vendor = false;
                }
            }
        }
    }

    result
}

/// Split a `pci.ids` row into `(hex_id, name)`.
fn split_id_and_name(row: &str) -> Option<(u16, &str)> {
    let (id_text, rest) = row.split_once(char::is_whitespace)?;
    if id_text.len() != 4 {
        return None;
    }
    let id = u16::from_str_radix(id_text, 16).ok()?;
    let name = rest.trim_start();
    if name.is_empty() {
        return None;
    }
    Some((id, name))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    const SAMPLE_DB: &str = "\
# header comment
0001  SafeNet (wrong ID)
10de  NVIDIA Corporation
\t1234  Some Old Card
\t2684  AD102 [GeForce RTX 4090]
\t\t1234 5678  Subsystem Name We Should Skip
\t2685  AD102 [other variant]
1002  Advanced Micro Devices, Inc. [AMD/ATI]
\t73a5  Navi 21 [Radeon RX 6950 XT]
8086  Intel Corporation
\ta780  Raptor Lake-S UHD Graphics
";

    #[test]
    fn parse_returns_vendor_and_device_when_both_present() {
        let names = parse(SAMPLE_DB, 0x10DE, 0x2684);
        assert_eq!(names.vendor.as_deref(), Some("NVIDIA Corporation"));
        assert_eq!(names.device.as_deref(), Some("AD102 [GeForce RTX 4090]"));
    }

    #[test]
    fn parse_returns_vendor_only_when_device_missing() {
        let names = parse(SAMPLE_DB, 0x10DE, 0xFFFF);
        assert_eq!(names.vendor.as_deref(), Some("NVIDIA Corporation"));
        assert!(names.device.is_none(), "missing device should be None");
    }

    #[test]
    fn parse_returns_default_when_vendor_missing() {
        let names = parse(SAMPLE_DB, 0xDEAD, 0xBEEF);
        assert!(names.vendor.is_none(), "missing vendor should be None");
        assert!(names.device.is_none(), "missing vendor implies missing device");
    }

    #[test]
    fn parse_does_not_cross_vendor_blocks_for_devices() {
        let names = parse(SAMPLE_DB, 0x10DE, 0x73A5);
        assert_eq!(
            names.vendor.as_deref(),
            Some("NVIDIA Corporation"),
            "vendor block must match the requested id, not bleed into the next vendor",
        );
        assert!(names.device.is_none(), "device id 0x73a5 lives under AMD, not NVIDIA",);
    }

    #[test]
    fn parse_skips_subsystem_rows() {
        let names = parse(SAMPLE_DB, 0x10DE, 0x2684);
        assert!(
            !names.device.as_deref().unwrap_or("").contains("Subsystem"),
            "double-tab subsystem rows must not pollute device names",
        );
    }

    #[test]
    fn parse_handles_amd_block_after_nvidia() {
        let names = parse(SAMPLE_DB, 0x1002, 0x73A5);
        assert_eq!(names.vendor.as_deref(), Some("Advanced Micro Devices, Inc. [AMD/ATI]"),);
        assert_eq!(names.device.as_deref(), Some("Navi 21 [Radeon RX 6950 XT]"));
    }

    #[test]
    fn parse_handles_intel_block_at_eof() {
        let names = parse(SAMPLE_DB, 0x8086, 0xA780);
        assert_eq!(names.vendor.as_deref(), Some("Intel Corporation"));
        assert_eq!(names.device.as_deref(), Some("Raptor Lake-S UHD Graphics"));
    }

    #[test]
    fn label_combines_vendor_and_device_when_both_known() {
        let names = PciNames {
            vendor: Some("NVIDIA Corporation".to_owned()),
            device: Some("GeForce RTX 4090".to_owned()),
        };
        assert_eq!(names.label(0x10DE, 0x2684), "NVIDIA Corporation GeForce RTX 4090");
    }

    #[test]
    fn label_falls_back_to_hex_when_device_unknown() {
        let names = PciNames {
            vendor: Some("NVIDIA Corporation".to_owned()),
            device: None,
        };
        assert_eq!(names.label(0x10DE, 0x2684), "NVIDIA Corporation [device 0x2684]");
    }

    #[test]
    fn label_falls_back_to_hex_for_both_when_unknown() {
        let names = PciNames::default();
        assert_eq!(names.label(0xDEAD, 0xBEEF), "[vendor 0xdead device 0xbeef]");
    }

    #[test]
    fn split_id_and_name_rejects_non_four_hex_prefix() {
        assert!(split_id_and_name("123  Name").is_none(), "3-digit prefix rejected");
        assert!(split_id_and_name("12345  Name").is_none(), "5-digit prefix rejected");
        assert!(split_id_and_name("zzzz  Name").is_none(), "non-hex rejected");
    }

    #[test]
    fn split_id_and_name_requires_a_name() {
        assert!(split_id_and_name("10de   ").is_none(), "trailing whitespace, no name");
        assert!(split_id_and_name("10de").is_none(), "no separator at all");
    }
}
