//! XDG-derived filesystem paths used by Tartarus.
//!
//! The on-disk layout is fixed by [`docs/spec.md`]:
//!
//! ```text
//! ~/.config/tartarus/
//!   config.toml
//!
//! ~/.local/share/tartarus/
//!   base/
//!     current -> ...
//!     fedora-NN-YYYY-MM-DD.qcow2
//!   sessions/
//!     by-uuid/<uuid>/
//!     by-name/<alias> -> ../by-uuid/<uuid>
//! ```
//!
//! Functions in this module return [`PathBuf`]s by computing paths from
//! [`directories::ProjectDirs`]. They never create directories on disk; the
//! call site is responsible for `fs::create_dir_all` when needed.
//!
//! [`docs/spec.md`]: https://github.com/the-lost-art-of-programming/tartarus/blob/main/docs/spec.md

use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::{Error, Result};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Application identifier for [`ProjectDirs::from`].
const APP_NAME: &str = "tartarus";

/// File name of the per-user config inside [`config_dir`].
const CONFIG_FILE_NAME: &str = "config.toml";

/// Subdirectory for base images.
const BASE_DIR_NAME: &str = "base";

/// Subdirectory for session state.
const SESSIONS_DIR_NAME: &str = "sessions";

/// Subdirectory of [`sessions_dir`] keyed by canonical session UUID.
const BY_UUID_DIR_NAME: &str = "by-uuid";

/// Subdirectory of [`sessions_dir`] holding alias symlinks into `by-uuid/`.
const BY_NAME_DIR_NAME: &str = "by-name";

// -----------------------------------------------------------------------------
// Path Resolution
// -----------------------------------------------------------------------------

/// Per-user config directory (e.g. `~/.config/tartarus`).
pub fn config_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// Path to the per-user `config.toml` file.
pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// Path to the per-user data directory (e.g. `~/.local/share/tartarus`).
pub fn data_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.data_dir().to_path_buf())
}

/// Path to the base image library directory.
pub fn base_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join(BASE_DIR_NAME))
}

/// Sessions directory containing `by-uuid/` and `by-name/`.
pub fn sessions_dir() -> Result<PathBuf> {
    Ok(data_dir()?.join(SESSIONS_DIR_NAME))
}

/// Path to the canonical, UUID-keyed session directory tree.
pub fn sessions_by_uuid_dir() -> Result<PathBuf> {
    Ok(sessions_dir()?.join(BY_UUID_DIR_NAME))
}

/// Path to the alias symlink directory under [`sessions_dir`].
pub fn sessions_by_name_dir() -> Result<PathBuf> {
    Ok(sessions_dir()?.join(BY_NAME_DIR_NAME))
}

// -----------------------------------------------------------------------------
// Project Directories
// -----------------------------------------------------------------------------

/// Resolve the [`ProjectDirs`] for Tartarus.
fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", APP_NAME).ok_or(Error::NoProjectDirs)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_lives_under_config_dir() {
        let config_dir = config_dir().expect("project dirs should resolve in test env");
        let config_file = config_file().expect("project dirs should resolve in test env");

        assert!(
            config_file.starts_with(&config_dir),
            "config_file ({config_file:?}) should be a child of config_dir ({config_dir:?})",
        );
        assert_eq!(
            config_file.file_name().and_then(|n| n.to_str()),
            Some("config.toml"),
            "config_file should be named config.toml",
        );
    }

    #[test]
    fn data_layout_matches_spec() {
        let data = data_dir().expect("project dirs should resolve in test env");
        let base = base_dir().expect("project dirs should resolve in test env");
        let sessions = sessions_dir().expect("project dirs should resolve in test env");
        let by_uuid = sessions_by_uuid_dir().expect("project dirs should resolve in test env");
        let by_name = sessions_by_name_dir().expect("project dirs should resolve in test env");

        assert!(base.starts_with(&data), "base/ should live under data dir");
        assert!(sessions.starts_with(&data), "sessions/ should live under data dir");
        assert!(
            by_uuid.starts_with(&sessions),
            "by-uuid/ should live under sessions dir",
        );
        assert!(
            by_name.starts_with(&sessions),
            "by-name/ should live under sessions dir",
        );
    }
}
