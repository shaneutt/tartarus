//! Host-side `qemu-guest-agent` client.
//!
//! Every operation is a JSON envelope sent through
//! [`virt::domain::Domain::qemu_agent_command`]. Each call carries a
//! per-call timeout so a misbehaving guest cannot hang the host.

use std::time::Duration;

use serde_json::{Value, json};

use crate::{error::Result, host::error::HostError};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Maximum bytes per `guest-file-write` chunk.
const FILE_WRITE_CHUNK_BYTES: usize = 65_536;

/// Maximum bytes per `guest-file-read` chunk.
const FILE_READ_CHUNK_BYTES: u64 = 65_536;

/// Polling interval for [`Agent::wait_exec_complete`].
const EXEC_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Per-call timeout for inner `exec_status` round-trips.
const EXEC_STATUS_PER_CALL_TIMEOUT: Duration = Duration::from_secs(5);

// -----------------------------------------------------------------------------
// Agent
// -----------------------------------------------------------------------------

/// Handle to the in-guest `qemu-guest-agent` for a single domain.
#[derive(Debug)]
pub struct Agent {
    domain: virt::domain::Domain,
}

impl Agent {
    /// Wrap a [`virt::domain::Domain`] in an agent client.
    pub fn new(domain: virt::domain::Domain) -> Self {
        Self { domain }
    }

    /// Launch `command` with `args` inside the guest via `guest-exec`.
    ///
    /// When `capture_output` is true, [`ExecStatus`] will carry
    /// stdout/stderr after exit.
    pub fn exec(&self, command: &str, args: &[&str], capture_output: bool, timeout: Duration) -> Result<ExecHandle> {
        let envelope = json!({
            "execute": "guest-exec",
            "arguments": {
                "path": command,
                "arg": args,
                "capture-output": capture_output,
            },
        });
        tracing::debug!(command, ?args, capture_output, "qemu-ga guest-exec");

        let response = self.dispatch(&envelope, timeout)?;
        let pid = extract_return_field(&response, "pid")?
            .as_i64()
            .ok_or_else(|| HostError::AgentProtocol {
                detail: "guest-exec reply missing integer pid".to_owned(),
            })?;

        Ok(ExecHandle { pid })
    }

    /// Poll `guest-exec-status` for the supplied [`ExecHandle`].
    pub fn exec_status(&self, handle: &ExecHandle, timeout: Duration) -> Result<ExecStatus> {
        let envelope = json!({
            "execute": "guest-exec-status",
            "arguments": { "pid": handle.pid },
        });

        let response = self.dispatch(&envelope, timeout)?;
        let payload = extract_return_object(&response)?;

        ExecStatus::from_payload(payload)
    }

    /// Close a guest file handle.
    pub fn file_close(&self, handle: i64, timeout: Duration) -> Result<()> {
        let envelope = json!({
            "execute": "guest-file-close",
            "arguments": { "handle": handle },
        });
        let _ = self.dispatch(&envelope, timeout)?;
        Ok(())
    }

    /// Read the entire contents of `path` from the guest.
    pub fn file_read(&self, path: &str, timeout: Duration) -> Result<Vec<u8>> {
        let handle = self.file_open(path, "r", timeout)?;

        let result = self.read_until_eof(handle, timeout);

        let _ = self.file_close(handle, timeout);

        result
    }

    /// Write `contents` to the guest, then apply mode and ownership
    /// via in-guest `chmod`/`chown`.
    pub fn file_write(&self, request: &FileWriteRequest<'_>) -> Result<()> {
        let handle = self.file_open(request.path, "w", request.timeout)?;

        let write_result = self.write_chunks(handle, request.contents, request.timeout);
        let _ = self.file_close(handle, request.timeout);
        write_result?;

        self.apply_mode_and_owner(request.path, request.mode, request.owner, request.timeout)
    }

    /// Send a `guest-ping` and return Ok on round-trip.
    pub fn ping(&self, timeout: Duration) -> Result<()> {
        let envelope = json!({ "execute": "guest-ping" });
        let _ = self.dispatch(&envelope, timeout)?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Agent Internals
    // -----------------------------------------------------------------------

    /// Run `chmod`/`chown` in-guest after a `file_write`.
    fn apply_mode_and_owner(&self, path: &str, mode: u32, owner: &str, timeout: Duration) -> Result<()> {
        let mode_octal = format!("{mode:o}");
        let chmod = self.exec("/usr/bin/chmod", &[&mode_octal, path], false, timeout)?;
        self.wait_exec_complete(&chmod, timeout)?;

        let chown = self.exec("/usr/bin/chown", &[owner, path], false, timeout)?;
        self.wait_exec_complete(&chown, timeout)?;

        Ok(())
    }

    /// Send a JSON envelope via `qemu_agent_command` and parse the
    /// reply.
    fn dispatch(&self, envelope: &Value, timeout: Duration) -> Result<Value> {
        let payload = serde_json::to_string(envelope).map_err(|source| HostError::AgentProtocol {
            detail: format!("failed to serialise envelope: {source}"),
        })?;

        let timeout_secs = duration_to_seconds(timeout);

        let raw = self
            .domain
            .qemu_agent_command(&payload, timeout_secs, 0)
            .map_err(|err| classify_agent_error(err, &payload))?;

        let reply: Value = serde_json::from_str(&raw).map_err(|_| HostError::AgentProtocol {
            detail: "agent reply was not valid JSON".to_owned(),
        })?;

        if let Some(error) = reply.get("error") {
            let detail = serde_json::to_string(error).unwrap_or_else(|_| "<unparseable>".to_owned());
            return Err(HostError::AgentProtocol { detail }.into());
        }

        Ok(reply)
    }

    /// Open `path` in-guest with `mode` (e.g. `r`, `w`, `a`).
    fn file_open(&self, path: &str, mode: &str, timeout: Duration) -> Result<i64> {
        let envelope = json!({
            "execute": "guest-file-open",
            "arguments": { "path": path, "mode": mode },
        });

        let response = self.dispatch(&envelope, timeout)?;
        let handle = response
            .get("return")
            .and_then(Value::as_i64)
            .ok_or(HostError::AgentProtocol {
                detail: "guest-file-open reply missing integer handle".to_owned(),
            })?;

        Ok(handle)
    }

    /// Read repeatedly until the agent reports `eof`.
    fn read_until_eof(&self, handle: i64, timeout: Duration) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let envelope = json!({
                "execute": "guest-file-read",
                "arguments": { "handle": handle, "count": FILE_READ_CHUNK_BYTES },
            });
            let response = self.dispatch(&envelope, timeout)?;
            let payload = extract_return_object(&response)?;

            let eof = payload
                .get("eof")
                .and_then(Value::as_bool)
                .ok_or(HostError::AgentProtocol {
                    detail: "guest-file-read reply missing boolean `eof`".to_owned(),
                })?;
            let chunk = decode_optional_b64(payload.get("buf-b64"))?.unwrap_or_default();
            out.extend_from_slice(&chunk);

            if eof {
                break;
            }
        }
        Ok(out)
    }

    /// Block until `handle`'s process exits, surfacing non-zero exit
    /// as [`HostError::AgentExecFailed`].
    fn wait_exec_complete(&self, handle: &ExecHandle, timeout: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + timeout;
        let per_call = EXEC_STATUS_PER_CALL_TIMEOUT.min(timeout);
        loop {
            let status = self.exec_status(handle, per_call)?;
            if status.exited {
                return match status.exit_code.unwrap_or(0) {
                    0 => Ok(()),
                    code => Err(HostError::AgentExecFailed {
                        code,
                        detail: "command exited non-zero",
                    }
                    .into()),
                };
            }
            if std::time::Instant::now() >= deadline {
                return Err(HostError::AgentExecFailed {
                    code: -1,
                    detail: "command did not exit within poll window",
                }
                .into());
            }
            std::thread::sleep(EXEC_POLL_INTERVAL);
        }
    }

    /// Push `contents` through `guest-file-write` in
    /// [`FILE_WRITE_CHUNK_BYTES`]-sized chunks.
    fn write_chunks(&self, handle: i64, contents: &[u8], timeout: Duration) -> Result<()> {
        for chunk in contents.chunks(FILE_WRITE_CHUNK_BYTES) {
            let envelope = json!({
                "execute": "guest-file-write",
                "arguments": {
                    "handle": handle,
                    "buf-b64": base64_encode(chunk),
                },
            });
            let _ = self.dispatch(&envelope, timeout)?;
        }
        Ok(())
    }
}

/// Arguments for [`Agent::file_write`].
#[derive(Clone, Copy, Debug)]
pub struct FileWriteRequest<'a> {
    /// File contents.
    pub contents: &'a [u8],

    /// Unix permission triplet (e.g. `0o600`).
    pub mode: u32,

    /// In-guest owner username.
    pub owner: &'a str,

    /// Absolute guest path.
    pub path: &'a str,

    /// Per-call timeout.
    pub timeout: Duration,
}

/// Handle returned by [`Agent::exec`]. The `pid` is a qemu-ga
/// identifier, not a host PID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecHandle {
    /// Opaque qemu-ga handle for `exec_status` follow-ups.
    pub pid: i64,
}

/// Outcome returned by [`Agent::exec_status`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecStatus {
    /// Exit code. `None` while running.
    pub exit_code: Option<i64>,

    /// True iff the process has terminated.
    pub exited: bool,

    /// Signal that terminated the process, if any.
    pub signal: Option<i64>,

    /// Captured stderr, if requested and process exited.
    pub stderr: Option<Vec<u8>>,

    /// Captured stdout, if requested and process exited.
    pub stdout: Option<Vec<u8>>,
}

impl ExecStatus {
    /// Parse a `guest-exec-status` `return` object.
    pub fn from_payload(payload: &Value) -> Result<Self> {
        let exited = payload
            .get("exited")
            .and_then(Value::as_bool)
            .ok_or(HostError::AgentProtocol {
                detail: "guest-exec-status reply missing boolean `exited`".to_owned(),
            })?;

        let exit_code = payload.get("exitcode").and_then(Value::as_i64);
        let signal = payload.get("signal").and_then(Value::as_i64);

        let stdout = decode_optional_b64(payload.get("out-data"))?;
        let stderr = decode_optional_b64(payload.get("err-data"))?;

        Ok(Self {
            exit_code,
            exited,
            signal,
            stderr,
            stdout,
        })
    }
}

// -----------------------------------------------------------------------------
// Base64 and Protocol Parsing
// -----------------------------------------------------------------------------

/// Build a `guest-exec` JSON envelope (test-only).
#[cfg(test)]
fn build_exec_envelope(command: &str, args: &[&str], capture_output: bool) -> Value {
    json!({
        "execute": "guest-exec",
        "arguments": {
            "path": command,
            "arg": args,
            "capture-output": capture_output,
        },
    })
}

/// Build a `guest-exec-status` JSON envelope (test-only).
#[cfg(test)]
fn build_exec_status_envelope(handle: &ExecHandle) -> Value {
    json!({
        "execute": "guest-exec-status",
        "arguments": { "pid": handle.pid },
    })
}

/// Build a `guest-file-open` JSON envelope (test-only).
#[cfg(test)]
fn build_file_open_envelope(path: &str, mode: &str) -> Value {
    json!({
        "execute": "guest-file-open",
        "arguments": { "path": path, "mode": mode },
    })
}

/// Build a `guest-file-write` JSON envelope (test-only).
#[cfg(test)]
fn build_file_write_envelope(handle: i64, chunk: &[u8]) -> Value {
    json!({
        "execute": "guest-file-write",
        "arguments": {
            "handle": handle,
            "buf-b64": base64_encode(chunk),
        },
    })
}

/// Build a `guest-file-read` JSON envelope (test-only).
#[cfg(test)]
fn build_file_read_envelope(handle: i64, count: u64) -> Value {
    json!({
        "execute": "guest-file-read",
        "arguments": { "handle": handle, "count": count },
    })
}

/// Build a `guest-file-close` JSON envelope (test-only).
#[cfg(test)]
fn build_file_close_envelope(handle: i64) -> Value {
    json!({
        "execute": "guest-file-close",
        "arguments": { "handle": handle },
    })
}

/// Encode `bytes` as RFC 4648 base64.
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = u32::from(bytes[i]) << 16;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

/// Decode an RFC 4648 base64 string. Rejects invalid bytes and
/// misplaced padding.
fn base64_decode(input: &str) -> Result<Vec<u8>> {
    let trimmed: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if !trimmed.len().is_multiple_of(4) {
        return Err(HostError::AgentProtocol {
            detail: "base64 input has invalid length".to_owned(),
        }
        .into());
    }

    let mut out = Vec::with_capacity(trimmed.len() / 4 * 3);
    if trimmed.is_empty() {
        return Ok(out);
    }
    let last_chunk_index = trimmed.len() / 4 - 1;
    for (chunk_idx, chunk) in trimmed.chunks(4).enumerate() {
        let mut buf = [0_u32; 4];
        let mut padding = 0;
        for (i, &b) in chunk.iter().enumerate() {
            buf[i] = match b {
                b'A'..=b'Z' => u32::from(b - b'A'),
                b'a'..=b'z' => u32::from(b - b'a' + 26),
                b'0'..=b'9' => u32::from(b - b'0' + 52),
                b'+' => 62,
                b'/' => 63,
                b'=' => {
                    if chunk_idx != last_chunk_index || i < 2 {
                        return Err(HostError::AgentProtocol {
                            detail: "base64 padding `=` is only valid in the trailing quartet at positions 2 or 3"
                                .to_owned(),
                        }
                        .into());
                    }
                    padding += 1;
                    0
                },
                _ => {
                    return Err(HostError::AgentProtocol {
                        detail: "base64 input contains an invalid byte".to_owned(),
                    }
                    .into());
                },
            };
        }
        if padding == 1 && chunk[3] != b'=' {
            return Err(HostError::AgentProtocol {
                detail: "base64 padding must be contiguous at the end of the trailing quartet".to_owned(),
            }
            .into());
        }
        let n = (buf[0] << 18) | (buf[1] << 12) | (buf[2] << 6) | buf[3];
        out.push(((n >> 16) & 0xFF) as u8);
        if padding < 2 {
            out.push(((n >> 8) & 0xFF) as u8);
        }
        if padding < 1 {
            out.push((n & 0xFF) as u8);
        }
    }

    Ok(out)
}

/// Classify a libvirt agent error into the appropriate [`HostError`]
/// variant.
fn classify_agent_error(err: virt::error::Error, payload: &str) -> crate::Error {
    use virt::error::ErrorNumber;

    let message = err.to_string();

    match err.code() {
        ErrorNumber::AgentUnresponsive
        | ErrorNumber::AgentCommandTimeout
        | ErrorNumber::AgentUnsynced
        | ErrorNumber::AgentCommandFailed => {
            return HostError::AgentNotResponding { detail: message }.into();
        },
        ErrorNumber::OperationInvalid => {
            let lower = message.to_lowercase();
            if lower.contains("guest agent") || (lower.contains("channel") && lower.contains("agent")) {
                return HostError::AgentChannelMissing.into();
            }
        },
        _ => {},
    }

    let lower = message.to_lowercase();
    if lower.contains("guest agent is not configured") || (lower.contains("channel") && lower.contains("agent")) {
        return HostError::AgentChannelMissing.into();
    }
    if lower.contains("not connected") || lower.contains("not responding") || lower.contains("timed out") {
        return HostError::AgentNotResponding { detail: message }.into();
    }

    tracing::debug!(?payload, %message, "qemu_agent_command failed; mapping to AgentNotResponding");
    HostError::AgentNotResponding { detail: message }.into()
}

/// Decode an optional base64-encoded JSON field into raw bytes.
fn decode_optional_b64(field: Option<&Value>) -> Result<Option<Vec<u8>>> {
    match field {
        Some(Value::String(s)) => Ok(Some(base64_decode(s)?)),
        Some(Value::Null) | None => Ok(None),
        Some(other) => {
            tracing::warn!(?other, "base64 field had unexpected JSON type");
            Err(HostError::AgentProtocol {
                detail: "base64 field had unexpected JSON type".to_owned(),
            }
            .into())
        },
    }
}

/// Convert a [`Duration`] to seconds as `i32`, clamping to
/// `[1, i32::MAX]`.
fn duration_to_seconds(timeout: Duration) -> i32 {
    let secs = timeout.as_secs();
    if secs > i32::MAX as u64 {
        i32::MAX
    } else if secs == 0 {
        1
    } else {
        secs as i32
    }
}

/// Extract `return.<label>` from a reply, or fail.
fn extract_return_field<'a>(response: &'a Value, label: &'static str) -> Result<&'a Value> {
    response.get("return").and_then(|r| r.get(label)).ok_or_else(|| {
        HostError::AgentProtocol {
            detail: format!("agent reply missing `return.{label}`"),
        }
        .into()
    })
}

/// Extract the top-level `return` object from a reply, or fail.
fn extract_return_object(response: &Value) -> Result<&Value> {
    response.get("return").ok_or_else(|| {
        HostError::AgentProtocol {
            detail: "agent reply missing top-level `return`".to_owned(),
        }
        .into()
    })
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn build_exec_envelope_pins_wire_shape() {
        let env = build_exec_envelope("/usr/bin/true", &["--quiet", "extra"], true);

        let expected = json!({
            "execute": "guest-exec",
            "arguments": {
                "path": "/usr/bin/true",
                "arg": ["--quiet", "extra"],
                "capture-output": true,
            },
        });

        assert_eq!(env, expected, "guest-exec envelope shape must be stable",);
    }

    #[test]
    fn build_exec_envelope_passes_capture_false_through() {
        let env = build_exec_envelope("/usr/local/bin/tartarus-update.sh", &[], false);

        assert_eq!(
            env["arguments"]["capture-output"],
            json!(false),
            "capture-output false should round-trip into the envelope",
        );
        assert_eq!(
            env["arguments"]["arg"],
            json!([]),
            "empty arg list should serialise as an empty JSON array",
        );
    }

    #[test]
    fn build_exec_status_envelope_shapes_pid() {
        let env = build_exec_status_envelope(&ExecHandle { pid: 1234 });

        let expected = json!({
            "execute": "guest-exec-status",
            "arguments": { "pid": 1234 },
        });

        assert_eq!(env, expected, "guest-exec-status envelope shape must be stable",);
    }

    #[test]
    fn build_file_open_envelope_carries_path_and_mode() {
        let env = build_file_open_envelope("/run/tartarus/marker", "w");

        let expected = json!({
            "execute": "guest-file-open",
            "arguments": { "path": "/run/tartarus/marker", "mode": "w" },
        });
        assert_eq!(env, expected);
    }

    #[test]
    fn build_file_write_envelope_base64_encodes_chunk() {
        let env = build_file_write_envelope(7, b"abc");

        assert_eq!(
            env["arguments"]["buf-b64"],
            json!(base64_encode(b"abc")),
            "buf-b64 must be the base64 encoding of the raw bytes",
        );
        assert_eq!(env["arguments"]["handle"], json!(7), "handle must round-trip");
    }

    #[test]
    fn build_file_read_envelope_carries_handle_and_count() {
        let env = build_file_read_envelope(13, 4096);

        let expected = json!({
            "execute": "guest-file-read",
            "arguments": { "handle": 13, "count": 4096 },
        });
        assert_eq!(env, expected);
    }

    #[test]
    fn build_file_close_envelope_carries_handle() {
        let env = build_file_close_envelope(99);

        let expected = json!({
            "execute": "guest-file-close",
            "arguments": { "handle": 99 },
        });
        assert_eq!(env, expected);
    }

    #[test]
    fn exec_status_parses_running_payload() {
        let payload = json!({ "exited": false });

        let status = ExecStatus::from_payload(&payload).expect("running payload should parse");

        assert!(!status.exited, "running process should report exited=false");
        assert!(status.exit_code.is_none(), "running process has no exit code");
        assert!(
            status.stdout.is_none() && status.stderr.is_none(),
            "running process has no captured streams",
        );
    }

    #[test]
    fn exec_status_parses_exited_payload_with_capture() {
        let payload = json!({
            "exited": true,
            "exitcode": 0,
            "out-data": base64_encode(b"hello\n"),
            "err-data": base64_encode(b"warn: x"),
        });

        let status = ExecStatus::from_payload(&payload).expect("exited payload should parse");

        assert!(status.exited, "exited=true should round-trip");
        assert_eq!(status.exit_code, Some(0), "exit code should round-trip");
        assert_eq!(
            status.stdout.as_deref(),
            Some(b"hello\n".as_ref()),
            "stdout should decode from base64",
        );
        assert_eq!(
            status.stderr.as_deref(),
            Some(b"warn: x".as_ref()),
            "stderr should decode from base64",
        );
    }

    #[test]
    fn exec_status_rejects_missing_exited_field() {
        let payload = json!({ "exitcode": 0 });

        let err = ExecStatus::from_payload(&payload).expect_err("missing exited must reject");

        match err {
            crate::Error::Host(HostError::AgentProtocol { .. }) => {},
            other => panic!("expected AgentProtocol, got {other:?}"),
        }
    }

    #[test]
    fn exec_status_carries_signal_when_present() {
        let payload = json!({ "exited": true, "signal": 9 });

        let status = ExecStatus::from_payload(&payload).expect("signal payload should parse");

        assert_eq!(status.signal, Some(9), "signal field should round-trip");
        assert!(status.exit_code.is_none(), "no exitcode when signal-terminated");
    }

    #[test]
    fn base64_round_trip_handles_zero_one_two_three_byte_inputs() {
        for raw in [b"".as_ref(), b"a", b"ab", b"abc", b"abcd", b"hello world"] {
            let encoded = base64_encode(raw);
            let decoded = base64_decode(&encoded).expect("round-trip should decode");
            assert_eq!(decoded, raw, "base64 round-trip must preserve bytes for {raw:?}",);
        }
    }

    #[test]
    fn base64_decode_rejects_invalid_alphabet() {
        let err = base64_decode("####").expect_err("invalid alphabet must fail");
        match err {
            crate::Error::Host(HostError::AgentProtocol { .. }) => {},
            other => panic!("expected AgentProtocol, got {other:?}"),
        }
    }

    #[test]
    fn base64_decode_rejects_unpadded_length() {
        let err = base64_decode("abc").expect_err("unpadded length must fail");
        match err {
            crate::Error::Host(HostError::AgentProtocol { .. }) => {},
            other => panic!("expected AgentProtocol, got {other:?}"),
        }
    }

    #[test]
    fn base64_decode_rejects_misplaced_padding() {
        for bad in ["A=B=", "=AAA", "AA=A", "A===", "====AAAA"] {
            let err = base64_decode(bad).expect_err("misplaced padding must fail: {bad}");
            match err {
                crate::Error::Host(HostError::AgentProtocol { .. }) => {},
                other => panic!("expected AgentProtocol for {bad:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn base64_decode_accepts_canonical_padding() {
        assert_eq!(base64_decode("AA==").expect("canonical xx==").as_slice(), &[0]);
        assert_eq!(base64_decode("AAA=").expect("canonical xxx=").as_slice(), &[0, 0]);
        assert_eq!(base64_decode("AAAA").expect("canonical xxxx").as_slice(), &[0, 0, 0]);
    }

    #[test]
    fn duration_to_seconds_clamps_zero_up_to_one() {
        assert_eq!(
            duration_to_seconds(Duration::from_millis(50)),
            1,
            "sub-second timeouts must round up to one second so we never block forever",
        );
    }

    #[test]
    fn duration_to_seconds_saturates_at_i32_max() {
        assert_eq!(
            duration_to_seconds(Duration::from_secs(u64::MAX / 2)),
            i32::MAX,
            "extremely large timeouts saturate at i32::MAX",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
    fn ping_dispatches_against_a_real_domain() {
        use crate::host::{
            connect::{Connection, DEFAULT_URI},
            domain::{self, DomainSpec},
        };

        let connection = Connection::open(DEFAULT_URI).expect("qemu:///session should be reachable");
        let name = format!("tartarus-test-agent-ping-{pid}", pid = std::process::id());
        let spec = DomainSpec::trivial(&name);

        let domain = domain::define(&connection, &spec).expect("define should succeed");
        let agent = Agent::new(domain);

        let _ = agent.ping(Duration::from_secs(5));

        domain::undefine(&connection, &spec.name).expect("undefine should succeed");
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
    fn exec_true_returns_exit_code_zero() {
        use crate::host::{
            connect::{Connection, DEFAULT_URI},
            domain::{self, DomainSpec},
        };

        let connection = Connection::open(DEFAULT_URI).expect("qemu:///session should be reachable");
        let name = format!("tartarus-test-exec-true-{pid}", pid = std::process::id());
        let spec = DomainSpec::trivial(&name);

        let domain = domain::define(&connection, &spec).expect("define should succeed");
        let agent = Agent::new(domain);

        let timeout = Duration::from_secs(10);
        let handle = agent
            .exec("/usr/bin/true", &[], false, timeout)
            .expect("exec should accept /usr/bin/true");

        let status = agent
            .exec_status(&handle, timeout)
            .expect("exec_status should reply against a live agent");

        assert!(status.exited || status.exit_code.is_some(), "true should exit promptly");

        domain::undefine(&connection, &spec.name).expect("undefine should succeed");
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd; run with --ignored after setting up locally"]
    fn file_write_then_file_read_round_trips() {
        use crate::host::{
            connect::{Connection, DEFAULT_URI},
            domain::{self, DomainSpec},
        };

        let connection = Connection::open(DEFAULT_URI).expect("qemu:///session should be reachable");
        let name = format!("tartarus-test-fw-fr-{pid}", pid = std::process::id());
        let spec = DomainSpec::trivial(&name);

        let domain = domain::define(&connection, &spec).expect("define should succeed");
        let agent = Agent::new(domain);

        let payload: &[u8] = b"hello, tartarus\n";
        let path = format!("/tmp/tartarus-agent-rt-{pid}", pid = std::process::id());
        let timeout = Duration::from_secs(10);

        let request = FileWriteRequest {
            contents: payload,
            mode: 0o600,
            owner: "fedora",
            path: &path,
            timeout,
        };
        agent.file_write(&request).expect("file_write should round-trip");

        let read_back = agent.file_read(&path, timeout).expect("file_read should succeed");
        assert_eq!(read_back, payload, "round-tripped bytes should equal the input");

        domain::undefine(&connection, &spec.name).expect("undefine should succeed");
    }
}
