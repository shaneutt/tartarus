//! GPU passthrough error variants.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// GpuError
// ---------------------------------------------------------------------------

/// Failure modes specific to the GPU passthrough path.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// AMD GPU with a known-broken FLR; requires the `vendor-reset`
    /// kernel module.
    #[error(
        "AMD GPU at {address} (device id 0x{device_id:04x}) has a known-broken FLR implementation. \
         hint: install the `vendor-reset` kernel module \
         (https://github.com/gnif/vendor-reset) before retrying, or pass a different `--gpu` target."
    )]
    AmdResetBugSuspected {
        /// PCI address of the refused device.
        address: String,

        /// PCI device ID that matched the refused list.
        device_id: u16,
    },

    /// `--gpu auto` found no GPU with a clean IOMMU group.
    #[error(
        "`--gpu auto` could not find a GPU with a clean IOMMU group. \
         hint: run `tartarus host gpu list` to see each device's group, \
         then pass an explicit `--gpu DDDD:BB:DD.F`."
    )]
    NoCleanGpuFound,

    /// PCI address could not be parsed as `DDDD:BB:DD.F`.
    #[error("PCI address {input:?} is not in the canonical DDDD:BB:DD.F form")]
    InvalidPciAddress {
        /// String the user supplied.
        input: String,
    },

    /// Device does not exist under `/sys/bus/pci/devices/`.
    #[error("PCI device {addr} does not exist (is the BDF correct, and is the device installed?)")]
    PciDeviceMissing {
        /// Address that was probed.
        addr: String,

        /// `/sys` path that did not exist.
        path: PathBuf,
    },

    /// One or more host pre-check gates returned `false`.
    #[error(
        "GPU host pre-check failed: {failed:?}. \
         hint: run `tartarus host gpu status` for a detailed report; \
         `tartarus host setup-gpu` (M2 Phase 3) installs the udev rules and detaches host drivers."
    )]
    PreCheckFailed {
        /// Names of the checks that returned `false`.
        failed: Vec<String>,
    },

    /// A `/sys` read or write failed.
    #[error("could not read {path}: {source}. hint: GPU passthrough requires a mounted, readable /sys.")]
    SysReadFailed {
        /// Path that could not be read.
        path: PathBuf,

        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
