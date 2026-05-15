//! `genisoimage` abstraction.
//!
//! Mirrors [`crate::disk::qemu_img`]: a trait with a production
//! shell-out impl and a test-only recorder so the seed-ISO authoring
//! path can be unit-tested without `genisoimage` installed.

use std::{path::Path, process::Command};

// -----------------------------------------------------------------------------
// GenisoimageOutput
// -----------------------------------------------------------------------------

/// Captured `genisoimage` result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenisoimageOutput {
    /// True when the process exited 0 (or the fake said so).
    pub success: bool,

    /// Captured stderr bytes (used to build the error detail).
    pub stderr: Vec<u8>,
}

impl GenisoimageOutput {
    /// Trimmed stderr as a string.
    pub fn stderr_trim(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_owned()
    }
}

// -----------------------------------------------------------------------------
// Genisoimage
// -----------------------------------------------------------------------------

/// `genisoimage` runner.
pub trait Genisoimage {
    /// Run `genisoimage` against `workdir` (containing `user-data`
    /// and `meta-data`) and write the cloud-init ISO to `iso`.
    fn write_iso(&self, workdir: &Path, iso: &Path) -> std::io::Result<GenisoimageOutput>;
}

// -----------------------------------------------------------------------------
// KernelGenisoimage
// -----------------------------------------------------------------------------

/// Production impl: shells out to the real `genisoimage` on `PATH`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KernelGenisoimage;

impl Genisoimage for KernelGenisoimage {
    fn write_iso(&self, workdir: &Path, iso: &Path) -> std::io::Result<GenisoimageOutput> {
        let output = Command::new("genisoimage")
            .arg("-output")
            .arg(iso)
            .arg("-volid")
            .arg("cidata")
            .arg("-joliet")
            .arg("-rock")
            .arg("user-data")
            .arg("meta-data")
            .current_dir(workdir)
            .output()?;
        Ok(GenisoimageOutput {
            success: output.status.success(),
            stderr: output.stderr,
        })
    }
}

// -----------------------------------------------------------------------------
// Test Recorder
// -----------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod recorder {
    //! Test-only fake `Genisoimage`.

    use std::{
        cell::RefCell,
        path::{Path, PathBuf},
    };

    use super::{Genisoimage, GenisoimageOutput};

    /// Captured invocation.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Call {
        /// Working directory the call was made from.
        pub workdir: PathBuf,

        /// ISO path the call was asked to write.
        pub iso: PathBuf,
    }

    /// Recording fake `Genisoimage`.
    pub struct Recorder {
        /// In-order call log.
        pub calls: RefCell<Vec<Call>>,

        /// Scripted outputs (LIFO; empty = generic success).
        pub responses: RefCell<Vec<GenisoimageOutput>>,

        /// Whether to actually create an empty ISO file at the
        /// requested path on every call. Useful for tests that need
        /// the file to exist on disk afterwards.
        pub create_iso_file: bool,
    }

    impl Default for Recorder {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Recorder {
        /// Empty recorder; every call gets a generic success and no
        /// file on disk.
        pub fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                responses: RefCell::new(Vec::new()),
                create_iso_file: false,
            }
        }

        /// Variant that also creates an empty ISO file on disk on
        /// every call, so callers using `iso.exists()` work.
        pub fn with_file_writes() -> Self {
            Self {
                create_iso_file: true,
                ..Self::new()
            }
        }

        /// Push a failing response onto the queue.
        pub fn enqueue_err(&self, stderr: impl Into<Vec<u8>>) {
            self.responses.borrow_mut().push(GenisoimageOutput {
                success: false,
                stderr: stderr.into(),
            });
        }

        fn next_response(&self) -> GenisoimageOutput {
            self.responses.borrow_mut().pop().unwrap_or(GenisoimageOutput {
                success: true,
                stderr: Vec::new(),
            })
        }
    }

    impl Genisoimage for Recorder {
        fn write_iso(&self, workdir: &Path, iso: &Path) -> std::io::Result<GenisoimageOutput> {
            self.calls.borrow_mut().push(Call {
                workdir: workdir.to_path_buf(),
                iso: iso.to_path_buf(),
            });
            let response = self.next_response();
            if self.create_iso_file && response.success {
                std::fs::write(iso, b"CD001 fake iso")?;
            }
            Ok(response)
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::genisoimage::recorder::Recorder;

    #[test]
    fn recorder_logs_calls_and_defaults_to_success() {
        let recorder = Recorder::new();
        let result = recorder
            .write_iso(Path::new("/work"), Path::new("/work/out.iso"))
            .expect("unscripted call should succeed");
        assert!(result.success);
        assert_eq!(recorder.calls.borrow().len(), 1);
    }

    #[test]
    fn recorder_with_file_writes_creates_an_iso_on_disk() {
        let dir = std::env::temp_dir().join(format!("tartarus-recorder-{pid}", pid = std::process::id()));
        std::fs::create_dir_all(&dir).expect("tempdir");
        let iso = dir.join("seed.iso");
        let recorder = Recorder::with_file_writes();
        let _ = recorder.write_iso(&dir, &iso).expect("write should succeed");
        assert!(iso.exists(), "with_file_writes should leave a file on disk");
        let _ = std::fs::remove_file(&iso);
    }

    #[test]
    fn recorder_enqueue_err_yields_failure_response() {
        let recorder = Recorder::new();
        recorder.enqueue_err("permission denied");
        let result = recorder
            .write_iso(Path::new("/a"), Path::new("/a/b.iso"))
            .expect("the underlying call still returns Ok, status carries failure");
        assert!(!result.success);
        assert_eq!(result.stderr_trim(), "permission denied");
    }

    #[test]
    fn genisoimage_output_stderr_trim_strips_whitespace() {
        let output = GenisoimageOutput {
            success: false,
            stderr: b"\n  bad option\n".to_vec(),
        };
        assert_eq!(output.stderr_trim(), "bad option");
    }
}
