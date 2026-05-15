//! `vfio-pci` kernel-module probing.

use std::path::Path;

use crate::Result;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Sysfs path indicating `vfio_pci` is loaded.
const VFIO_PCI_SYS_PATH: &str = "/sys/module/vfio_pci";

// -----------------------------------------------------------------------------
// VFIO Module Probing
// -----------------------------------------------------------------------------

/// True when `vfio_pci` is loaded or built into the kernel.
pub fn is_loaded() -> Result<bool> {
    Ok(Path::new(VFIO_PCI_SYS_PATH).exists())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_loaded_does_not_panic_on_any_host() {
        let _ = is_loaded().expect("is_loaded must not error on a healthy host");
    }
}
