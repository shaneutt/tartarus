//! Invoking host user identity (username, UID, GID).
//!
//! Reads `/proc/self/status` for UID/GID and `$USER`/`$LOGNAME` for the
//! username.

use std::path::PathBuf;

use crate::error::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default UID used when `/proc/self/status` cannot be read.
const DEFAULT_UID: u32 = 1000;

/// Default GID used when `/proc/self/status` cannot be read.
const DEFAULT_GID: u32 = 1000;

/// Default username used when no environment variable supplies one.
const DEFAULT_USERNAME: &str = "tartarus";

/// Maximum allowed length of a POSIX-portable username, in chars.
const MAX_USERNAME_LEN: usize = 32;

// ---------------------------------------------------------------------------
// HostUser
// ---------------------------------------------------------------------------

/// Resolved host-user identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostUser {
    /// Effective primary group ID.
    pub gid: u32,

    /// Effective user ID.
    pub uid: u32,

    /// Username, derived from `USER` then `LOGNAME` then `/proc/self/status`'s
    /// `Name:` field as a last resort.
    pub username: String,
}

/// Resolve the invoking user's identity.
pub fn current() -> Result<HostUser> {
    #[cfg(unix)]
    {
        let uid = read_id_from_status("Uid:").unwrap_or(DEFAULT_UID);
        let gid = read_id_from_status("Gid:").unwrap_or(DEFAULT_GID);
        let username = resolve_username();

        Ok(HostUser { gid, uid, username })
    }

    #[cfg(not(unix))]
    {
        Ok(HostUser {
            gid: DEFAULT_GID,
            uid: DEFAULT_UID,
            username: "tartarus".to_owned(),
        })
    }
}

/// Return `/home/<username>` as the in-guest home directory.
pub fn home_dir(user: &HostUser) -> PathBuf {
    PathBuf::from(format!("/home/{name}", name = user.username))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Test whether `username` matches `^[a-z_][a-z0-9_-]{0,31}$`.
pub fn is_valid_username(username: &str) -> bool {
    let mut chars = username.chars();
    let Some(first) = chars.next() else { return false };

    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }

    let mut len = 1usize;
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
            return false;
        }
        len += 1;
        if len > MAX_USERNAME_LEN {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Utility Functions
// ---------------------------------------------------------------------------

/// Read the effective UID/GID from `/proc/self/status`.
#[cfg(unix)]
fn read_id_from_status(prefix: &str) -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;

    status
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .and_then(|rest| rest.split_whitespace().nth(1).map(str::to_owned))
        .and_then(|s| s.parse::<u32>().ok())
}

/// Resolve the invoker's username from `$USER` or `$LOGNAME`, falling
/// back to [`DEFAULT_USERNAME`].
#[cfg(unix)]
fn resolve_username() -> String {
    let from_env = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .filter(|s| !s.trim().is_empty());

    match from_env {
        Some(name) if is_valid_username(&name) => name,
        Some(name) => {
            tracing::warn!(
                username = %name,
                "host username from $USER/$LOGNAME failed POSIX portable-identifier validation; falling back",
            );
            DEFAULT_USERNAME.to_owned()
        },
        None => DEFAULT_USERNAME.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_returns_a_populated_identity() {
        let user = current().expect("current() should resolve in test env");

        assert!(!user.username.is_empty(), "username should be populated, got empty");
    }

    #[cfg(unix)]
    #[test]
    fn read_id_from_status_finds_uid() {
        let uid = read_id_from_status("Uid:");

        assert!(uid.is_some(), "/proc/self/status should expose a Uid: line on Linux");
    }

    #[cfg(unix)]
    #[test]
    fn read_id_from_status_returns_none_for_missing_prefix() {
        let synthetic = read_id_from_status("Synthetic-Prefix-Not-Present:");

        assert!(synthetic.is_none(), "missing prefix should return None");
    }

    #[test]
    fn home_dir_uses_canonical_in_guest_path() {
        let user = HostUser {
            gid: 1000,
            uid: 1000,
            username: "alice".to_owned(),
        };

        let home = home_dir(&user);

        assert_eq!(
            home,
            PathBuf::from("/home/alice"),
            "in-guest home should be /home/<username>",
        );
    }

    #[test]
    fn is_valid_username_accepts_typical_values() {
        for ok in [
            "alice",
            "_systemd",
            "alice123",
            "a-b",
            "a_b",
            "u",
            "x".repeat(32).as_str(),
        ] {
            assert!(is_valid_username(ok), "{ok} should validate as a POSIX username");
        }
    }

    #[test]
    fn is_valid_username_rejects_metacharacters_and_overlong() {
        for bad in [
            "",
            "Alice",
            "0alice",
            "-alice",
            "alice; rm -rf /",
            "alice'\necho",
            "a/b",
            "root\x00",
            "x".repeat(33).as_str(),
        ] {
            assert!(!is_valid_username(bad), "{bad:?} must not validate as a POSIX username");
        }
    }
}
