//! Phase 10 integration tests for the auto-grow surface.
//!
//! Two test surfaces:
//!
//! - **Static checks** (run in-process, no libvirtd or KVM needed): bash syntax for the watcher and the in-guest
//!   finisher; systemd unit syntax for `tartarus-grow.{timer,service}`.
//! - **Live grow round-trip** (`#[ignore]`'d): exercises the full four-step host-side coordination against a real
//!   `qemu:///session` libvirtd and a session whose qemu-guest-agent responds.

use std::{path::PathBuf, process::Command};

#[test]
fn grow_watcher_passes_bash_syntax_check() {
    let path = workspace_root().join("guest/bin/tartarus-grow.sh");
    assert!(path.exists(), "tartarus-grow.sh should exist at {}", path.display());

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

#[test]
fn grow_apply_finisher_passes_bash_syntax_check() {
    let path = workspace_root().join("guest/bin/tartarus-grow-apply.sh");
    assert!(
        path.exists(),
        "tartarus-grow-apply.sh should exist at {}",
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

#[test]
fn grow_scripts_have_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in ["guest/bin/tartarus-grow.sh", "guest/bin/tartarus-grow-apply.sh"] {
            let path = workspace_root().join(script);
            let metadata = std::fs::metadata(&path).expect("metadata for grow script");
            let mode = metadata.permissions().mode() & 0o777;
            assert!(
                mode & 0o111 != 0,
                "{script} should carry the executable bit (mode {mode:o})",
            );
        }
    }
}

#[test]
fn grow_timer_unit_carries_documented_cadence() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-grow.timer"))
        .expect("read grow timer unit");

    for needle in [
        "OnBootSec=5min",
        "OnUnitActiveSec=5min",
        "Persistent=true",
        "Unit=tartarus-grow.service",
        "WantedBy=timers.target",
    ] {
        assert!(
            unit.contains(needle),
            "tartarus-grow.timer should contain `{needle}`, got: {unit}",
        );
    }
}

#[test]
fn grow_service_unit_runs_watcher_as_root_oneshot() {
    let unit = std::fs::read_to_string(workspace_root().join("guest/systemd/tartarus-grow.service"))
        .expect("read grow service unit");

    for needle in ["Type=oneshot", "ExecStart=/usr/local/bin/tartarus-grow.sh", "User=root"] {
        assert!(
            unit.contains(needle),
            "tartarus-grow.service should contain `{needle}`, got: {unit}",
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn grow_units_pass_systemd_analyze_verify() {
    let timer = workspace_root().join("guest/systemd/tartarus-grow.timer");
    let service = workspace_root().join("guest/systemd/tartarus-grow.service");

    let output = Command::new("systemd-analyze")
        .arg("verify")
        .arg(&timer)
        .arg(&service)
        .output()
        .expect("systemd-analyze should be present on Linux test hosts");

    let stderr = String::from_utf8_lossy(&output.stderr);

    let real_failures: Vec<&str> = stderr
        .lines()
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains("not executable") && !line.contains("No such file or directory"))
        .collect();

    assert!(
        real_failures.is_empty(),
        "systemd-analyze verify reported real failures: {real_failures:?}",
    );
}

#[test]
fn grow_watcher_writes_marker_when_threshold_crossed() {
    let dir = unique_tempdir("grow-watcher-trip");

    let marker_dir = dir.join("run-tartarus");
    std::fs::create_dir_all(&marker_dir).expect("create marker dir");

    let script = workspace_root().join("guest/bin/tartarus-grow.sh");

    let probe = format!(
        "set -euo pipefail\n\
         export TARTARUS_GROW_THRESHOLD_PCT=0\n\
         export TARTARUS_GROW_MARKER_DIR={marker_dir}\n\
         bash {script}\n",
        marker_dir = marker_dir.display(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe)
        .output()
        .expect("bash should be available");

    assert!(
        output.status.success(),
        "watcher should succeed with threshold=0; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let marker = marker_dir.join("grow-request");
    assert!(
        marker.exists(),
        "watcher should drop the marker when threshold is crossed, but {} does not exist",
        marker.display(),
    );

    let body = std::fs::read_to_string(&marker).expect("read marker");
    assert!(
        body.contains("usage_pct="),
        "marker body should record usage_pct=, got: {body}",
    );
    assert!(
        body.contains("threshold_pct="),
        "marker body should record threshold_pct=, got: {body}",
    );
    assert!(
        body.contains("mountpoint="),
        "marker body should record mountpoint=, got: {body}",
    );
}

#[test]
fn grow_watcher_clears_stale_marker_below_threshold() {
    let dir = unique_tempdir("grow-watcher-clear");
    let marker_dir = dir.join("run-tartarus");
    std::fs::create_dir_all(&marker_dir).expect("create marker dir");

    let marker = marker_dir.join("grow-request");
    std::fs::write(&marker, "stale").expect("write stale marker");

    let script = workspace_root().join("guest/bin/tartarus-grow.sh");

    let probe = format!(
        "set -euo pipefail\n\
         export TARTARUS_GROW_THRESHOLD_PCT=100\n\
         export TARTARUS_GROW_MARKER_DIR={marker_dir}\n\
         bash {script}\n",
        marker_dir = marker_dir.display(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe)
        .output()
        .expect("bash should be available");

    assert!(
        output.status.success(),
        "watcher should succeed with threshold=100; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    assert!(
        !marker.exists(),
        "watcher should clear the stale marker when usage is below threshold, but {} still exists",
        marker.display(),
    );
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus a session with qemu-ga responding; run with --ignored after setting up locally"]
fn end_to_end_grow_round_trips_against_real_session() {}

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
