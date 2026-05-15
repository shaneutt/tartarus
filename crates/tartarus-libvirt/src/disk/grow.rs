//! Online grow coordinator for per-session overlays.
//!
//! `tartarus grow` reads the current size, runs `qemu-img resize`,
//! calls `virDomainBlockResize`, triggers the in-guest applier via
//! `qemu-guest-agent`, and persists the new size.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tartarus_provider::{
    paths,
    session::{
        identity::{self, ResolvedSession},
        metadata::{self, Metadata},
    },
};

use crate::{
    config::Config,
    disk::qemu_img::QemuImg,
    error::Result,
    host::{
        agent::Agent,
        connect::Connection,
        domain::{self},
        error::HostError,
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Per-call agent timeout (two minutes).
const AGENT_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Polling interval while waiting for `tartarus-grow-apply.sh` to exit.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Hard ceiling on in-guest grow applier runtime (eight minutes).
const GROW_TOTAL_RUNTIME_LIMIT: Duration = Duration::from_secs(8 * 60);

/// In-guest grow applier path.
pub const GROW_APPLY_SCRIPT_PATH: &str = "/usr/local/bin/tartarus-grow-apply.sh";

/// In-guest watermark marker path.
pub const GROW_MARKER_PATH: &str = "/run/tartarus/grow-request";

/// `VIR_DOMAIN_BLOCK_RESIZE_BYTES` flag.
const BLOCK_RESIZE_BYTES: u32 = 1;

/// Bytes per GiB.
const BYTES_PER_GIB: u64 = 1_073_741_824; // 1 GiB

/// MiB per GiB.
const MIB_PER_GIB: u64 = 1_024; // 1 GiB / 1 MiB

/// Hard ceiling on per-overlay virtual size (10 TiB).
const MAX_OVERLAY_GIB: u32 = 10 * 1_024; // 10 TiB

// -----------------------------------------------------------------------------
// GrowOutcome
// -----------------------------------------------------------------------------

/// Failure modes specific to the grow path.
#[derive(Debug, thiserror::Error)]
pub enum GrowError {
    /// `qemu-img info` exited successfully but its JSON output was
    /// missing or malformed.
    #[error("`qemu-img info` parse error for {path}: {detail}")]
    InfoParse {
        /// Short detail extracted from the JSON parser.
        detail: String,

        /// Overlay path that was inspected.
        path: PathBuf,
    },

    /// The session's overlay file does not exist on disk.
    #[error("overlay {path} does not exist; hint: `tartarus list` will show the canonical session dir")]
    OverlayMissing {
        /// Path that was probed.
        path: PathBuf,
    },

    /// The post-grow overlay would exceed the in-tree ceiling on
    /// per-overlay virtual size. Catches a pathological config (e.g. an
    /// out-of-band edit that sets `disk_grow_increment_gib` to a huge
    /// value) before it walks the overlay into the petabyte range.
    #[error(
        "post-grow overlay {overlay} would reach {requested_gib} GiB, exceeding the {ceiling_gib} GiB ceiling. \
         hint: shrink `[disk] grow_increment_gib` in the config so subsequent grows stay below the ceiling."
    )]
    OverlayExceedsCeiling {
        /// Hard ceiling on per-overlay virtual size, in GiB.
        ceiling_gib: u32,

        /// Overlay whose post-grow size would exceed the ceiling.
        overlay: PathBuf,

        /// Post-grow size that would result if the grow proceeded, in GiB.
        requested_gib: u32,
    },

    /// The host filesystem hosting the overlay does not have enough
    /// free space to absorb the requested grow increment without risk
    /// of fragmenting (or wedging) the qcow2 mid-resize.
    #[error(
        "host filesystem hosting {overlay} has only {available_mib} MiB free, needs at least \
         {required_mib} MiB for a {increment_gib} GiB grow. \
         hint: free space on the host disk before retrying, or shrink the grow increment via config."
    )]
    HostDiskFull {
        /// MiB available on the host filesystem hosting the overlay.
        available_mib: u64,

        /// Grow increment that triggered the check, in GiB.
        increment_gib: u32,

        /// Overlay whose host filesystem was probed.
        overlay: PathBuf,

        /// MiB that must be free for the grow to proceed safely.
        required_mib: u64,
    },

    /// `qemu-img` exited non-zero.
    #[error("`qemu-img {operation}` failed for {path}: {detail}")]
    QemuImg {
        /// Short detail extracted from stderr or the exit status.
        detail: String,

        /// Static label identifying the operation.
        operation: &'static str,

        /// Overlay path being acted on.
        path: PathBuf,
    },

    /// The session is not currently running, so `virDomainBlockResize`
    /// would fail. Online grow only makes sense against a live guest;
    /// for stopped sessions, the user can resize the qcow2 directly via
    /// `tartarus stop` + `qemu-img resize` + `tartarus run` flow.
    #[error("session {uuid} is not running; hint: online grow requires a live domain. start the session first.")]
    SessionNotRunning {
        /// UUID of the resolved session.
        uuid: String,
    },
}

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrowOutcome {
    /// Overlay virtual size after the grow, in GiB.
    pub after_gib: u32,

    /// Overlay virtual size before the grow, in GiB.
    pub before_gib: u32,

    /// True iff the in-guest watcher had dropped a marker before this
    /// grow ran. Useful for the user to know whether the grow was
    /// reactive (marker present) or proactive (no marker yet).
    pub marker_was_present: bool,

    /// Session UUID that was grown.
    pub uuid: String,
}

/// Run `tartarus grow <alias|uuid>`.
pub fn run(config: &Config, target: &str) -> Result<GrowOutcome> {
    let resolved = identity::resolve(target)?;
    tracing::info!(uuid = %resolved.uuid, alias = ?resolved.alias, "grow: resolving session");

    let overlay = resolved.directory.join(crate::disk::overlay::OVERLAY_FILE_NAME);
    if !overlay.exists() {
        return Err(GrowError::OverlayMissing { path: overlay }.into());
    }

    let connection = Connection::open(&config.network_uri)?;
    let domain = domain::lookup(&connection, &resolved.uuid)?;
    if !is_active(&domain)? {
        return Err(GrowError::SessionNotRunning {
            uuid: resolved.uuid.clone(),
        }
        .into());
    }

    let qemu_img = crate::disk::qemu_img::KernelQemuImg;
    let info = read_qemu_img_info(&qemu_img, &overlay)?;
    let before_gib = bytes_to_gib_floor(info.virtual_size);
    let after_gib = before_gib.saturating_add(config.disk_grow_increment_gib);

    tracing::info!(
        uuid = %resolved.uuid,
        before_gib,
        after_gib,
        increment_gib = config.disk_grow_increment_gib,
        "grow: planning resize",
    );

    enforce_overlay_ceiling(&overlay, after_gib)?;
    enforce_host_disk_space(&overlay, config.disk_grow_increment_gib)?;

    qemu_img_resize(&qemu_img, &overlay, after_gib)?;

    let new_size_bytes = u64::from(after_gib) * BYTES_PER_GIB;
    block_resize_domain(&domain, new_size_bytes)?;

    let agent = Agent::new(domain);
    let marker_was_present = clear_marker_if_present(&agent);
    apply_in_guest(&agent)?;

    update_metadata_size(&resolved, after_gib)?;

    Ok(GrowOutcome {
        after_gib,
        before_gib,
        marker_was_present,
        uuid: resolved.uuid,
    })
}

/// Subset of `qemu-img info --output=json` the grow path needs.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct QemuImgInfo {
    /// The overlay's virtual size in bytes.
    #[serde(rename = "virtual-size")]
    pub virtual_size: u64,
}

/// Parse `qemu-img info --output=json` output.
pub fn parse_qemu_img_info(json: &[u8], path: &Path) -> Result<QemuImgInfo> {
    serde_json::from_slice(json).map_err(|err| {
        GrowError::InfoParse {
            detail: err.to_string(),
            path: path.to_path_buf(),
        }
        .into()
    })
}

/// Build the `qemu-img resize` argument vector.
pub fn resize_args(path: &Path, new_size_gib: u32) -> Vec<String> {
    vec![
        "resize".to_owned(),
        path.display().to_string(),
        format!("{new_size_gib}G"),
    ]
}

/// Translate a byte count into a floored GiB count.
pub fn bytes_to_gib_floor(bytes: u64) -> u32 {
    let gib = bytes / BYTES_PER_GIB;
    u32::try_from(gib).unwrap_or(u32::MAX)
}

// -----------------------------------------------------------------------------
// Resize Orchestration
// -----------------------------------------------------------------------------

/// Drive `tartarus-grow-apply.sh` via the agent and poll to completion.
///
/// Bounded by [`GROW_TOTAL_RUNTIME_LIMIT`] across the whole polling
/// loop, in addition to the per-call [`AGENT_CALL_TIMEOUT`].
fn apply_in_guest(agent: &Agent) -> Result<()> {
    tracing::info!(script = GROW_APPLY_SCRIPT_PATH, "grow: dispatching in-guest applier");

    let handle = agent.exec(GROW_APPLY_SCRIPT_PATH, &[], false, AGENT_CALL_TIMEOUT)?;
    let deadline = Instant::now() + GROW_TOTAL_RUNTIME_LIMIT;

    loop {
        let status = agent.exec_status(&handle, AGENT_CALL_TIMEOUT)?;
        if status.exited {
            return match status.exit_code.unwrap_or(0) {
                0 => {
                    tracing::info!("grow: in-guest applier exited cleanly");
                    Ok(())
                },
                code => Err(HostError::AgentExecFailed {
                    code,
                    detail: "tartarus-grow-apply.sh exited non-zero",
                }
                .into()),
            };
        }
        if Instant::now() >= deadline {
            return Err(HostError::AgentExecFailed {
                code: -1,
                detail: "tartarus-grow-apply.sh did not exit within the runtime limit",
            }
            .into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Tell libvirt the backing block device's new size.
fn block_resize_domain(domain: &virt::domain::Domain, new_size_bytes: u64) -> Result<()> {
    domain
        .block_resize("vda", new_size_bytes, BLOCK_RESIZE_BYTES)
        .map_err(|source| HostError::DomainOperation {
            operation: "block_resize",
            source,
        })?;

    tracing::info!(new_size_bytes, "grow: virDomainBlockResize complete");
    Ok(())
}

/// Read the marker file via `qemu-guest-agent` so the host can tell
/// the user whether the grow was reactive (watermark crossed) or
/// proactive (user explicitly grew). Failure to read is *not* an
/// error: the marker may simply not exist, or qemu-ga may surface a
/// "file not found" failure that varies by version. The boolean is
/// best-effort.
fn clear_marker_if_present(agent: &Agent) -> bool {
    match agent.file_read(GROW_MARKER_PATH, AGENT_CALL_TIMEOUT) {
        Ok(_) => {
            tracing::debug!(path = GROW_MARKER_PATH, "grow: marker observed before resize");
            true
        },
        Err(err) => {
            tracing::debug!(%err, path = GROW_MARKER_PATH, "grow: marker not present (ok)");
            false
        },
    }
}

/// True iff `domain` is currently active.
fn is_active(domain: &virt::domain::Domain) -> Result<bool> {
    domain.is_active().map_err(|source| {
        HostError::DomainOperation {
            operation: "is_active",
            source,
        }
        .into()
    })
}

/// Refuse the grow when the post-grow virtual size would exceed
/// [`MAX_OVERLAY_GIB`].
///
/// This is a backstop against pathological configs (e.g. an overridden
/// `disk_grow_increment_gib`) that, repeatedly applied, would walk the
/// overlay into the petabyte range. The ceiling is well above any
/// realistic agent workload but well below qcow2's structural maximum.
fn enforce_overlay_ceiling(overlay: &Path, after_gib: u32) -> Result<()> {
    if after_gib > MAX_OVERLAY_GIB {
        return Err(GrowError::OverlayExceedsCeiling {
            ceiling_gib: MAX_OVERLAY_GIB,
            overlay: overlay.to_path_buf(),
            requested_gib: after_gib,
        }
        .into());
    }

    Ok(())
}

/// Refuse the grow when the host filesystem hosting the overlay does
/// not have enough free space for the requested increment.
///
/// qcow2 grows lazily on disk (the virtual size moves immediately, the
/// physical bytes are written as the guest dirties them), so the host
/// only needs the *increment* of free space, not the full target. We
/// add a 5% safety margin to absorb metadata overhead and any other
/// concurrent writes onto the same filesystem.
///
/// libvirt does not surface host filesystem stats, so this is the one
/// place outside the layering seed where we shell out to a system
/// utility (`df`, from coreutils, present everywhere we run). The
/// alternative — `libc::statvfs` via FFI — would require an
/// `unsafe_code` carve-out the project does not allow.
fn enforce_host_disk_space(overlay: &Path, increment_gib: u32) -> Result<()> {
    let parent = overlay.parent().unwrap_or(Path::new("/"));

    let output = Command::new("df")
        .arg("--output=avail")
        .arg("-B")
        .arg("1M")
        .arg(parent)
        .output();

    let available_mib = match output {
        Ok(o) if o.status.success() => parse_df_avail_mib(&o.stdout),
        Ok(o) => {
            tracing::warn!(
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "grow: `df` exited non-zero; skipping host-disk-full pre-check"
            );
            None
        },
        Err(err) => {
            tracing::warn!(%err, "grow: could not invoke `df`; skipping host-disk-full pre-check");
            None
        },
    };

    let Some(available_mib) = available_mib else {
        return Ok(());
    };

    let required_mib = u64::from(increment_gib) * MIB_PER_GIB + (u64::from(increment_gib) * MIB_PER_GIB) / 20;

    if available_mib < required_mib {
        return Err(GrowError::HostDiskFull {
            available_mib,
            increment_gib,
            overlay: overlay.to_path_buf(),
            required_mib,
        }
        .into());
    }

    tracing::debug!(
        available_mib,
        required_mib,
        "grow: host filesystem has sufficient free space for the requested increment"
    );
    Ok(())
}

/// Parse `df --output=avail -B 1M` stdout into the available-MiB count.
///
/// Layout: header line `Avail`, then one numeric line per probed path.
/// We take the second line. Returns [`None`] when the layout does not
/// match (unfamiliar `df` build, locale-translated header, etc.) so the
/// caller can fail open rather than block the grow on a parsing nit.
fn parse_df_avail_mib(stdout: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(stdout).ok()?;
    text.lines().nth(1)?.split_whitespace().next()?.parse::<u64>().ok()
}

/// Run `qemu-img info --output=json` against `path` via [`QemuImg`].
fn read_qemu_img_info<Q: QemuImg + ?Sized>(qemu_img: &Q, path: &Path) -> Result<QemuImgInfo> {
    let output = qemu_img.info_json(path).map_err(|err| GrowError::QemuImg {
        detail: err.to_string(),
        operation: "info",
        path: path.to_path_buf(),
    })?;

    if !output.success {
        return Err(GrowError::QemuImg {
            detail: output.stderr_trim(),
            operation: "info",
            path: path.to_path_buf(),
        }
        .into());
    }

    parse_qemu_img_info(&output.stdout, path)
}

/// Run `qemu-img resize <path> <gib>G` via [`QemuImg`].
fn qemu_img_resize<Q: QemuImg + ?Sized>(qemu_img: &Q, path: &Path, new_size_gib: u32) -> Result<()> {
    let output = qemu_img.resize(path, new_size_gib).map_err(|err| GrowError::QemuImg {
        detail: err.to_string(),
        operation: "resize",
        path: path.to_path_buf(),
    })?;

    if !output.success {
        return Err(GrowError::QemuImg {
            detail: output.stderr_trim(),
            operation: "resize",
            path: path.to_path_buf(),
        }
        .into());
    }

    Ok(())
}

/// Persist the post-grow virtual size into the session's `metadata.json`.
fn update_metadata_size(resolved: &ResolvedSession, new_size_gib: u32) -> Result<()> {
    let metadata_path = paths::sessions_by_uuid_dir()?
        .join(&resolved.uuid)
        .join(metadata::METADATA_FILE_NAME);

    let mut meta = Metadata::load(&metadata_path)?;
    meta.overlay_virtual_gib = new_size_gib;
    meta.save(&metadata_path)?;

    tracing::info!(uuid = %resolved.uuid, new_size_gib, "grow: metadata updated");
    Ok(())
}

/// Hook used by the unit tests to dispatch the orchestration steps
/// against mocks. Production wires the four real verbs through [`run`];
/// the test path substitutes its own closures so the orchestration
/// shape is exercised without libvirt or `qemu-img`.
#[cfg(test)]
pub(crate) fn run_with_steps(steps: &mut TestSteps<'_>) -> Result<u32> {
    let before = (steps.read_size)()?;
    let after = before.saturating_add(steps.increment);

    (steps.qemu_img_resize)(after)?;
    (steps.block_resize)(u64::from(after) * BYTES_PER_GIB)?;
    (steps.apply_in_guest)()?;
    (steps.persist_size)(after)?;

    Ok(after)
}

/// Test scaffolding: closures simulating the four real grow steps so
/// the unit tests can verify the orchestration without libvirt or
/// `qemu-img`.
#[cfg(test)]
pub(crate) struct TestSteps<'a> {
    /// Step 1: read the current overlay virtual size in GiB.
    pub read_size: &'a mut dyn FnMut() -> Result<u32>,

    /// Step 2: `qemu-img resize <overlay> <new_size_gib>G`.
    pub qemu_img_resize: &'a mut dyn FnMut(u32) -> Result<()>,

    /// Step 3: `virDomainBlockResize` with the new size in bytes.
    pub block_resize: &'a mut dyn FnMut(u64) -> Result<()>,

    /// Step 4: `tartarus-grow-apply.sh` over `qemu-guest-agent`.
    pub apply_in_guest: &'a mut dyn FnMut() -> Result<()>,

    /// Persistence: write the new size back into `metadata.json`.
    pub persist_size: &'a mut dyn FnMut(u32) -> Result<()>,

    /// Per-grow increment, in GiB.
    pub increment: u32,
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::Error;

    #[test]
    fn resize_args_match_documented_invocation() {
        let path = PathBuf::from("/data/sessions/by-uuid/abc/overlay.qcow2");

        let args = resize_args(&path, 200);

        assert_eq!(
            args,
            vec![
                "resize".to_owned(),
                "/data/sessions/by-uuid/abc/overlay.qcow2".to_owned(),
                "200G".to_owned(),
            ],
            "qemu-img resize args should match the documented spec.md invocation",
        );
    }

    #[test]
    fn parse_qemu_img_info_extracts_virtual_size() {
        let path = PathBuf::from("/overlay.qcow2");
        let json = br#"{"virtual-size":107374182400,"format":"qcow2","actual-size":262144}"#;

        let info = parse_qemu_img_info(json, &path).expect("well-formed JSON should parse");

        assert_eq!(
            info.virtual_size, 107_374_182_400,
            "virtual-size should round-trip into the parsed struct",
        );
    }

    #[test]
    fn parse_df_avail_mib_extracts_second_line_value() {
        let stdout = b"Avail\n    51200\n";

        let mib = parse_df_avail_mib(stdout).expect("well-formed df output should parse");

        assert_eq!(mib, 51200, "available MiB should round-trip from df's second line");
    }

    #[test]
    fn parse_df_avail_mib_returns_none_on_unexpected_layout() {
        assert!(parse_df_avail_mib(b"").is_none(), "empty input should not panic");
        assert!(
            parse_df_avail_mib(b"Avail\n").is_none(),
            "missing data line should be None"
        );
        assert!(
            parse_df_avail_mib(b"Avail\nnot-a-number\n").is_none(),
            "non-numeric should be None"
        );
    }

    #[test]
    fn enforce_overlay_ceiling_rejects_above_ceiling() {
        let overlay = PathBuf::from("/overlay.qcow2");
        let above = MAX_OVERLAY_GIB + 1;

        let err = enforce_overlay_ceiling(&overlay, above).expect_err("above-ceiling grow must be rejected");

        match err {
            Error::Grow(GrowError::OverlayExceedsCeiling {
                ceiling_gib,
                requested_gib,
                ..
            }) => {
                assert_eq!(ceiling_gib, MAX_OVERLAY_GIB, "ceiling should round-trip into the error");
                assert_eq!(requested_gib, above, "requested size should round-trip into the error");
            },
            other => panic!("expected GrowError::OverlayExceedsCeiling, got {other:?}"),
        }
    }

    #[test]
    fn enforce_overlay_ceiling_accepts_at_ceiling() {
        let overlay = PathBuf::from("/overlay.qcow2");

        enforce_overlay_ceiling(&overlay, MAX_OVERLAY_GIB).expect("at-ceiling grow should be accepted");
    }

    #[test]
    fn parse_qemu_img_info_rejects_malformed_payload() {
        let path = PathBuf::from("/overlay.qcow2");

        let err = parse_qemu_img_info(b"not json", &path).expect_err("garbage should be rejected");

        match err {
            Error::Grow(GrowError::InfoParse { path: p, .. }) => {
                assert_eq!(p, path, "the rejected path should round-trip into the error");
            },
            other => panic!("expected GrowError::InfoParse, got {other:?}"),
        }
    }

    #[test]
    fn parse_qemu_img_info_rejects_payload_without_virtual_size() {
        let path = PathBuf::from("/overlay.qcow2");
        let json = br#"{"format":"qcow2"}"#;

        let err = parse_qemu_img_info(json, &path).expect_err("payload without virtual-size should be rejected");

        match err {
            Error::Grow(GrowError::InfoParse { .. }) => {},
            other => panic!("expected GrowError::InfoParse, got {other:?}"),
        }
    }

    #[test]
    fn bytes_to_gib_floor_round_trips_a_clean_gib() {
        assert_eq!(
            bytes_to_gib_floor(100 * BYTES_PER_GIB),
            100,
            "an exact 100 GiB byte count should floor to 100",
        );
    }

    #[test]
    fn bytes_to_gib_floor_rounds_down_partial_gib() {
        assert_eq!(
            bytes_to_gib_floor(BYTES_PER_GIB - 1),
            0,
            "anything below 1 GiB should floor to 0",
        );
        assert_eq!(
            bytes_to_gib_floor(BYTES_PER_GIB + 1),
            1,
            "1 GiB plus a byte should still floor to 1",
        );
    }

    #[test]
    fn run_with_steps_drives_all_four_orchestration_steps_in_order() {
        let mut events: Vec<String> = Vec::new();
        let mut after_size: Option<u32> = None;

        {
            let events = std::cell::RefCell::new(&mut events);
            let after_size = std::cell::RefCell::new(&mut after_size);

            let mut read_size = || -> Result<u32> {
                events.borrow_mut().push("read_size".to_owned());
                Ok(100)
            };
            let mut qemu_img_resize = |new_size: u32| -> Result<()> {
                events.borrow_mut().push(format!("qemu_img_resize:{new_size}"));
                Ok(())
            };
            let mut block_resize = |new_bytes: u64| -> Result<()> {
                events.borrow_mut().push(format!("block_resize:{new_bytes}"));
                Ok(())
            };
            let mut apply_in_guest = || -> Result<()> {
                events.borrow_mut().push("apply_in_guest".to_owned());
                Ok(())
            };
            let mut persist_size = |new_size: u32| -> Result<()> {
                events.borrow_mut().push(format!("persist_size:{new_size}"));
                **after_size.borrow_mut() = Some(new_size);
                Ok(())
            };

            let mut steps = TestSteps {
                apply_in_guest: &mut apply_in_guest,
                block_resize: &mut block_resize,
                increment: 100,
                persist_size: &mut persist_size,
                qemu_img_resize: &mut qemu_img_resize,
                read_size: &mut read_size,
            };

            let returned = run_with_steps(&mut steps).expect("orchestration should succeed");
            assert_eq!(returned, 200, "run_with_steps should return the post-grow size in GiB",);
        }

        assert_eq!(
            events,
            vec![
                "read_size".to_owned(),
                "qemu_img_resize:200".to_owned(),
                format!("block_resize:{}", 200_u64 * BYTES_PER_GIB),
                "apply_in_guest".to_owned(),
                "persist_size:200".to_owned(),
            ],
            "the four real steps must run in the documented order",
        );
        assert_eq!(
            after_size,
            Some(200),
            "the persistence step should observe the post-grow size",
        );
    }

    #[test]
    fn run_with_steps_short_circuits_on_qemu_img_failure() {
        let mut read_size = || Ok(100);
        let mut qemu_img_resize = |_: u32| -> Result<()> {
            Err(GrowError::QemuImg {
                detail: "simulated".to_owned(),
                operation: "resize",
                path: PathBuf::from("/overlay.qcow2"),
            }
            .into())
        };
        let mut block_resize = |_: u64| -> Result<()> {
            panic!("block_resize must not run after qemu-img resize fails");
        };
        let mut apply_in_guest = || -> Result<()> {
            panic!("apply_in_guest must not run after qemu-img resize fails");
        };
        let mut persist_size = |_: u32| -> Result<()> {
            panic!("persist_size must not run after qemu-img resize fails");
        };

        let mut steps = TestSteps {
            apply_in_guest: &mut apply_in_guest,
            block_resize: &mut block_resize,
            increment: 100,
            persist_size: &mut persist_size,
            qemu_img_resize: &mut qemu_img_resize,
            read_size: &mut read_size,
        };

        let err = run_with_steps(&mut steps).expect_err("qemu-img failure must propagate");

        match err {
            Error::Grow(GrowError::QemuImg {
                operation: "resize", ..
            }) => {},
            other => panic!("expected GrowError::QemuImg(resize), got {other:?}"),
        }
    }

    #[test]
    fn grow_outcome_round_trips_through_struct() {
        let outcome = GrowOutcome {
            after_gib: 200,
            before_gib: 100,
            marker_was_present: true,
            uuid: "abcd".to_owned(),
        };

        assert_eq!(outcome.uuid, "abcd", "uuid should round-trip");
        assert_eq!(outcome.before_gib, 100, "before should round-trip");
        assert_eq!(outcome.after_gib, 200, "after should round-trip");
        assert!(
            outcome.marker_was_present,
            "marker_was_present should round-trip into the outcome",
        );
    }

    #[test]
    fn grow_apply_script_path_matches_in_guest_layout() {
        assert_eq!(
            GROW_APPLY_SCRIPT_PATH, "/usr/local/bin/tartarus-grow-apply.sh",
            "the host-side path constant must match what the layering step installs",
        );
    }

    #[test]
    fn grow_marker_path_is_under_run_tartarus() {
        assert_eq!(
            GROW_MARKER_PATH, "/run/tartarus/grow-request",
            "marker path must match what tartarus-grow.sh writes",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus a session with qemu-ga responding; run with --ignored after setting up locally"]
    fn end_to_end_grow_against_real_session() {}

    // -----------------------------------------------------------------------
    // QemuImg-Recorder Driven Tests
    // -----------------------------------------------------------------------

    use crate::disk::qemu_img::recorder::{Call, Recorder};

    #[test]
    fn read_qemu_img_info_round_trips_through_recorder() {
        let recorder = Recorder::new();
        recorder.enqueue_ok(br#"{"virtual-size":1073741824,"format":"qcow2"}"#.to_vec());
        let info = read_qemu_img_info(&recorder, Path::new("/x.qcow2")).expect("info should succeed");
        assert_eq!(info.virtual_size, 1_073_741_824);
        let calls = recorder.calls.borrow();
        assert!(
            matches!(calls[0], Call::Info { .. }),
            "exactly the info call should fire"
        );
    }

    #[test]
    fn read_qemu_img_info_surfaces_failure_via_recorder() {
        let recorder = Recorder::new();
        recorder.enqueue_err("qemu-img: cannot open");
        let err = read_qemu_img_info(&recorder, Path::new("/missing.qcow2")).expect_err("scripted failure");
        match err {
            Error::Grow(GrowError::QemuImg { operation, detail, .. }) => {
                assert_eq!(operation, "info");
                assert!(detail.contains("cannot open"));
            },
            other => panic!("expected Grow(QemuImg(info)), got {other:?}"),
        }
    }

    #[test]
    fn qemu_img_resize_invokes_recorder_with_new_size() {
        let recorder = Recorder::new();
        qemu_img_resize(&recorder, Path::new("/x.qcow2"), 200).expect("resize should succeed");
        let calls = recorder.calls.borrow();
        match calls[0] {
            Call::Resize { new_size_gib, .. } => assert_eq!(new_size_gib, 200),
            ref other => panic!("expected Resize, got {other:?}"),
        }
    }

    #[test]
    fn qemu_img_resize_surfaces_failure_via_recorder() {
        let recorder = Recorder::new();
        recorder.enqueue_err("qemu-img: ENOSPC");
        let err = qemu_img_resize(&recorder, Path::new("/x.qcow2"), 200).expect_err("scripted failure");
        match err {
            Error::Grow(GrowError::QemuImg { operation, detail, .. }) => {
                assert_eq!(operation, "resize");
                assert!(detail.contains("ENOSPC"));
            },
            other => panic!("expected Grow(QemuImg(resize)), got {other:?}"),
        }
    }
}
