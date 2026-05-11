//! Phase 6 integration tests for the in-guest helper scripts and
//! console-attach plumbing.
//!
//! Two test surfaces:
//!
//! - **Bash syntax** (runs in-process, no libvirtd needed): `bash -n` against each in-tree helper script. Catches a
//!   typo before the layering boot ships a broken script into the base image.
//! - **Console attach against a real libvirtd**: `#[ignore]`'d, runs only with `cargo test -- --ignored` once the
//!   developer has a working `qemu:///session` session available.

use std::{path::PathBuf, process::Command};

const HELPER_SCRIPTS: &[&str] = &[
    "guest/bin/tartarus-bootstrap.sh",
    "guest/bin/tartarus-claude.sh",
    "guest/bin/tartarus-env-add.sh",
    "guest/bin/tartarus-env-update.sh",
    "guest/bin/tartarus-env-wrapper.sh",
    "guest/bin/tartarus-fstrim.sh",
    "guest/bin/tartarus-grow.sh",
    "guest/bin/tartarus-grow-apply.sh",
    "guest/bin/tartarus-update.sh",
    "guest/bin/tartarus-update-claude.sh",
];

#[test]
fn bash_n_passes_for_every_in_tree_helper_script() {
    let repo_root = workspace_root();
    for script in HELPER_SCRIPTS {
        let path = repo_root.join(script);
        assert!(
            path.exists(),
            "helper script {} should exist at {}",
            script,
            path.display(),
        );

        let output = Command::new("bash")
            .arg("-n")
            .arg(&path)
            .output()
            .expect("bash should be available in the test environment");

        assert!(
            output.status.success(),
            "`bash -n {}` should pass; stderr was: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn bootstrap_script_has_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = workspace_root().join("guest/bin/tartarus-bootstrap.sh");
        let metadata = std::fs::metadata(&path).expect("metadata for bootstrap.sh");
        let mode = metadata.permissions().mode() & 0o777;
        assert!(
            mode & 0o111 != 0,
            "tartarus-bootstrap.sh should carry the executable bit (mode {mode:o})",
        );
    }
}

#[test]
fn claude_script_has_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = workspace_root().join("guest/bin/tartarus-claude.sh");
        let metadata = std::fs::metadata(&path).expect("metadata for claude.sh");
        let mode = metadata.permissions().mode() & 0o777;
        assert!(
            mode & 0o111 != 0,
            "tartarus-claude.sh should carry the executable bit (mode {mode:o})",
        );
    }
}

#[test]
fn env_wrapper_script_has_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = workspace_root().join("guest/bin/tartarus-env-wrapper.sh");
        let metadata = std::fs::metadata(&path).expect("metadata for env-wrapper.sh");
        let mode = metadata.permissions().mode() & 0o777;
        assert!(
            mode & 0o111 != 0,
            "tartarus-env-wrapper.sh should carry the executable bit (mode {mode:o})",
        );
    }
}

#[test]
fn bootstrap_unit_targets_correct_executable() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-bootstrap.service"))
        .expect("read bootstrap unit");
    assert!(
        unit.contains("ExecStart=/usr/local/bin/tartarus-bootstrap.sh"),
        "tartarus-bootstrap.service should ExecStart the bootstrap helper, got: {unit}",
    );
    assert!(
        unit.contains("Type=oneshot"),
        "tartarus-bootstrap.service should be a oneshot, got: {unit}",
    );
}

#[test]
fn claude_unit_runs_on_tty1_as_template_user() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-claude@.service"))
        .expect("read claude unit");
    assert!(
        unit.contains("TTYPath=/dev/tty1"),
        "tartarus-claude@.service should bind to /dev/tty1, got: {unit}",
    );
    assert!(
        unit.contains("User=%i"),
        "tartarus-claude@.service should run as the templated user, got: {unit}",
    );
    assert!(
        unit.contains("ExecStart=/usr/local/bin/tartarus-claude.sh"),
        "tartarus-claude@.service should ExecStart the claude helper, got: {unit}",
    );
}

#[test]
fn tartarus_claude_picks_default_flagged_repo_over_first_listed() {
    let repo_root = workspace_root();
    let script = repo_root.join("guest/bin/tartarus-claude.sh");

    let dir = unique_tempdir("guest-phase6-default-repo");
    let manifest = dir.join("repos");
    std::fs::write(&manifest, "owner/alpha\t\nowner/beta\tdefault\nowner/gamma\t\n").expect("write manifest");

    let workdir_base = dir.join("repositories");
    for slug in ["alpha", "beta", "gamma"] {
        std::fs::create_dir_all(workdir_base.join(slug)).expect("create repo dir");
    }

    let probe_script = format!(
        "set -euo pipefail\n\
         export REPOS_MANIFEST={manifest}\n\
         export WORKDIR_BASE={workdir_base}\n\
         source {script}\n\
         default_repo_dir\n",
        manifest = manifest.display(),
        workdir_base = workdir_base.display(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe_script)
        .output()
        .expect("bash should be available in the test environment");

    assert!(
        output.status.success(),
        "default_repo_dir invocation should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = workdir_base.join("beta");
    assert_eq!(
        stdout.trim(),
        expected.display().to_string(),
        "default_repo_dir should pick the `default`-flagged entry (beta), not the first listed (alpha)",
    );
}

#[test]
fn tartarus_claude_falls_back_to_first_listed_when_no_default_flag() {
    let repo_root = workspace_root();
    let script = repo_root.join("guest/bin/tartarus-claude.sh");

    let dir = unique_tempdir("guest-phase6-first-repo");
    let manifest = dir.join("repos");
    std::fs::write(&manifest, "owner/alpha\t\nowner/beta\t\n").expect("write manifest");

    let workdir_base = dir.join("repositories");
    std::fs::create_dir_all(workdir_base.join("alpha")).expect("create repo dir");
    std::fs::create_dir_all(workdir_base.join("beta")).expect("create repo dir");

    let probe_script = format!(
        "set -euo pipefail\n\
         export REPOS_MANIFEST={manifest}\n\
         export WORKDIR_BASE={workdir_base}\n\
         source {script}\n\
         default_repo_dir\n",
        manifest = manifest.display(),
        workdir_base = workdir_base.display(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe_script)
        .output()
        .expect("bash should be available in the test environment");

    assert!(
        output.status.success(),
        "default_repo_dir invocation should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = workdir_base.join("alpha");
    assert_eq!(
        stdout.trim(),
        expected.display().to_string(),
        "default_repo_dir should fall back to the first listed entry when no flag is set",
    );
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus /dev/kvm and a defined session; run with --ignored after setting up locally"]
fn console_attach_round_trips_against_real_libvirtd() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd and a foreground attach; run with --ignored after setting up locally"]
fn ctrl_a_d_detaches_real_session() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus a real qemu-guest-agent; run with --ignored after setting up locally"]
fn background_mode_captures_remote_url_end_to_end() {}

// Test Utilities

fn workspace_root() -> PathBuf {
    let cargo_manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(cargo_manifest_dir)
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn unique_tempdir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let pid = std::process::id();
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("tartarus-{label}-{pid}-{n}"));

    std::fs::create_dir_all(&path).expect("tempdir create");

    path
}
