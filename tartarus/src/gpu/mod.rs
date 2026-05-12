//! GPU passthrough subsystem: opt-in VFIO PCI passthrough for
//! sessions.
//!
//! Covers PCI address parsing, IOMMU/vfio probing, host pre-checks,
//! driver borrow/release lifecycle, vendor quirks (NVIDIA Code 43,
//! AMD reset bug), and udev rule generation.

pub mod driver;
pub mod error;
pub mod iommu;
pub mod pci;
pub mod pci_ids;
pub mod quirks;
pub mod setup;
pub mod vfio;

pub use error::GpuError;
pub use iommu::IommuGroup;
pub use pci::{PciAddress, PciDevice};

use crate::error::Result;

// -----------------------------------------------------------------------------
// HostPreCheck
// -----------------------------------------------------------------------------

/// Outcome of the host-side gate before a GPU passthrough session
/// starts. Each field is a binary check; the gate fails if any
/// required check is `false`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostPreCheck {
    /// True when the IOMMU is enabled at boot.
    pub iommu_enabled: bool,

    /// IOMMU group of the requested device, if specified.
    pub iommu_group: Option<IommuGroup>,

    /// True iff the device's IOMMU group is clean for passthrough.
    pub iommu_group_clean: Option<bool>,

    /// True when `vfio-pci` is loaded or built-in.
    pub vfio_pci_loaded: bool,
}

impl HostPreCheck {
    /// Run every host-side check. Pass `target` to include per-device
    /// IOMMU group inspection, or `None` to skip it.
    pub fn probe(target: Option<&PciAddress>) -> Result<Self> {
        let iommu_enabled = iommu::is_enabled()?;
        let vfio_pci_loaded = vfio::is_loaded()?;

        let (iommu_group, iommu_group_clean) = match target {
            Some(addr) => {
                let group = iommu::group_for(addr)?;
                let clean = group.is_clean_for_passthrough(addr);
                (Some(group), Some(clean))
            },
            None => (None, None),
        };

        Ok(Self {
            iommu_enabled,
            iommu_group,
            iommu_group_clean,
            vfio_pci_loaded,
        })
    }

    /// `Ok(())` when every required check passed,
    /// [`GpuError::PreCheckFailed`] otherwise.
    pub fn into_result(self) -> Result<()> {
        let mut failures: Vec<&'static str> = Vec::new();

        if !self.iommu_enabled {
            failures.push("iommu_enabled");
        }
        if !self.vfio_pci_loaded {
            failures.push("vfio_pci_loaded");
        }
        if matches!(self.iommu_group_clean, Some(false)) {
            failures.push("iommu_group_clean");
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(GpuError::PreCheckFailed {
                failed: failures.iter().map(|s| (*s).to_owned()).collect(),
            }
            .into())
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_result_ok_when_required_checks_pass() {
        let outcome = HostPreCheck {
            iommu_enabled: true,
            iommu_group: None,
            iommu_group_clean: None,
            vfio_pci_loaded: true,
        };

        outcome
            .into_result()
            .expect("required checks pass should produce Ok(())");
    }

    #[test]
    fn into_result_lists_each_failed_check_by_name() {
        let outcome = HostPreCheck {
            iommu_enabled: false,
            iommu_group: None,
            iommu_group_clean: None,
            vfio_pci_loaded: false,
        };

        let err = outcome.into_result().expect_err("missing iommu and vfio should fail");

        match err {
            crate::error::Error::Gpu(GpuError::PreCheckFailed { failed }) => {
                assert!(
                    failed.contains(&"iommu_enabled".to_owned()),
                    "iommu should be in failed: {failed:?}"
                );
                assert!(
                    failed.contains(&"vfio_pci_loaded".to_owned()),
                    "vfio should be in failed: {failed:?}"
                );
            },
            other => panic!("expected GpuError::PreCheckFailed, got {other:?}"),
        }
    }

    #[test]
    fn into_result_passes_when_target_specific_check_is_none() {
        let outcome = HostPreCheck {
            iommu_enabled: true,
            iommu_group: None,
            iommu_group_clean: None,
            vfio_pci_loaded: true,
        };

        outcome
            .into_result()
            .expect("None per-device fields should not contribute failures");
    }

    #[test]
    fn into_result_fails_when_iommu_group_dirty() {
        let outcome = HostPreCheck {
            iommu_enabled: true,
            iommu_group: None,
            iommu_group_clean: Some(false),
            vfio_pci_loaded: true,
        };

        let err = outcome
            .into_result()
            .expect_err("dirty iommu group should fail the gate");

        match err {
            crate::error::Error::Gpu(GpuError::PreCheckFailed { failed }) => {
                assert_eq!(
                    failed,
                    vec!["iommu_group_clean".to_owned()],
                    "only the dirty-group check should fail"
                );
            },
            other => panic!("expected GpuError::PreCheckFailed, got {other:?}"),
        }
    }
}
