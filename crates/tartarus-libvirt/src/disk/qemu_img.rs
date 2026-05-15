//! `qemu-img` abstraction.
//!
//! Production callers shell out via [`KernelQemuImg`]; tests inject
//! a [`recorder::Recorder`] to capture invocations and script
//! responses without needing the real binary on `PATH`.
//!
//! All three operations Tartarus performs (`create`, `info`,
//! `resize`) flow through this trait so the disk subsystem can be
//! unit-tested without `qemu-img` installed.

use std::{path::Path, process::Command};

// -----------------------------------------------------------------------------
// QemuImgOutput
// -----------------------------------------------------------------------------

/// Captured `qemu-img` result.
///
/// Mirrors the subset of `std::process::Output` callers actually
/// consume (stdout + a Boolean success). Fake impls fabricate this
/// without spawning anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QemuImgOutput {
    /// True when the process exited 0 (or the fake impl said so).
    pub success: bool,

    /// Captured stdout bytes.
    pub stdout: Vec<u8>,

    /// Captured stderr bytes.
    pub stderr: Vec<u8>,
}

impl QemuImgOutput {
    /// Trimmed stderr as a string, for embedding in error messages.
    pub fn stderr_trim(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

// -----------------------------------------------------------------------------
// QemuImg
// -----------------------------------------------------------------------------

/// `qemu-img` runner.
///
/// Tests construct a fake impl; production passes [`KernelQemuImg`].
pub trait QemuImg {
    /// Run `qemu-img create -f qcow2 -b <base> -F qcow2 <path>
    /// <size>G`. Returns the spawn outcome verbatim so the caller
    /// can interpret a non-zero exit cleanly.
    fn create_overlay(&self, path: &Path, base: &Path, size_gib: u32) -> std::io::Result<QemuImgOutput>;

    /// Run `qemu-img info --output=json <path>`.
    fn info_json(&self, path: &Path) -> std::io::Result<QemuImgOutput>;

    /// Run `qemu-img resize <path> <size>G` (offline resize).
    fn resize(&self, path: &Path, new_size_gib: u32) -> std::io::Result<QemuImgOutput>;
}

// -----------------------------------------------------------------------------
// KernelQemuImg
// -----------------------------------------------------------------------------

/// Production impl: shells out to the real `qemu-img` on `PATH`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KernelQemuImg;

impl QemuImg for KernelQemuImg {
    fn create_overlay(&self, path: &Path, base: &Path, size_gib: u32) -> std::io::Result<QemuImgOutput> {
        let output = Command::new("qemu-img")
            .args(create_args(path, base, size_gib))
            .output()?;
        Ok(QemuImgOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn info_json(&self, path: &Path) -> std::io::Result<QemuImgOutput> {
        let output = Command::new("qemu-img")
            .arg("info")
            .arg("--output=json")
            .arg(path)
            .output()?;
        Ok(QemuImgOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }

    fn resize(&self, path: &Path, new_size_gib: u32) -> std::io::Result<QemuImgOutput> {
        let output = Command::new("qemu-img")
            .arg("resize")
            .arg(path)
            .arg(format!("{new_size_gib}G"))
            .output()?;
        Ok(QemuImgOutput {
            success: output.status.success(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

// -----------------------------------------------------------------------------
// Argument Builders
// -----------------------------------------------------------------------------

/// Build the `qemu-img create` argument vector. Kept as a free
/// function so unit tests can assert the exact invocation matches
/// the spec without spawning anything.
pub fn create_args(path: &Path, base: &Path, size_gib: u32) -> Vec<String> {
    vec![
        "create".to_owned(),
        "-f".to_owned(),
        "qcow2".to_owned(),
        "-b".to_owned(),
        base.display().to_string(),
        "-F".to_owned(),
        "qcow2".to_owned(),
        path.display().to_string(),
        format!("{size_gib}G"),
    ]
}

// -----------------------------------------------------------------------------
// Test Recorder
// -----------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod recorder {
    //! Test-only fake [`QemuImg`].

    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };

    use super::{QemuImg, QemuImgOutput};

    /// Captured call.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Call {
        /// `create -f qcow2 -b <base> ...`.
        Create {
            path: PathBuf,
            base: PathBuf,
            size_gib: u32,
        },

        /// `info --output=json <path>`.
        Info { path: PathBuf },

        /// `resize <path> <new_size_gib>G`.
        Resize { path: PathBuf, new_size_gib: u32 },
    }

    /// Recording fake `QemuImg`.
    pub struct Recorder {
        /// In-order invocation log.
        pub calls: RefCell<Vec<Call>>,

        /// Scripted [`QemuImgOutput`]s, popped from the front per
        /// call. When empty, every call returns a generic success
        /// with empty stdout.
        pub responses: RefCell<Vec<QemuImgOutput>>,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Recorder {
        /// Empty recorder; every call gets a generic success.
        pub fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                responses: RefCell::new(Vec::new()),
            }
        }

        /// Push a scripted response onto the queue.
        pub fn enqueue(&self, output: QemuImgOutput) {
            self.responses.borrow_mut().push(output);
        }

        /// Shortcut for "this call succeeds with `stdout`".
        pub fn enqueue_ok(&self, stdout: impl Into<Vec<u8>>) {
            self.enqueue(QemuImgOutput {
                success: true,
                stdout: stdout.into(),
                stderr: Vec::new(),
            });
        }

        /// Shortcut for "this call fails with `stderr` text".
        pub fn enqueue_err(&self, stderr: impl Into<Vec<u8>>) {
            self.enqueue(QemuImgOutput {
                success: false,
                stdout: Vec::new(),
                stderr: stderr.into(),
            });
        }

        fn next_response(&self) -> QemuImgOutput {
            self.responses.borrow_mut().pop().unwrap_or(QemuImgOutput {
                success: true,
                stdout: Vec::new(),
                stderr: Vec::new(),
            })
        }
    }

    impl QemuImg for Recorder {
        fn create_overlay(&self, path: &Path, base: &Path, size_gib: u32) -> std::io::Result<QemuImgOutput> {
            self.calls.borrow_mut().push(Call::Create {
                path: path.to_path_buf(),
                base: base.to_path_buf(),
                size_gib,
            });
            Ok(self.next_response())
        }

        fn info_json(&self, path: &Path) -> std::io::Result<QemuImgOutput> {
            self.calls.borrow_mut().push(Call::Info {
                path: path.to_path_buf(),
            });
            Ok(self.next_response())
        }

        fn resize(&self, path: &Path, new_size_gib: u32) -> std::io::Result<QemuImgOutput> {
            self.calls.borrow_mut().push(Call::Resize {
                path: path.to_path_buf(),
                new_size_gib,
            });
            Ok(self.next_response())
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::qemu_img::recorder::{Call, Recorder};

    #[test]
    fn create_args_match_documented_invocation() {
        let args = create_args(
            Path::new("/data/sessions/by-uuid/abc/overlay.qcow2"),
            Path::new("/data/base/fedora-41-2026-05-01.qcow2"),
            100,
        );
        assert_eq!(args[0], "create");
        assert_eq!(args[2], "qcow2");
        assert_eq!(args[4], "/data/base/fedora-41-2026-05-01.qcow2");
        assert_eq!(args.last().map(String::as_str), Some("100G"));
    }

    #[test]
    fn recorder_logs_calls_in_order() {
        let recorder = Recorder::new();
        let _ = recorder.create_overlay(Path::new("/a.qcow2"), Path::new("/b.qcow2"), 10);
        let _ = recorder.info_json(Path::new("/a.qcow2"));
        let _ = recorder.resize(Path::new("/a.qcow2"), 20);
        let calls = recorder.calls.borrow();
        assert_eq!(calls.len(), 3);
        assert!(matches!(calls[0], Call::Create { size_gib: 10, .. }));
        assert!(matches!(calls[1], Call::Info { .. }));
        assert!(matches!(calls[2], Call::Resize { new_size_gib: 20, .. }));
    }

    #[test]
    fn recorder_returns_default_success_when_unscripted() {
        let recorder = Recorder::new();
        let output = recorder
            .create_overlay(Path::new("/x"), Path::new("/y"), 1)
            .expect("unscripted call should still succeed");
        assert!(output.success);
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn qemu_img_output_stderr_trim_strips_whitespace() {
        let output = QemuImgOutput {
            success: false,
            stdout: vec![],
            stderr: b"\n  error: bad arg\n".to_vec(),
        };
        assert_eq!(output.stderr_trim(), "error: bad arg");
    }
}
