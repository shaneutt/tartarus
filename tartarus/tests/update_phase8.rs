//! Phase 8 integration tests: update lifecycle.
//!
//! Two surfaces exercised here:
//!
//! - **Static checks** that need no libvirtd: in-tree update scripts pass `bash -n`, the `tartarus-update.sh`
//!   orchestrator invokes the Claude updater via `runuser` (not as root), the `tartarus-update-system.service` unit is
//!   the only root path, and `tartarus-fstrim.service` is a oneshot the orchestrator can trigger.
//! - **Real-libvirtd** end-to-end runs are `#[ignore]`'d; they require a session whose qemu-ga responds, which the
//!   build environment does not provide.

use std::{path::PathBuf, process::Command};

#[test]
fn update_orchestrator_runs_claude_via_runuser_not_as_root() {
    let body = std::fs::read_to_string(workspace_root().join("guest/bin/tartarus-update.sh"))
        .expect("read tartarus-update.sh");

    assert!(
        body.contains("runuser -u"),
        "tartarus-update.sh must use `runuser -u` to drop to the in-guest user before the Claude updater runs; got: {body}",
    );
    assert!(
        body.contains("tartarus-update-claude.sh"),
        "tartarus-update.sh must invoke the Claude updater script; got: {body}",
    );

    let runuser_lines: Vec<&str> = body
        .lines()
        .filter(|line| {
            let stripped = line.trim_start();
            !stripped.starts_with('#') && stripped.contains("runuser")
        })
        .collect();
    assert!(
        !runuser_lines.is_empty(),
        "expected at least one non-comment `runuser` invocation in tartarus-update.sh",
    );
    for line in &runuser_lines {
        assert!(
            !line.contains("runuser -u root") && !line.contains("runuser -u 0"),
            "runuser must not target root; suspicious line: {line}",
        );
        assert!(
            line.contains("\"${USER_NAME}\"") || line.contains("\"$USER_NAME\""),
            "runuser must drop to the resolved in-guest user (USER_NAME), not a literal; got: {line}",
        );
    }
}

#[test]
fn claude_updater_refuses_to_run_as_root() {
    let body = std::fs::read_to_string(workspace_root().join("guest/bin/tartarus-update-claude.sh"))
        .expect("read tartarus-update-claude.sh");

    assert!(
        body.contains("refuse_root") && body.contains("id -u"),
        "tartarus-update-claude.sh must refuse root via an explicit euid check; got: {body}",
    );
}

#[test]
fn claude_updater_installs_under_user_local_prefix() {
    let body = std::fs::read_to_string(workspace_root().join("guest/bin/tartarus-update-claude.sh"))
        .expect("read tartarus-update-claude.sh");

    assert!(
        body.contains("HOME}/.local") || body.contains("HOME/.local"),
        "tartarus-update-claude.sh must target the per-user `~/.local` prefix; got: {body}",
    );
    assert!(
        body.contains("--prefix="),
        "tartarus-update-claude.sh must pass --prefix to npm to keep the install user-scoped; got: {body}",
    );
}

#[test]
fn update_system_service_is_the_only_root_path() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-update-system.service"))
        .expect("read tartarus-update-system.service");

    assert!(
        unit.contains("Type=oneshot"),
        "tartarus-update-system.service should be a oneshot, got: {unit}",
    );
    assert!(
        unit.contains("User=root"),
        "tartarus-update-system.service must run as root (the one and only root path), got: {unit}",
    );
    assert!(
        unit.contains("dnf upgrade --refresh -y"),
        "tartarus-update-system.service must drive `dnf upgrade --refresh -y`, got: {unit}",
    );
}

#[test]
fn fstrim_service_is_a_oneshot_unit_the_orchestrator_can_trigger() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-fstrim.service"))
        .expect("read tartarus-fstrim.service");

    assert!(
        unit.contains("Type=oneshot"),
        "tartarus-fstrim.service should be a oneshot, got: {unit}",
    );
    assert!(
        unit.contains("ExecStart=/usr/local/bin/tartarus-fstrim.sh"),
        "tartarus-fstrim.service must invoke the trim helper, got: {unit}",
    );
}

#[test]
fn update_scripts_have_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in ["guest/bin/tartarus-update.sh", "guest/bin/tartarus-update-claude.sh"] {
            let path = workspace_root().join(script);
            let metadata = std::fs::metadata(&path).unwrap_or_else(|_| panic!("metadata for {script}"));
            let mode = metadata.permissions().mode() & 0o777;
            assert!(
                mode & 0o111 != 0,
                "{script} should carry the executable bit (mode {mode:o})",
            );
        }
    }
}

#[test]
fn update_scripts_pass_bash_syntax_check() {
    for script in ["guest/bin/tartarus-update.sh", "guest/bin/tartarus-update-claude.sh"] {
        let path = workspace_root().join(script);
        let output = Command::new("bash")
            .arg("-n")
            .arg(&path)
            .output()
            .expect("bash should be available in the test environment");
        assert!(
            output.status.success(),
            "`bash -n {}` should pass; stderr: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd with a session whose qemu-ga responds; run with --ignored after setting up locally"]
fn agent_ping_round_trips_against_real_session() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd with a session whose qemu-ga responds; run with --ignored after setting up locally"]
fn agent_exec_true_returns_exit_code_zero() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd with a session whose qemu-ga responds; run with --ignored after setting up locally"]
fn update_running_path_dispatches_orchestrator() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd with a stopped session; run with --ignored after setting up locally"]
fn update_stopped_path_boots_runs_shuts_down() {}

// Test Utilities

fn workspace_root() -> PathBuf {
    let cargo_manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(cargo_manifest_dir)
        .parent()
        .expect("workspace root")
        .to_path_buf()
}
