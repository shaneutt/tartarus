//! Vendor-specific GPU passthrough quirks: NVIDIA Code 43
//! workaround and AMD function-level-reset bug refusal.

use crate::gpu::{GpuError, PciDevice};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// AMD vendor id (`pci.ids` `1002`).
const AMD_VENDOR_ID: u16 = 0x1002;

/// NVIDIA vendor id (`pci.ids` `10de`).
const NVIDIA_VENDOR_ID: u16 = 0x10DE;

/// AMD GPU device IDs with known-broken FLR (from `vendor-reset`
/// device table).
const AMD_RESET_BROKEN_DEVICE_PREFIXES: &[u16] = &[
    // Polaris 10/11 (RX 470/480/570/580)
    0x67DF, 0x67EF, 0x67FF, // Vega 10 (RX Vega 56/64)
    0x687F, 0x6863, // Vega 20 (Radeon VII)
    0x66AF, // Navi 10 (RX 5700/5700 XT)
    0x731F, 0x7340,
];

// -----------------------------------------------------------------------------
// Vendor Quirk Evaluation
// -----------------------------------------------------------------------------

/// Quirk decisions for a borrowed GPU.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VendorQuirks {
    /// True iff the device needs the NVIDIA Code 43 workaround
    /// (hide KVM, spoof hyperv vendor_id, disable hypervisor CPUID).
    pub apply_nvidia_hide_kvm: bool,
}

/// Evaluate quirks for `device`. Returns
/// [`GpuError::AmdResetBugSuspected`] for known-bad AMD GPUs.
pub fn evaluate(device: &PciDevice) -> crate::error::Result<VendorQuirks> {
    if device.vendor_id == AMD_VENDOR_ID && AMD_RESET_BROKEN_DEVICE_PREFIXES.contains(&device.device_id) {
        return Err(GpuError::AmdResetBugSuspected {
            address: device.address.to_string(),
            device_id: device.device_id,
        }
        .into());
    }

    Ok(VendorQuirks {
        apply_nvidia_hide_kvm: device.vendor_id == NVIDIA_VENDOR_ID,
    })
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::PciAddress;

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    fn device_with_ids(vendor: u16, device: u16) -> PciDevice {
        PciDevice {
            address: "0000:01:00.0".parse::<PciAddress>().expect("parse"),
            class_code: 0x030000,
            device_id: device,
            device_name: None,
            vendor_id: vendor,
            vendor_name: None,
        }
    }

    #[test]
    fn evaluate_flags_nvidia_for_kvm_hide() {
        let device = device_with_ids(NVIDIA_VENDOR_ID, 0x2684);
        let quirks = evaluate(&device).expect("nvidia-without-flr-bug should not refuse");

        assert!(
            quirks.apply_nvidia_hide_kvm,
            "NVIDIA cards must be flagged for the Code 43 workaround",
        );
    }

    #[test]
    fn evaluate_does_not_flag_amd_for_kvm_hide() {
        let device = device_with_ids(AMD_VENDOR_ID, 0x73A5);
        let quirks = evaluate(&device).expect("non-broken AMD should pass");

        assert!(!quirks.apply_nvidia_hide_kvm, "AMD cards must not get the NVIDIA quirk",);
    }

    #[test]
    fn evaluate_does_not_flag_intel_for_kvm_hide() {
        let device = device_with_ids(0x8086, 0xA780);
        let quirks = evaluate(&device).expect("Intel iGPU should pass");

        assert!(!quirks.apply_nvidia_hide_kvm);
    }

    #[test]
    fn evaluate_refuses_known_bad_amd_polaris() {
        let device = device_with_ids(AMD_VENDOR_ID, 0x67DF);
        let err = evaluate(&device).expect_err("Polaris should be refused");

        assert!(
            matches!(
                err,
                crate::error::Error::Gpu(GpuError::AmdResetBugSuspected { device_id: 0x67DF, .. })
            ),
            "expected AmdResetBugSuspected for 0x67df, got {err:?}",
        );
    }

    #[test]
    fn evaluate_refuses_known_bad_amd_navi_10() {
        let device = device_with_ids(AMD_VENDOR_ID, 0x731F);
        assert!(
            evaluate(&device).is_err(),
            "Navi 10 should be refused without vendor-reset",
        );
    }

    #[test]
    fn evaluate_passes_unknown_amd_card() {
        let device = device_with_ids(AMD_VENDOR_ID, 0xDEAD);
        let quirks = evaluate(&device).expect("an unknown AMD device id should pass through");
        assert!(
            !quirks.apply_nvidia_hide_kvm,
            "AMD-but-unknown should not get the NVIDIA quirk",
        );
    }
}
