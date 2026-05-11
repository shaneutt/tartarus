//! Cloud-init NoCloud seed authoring for the layering boot.
//!
//! Single-shot, identity-agnostic: installs packages, drops the in-guest
//! helper bundle, enables `qemu-guest-agent`, caches the Claude Code
//! tarball, and powers off. Per-user setup is deferred to session-start.

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::error::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Cloud-init `instance-id` for the layering boot.
const INSTANCE_ID: &str = "tartarus-layering";

/// Cloud-init `local-hostname` for the layering boot.
const LOCAL_HOSTNAME: &str = "tartarus-layering";

/// On-image path for the cached Claude Code tarball.
pub const CLAUDE_TARBALL_PATH: &str = "/opt/tartarus/skeleton/claude-code.tgz";

/// In-guest install prefix for the helper scripts.
const GUEST_BIN_DIR: &str = "/usr/local/bin";

/// In-guest install prefix for the helper systemd units.
const GUEST_SYSTEMD_DIR: &str = "/etc/systemd/system";

// Guest helper bundle. Pulled in at compile time via `include_str!` so the
// resulting layering seed is self-contained and the build fails fast if a
// helper file moves or disappears.

/// Embedded guest scripts: `(filename, contents)` tuples.
const GUEST_SCRIPTS: &[(&str, &str)] = &[
    (
        "tartarus-bootstrap.sh",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/bin/tartarus-bootstrap.sh"
        )),
    ),
    (
        "tartarus-claude.sh",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/bin/tartarus-claude.sh")),
    ),
    (
        "tartarus-env-add.sh",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/bin/tartarus-env-add.sh")),
    ),
    (
        "tartarus-env-update.sh",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/bin/tartarus-env-update.sh"
        )),
    ),
    (
        "tartarus-env-wrapper.sh",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/bin/tartarus-env-wrapper.sh"
        )),
    ),
    (
        "tartarus-fstrim.sh",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/bin/tartarus-fstrim.sh")),
    ),
    (
        "tartarus-grow.sh",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/bin/tartarus-grow.sh")),
    ),
    (
        "tartarus-grow-apply.sh",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/bin/tartarus-grow-apply.sh"
        )),
    ),
    (
        "tartarus-update.sh",
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/bin/tartarus-update.sh")),
    ),
    (
        "tartarus-update-claude.sh",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/bin/tartarus-update-claude.sh"
        )),
    ),
];

/// Embedded systemd units copied into `/etc/systemd/system/` by cloud-init.
const GUEST_UNITS: &[(&str, &str)] = &[
    (
        "tartarus-bootstrap.service",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-bootstrap.service"
        )),
    ),
    (
        "tartarus-claude@.service",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-claude@.service"
        )),
    ),
    (
        "tartarus-fstrim.service",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-fstrim.service"
        )),
    ),
    (
        "tartarus-grow.timer",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-grow.timer"
        )),
    ),
    (
        "tartarus-grow.service",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-grow.service"
        )),
    ),
    (
        "tartarus-update-system.service",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../guest/systemd/tartarus-update-system.service"
        )),
    ),
];

/// Units enabled by cloud-init's `runcmd` block.
const ENABLE_UNITS: &[&str] = &[
    "qemu-guest-agent.service",
    "tartarus-bootstrap.service",
    "tartarus-fstrim.service",
    "tartarus-grow.timer",
];

/// Packages installed by the layering seed.
const LAYER_PACKAGES: &[&str] = &[
    "git",
    "gh",
    "tmux",
    "golang",
    "python3",
    "python3-virtualenv",
    "python3-pip",
    "rustup",
    "qemu-guest-agent",
    "nodejs",
    "npm",
    "cloud-utils-growpart",
    "e2fsprogs",
];

// ---------------------------------------------------------------------------
// Seed Rendering
// ---------------------------------------------------------------------------

/// The two files cloud-init's NoCloud datasource expects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeedFiles {
    /// Contents of `meta-data` (cloud-init NoCloud schema).
    pub meta_data: String,

    /// Contents of `user-data` (cloud-init script with `write_files`,
    /// `runcmd`, `packages`, etc.).
    pub user_data: String,
}

/// Render the layering seed into two in-memory files.
pub fn render_seed() -> SeedFiles {
    SeedFiles {
        meta_data: render_meta_data(),
        user_data: render_user_data(),
    }
}

/// Write the rendered seed files into `workdir`.
pub fn write_seed_files(workdir: &Path) -> Result<SeedPaths> {
    let seed = render_seed();

    let user_data = workdir.join("user-data");
    let meta_data = workdir.join("meta-data");

    fs::write(&user_data, &seed.user_data)?;
    fs::write(&meta_data, &seed.meta_data)?;

    Ok(SeedPaths { meta_data, user_data })
}

/// Filesystem locations of the seed files written by [`write_seed_files`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SeedPaths {
    /// Absolute path to the rendered `meta-data` file.
    pub meta_data: PathBuf,

    /// Absolute path to the rendered `user-data` file.
    pub user_data: PathBuf,
}

// ---------------------------------------------------------------------------
// Cloud-Init Authoring
// ---------------------------------------------------------------------------

/// Render the cloud-init `meta-data` document.
fn render_meta_data() -> String {
    format!("instance-id: {INSTANCE_ID}\nlocal-hostname: {LOCAL_HOSTNAME}\n")
}

/// Render the cloud-init `user-data` document.
fn render_user_data() -> String {
    let mut out = String::new();
    out.push_str("#cloud-config\n");
    out.push_str("# Tartarus layering seed (generated). Single-shot; powers off when done.\n");
    out.push('\n');

    out.push_str("package_update: true\n");
    out.push_str("package_upgrade: true\n");
    out.push('\n');

    out.push_str("packages:\n");
    for pkg in LAYER_PACKAGES {
        out.push_str(&format!("  - {pkg}\n"));
    }
    out.push('\n');

    out.push_str("write_files:\n");
    for (name, body) in GUEST_SCRIPTS {
        emit_write_file(&mut out, &format!("{GUEST_BIN_DIR}/{name}"), body, "0755");
    }
    for (name, body) in GUEST_UNITS {
        emit_write_file(&mut out, &format!("{GUEST_SYSTEMD_DIR}/{name}"), body, "0644");
    }
    out.push('\n');

    out.push_str("runcmd:\n");
    out.push_str("  - mkdir -p /opt/tartarus/skeleton\n");
    out.push_str("  - npm pack --pack-destination /opt/tartarus/skeleton @anthropic-ai/claude-code\n");
    out.push_str(&format!(
        "  - sh -c 'mv /opt/tartarus/skeleton/anthropic-ai-claude-code-*.tgz {CLAUDE_TARBALL_PATH}'\n",
    ));
    for unit in ENABLE_UNITS {
        out.push_str(&format!("  - systemctl enable {unit}\n"));
    }
    out.push_str("  - poweroff\n");
    out.push('\n');

    out.push_str("power_state:\n");
    out.push_str("  delay: now\n");
    out.push_str("  mode: poweroff\n");
    out.push_str("  message: tartarus layering complete\n");
    out.push_str("  condition: true\n");

    out
}

/// Append a single `write_files` entry to `out`.
fn emit_write_file(out: &mut String, path: &str, body: &str, permissions: &str) {
    out.push_str(&format!("  - path: {path}\n"));
    out.push_str(&format!("    permissions: '{permissions}'\n"));
    out.push_str("    owner: root:root\n");
    out.push_str("    content: |\n");
    for line in body.lines() {
        out.push_str("      ");
        out.push_str(line);
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_data_is_well_formed() {
        let meta = render_meta_data();

        assert!(
            meta.contains("instance-id: tartarus-layering"),
            "meta-data should carry the instance-id, got: {meta}"
        );
        assert!(
            meta.contains("local-hostname: tartarus-layering"),
            "meta-data should carry the local-hostname, got: {meta}",
        );
    }

    #[test]
    fn user_data_starts_with_cloud_config_marker() {
        let user_data = render_user_data();

        assert!(
            user_data.starts_with("#cloud-config\n"),
            "user-data must start with the cloud-config magic comment for cloud-init to parse it, got: {}",
            &user_data[..user_data.len().min(80)],
        );
    }

    #[test]
    fn user_data_lists_every_required_package() {
        let user_data = render_user_data();

        for pkg in [
            "git",
            "gh",
            "tmux",
            "golang",
            "python3",
            "python3-virtualenv",
            "rustup",
            "qemu-guest-agent",
        ] {
            assert!(
                user_data.contains(&format!("- {pkg}\n")),
                "package list should contain {pkg}, got: {user_data}",
            );
        }
    }

    #[test]
    fn user_data_drops_every_guest_script_and_unit() {
        let user_data = render_user_data();

        for (name, _) in GUEST_SCRIPTS {
            assert!(
                user_data.contains(&format!("path: {GUEST_BIN_DIR}/{name}")),
                "user-data should drop {name} into {GUEST_BIN_DIR}, got: {user_data}",
            );
        }
        for (name, _) in GUEST_UNITS {
            assert!(
                user_data.contains(&format!("path: {GUEST_SYSTEMD_DIR}/{name}")),
                "user-data should drop {name} into {GUEST_SYSTEMD_DIR}, got: {user_data}",
            );
        }
    }

    #[test]
    fn user_data_enables_qemu_guest_agent() {
        let user_data = render_user_data();

        assert!(
            user_data.contains("systemctl enable qemu-guest-agent.service"),
            "qemu-guest-agent must be enabled by the layering seed (it is the host->guest control channel), got: {user_data}",
        );
    }

    #[test]
    fn user_data_caches_claude_tarball_at_documented_path() {
        let user_data = render_user_data();

        assert!(
            user_data.contains("npm pack --pack-destination /opt/tartarus/skeleton @anthropic-ai/claude-code"),
            "layering seed should pre-stage @anthropic-ai/claude-code via `npm pack`, got: {user_data}",
        );
        assert!(
            user_data.contains(CLAUDE_TARBALL_PATH),
            "layering seed should rename the pack output to the documented cache path {CLAUDE_TARBALL_PATH}, got: {user_data}",
        );
    }

    #[test]
    fn user_data_powers_off_at_the_end() {
        let user_data = render_user_data();

        assert!(
            user_data.contains("- poweroff\n"),
            "the layering seed must terminate with poweroff, got: {user_data}",
        );
        assert!(
            user_data.contains("mode: poweroff"),
            "layering seed should set power_state.mode = poweroff as the belt-and-braces shutdown trigger, got: {user_data}",
        );
    }

    #[test]
    fn write_seed_files_round_trips_into_workdir() {
        let dir = tempdir();

        let paths = write_seed_files(&dir).expect("write_seed_files should succeed in a fresh tempdir");

        assert_eq!(
            paths.user_data,
            dir.join("user-data"),
            "user-data path should round-trip"
        );
        assert_eq!(
            paths.meta_data,
            dir.join("meta-data"),
            "meta-data path should round-trip"
        );

        let user_data = std::fs::read_to_string(&paths.user_data).expect("user-data should be readable");
        let meta_data = std::fs::read_to_string(&paths.meta_data).expect("meta-data should be readable");

        assert!(
            user_data.starts_with("#cloud-config\n"),
            "round-tripped user-data should keep the magic comment",
        );
        assert!(
            meta_data.contains("instance-id"),
            "round-tripped meta-data should carry the instance-id",
        );
    }

    // ---------------------------------------------------------------------------
    // Test Utilities
    // ---------------------------------------------------------------------------

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-layering-seed-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir");

        path
    }
}
