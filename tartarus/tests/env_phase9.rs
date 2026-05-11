//! Phase 9 integration tests: env add / env update.
//!
//! Two test surfaces:
//!
//! - **Static checks** that need no libvirtd: in-tree env scripts pass `bash -n`, the executable bit is set, the
//!   "already present" branch in `tartarus-env-add.sh` short-circuits without invoking dnf or rustup, and the
//!   `tartarus-env-update.sh` script skips uninstalled envs without invoking dnf.
//! - **Real-libvirtd** end-to-end runs are `#[ignore]`'d; they require a session whose qemu-ga responds, which the
//!   build environment does not provide.

use std::{path::PathBuf, process::Command};

#[test]
fn env_scripts_pass_bash_syntax_check() {
    for script in ["guest/bin/tartarus-env-add.sh", "guest/bin/tartarus-env-update.sh"] {
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
fn env_scripts_have_executable_bit() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for script in ["guest/bin/tartarus-env-add.sh", "guest/bin/tartarus-env-update.sh"] {
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
fn env_add_short_circuits_when_env_already_present() {
    let repo_root = workspace_root();
    let script = repo_root.join("guest/bin/tartarus-env-add.sh");

    let dir = unique_tempdir("env-phase9-rust-present");
    let fake_bin = dir.join("bin");
    std::fs::create_dir_all(&fake_bin).expect("create fake bin dir");

    write_executable(&fake_bin.join("rustup"), "#!/bin/bash\necho 'rustup 1.0' >&2\nexit 0\n");
    write_executable(
        &fake_bin.join("runuser"),
        &format!(
            "#!/bin/bash\nshift; shift; shift\nexec env PATH={fake_bin}:$PATH \"$@\"\n",
            fake_bin = fake_bin.display(),
        ),
    );
    write_executable(
        &fake_bin.join("dnf"),
        "#!/bin/bash\necho 'dnf MUST NOT BE CALLED' >&2\nexit 99\n",
    );
    write_executable(
        &fake_bin.join("rustup-init"),
        "#!/bin/bash\necho 'init MUST NOT BE CALLED' >&2\nexit 99\n",
    );

    let probe = format!(
        "set -euo pipefail\n\
         export PATH={fake_bin}:$PATH\n\
         export TARTARUS_USER={user}\n\
         {script} rust\n",
        fake_bin = fake_bin.display(),
        user = current_user(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe)
        .output()
        .expect("bash should be available in the test environment");

    assert!(
        output.status.success(),
        "env-add should succeed when rust is already present; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rust already present"),
        "env-add should log 'rust already present'; got stderr: {stderr}",
    );
    assert!(
        !stderr.contains("MUST NOT BE CALLED"),
        "the install branch must not run when rust is already present; got stderr: {stderr}",
    );
}

#[test]
fn env_update_skips_when_env_not_installed() {
    let repo_root = workspace_root();
    let script = repo_root.join("guest/bin/tartarus-env-update.sh");

    let dir = unique_tempdir("env-phase9-rust-absent");
    let fake_bin = dir.join("bin");
    std::fs::create_dir_all(&fake_bin).expect("create fake bin dir");

    write_executable(
        &fake_bin.join("runuser"),
        &format!(
            "#!/bin/bash\nshift; shift; shift\nexec env PATH={fake_bin}:$PATH \"$@\"\n",
            fake_bin = fake_bin.display(),
        ),
    );
    write_executable(
        &fake_bin.join("dnf"),
        "#!/bin/bash\necho 'dnf MUST NOT BE CALLED' >&2\nexit 99\n",
    );
    write_executable(&fake_bin.join("rpm"), "#!/bin/bash\nexit 1\n");
    write_executable(&fake_bin.join("go"), "#!/bin/bash\nexit 1\n");
    write_executable(&fake_bin.join("rustup"), "#!/bin/bash\nexit 1\n");

    let probe = format!(
        "set -euo pipefail\n\
         export PATH={fake_bin}:/usr/bin:/bin\n\
         export TARTARUS_USER={user}\n\
         {script}\n",
        fake_bin = fake_bin.display(),
        user = current_user(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe)
        .output()
        .expect("bash should be available in the test environment");

    assert!(
        output.status.success(),
        "env-update should succeed when no env is installed; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("MUST NOT BE CALLED"),
        "the update branch must not invoke dnf when nothing is installed; got stderr: {stderr}",
    );
}

#[test]
fn env_add_rejects_unknown_env() {
    let repo_root = workspace_root();
    let script = repo_root.join("guest/bin/tartarus-env-add.sh");

    let probe = format!(
        "set -euo pipefail\n\
         export TARTARUS_USER={user}\n\
         {script} haskell\n",
        user = current_user(),
        script = script.display(),
    );

    let output = Command::new("bash")
        .arg("-c")
        .arg(&probe)
        .output()
        .expect("bash should be available in the test environment");

    assert!(
        !output.status.success(),
        "env-add must reject an unknown env name with a non-zero exit, got exit {:?}",
        output.status.code(),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown env"),
        "env-add must explain the unknown-env failure; got stderr: {stderr}",
    );
}

#[test]
fn python_present_probe_checks_rpm_not_stdlib_venv() {
    for script in ["guest/bin/tartarus-env-add.sh", "guest/bin/tartarus-env-update.sh"] {
        let body = std::fs::read_to_string(workspace_root().join(script)).unwrap_or_else(|_| panic!("read {script}"));

        assert!(
            body.contains("rpm -q python3-virtualenv"),
            "{script} python probe must check `rpm -q python3-virtualenv` because python3 + venv ship in every Fedora image; got: {body}",
        );
        assert!(
            !body.contains("python3 -m venv --help"),
            "{script} must not probe `python3 -m venv --help` — it is a stdlib module and always succeeds; got: {body}",
        );
    }
}

#[test]
fn env_scripts_drop_to_user_not_root_for_per_user_steps() {
    for script in ["guest/bin/tartarus-env-add.sh", "guest/bin/tartarus-env-update.sh"] {
        let body = std::fs::read_to_string(workspace_root().join(script)).unwrap_or_else(|_| panic!("read {script}"));

        let runuser_lines: Vec<&str> = body
            .lines()
            .filter(|line| {
                let stripped = line.trim_start();
                !stripped.starts_with('#') && stripped.contains("runuser")
            })
            .collect();
        assert!(
            !runuser_lines.is_empty(),
            "{script} must use `runuser -u` for per-user steps; no non-comment runuser invocations found",
        );
        for line in &runuser_lines {
            assert!(
                !line.contains("runuser -u root") && !line.contains("runuser -u 0"),
                "{script} must not target root via runuser; suspicious line: {line}",
            );
            assert!(
                line.contains("\"${USER_NAME}\"") || line.contains("\"$USER_NAME\""),
                "{script} must drop to the resolved in-guest user (USER_NAME), not a literal; got: {line}",
            );
        }
    }
}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus a session whose qemu-ga responds; run with --ignored after setting up locally"]
fn env_add_installs_rust_into_real_session() {}

#[test]
#[ignore = "requires a running qemu:///session libvirtd plus a session whose qemu-ga responds; run with --ignored after setting up locally"]
fn env_update_is_idempotent_against_real_session() {}

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

fn write_executable(path: &PathBuf, body: &str) {
    std::fs::write(path, body).expect("write fake bin");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod fake bin");
    }
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".to_owned())
}
