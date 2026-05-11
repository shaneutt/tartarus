//! Per-session SSH keypair, host-key, and known-hosts authoring.
//!
//! Generates a fresh ed25519 keypair per session via `ssh-keygen`,
//! injects the public key via cloud-init, and captures the guest
//! host key via `qemu-guest-agent` for strict host-key checking.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use crate::{error::Result, session::error::SessionError};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Directory inside the per-session dir holding SSH artefacts.
pub const SESSION_SSH_DIR_NAME: &str = "ssh";

/// Filename of the per-session ed25519 private key.
pub const PRIVATE_KEY_FILENAME: &str = "id_ed25519";

/// Filename of the per-session ed25519 public key.
pub const PUBLIC_KEY_FILENAME: &str = "id_ed25519.pub";

/// Filename of the per-session OpenSSH-format known_hosts file.
pub const KNOWN_HOSTS_FILENAME: &str = "known_hosts";

// ---------------------------------------------------------------------------
// SessionSshLayout
// ---------------------------------------------------------------------------

/// Materialised paths for one session's SSH artefacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSshLayout {
    /// `<session_dir>/ssh/`.
    pub dir: PathBuf,

    /// Per-session OpenSSH-format known_hosts. Pinned via
    /// `-o UserKnownHostsFile=...` at attach time.
    pub known_hosts: PathBuf,

    /// Per-session ed25519 private key (`id_ed25519`, 0600).
    pub private_key: PathBuf,

    /// Per-session ed25519 public key (`id_ed25519.pub`, 0644).
    pub public_key: PathBuf,
}

impl SessionSshLayout {
    /// Compose paths for `session_dir`'s SSH subdirectory.
    pub fn for_session(session_dir: &Path) -> Self {
        let dir = session_dir.join(SESSION_SSH_DIR_NAME);
        Self {
            known_hosts: dir.join(KNOWN_HOSTS_FILENAME),
            private_key: dir.join(PRIVATE_KEY_FILENAME),
            public_key: dir.join(PUBLIC_KEY_FILENAME),
            dir,
        }
    }
}

/// Generate an ed25519 keypair into `layout`. Idempotent.
pub fn ensure_keypair(layout: &SessionSshLayout) -> Result<()> {
    if layout.private_key.exists() && layout.public_key.exists() {
        return Ok(());
    }

    create_dir_owner_only(&layout.dir)?;

    let status = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg("tartarus-session")
        .arg("-f")
        .arg(&layout.private_key)
        .arg("-q")
        .status()
        .map_err(|source| SessionError::SshKeygenFailed {
            detail: format!("could not spawn ssh-keygen: {source}"),
        })?;

    if !status.success() {
        return Err(SessionError::SshKeygenFailed {
            detail: format!("ssh-keygen exited with {status}"),
        }
        .into());
    }

    set_private_key_mode(&layout.private_key)?;
    Ok(())
}

/// Read the public key as a trimmed line for cloud-init injection.
pub fn read_public_key(layout: &SessionSshLayout) -> Result<String> {
    let raw = std::fs::read_to_string(&layout.public_key)?;
    Ok(raw.trim_end().to_owned())
}

/// Write an OpenSSH `known_hosts` entry to the per-session file.
pub fn write_known_hosts(layout: &SessionSshLayout, host_key_line: &str) -> Result<()> {
    let mut body = host_key_line.trim_end().to_owned();
    body.push('\n');
    std::fs::write(&layout.known_hosts, body)?;
    Ok(())
}

/// Build a `known_hosts` entry for `[127.0.0.1]:<port>`.
pub fn known_hosts_entry(port: u16, guest_pubkey_line: &str) -> String {
    let payload = trim_pubkey_to_payload(guest_pubkey_line);
    format!("[127.0.0.1]:{port} {payload}\n")
}

// ---------------------------------------------------------------------------
// Key Management
// ---------------------------------------------------------------------------

/// Create `path` at mode 0700 (the SSH directory contract).
#[cfg(unix)]
fn create_dir_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).or_else(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(err.into())
        }
    })
}

#[cfg(not(unix))]
fn create_dir_owner_only(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).map_err(Into::into)
}

/// Tighten the private key to 0600.
#[cfg(unix)]
fn set_private_key_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_key_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Trim the trailing comment off a public-key line.
fn trim_pubkey_to_payload(line: &str) -> String {
    let trimmed = line.trim();
    let mut parts = trimmed.split_ascii_whitespace();
    match (parts.next(), parts.next()) {
        (Some(kind), Some(b64)) => format!("{kind} {b64}"),
        _ => trimmed.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    // ---------------------------------------------------------------------------
    // Test Utilities
    // ---------------------------------------------------------------------------

    fn tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-ssh-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("create_dir_all");
        path
    }

    #[test]
    fn layout_paths_are_under_session_ssh_dir() {
        let session_dir = tempdir();
        let layout = SessionSshLayout::for_session(&session_dir);

        assert!(
            layout.dir.starts_with(&session_dir),
            "ssh dir must live under session_dir, got {dir:?}",
            dir = layout.dir,
        );
        assert!(layout.private_key.ends_with(PRIVATE_KEY_FILENAME));
        assert!(layout.public_key.ends_with(PUBLIC_KEY_FILENAME));
        assert!(layout.known_hosts.ends_with(KNOWN_HOSTS_FILENAME));
    }

    #[test]
    fn ensure_keypair_creates_both_keys_and_is_idempotent() {
        if which::ssh_keygen_missing() {
            return;
        }

        let session_dir = tempdir();
        let layout = SessionSshLayout::for_session(&session_dir);

        ensure_keypair(&layout).expect("first ensure_keypair should succeed");
        let pub_first = std::fs::read(&layout.public_key).expect("read public key");
        let priv_first = std::fs::read(&layout.private_key).expect("read private key");

        ensure_keypair(&layout).expect("second ensure_keypair should be a no-op");
        let pub_second = std::fs::read(&layout.public_key).expect("read public key after second call");
        let priv_second = std::fs::read(&layout.private_key).expect("read private key after second call");

        assert_eq!(
            pub_first, pub_second,
            "public key must not change on second ensure_keypair"
        );
        assert_eq!(
            priv_first, priv_second,
            "private key must not change on second ensure_keypair",
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_keypair_writes_private_key_at_0600() {
        if which::ssh_keygen_missing() {
            return;
        }

        use std::os::unix::fs::PermissionsExt;

        let session_dir = tempdir();
        let layout = SessionSshLayout::for_session(&session_dir);

        ensure_keypair(&layout).expect("ensure_keypair");
        let mode = std::fs::metadata(&layout.private_key)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "private key must be mode 0600, got {mode:o}");
    }

    #[test]
    fn read_public_key_round_trips_a_trimmed_line() {
        let session_dir = tempdir();
        let layout = SessionSshLayout::for_session(&session_dir);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");

        std::fs::write(&layout.public_key, "ssh-ed25519 AAAA dummy\n\n").expect("write");
        let line = read_public_key(&layout).expect("read");

        assert_eq!(line, "ssh-ed25519 AAAA dummy");
    }

    #[test]
    fn known_hosts_entry_brackets_loopback_and_strips_comment() {
        let entry = known_hosts_entry(32000, "ssh-ed25519 BASE64== root@guest");

        assert_eq!(entry, "[127.0.0.1]:32000 ssh-ed25519 BASE64==\n");
    }

    #[test]
    fn known_hosts_entry_handles_missing_comment_gracefully() {
        let entry = known_hosts_entry(32000, "ssh-ed25519 BASE64==");

        assert_eq!(entry, "[127.0.0.1]:32000 ssh-ed25519 BASE64==\n");
    }

    #[test]
    fn write_known_hosts_terminates_with_newline() {
        let session_dir = tempdir();
        let layout = SessionSshLayout::for_session(&session_dir);
        std::fs::create_dir_all(&layout.dir).expect("mkdir");

        write_known_hosts(&layout, "[127.0.0.1]:32000 ssh-ed25519 BASE64==").expect("write");
        let body = std::fs::read_to_string(&layout.known_hosts).expect("read");

        assert!(
            body.ends_with('\n'),
            "known_hosts must end with a newline, got {body:?}"
        );
        assert_eq!(
            body.matches('\n').count(),
            1,
            "exactly one newline expected, got {body:?}"
        );
    }

    mod which {
        use std::process::Command;

        /// Sandbox-friendly check: is `ssh-keygen` absent from PATH?
        ///
        /// Tests that exec ssh-keygen short-circuit cleanly when run
        /// in a stripped CI image without OpenSSH.
        pub fn ssh_keygen_missing() -> bool {
            Command::new("ssh-keygen").arg("-?").output().is_err()
        }
    }
}
