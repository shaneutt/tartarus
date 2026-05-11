//! IOMMU detection helpers.

use std::{fs, path::Path};

use crate::{
    error::Result,
    gpu::{error::GpuError, pci::PciAddress},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Root of the kernel's IOMMU group hierarchy.
const IOMMU_GROUPS_ROOT: &str = "/sys/kernel/iommu_groups";

/// Kernel command-line file, where boot-time `*_iommu=on` flags appear.
const PROC_CMDLINE: &str = "/proc/cmdline";

// ---------------------------------------------------------------------------
// IOMMU Group Inspection
// ---------------------------------------------------------------------------

/// One IOMMU group's identifier and member devices.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IommuGroup {
    /// Numeric group id as the kernel reports it.
    pub id: u32,

    /// Every PCI device in this group, in `/sys`-listing order.
    /// Includes the device the caller asked about.
    pub members: Vec<PciAddress>,
}

impl IommuGroup {
    /// True iff the group contains only `target`.
    pub fn is_clean_for_passthrough(&self, target: &PciAddress) -> bool {
        self.members.len() == 1 && self.members[0] == *target
    }
}

/// Look up the IOMMU group of `addr` and enumerate its members.
pub fn group_for(addr: &PciAddress) -> Result<IommuGroup> {
    let device_path = addr.sysfs_path();
    if !device_path.exists() {
        return Err(GpuError::PciDeviceMissing {
            addr: addr.to_string(),
            path: device_path,
        }
        .into());
    }

    let group_link = device_path.join("iommu_group");
    let group_target = fs::read_link(&group_link).map_err(|source| GpuError::SysReadFailed {
        path: group_link.clone(),
        source,
    })?;

    let group_id = parse_group_id(&group_target).ok_or_else(|| GpuError::SysReadFailed {
        path: group_target.clone(),
        source: std::io::Error::other(format!("could not parse IOMMU group id from {group_target:?}")),
    })?;

    let resolved_group = device_path
        .join("iommu_group")
        .canonicalize()
        .map_err(|source| GpuError::SysReadFailed {
            path: group_link,
            source,
        })?;

    let members = enumerate_group_devices(&resolved_group)?;

    Ok(IommuGroup { id: group_id, members })
}

/// True iff the IOMMU is enabled (cmdline flag or populated
/// groups).
pub fn is_enabled() -> Result<bool> {
    if cmdline_carries_iommu_flag()? {
        return Ok(true);
    }

    let groups_root = Path::new(IOMMU_GROUPS_ROOT);
    if !groups_root.exists() {
        return Ok(false);
    }

    let entries = fs::read_dir(groups_root).map_err(|source| GpuError::SysReadFailed {
        path: groups_root.to_path_buf(),
        source,
    })?;

    Ok(entries.flatten().any(|e| e.path().is_dir()))
}

// ---------------------------------------------------------------------------
// Sysfs Enumeration
// ---------------------------------------------------------------------------

/// True iff `/proc/cmdline` contains an IOMMU enable token.
fn cmdline_carries_iommu_flag() -> Result<bool> {
    let cmdline_path = Path::new(PROC_CMDLINE);
    if !cmdline_path.exists() {
        return Ok(false);
    }

    let cmdline = fs::read_to_string(cmdline_path).map_err(|source| GpuError::SysReadFailed {
        path: cmdline_path.to_path_buf(),
        source,
    })?;

    Ok(cmdline_carries_iommu_token(&cmdline))
}

/// Pure: does `cmdline` contain `intel_iommu=on` or
/// `amd_iommu=on`?
fn cmdline_carries_iommu_token(cmdline: &str) -> bool {
    cmdline
        .split_ascii_whitespace()
        .any(|token| token.eq_ignore_ascii_case("intel_iommu=on") || token.eq_ignore_ascii_case("amd_iommu=on"))
}

/// Read every PCI device under a resolved IOMMU group directory.
fn enumerate_group_devices(group_dir: &Path) -> Result<Vec<PciAddress>> {
    let devices_dir = group_dir.join("devices");

    let entries = fs::read_dir(&devices_dir).map_err(|source| GpuError::SysReadFailed {
        path: devices_dir.clone(),
        source,
    })?;

    let mut addresses: Vec<PciAddress> = Vec::new();
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str()
            && let Ok(addr) = name.parse::<PciAddress>()
        {
            addresses.push(addr);
        }
    }

    addresses.sort_by_key(|a| (a.domain, a.bus, a.device, a.function));
    Ok(addresses)
}

/// Pull the trailing numeric segment off a `.../iommu_groups/<id>` path.
fn parse_group_id(path: &Path) -> Option<u32> {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|s| s.parse::<u32>().ok())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_token_detector_finds_intel_iommu_on() {
        assert!(cmdline_carries_iommu_token(
            "BOOT_IMAGE=/boot/vmlinuz intel_iommu=on quiet"
        ));
    }

    #[test]
    fn cmdline_token_detector_finds_amd_iommu_on() {
        assert!(cmdline_carries_iommu_token("amd_iommu=on iommu=pt"));
    }

    #[test]
    fn cmdline_token_detector_is_token_boundary_aware() {
        assert!(
            !cmdline_carries_iommu_token("not_amd_iommu=on"),
            "must not match a substring of an unrelated token",
        );
        assert!(
            !cmdline_carries_iommu_token("intel_iommu=off"),
            "off must not be treated as on",
        );
    }

    #[test]
    fn cmdline_token_detector_is_case_insensitive() {
        assert!(cmdline_carries_iommu_token("INTEL_IOMMU=on"));
        assert!(cmdline_carries_iommu_token("Amd_Iommu=ON"));
    }

    #[test]
    fn parse_group_id_handles_trailing_numeric_segment() {
        let path = Path::new("/sys/kernel/iommu_groups/17");
        assert_eq!(parse_group_id(path), Some(17), "trailing 17 should parse");
    }

    #[test]
    fn parse_group_id_rejects_non_numeric_tail() {
        let path = Path::new("/sys/kernel/iommu_groups/devices");
        assert_eq!(parse_group_id(path), None, "non-numeric tail should be None");
    }

    #[test]
    fn iommu_group_clean_when_only_target_member() {
        let target: PciAddress = "0000:01:00.0".parse().expect("parse");
        let group = IommuGroup {
            id: 17,
            members: vec![target.clone()],
        };
        assert!(group.is_clean_for_passthrough(&target));
    }

    #[test]
    fn iommu_group_dirty_when_other_members_present() {
        let target: PciAddress = "0000:01:00.0".parse().expect("parse");
        let other: PciAddress = "0000:01:00.1".parse().expect("parse");
        let group = IommuGroup {
            id: 17,
            members: vec![target.clone(), other],
        };
        assert!(
            !group.is_clean_for_passthrough(&target),
            "a group with non-target members must not be clean for passthrough",
        );
    }
}
