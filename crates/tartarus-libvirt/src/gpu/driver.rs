//! Vendor-driver detach + `vfio-pci` rebind, and the inverse.
//!
//! All `/sys` writes go through [`SysfsIo`] (real kernel or
//! test recorder).

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::{
    error::Result,
    gpu::{error::GpuError, pci::PciAddress},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Kernel-wide driver re-probe path.
const DRIVERS_PROBE_PATH: &str = "/sys/bus/pci/drivers_probe";

/// `vfio-pci` driver name for `driver_override`.
const VFIO_PCI_DRIVER: &str = "vfio-pci";

// -----------------------------------------------------------------------------
// Driver Borrow and Release
// -----------------------------------------------------------------------------

/// Record of a driver borrow, used to restore the original state
/// on release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    /// PCI address of the borrowed device.
    pub address: PciAddress,

    /// Driver bound before the borrow, or `None` if unbound.
    pub previous_driver: Option<String>,
}

/// Capture a [`Receipt`] into its persistable
/// [`tartarus_provider::session::metadata::GpuBorrowRecord`] form.
pub fn record_from_receipt(receipt: &Receipt) -> tartarus_provider::session::metadata::GpuBorrowRecord {
    tartarus_provider::session::metadata::GpuBorrowRecord {
        address: receipt.address.to_string(),
        previous_driver: receipt.previous_driver.clone(),
    }
}

/// Reconstruct a typed [`Receipt`] from its persisted record. Fails
/// when the persisted PCI address no longer parses.
pub fn record_into_receipt(record: tartarus_provider::session::metadata::GpuBorrowRecord) -> Result<Receipt> {
    let address = record.address.parse()?;
    Ok(Receipt {
        address,
        previous_driver: record.previous_driver,
    })
}

/// Trait for `/sys` writes (real kernel or test recorder).
pub trait SysfsIo {
    /// Current driver name, or `None` if unbound.
    fn current_driver(&self, address: &PciAddress) -> Result<Option<String>>;

    /// Set `driver_override`. Pass `""` to clear.
    fn set_driver_override(&self, address: &PciAddress, value: &str) -> Result<()>;

    /// Trigger a driver re-probe for `address`.
    fn trigger_probe(&self, address: &PciAddress) -> Result<()>;

    /// Write `address` to `/sys/bus/pci/drivers/<driver>/unbind`.
    fn unbind_from(&self, address: &PciAddress, driver: &str) -> Result<()>;
}

/// Detach the vendor driver and bind `vfio-pci`. Returns a
/// [`Receipt`] for later [`release_with_receipt`].
pub fn borrow<S: SysfsIo>(io: &S, address: &PciAddress) -> Result<Receipt> {
    let previous_driver = io.current_driver(address)?;

    io.set_driver_override(address, VFIO_PCI_DRIVER)?;

    if let Some(driver) = previous_driver.as_deref() {
        io.unbind_from(address, driver)?;
    }

    io.trigger_probe(address)?;

    Ok(Receipt {
        address: address.clone(),
        previous_driver,
    })
}

/// Reverse a [`borrow`]: unbind `vfio-pci` and re-probe for the
/// original driver. Idempotent.
pub fn release_with_receipt<S: SysfsIo>(io: &S, receipt: &Receipt) -> Result<()> {
    io.set_driver_override(&receipt.address, "")?;

    let now_driver = io.current_driver(&receipt.address)?;
    if let Some(driver) = now_driver.as_deref()
        && driver == VFIO_PCI_DRIVER
    {
        io.unbind_from(&receipt.address, VFIO_PCI_DRIVER)?;
    }

    io.trigger_probe(&receipt.address)?;
    Ok(())
}

/// Real [`SysfsIo`] that writes to the kernel's `/sys`.
#[derive(Debug, Default)]
pub struct KernelSysfs;

impl SysfsIo for KernelSysfs {
    fn current_driver(&self, address: &PciAddress) -> Result<Option<String>> {
        let driver_link = address.sysfs_path().join("driver");
        match fs::read_link(&driver_link) {
            Ok(target) => Ok(target.file_name().and_then(|n| n.to_str()).map(str::to_owned)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(GpuError::SysReadFailed {
                path: driver_link,
                source,
            }
            .into()),
        }
    }

    fn set_driver_override(&self, address: &PciAddress, value: &str) -> Result<()> {
        let path = address.sysfs_path().join("driver_override");
        write_sysfs(&path, value)
    }

    fn trigger_probe(&self, address: &PciAddress) -> Result<()> {
        write_sysfs(Path::new(DRIVERS_PROBE_PATH), &address.to_string())
    }

    fn unbind_from(&self, address: &PciAddress, driver: &str) -> Result<()> {
        let path = pci_drivers_root().join(driver).join("unbind");
        write_sysfs(&path, &address.to_string())
    }
}

/// Test-only [`SysfsIo`] that records calls and serves canned
/// reads.
#[derive(Debug, Default)]
pub struct RecordingSysfs {
    inner: Mutex<RecordingSysfsState>,
}

#[derive(Clone, Debug, Default)]
struct RecordingSysfsState {
    canned_driver: Option<String>,
    overrides: Vec<(PciAddress, String)>,
    probes: Vec<PciAddress>,
    unbinds: Vec<(PciAddress, String)>,
}

impl RecordingSysfs {
    /// Set the canned driver return value.
    pub fn set_canned_driver(&self, driver: Option<&str>) {
        self.inner.lock().expect("RecordingSysfs poisoned").canned_driver = driver.map(str::to_owned);
    }

    /// Recorded `set_driver_override` calls.
    pub fn overrides(&self) -> Vec<(PciAddress, String)> {
        self.inner.lock().expect("RecordingSysfs poisoned").overrides.clone()
    }

    /// Recorded `trigger_probe` calls.
    pub fn probes(&self) -> Vec<PciAddress> {
        self.inner.lock().expect("RecordingSysfs poisoned").probes.clone()
    }

    /// Recorded `unbind_from` calls.
    pub fn unbinds(&self) -> Vec<(PciAddress, String)> {
        self.inner.lock().expect("RecordingSysfs poisoned").unbinds.clone()
    }
}

impl SysfsIo for RecordingSysfs {
    fn current_driver(&self, _address: &PciAddress) -> Result<Option<String>> {
        Ok(self
            .inner
            .lock()
            .expect("RecordingSysfs poisoned")
            .canned_driver
            .clone())
    }

    fn set_driver_override(&self, address: &PciAddress, value: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("RecordingSysfs poisoned")
            .overrides
            .push((address.clone(), value.to_owned()));
        Ok(())
    }

    fn trigger_probe(&self, address: &PciAddress) -> Result<()> {
        self.inner
            .lock()
            .expect("RecordingSysfs poisoned")
            .probes
            .push(address.clone());
        Ok(())
    }

    fn unbind_from(&self, address: &PciAddress, driver: &str) -> Result<()> {
        self.inner
            .lock()
            .expect("RecordingSysfs poisoned")
            .unbinds
            .push((address.clone(), driver.to_owned()));
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Sysfs I/O
// -----------------------------------------------------------------------------

/// Root of the kernel's per-driver PCI directory.
fn pci_drivers_root() -> PathBuf {
    PathBuf::from("/sys/bus/pci/drivers")
}

/// Write `value` to a sysfs `path`.
fn write_sysfs(path: &Path, value: &str) -> Result<()> {
    fs::write(path, value).map_err(|source| {
        GpuError::SysReadFailed {
            path: path.to_path_buf(),
            source,
        }
        .into()
    })
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrow_records_previous_driver_then_overrides_and_unbinds_and_probes() {
        let io = RecordingSysfs::default();
        io.set_canned_driver(Some("nvidia"));
        let address: PciAddress = "0000:01:00.0".parse().expect("parse");

        let receipt = borrow(&io, &address).expect("borrow should succeed");

        assert_eq!(receipt.previous_driver.as_deref(), Some("nvidia"));
        assert_eq!(io.overrides(), vec![(address.clone(), VFIO_PCI_DRIVER.to_owned())]);
        assert_eq!(io.unbinds(), vec![(address.clone(), "nvidia".to_owned())]);
        assert_eq!(io.probes(), vec![address]);
    }

    #[test]
    fn borrow_skips_unbind_when_device_is_already_unbound() {
        let io = RecordingSysfs::default();
        io.set_canned_driver(None);
        let address: PciAddress = "0000:01:00.0".parse().expect("parse");

        let receipt = borrow(&io, &address).expect("borrow should succeed");

        assert!(
            receipt.previous_driver.is_none(),
            "an unbound device must produce a None receipt",
        );
        assert!(
            io.unbinds().is_empty(),
            "no unbind should be issued when no vendor driver was bound, got {:?}",
            io.unbinds(),
        );
        assert_eq!(io.probes(), vec![address], "probe still needed to bind vfio-pci");
    }

    #[test]
    fn release_clears_override_and_probes() {
        let io = RecordingSysfs::default();
        io.set_canned_driver(Some(VFIO_PCI_DRIVER));
        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        let receipt = Receipt {
            address: address.clone(),
            previous_driver: Some("nvidia".to_owned()),
        };

        release_with_receipt(&io, &receipt).expect("release should succeed");

        assert_eq!(
            io.overrides(),
            vec![(address.clone(), String::new())],
            "release must clear driver_override",
        );
        assert_eq!(
            io.unbinds(),
            vec![(address.clone(), VFIO_PCI_DRIVER.to_owned())],
            "release must unbind from vfio-pci so the vendor driver can take over",
        );
        assert_eq!(io.probes(), vec![address]);
    }

    #[test]
    fn release_skips_unbind_when_device_is_not_currently_on_vfio_pci() {
        let io = RecordingSysfs::default();
        io.set_canned_driver(Some("nvidia"));
        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        let receipt = Receipt {
            address: address.clone(),
            previous_driver: Some("nvidia".to_owned()),
        };

        release_with_receipt(&io, &receipt).expect("release should succeed");

        assert!(
            io.unbinds().is_empty(),
            "unbind must not run when the device is no longer on vfio-pci, got {:?}",
            io.unbinds(),
        );
        assert_eq!(io.probes(), vec![address]);
    }
}
