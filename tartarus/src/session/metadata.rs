//! Per-session `metadata.json` schema and IO.
//!
//! The metadata file is the host-side ground truth for a session.
//! Schema is **v1**; the loader accepts `version >= 1`, the writer
//! always emits the current shape. Timestamps are RFC 3339 UTC
//! strings (`YYYY-MM-DDTHH:MM:SSZ`).

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

use crate::{error::Result, seed::input::RepoSpec, session::error::SessionError};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// File name of the per-session metadata document.
pub const METADATA_FILE_NAME: &str = "metadata.json";

/// Current schema version emitted by the writer.
pub const CURRENT_VERSION: u32 = 1;

/// Lowest schema version the loader will accept.
const MIN_VERSION: u32 = 1;

/// File mode for `metadata.json`: `0600` because it may contain
/// bearer secrets (e.g. the remote-connect URL).
#[cfg(unix)]
const METADATA_FILE_MODE: u32 = 0o600;

/// Counter feeding the per-process suffix on temp metadata files.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// -----------------------------------------------------------------------------
// Metadata
// -----------------------------------------------------------------------------

/// Persisted per-session metadata.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct Metadata {
    /// Schema version (v1 today).
    pub version: u32,

    /// Alias from `--name`, if set.
    pub alias: Option<String>,

    /// Base image filename the session's overlay backs onto.
    pub base: String,

    /// Session creation timestamp (RFC 3339, UTC).
    pub created_at: String,

    /// Programming envs requested at session start.
    pub envs: Vec<String>,

    /// Most recent attach timestamp, or `None`.
    pub last_attached_at: Option<String>,

    /// Session memory in MiB. `0` means the field was absent
    /// (pre-vm-config metadata).
    #[serde(default)]
    pub memory_mib: u32,

    /// Overlay virtual size in GiB. `0` means the field was absent
    /// (pre-P10 metadata); callers should fall back to `qemu-img info`.
    #[serde(default)]
    pub overlay_virtual_gib: u32,

    /// True iff the overlay survives session exit (`--ephemeral` = false).
    pub persist: bool,

    /// Claude remote-connect URL for background-mode sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,

    /// Repos cloned at first boot.
    pub repos: Vec<RepoSpec>,

    /// Loopback TCP port forwarded to guest port 22. `None` for
    /// sessions predating M2 SSH attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_port: Option<u16>,

    /// GPU borrow record from `--gpu`, or `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_borrow: Option<GpuBorrowRecord>,

    /// Session UUID (matches the parent directory under `by-uuid/`).
    pub uuid: String,

    /// Session vCPU count. `0` means the field was absent
    /// (pre-vm-config metadata).
    #[serde(default)]
    pub vcpus: u32,
}

/// Persisted form of a GPU borrow, serialised as strings for JSON.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct GpuBorrowRecord {
    /// Canonical PCI address (`DDDD:BB:DD.F`).
    pub address: String,

    /// Driver bound to the device immediately before the borrow.
    /// `None` when the device was unbound at borrow time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_driver: Option<String>,
}

impl GpuBorrowRecord {
    /// Capture a borrow into a persistable record.
    pub fn from_receipt(receipt: &crate::gpu::driver::Receipt) -> Self {
        Self {
            address: receipt.address.to_string(),
            previous_driver: receipt.previous_driver.clone(),
        }
    }

    /// Reconstruct the [`crate::gpu::driver::Receipt`] for replay.
    pub fn into_receipt(self) -> crate::error::Result<crate::gpu::driver::Receipt> {
        let address = self.address.parse()?;
        Ok(crate::gpu::driver::Receipt {
            address,
            previous_driver: self.previous_driver,
        })
    }
}

impl Metadata {
    /// Load and validate `metadata.json` from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let body = std::fs::read_to_string(path)?;
        let parsed: Self = serde_json::from_str(&body).map_err(|source| SessionError::MetadataParse {
            path: path.to_path_buf(),
            source,
        })?;

        if parsed.version < MIN_VERSION {
            return Err(SessionError::MetadataVersion {
                version: parsed.version,
            }
            .into());
        }

        Ok(parsed)
    }

    /// Atomically write `metadata.json` to `path` at mode `0600`.
    pub fn save(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;

        let tmp = temp_path_for(path);
        if let Err(err) = write_temp_atomically(&tmp, body.as_bytes()) {
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(err.into());
        }

        enforce_owner_only_mode(path)?;
        sync_parent_dir(path)?;

        Ok(())
    }
}

/// Fields [`fresh`] needs to build a [`Metadata`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreshFields {
    /// Optional alias from `--name`.
    pub alias: Option<String>,

    /// Base image filename the overlay backs onto.
    pub base: String,

    /// Programming envs requested at session start.
    pub envs: Vec<String>,

    /// Session memory in MiB.
    pub memory_mib: u32,

    /// Per-session overlay virtual size in GiB at creation time.
    pub overlay_virtual_gib: u32,

    /// Persist flag (false iff the session was started `--ephemeral`).
    pub persist: bool,

    /// Repos cloned at first boot.
    pub repos: Vec<RepoSpec>,

    /// Session UUID.
    pub uuid: String,

    /// Session vCPU count.
    pub vcpus: u32,
}

/// Build a fresh [`Metadata`] for a newly-created session.
pub fn fresh(fields: FreshFields) -> Metadata {
    Metadata {
        version: CURRENT_VERSION,
        alias: fields.alias,
        base: fields.base,
        created_at: now_iso(),
        envs: fields.envs,
        gpu_borrow: None,
        last_attached_at: None,
        memory_mib: fields.memory_mib,
        overlay_virtual_gib: fields.overlay_virtual_gib,
        persist: fields.persist,
        remote_url: None,
        repos: fields.repos,
        ssh_port: None,
        uuid: fields.uuid,
        vcpus: fields.vcpus,
    }
}

/// Return the current time as `YYYY-MM-DDTHH:MM:SSZ` (UTC).
pub fn now_iso() -> String {
    crate::time::now_iso()
}

/// Compose the temp file path used for the atomic `metadata.json` write.
fn temp_path_for(path: &Path) -> PathBuf {
    let pid = std::process::id();
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let parent = path.parent().expect("metadata path always has a parent");
    parent.join(format!(".metadata.json.tartarus-{pid}-{n}"))
}

/// Write `bytes` to `tmp` at mode `0600` and fsync.
#[cfg(unix)]
fn write_temp_atomically(tmp: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(METADATA_FILE_MODE)
        .open(tmp)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(())
}

/// Non-Unix shim.
#[cfg(not(unix))]
fn write_temp_atomically(tmp: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(tmp)?;

    file.write_all(bytes)?;
    file.sync_all()?;

    Ok(())
}

/// Enforce mode `0600` on the file.
#[cfg(unix)]
fn enforce_owner_only_mode(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.permissions().mode() & 0o777;

    if mode != METADATA_FILE_MODE {
        fs::set_permissions(path, fs::Permissions::from_mode(METADATA_FILE_MODE))?;
    }

    Ok(())
}

/// Non-Unix shim.
#[cfg(not(unix))]
fn enforce_owner_only_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Fsync the parent directory of `path` so the rename is durable.
fn sync_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };

    if parent.as_os_str().is_empty() {
        return Ok(());
    }

    let dir = fs::File::open(parent)?;
    dir.sync_all()?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_every_field() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        let written = sample_metadata();
        written.save(&path).expect("save should succeed");

        let read = Metadata::load(&path).expect("load should succeed");

        assert_eq!(read, written, "metadata should round-trip through JSON cleanly");
    }

    #[test]
    fn save_pretty_prints_for_readability() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);
        sample_metadata().save(&path).expect("save");

        let body = std::fs::read_to_string(&path).expect("read back");

        assert!(
            body.contains('\n'),
            "saved metadata should be pretty-printed for human readability, got: {body}",
        );
    }

    #[test]
    fn load_rejects_below_min_version() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        let mut bad = sample_metadata();
        bad.version = 0;
        std::fs::write(&path, serde_json::to_string(&bad).unwrap()).expect("write");

        let err = Metadata::load(&path).expect_err("version 0 must be rejected");

        match err {
            crate::error::Error::Session(SessionError::MetadataVersion { version }) => {
                assert_eq!(version, 0, "rejected version should round-trip into the error");
            },
            other => panic!("expected MetadataVersion, got {other:?}"),
        }
    }

    #[test]
    fn load_accepts_future_versions() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        let mut future = sample_metadata();
        future.version = 999;
        std::fs::write(&path, serde_json::to_string(&future).unwrap()).expect("write");

        let read = Metadata::load(&path).expect("future version should load (forward-compat)");

        assert_eq!(read.version, 999, "future version should pass through unmodified");
    }

    #[test]
    fn load_returns_typed_error_on_bad_json() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);
        std::fs::write(&path, "not json").expect("write");

        let err = Metadata::load(&path).expect_err("garbage should fail parse");

        match err {
            crate::error::Error::Session(SessionError::MetadataParse { .. }) => {},
            other => panic!("expected MetadataParse, got {other:?}"),
        }
    }

    #[test]
    fn fresh_yields_v1_with_no_attach() {
        let metadata = fresh(FreshFields {
            alias: Some("name".to_owned()),
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: vec!["rust".to_owned()],
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            uuid: "abcd".to_owned(),
            vcpus: 2,
        });

        assert_eq!(
            metadata.version, CURRENT_VERSION,
            "fresh metadata should carry the current version"
        );
        assert!(
            metadata.last_attached_at.is_none(),
            "fresh metadata should have no last-attached marker",
        );
        assert!(
            metadata.created_at.ends_with('Z'),
            "created_at should be UTC, got: {created}",
            created = metadata.created_at,
        );
    }

    #[test]
    fn load_accepts_v1_files_without_overlay_virtual_gib() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        let json = serde_json::json!({
            "version": 1,
            "alias": null,
            "base": "fedora-41-2026-05-01.qcow2",
            "created_at": "2026-05-05T10:00:00Z",
            "envs": ["rust"],
            "last_attached_at": null,
            "persist": true,
            "repos": [{"slug": "owner/name", "default": true}],
            "uuid": "11111111-2222-3333-4444-555555555555",
        });
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).expect("write");

        let read = Metadata::load(&path).expect("v1 metadata without overlay_virtual_gib should load");

        assert_eq!(
            read.overlay_virtual_gib, 0,
            "missing overlay_virtual_gib should default to 0 (caller falls back to qemu-img info)",
        );
    }

    #[cfg(unix)]
    #[test]
    fn save_lands_at_mode_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        sample_metadata().save(&path).expect("save");

        let mode = std::fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o600,
            "metadata.json carries the remote-connect URL so must be world-unreadable",
        );
    }

    #[test]
    fn save_round_trips_overlay_virtual_gib() {
        let dir = unique_tempdir();
        let path = dir.join(METADATA_FILE_NAME);

        let mut metadata = sample_metadata();
        metadata.overlay_virtual_gib = 200;
        metadata.save(&path).expect("save");

        let read = Metadata::load(&path).expect("load");
        assert_eq!(
            read.overlay_virtual_gib, 200,
            "overlay_virtual_gib should round-trip through serialise + deserialise",
        );
    }

    #[test]
    fn now_iso_format_is_well_shaped() {
        let s = now_iso();

        assert_eq!(
            s.len(),
            20,
            "RFC 3339 string should be 20 chars, got: {s} ({len})",
            len = s.len()
        );
        assert!(s.ends_with('Z'), "now_iso should end with Z (UTC), got: {s}");
        assert_eq!(&s[4..5], "-", "year-month separator at index 4, got: {s}");
        assert_eq!(&s[10..11], "T", "date-time separator at index 10, got: {s}");
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn sample_metadata() -> Metadata {
        Metadata {
            version: CURRENT_VERSION,
            alias: Some("fix-bug".to_owned()),
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            created_at: "2026-05-05T10:00:00Z".to_owned(),
            envs: vec!["rust".to_owned(), "go".to_owned()],
            gpu_borrow: None,
            last_attached_at: None,
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            remote_url: None,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_port: None,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            vcpus: 2,
        }
    }

    fn unique_tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-session-meta-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
