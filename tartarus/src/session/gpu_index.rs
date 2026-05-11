//! Cross-session GPU borrow lookup: conflict checks at borrow time
//! and force-release via [`lookup_receipt`] + [`clear_receipt`].

use std::path::PathBuf;

use crate::{
    error::Result,
    gpu::{driver::Receipt, pci::PciAddress},
    paths,
    session::metadata::Metadata,
};

// ---------------------------------------------------------------------------
// Borrow Lookup
// ---------------------------------------------------------------------------

/// One session's identity for the purpose of conflict reporting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BorrowingSession {
    /// Session alias if one was set, else `None`.
    pub alias: Option<String>,

    /// The borrow record itself.
    pub address: PciAddress,

    /// Session UUID.
    pub uuid: String,
}

/// Return the session holding `address`, or `None`.
pub fn find_borrowing_session(address: &PciAddress) -> Result<Option<BorrowingSession>> {
    let target = address.to_string();
    for metadata in iter_session_metadata()? {
        if let Some(borrow) = metadata.gpu_borrow.as_ref()
            && borrow.address == target
        {
            let parsed: PciAddress = borrow.address.parse()?;
            return Ok(Some(BorrowingSession {
                alias: metadata.alias.clone(),
                address: parsed,
                uuid: metadata.uuid.clone(),
            }));
        }
    }
    Ok(None)
}

/// Find the borrow receipt for `address`.
pub fn lookup_receipt(address: &PciAddress) -> Result<Receipt> {
    let session = find_borrowing_session(address)?.ok_or_else(|| crate::session::error::SessionError::NotFound {
        target: address.to_string(),
    })?;

    let metadata = load_metadata_for_uuid(&session.uuid)?;
    let record = metadata
        .gpu_borrow
        .expect("borrow lookup found this session by its borrow record");

    record.into_receipt()
}

/// Erase the borrow record for `address`. No-op if none exists.
pub fn clear_receipt(address: &PciAddress) -> Result<()> {
    let Some(session) = find_borrowing_session(address)? else {
        return Ok(());
    };

    let mut metadata = load_metadata_for_uuid(&session.uuid)?;
    metadata.gpu_borrow = None;

    let metadata_path = paths::sessions_by_uuid_dir()?
        .join(&session.uuid)
        .join(crate::session::metadata::METADATA_FILE_NAME);
    metadata.save(&metadata_path)?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Metadata Scanning
// ---------------------------------------------------------------------------

/// Load `metadata.json` for one session UUID.
fn load_metadata_for_uuid(uuid: &str) -> Result<Metadata> {
    let path = paths::sessions_by_uuid_dir()?
        .join(uuid)
        .join(crate::session::metadata::METADATA_FILE_NAME);
    Metadata::load(&path)
}

/// Iterate over every readable `metadata.json` under `sessions/by-uuid/`.
fn iter_session_metadata() -> Result<impl Iterator<Item = Metadata>> {
    let root = paths::sessions_by_uuid_dir()?;
    if !root.exists() {
        return Ok(Vec::new().into_iter());
    }

    let mut found: Vec<Metadata> = Vec::new();
    let entries = std::fs::read_dir(&root)?;
    for entry in entries.flatten() {
        let path: PathBuf = entry.path().join(crate::session::metadata::METADATA_FILE_NAME);
        if let Ok(metadata) = Metadata::load(&path) {
            found.push(metadata);
        }
    }
    Ok(found.into_iter())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::{fs, sync::Mutex};

    use super::*;
    use crate::{
        gpu::driver::Receipt as DriverReceipt,
        session::metadata::{self as md, GpuBorrowRecord},
    };

    /// `cargo test` runs tests in parallel; every gpu_index test
    /// touches the shared XDG `sessions/by-uuid/` tree, so they
    /// must run one at a time. A free function `serial()` returns
    /// the lock guard; tests `let _g = serial();` at the top.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---------------------------------------------------------------------------
    // Test Utilities
    // ---------------------------------------------------------------------------

    fn fake_metadata(uuid: &str, borrow: Option<GpuBorrowRecord>) -> Metadata {
        let mut m = md::fresh(md::FreshFields {
            alias: None,
            base: "fedora-41-test.qcow2".to_owned(),
            envs: vec![],
            overlay_virtual_gib: 64,
            persist: true,
            repos: vec![],
            uuid: uuid.to_owned(),
        });
        m.gpu_borrow = borrow;
        m
    }

    fn write_session(uuid: &str, borrow: Option<GpuBorrowRecord>) {
        let dir = paths::sessions_by_uuid_dir().expect("paths").join(uuid);
        fs::create_dir_all(&dir).expect("mkdir");
        let metadata = fake_metadata(uuid, borrow);
        metadata.save(&dir.join(md::METADATA_FILE_NAME)).expect("save metadata");
    }

    fn clear_sessions_root() {
        let root = paths::sessions_by_uuid_dir().expect("paths");
        if root.exists() {
            fs::remove_dir_all(&root).expect("rm sessions root");
        }
    }

    #[test]
    fn find_borrowing_session_returns_none_when_no_session_holds_device() {
        let _g = serial();
        clear_sessions_root();
        write_session("11111111-1111-1111-1111-111111111111", None);

        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        let found = find_borrowing_session(&address).expect("walk should succeed");

        assert!(found.is_none(), "no session holds this device, got {found:?}");
    }

    #[test]
    fn find_borrowing_session_locates_holder() {
        let _g = serial();
        clear_sessions_root();
        let uuid = "22222222-2222-2222-2222-222222222222";
        write_session(
            uuid,
            Some(GpuBorrowRecord {
                address: "0000:01:00.0".to_owned(),
                previous_driver: Some("nvidia".to_owned()),
            }),
        );

        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        let found = find_borrowing_session(&address)
            .expect("walk should succeed")
            .expect("the borrow we wrote should be findable");

        assert_eq!(found.uuid, uuid);
        assert_eq!(found.address, address);
    }

    #[test]
    fn lookup_receipt_round_trips_via_metadata() {
        let _g = serial();
        clear_sessions_root();
        let uuid = "33333333-3333-3333-3333-333333333333";
        write_session(
            uuid,
            Some(GpuBorrowRecord {
                address: "0000:01:00.0".to_owned(),
                previous_driver: Some("nvidia".to_owned()),
            }),
        );

        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        let receipt = lookup_receipt(&address).expect("lookup should succeed");

        let expected = DriverReceipt {
            address,
            previous_driver: Some("nvidia".to_owned()),
        };
        assert_eq!(receipt, expected);
    }

    #[test]
    fn clear_receipt_removes_borrow_from_metadata() {
        let _g = serial();
        clear_sessions_root();
        let uuid = "44444444-4444-4444-4444-444444444444";
        write_session(
            uuid,
            Some(GpuBorrowRecord {
                address: "0000:01:00.0".to_owned(),
                previous_driver: None,
            }),
        );

        let address: PciAddress = "0000:01:00.0".parse().expect("parse");
        clear_receipt(&address).expect("clear should succeed");

        let path = paths::sessions_by_uuid_dir()
            .expect("paths")
            .join(uuid)
            .join(md::METADATA_FILE_NAME);
        let reloaded = Metadata::load(&path).expect("reload");
        assert!(
            reloaded.gpu_borrow.is_none(),
            "clear_receipt should drop the borrow record, got {:?}",
            reloaded.gpu_borrow,
        );
    }
}
