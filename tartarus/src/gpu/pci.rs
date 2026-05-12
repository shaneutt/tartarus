//! PCI device addressing (`DDDD:BB:DD.F`).

use std::{
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::{
    error::Result,
    gpu::{error::GpuError, pci_ids},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// PCI base-class code for display controllers (all GPU types).
pub const CLASS_DISPLAY_CONTROLLER: u8 = 0x03;

/// Root of the kernel's PCI device tree.
const PCI_DEVICES_ROOT: &str = "/sys/bus/pci/devices";

// -----------------------------------------------------------------------------
// PCI Addressing
// -----------------------------------------------------------------------------

/// A parsed PCI address.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PciAddress {
    /// Bus number (`BB`), 0..=255.
    pub bus: u8,

    /// Device number (`DD`), 0..=31.
    pub device: u8,

    /// PCI segment / domain (`DDDD`), 0..=65535.
    pub domain: u16,

    /// Function number (`F`), 0..=7.
    pub function: u8,
}

impl PciAddress {
    /// Build a [`PciAddress`], rejecting out-of-range values.
    pub fn new(domain: u16, bus: u8, device: u8, function: u8) -> Result<Self> {
        if device > 31 {
            return Err(GpuError::InvalidPciAddress {
                input: format!("{domain:04x}:{bus:02x}:{device:02x}.{function}"),
            }
            .into());
        }
        if function > 7 {
            return Err(GpuError::InvalidPciAddress {
                input: format!("{domain:04x}:{bus:02x}:{device:02x}.{function}"),
            }
            .into());
        }

        Ok(Self {
            bus,
            device,
            domain,
            function,
        })
    }

    /// Path under `/sys/bus/pci/devices/` (not guaranteed to exist).
    pub fn sysfs_path(&self) -> PathBuf {
        Path::new(PCI_DEVICES_ROOT).join(self.to_string())
    }
}

impl fmt::Display for PciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{:01x}",
            self.domain, self.bus, self.device, self.function,
        )
    }
}

impl FromStr for PciAddress {
    type Err = crate::error::Error;

    /// Parse `DDDD:BB:DD.F` or bare `BB:DD.F` (domain defaults to
    /// `0000`).
    fn from_str(s: &str) -> Result<Self> {
        let invalid = || GpuError::InvalidPciAddress { input: s.to_owned() };

        let (domain_part, rest) = match s.split_once(':') {
            Some((head, _)) if head.len() == 4 => {
                let (d, r) = s.split_once(':').ok_or_else(invalid)?;
                (d, r)
            },
            Some(_) => ("0000", s),
            None => return Err(invalid().into()),
        };

        let (bus_part, devfunc_part) = rest.split_once(':').ok_or_else(invalid)?;
        let (device_part, function_part) = devfunc_part.split_once('.').ok_or_else(invalid)?;

        let domain = u16::from_str_radix(domain_part, 16).map_err(|_| invalid())?;
        let bus = u8::from_str_radix(bus_part, 16).map_err(|_| invalid())?;
        let device = u8::from_str_radix(device_part, 16).map_err(|_| invalid())?;
        let function = u8::from_str_radix(function_part, 16).map_err(|_| invalid())?;

        Self::new(domain, bus, device, function)
    }
}

/// A PCI device: address, vendor/device IDs, class code, and
/// human-readable names from `pci.ids`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciDevice {
    /// PCI address (`DDDD:BB:DD.F`).
    pub address: PciAddress,

    /// 24-bit class code (base class, subclass, prog-if).
    pub class_code: u32,

    /// Device id (`/sys/.../device`).
    pub device_id: u16,

    /// Resolved device-name string, if available.
    pub device_name: Option<String>,

    /// Vendor id (`/sys/.../vendor`).
    pub vendor_id: u16,

    /// Resolved vendor-name string, if available.
    pub vendor_name: Option<String>,
}

impl PciDevice {
    /// Build a [`PciDevice`] from sysfs at `address`.
    pub fn at(address: PciAddress) -> Result<Self> {
        let device_path = address.sysfs_path();
        if !device_path.exists() {
            return Err(GpuError::PciDeviceMissing {
                addr: address.to_string(),
                path: device_path,
            }
            .into());
        }

        let vendor_id = read_hex_u16(&device_path.join("vendor"))?;
        let device_id = read_hex_u16(&device_path.join("device"))?;
        let class_code = read_hex_u32(&device_path.join("class"))?;

        let names = pci_ids::lookup(vendor_id, device_id);

        Ok(Self {
            address,
            class_code,
            device_id,
            device_name: names.device,
            vendor_id,
            vendor_name: names.vendor,
        })
    }

    /// Top 8 bits of [`Self::class_code`]: the PCI base class.
    pub fn base_class(&self) -> u8 {
        ((self.class_code >> 16) & 0xFF) as u8
    }

    /// True iff this is a display-class device (GPU).
    pub fn is_display_controller(&self) -> bool {
        self.base_class() == CLASS_DISPLAY_CONTROLLER
    }

    /// One-line human-readable label.
    pub fn label(&self) -> String {
        let names = pci_ids::PciNames {
            device: self.device_name.clone(),
            vendor: self.vendor_name.clone(),
        };
        names.label(self.vendor_id, self.device_id)
    }
}

/// Enumerate every PCI device matching `predicate`. Unparseable
/// sysfs entries are silently skipped.
pub fn list_devices<F>(predicate: F) -> Result<Vec<PciDevice>>
where
    F: Fn(&PciDevice) -> bool,
{
    let root = Path::new(PCI_DEVICES_ROOT);
    if !root.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(root).map_err(|source| GpuError::SysReadFailed {
        path: root.to_path_buf(),
        source,
    })?;

    let mut devices: Vec<PciDevice> = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(address) = name.parse::<PciAddress>() else {
            continue;
        };
        if let Ok(device) = PciDevice::at(address)
            && predicate(&device)
        {
            devices.push(device);
        }
    }

    devices.sort_by_key(|d| (d.address.domain, d.address.bus, d.address.device, d.address.function));
    Ok(devices)
}

/// Enumerate every display-class PCI device (GPU) on the host.
pub fn list_gpus() -> Result<Vec<PciDevice>> {
    list_devices(PciDevice::is_display_controller)
}

/// Pick the first GPU whose IOMMU group is clean for passthrough.
/// Returns `None` when no suitable GPU exists.
pub fn pick_auto_gpu() -> Result<Option<PciDevice>> {
    for gpu in list_gpus()? {
        let group = match crate::gpu::iommu::group_for(&gpu.address) {
            Ok(g) => g,
            Err(_) => continue,
        };
        if group.is_clean_for_passthrough(&gpu.address) {
            return Ok(Some(gpu));
        }
    }
    Ok(None)
}

// -----------------------------------------------------------------------------
// Sysfs Hex Parsing
// -----------------------------------------------------------------------------

/// Read a `/sys` file whose contents are `0xHHHH\n` and parse the
/// trailing hex.
fn read_hex_u16(path: &Path) -> Result<u16> {
    let raw = fs::read_to_string(path).map_err(|source| GpuError::SysReadFailed {
        path: path.to_path_buf(),
        source,
    })?;
    parse_hex_prefixed(&raw).ok_or_else(|| {
        GpuError::SysReadFailed {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("expected `0xHHHH` in {path:?}, got {raw:?}")),
        }
        .into()
    })
}

/// Read a `/sys` file whose contents are `0xHHHHHH\n` and parse the
/// trailing hex.
fn read_hex_u32(path: &Path) -> Result<u32> {
    let raw = fs::read_to_string(path).map_err(|source| GpuError::SysReadFailed {
        path: path.to_path_buf(),
        source,
    })?;
    parse_hex_prefixed_wide(&raw).ok_or_else(|| {
        GpuError::SysReadFailed {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("expected `0xHHHHHH` in {path:?}, got {raw:?}")),
        }
        .into()
    })
}

/// Pure: parse `"0xHHHH\n"` (trimmed) into a `u16`.
fn parse_hex_prefixed(raw: &str) -> Option<u16> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X"))?;
    u16::from_str_radix(hex, 16).ok()
}

/// Pure: parse `"0xHHHHHH\n"` (trimmed) into a `u32`.
fn parse_hex_prefixed_wide(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_canonical_form_round_trips() {
        let addr: PciAddress = "0000:01:00.0".parse().expect("canonical form should parse");
        assert_eq!(addr.domain, 0);
        assert_eq!(addr.bus, 1);
        assert_eq!(addr.device, 0);
        assert_eq!(addr.function, 0);
        assert_eq!(
            addr.to_string(),
            "0000:01:00.0",
            "Display should round-trip the canonical form"
        );
    }

    #[test]
    fn parse_short_form_treats_missing_domain_as_zero() {
        let addr: PciAddress = "01:00.0".parse().expect("short form should parse");
        assert_eq!(addr.domain, 0, "missing domain should default to 0");
        assert_eq!(addr.bus, 1);
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!("not-a-pci-addr".parse::<PciAddress>().is_err());
        assert!(
            "0000:01:00".parse::<PciAddress>().is_err(),
            "missing function separator should fail"
        );
        assert!(
            "0000:01:zz.0".parse::<PciAddress>().is_err(),
            "non-hex device should fail"
        );
    }

    #[test]
    fn new_rejects_out_of_range_device_or_function() {
        assert!(PciAddress::new(0, 0, 32, 0).is_err(), "device 32 should be rejected");
        assert!(PciAddress::new(0, 0, 0, 8).is_err(), "function 8 should be rejected");
    }

    #[test]
    fn sysfs_path_lives_under_pci_devices_root() {
        let addr: PciAddress = "0000:01:00.0".parse().expect("parse");
        let path = addr.sysfs_path();
        assert!(
            path.starts_with(PCI_DEVICES_ROOT),
            "sysfs_path should be rooted at {PCI_DEVICES_ROOT}, got {path:?}",
        );
        assert!(
            path.ends_with("0000:01:00.0"),
            "sysfs_path should end with the canonical address, got {path:?}",
        );
    }
}
