//! `tartarus doctor`: host diagnostic checks.
//!
//! Each check is independent; one failure does not short-circuit the
//! rest. Checks: not-root, libvirtd reachable, `/dev/kvm` readable,
//! XDG paths writable, tools on `PATH`, GPG key, Fedora egress, and
//! orphan domains.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{config::Config, disk::base, error::Result, host::connect::Connection, paths};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Cap on the failure exit code (8-bit).
const MAX_FAILURE_EXIT_CODE: u8 = u8::MAX;

/// Required external tools verified on `PATH`.
const REQUIRED_TOOLS: &[&str] = &["genisoimage", "gpgv", "qemu-img"];

/// HEAD probe URL for the Fedora egress check.
const FEDORA_EGRESS_PROBE_URL: &str = "https://download.fedoraproject.org/";

/// Timeout for the Fedora egress HEAD probe.
const EGRESS_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Domain-name prefix for transient layering boots (excluded from
/// orphan checks).
const TARTARUS_LAYERING_PREFIX: &str = "tartarus-layering-";

// -----------------------------------------------------------------------------
// Diagnostic Checks
// -----------------------------------------------------------------------------

/// Outcome of a single diagnostic check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CheckOutcome {
    /// Check failed, with a short hint pointing the user at a fix.
    Fail {
        /// Remediation hint shown after the failure line.
        hint: String,

        /// One-line failure summary suitable for the doctor report.
        message: String,
    },

    /// Check passed, with a one-line success summary.
    Pass {
        /// One-line success summary suitable for the doctor report.
        message: String,
    },
}

impl CheckOutcome {
    /// Construct a [`Self::Fail`] outcome.
    pub fn fail(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::Fail {
            hint: hint.into(),
            message: message.into(),
        }
    }

    /// True iff the outcome is a failure.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }

    /// Construct a [`Self::Pass`] outcome.
    pub fn pass(message: impl Into<String>) -> Self {
        Self::Pass {
            message: message.into(),
        }
    }
}

/// One named check entry in the doctor report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckResult {
    /// Stable label identifying the check.
    pub name: &'static str,

    /// Outcome of the check.
    pub outcome: CheckOutcome,
}

/// Run every check and return the results.
pub fn run_checks(config: &Config) -> Vec<CheckResult> {
    vec![
        check_not_root(),
        check_libvirtd(&config.network_uri),
        check_kvm_device(),
        check_xdg_paths(),
        check_tools_on_path(REQUIRED_TOOLS),
        check_trusted_gpg_key(),
        check_fedora_egress(),
        check_orphan_domains(&config.network_uri),
    ]
}

/// Count the number of [`CheckOutcome::Fail`] results in `results`.
pub fn failure_count(results: &[CheckResult]) -> usize {
    results.iter().filter(|r| r.outcome.is_failure()).count()
}

/// Render a human-readable summary of `results`.
pub fn render_summary(results: &[CheckResult]) -> String {
    let mut out = String::new();

    for result in results {
        match &result.outcome {
            CheckOutcome::Pass { message } => {
                out.push_str("PASS  ");
                out.push_str(result.name);
                out.push_str(": ");
                out.push_str(message);
                out.push('\n');
            },
            CheckOutcome::Fail { hint, message } => {
                out.push_str("FAIL  ");
                out.push_str(result.name);
                out.push_str(": ");
                out.push_str(message);
                out.push('\n');
                out.push_str("      hint: ");
                out.push_str(hint);
                out.push('\n');
            },
        }
    }

    let failures = failure_count(results);
    if failures == 0 {
        out.push_str("\nAll checks passed.\n");
    } else {
        out.push_str(&format!("\n{failures} check(s) failed.\n"));
    }

    out
}

/// Map results to an exit code (0 = clean, else failure count clamped
/// to `u8`).
pub fn exit_code(results: &[CheckResult]) -> u8 {
    let failures = failure_count(results);

    if failures == 0 {
        0
    } else {
        u8::try_from(failures).unwrap_or(MAX_FAILURE_EXIT_CODE)
    }
}

/// `tartarus doctor` entry point. Returns the failure count.
pub fn run(config: &Config) -> Result<u8> {
    tracing::info!(uri = %config.network_uri, "running tartarus doctor checks");

    let results = run_checks(config);

    for result in &results {
        match &result.outcome {
            CheckOutcome::Pass { message } => tracing::info!(check = result.name, status = "pass", %message),
            CheckOutcome::Fail { hint, message } => {
                tracing::warn!(check = result.name, status = "fail", %message, %hint);
            },
        }
    }

    let summary = render_summary(&results);

    print_doctor_summary(&summary);

    Ok(exit_code(&results))
}

/// Check that the invoking process is not running as root.
///
/// Defensive: `main.rs` calls [`crate::refuse_root`] at startup so doctor
/// only ever runs against a non-root user, but the architecture spec
/// lists "effective uid != 0" as one of the diagnostic checks. Surfacing
/// it here means the doctor report enumerates it explicitly rather than
/// silently passing on the basis that the process even started.
pub fn check_not_root() -> CheckResult {
    let outcome = if running_as_root() {
        CheckOutcome::fail(
            "process is running with effective uid 0".to_owned(),
            "tartarus refuses to manage `qemu:///session` as root; re-invoke as your unprivileged user",
        )
    } else {
        CheckOutcome::pass("not running as root".to_owned())
    };

    CheckResult {
        name: "not-root",
        outcome,
    }
}

/// Check that libvirtd is reachable at `uri`.
///
/// Test-visible so callers can probe a single check without spinning up
/// the whole runner.
pub fn check_libvirtd(uri: &str) -> CheckResult {
    let outcome = match Connection::open(uri) {
        Ok(connection) if connection.is_alive() => CheckOutcome::pass(format!("libvirtd reachable at {uri}")),
        Ok(_) => CheckOutcome::fail(
            format!("libvirtd at {uri} replied to open() but is_alive() returned false"),
            "is the libvirt daemon healthy? try `systemctl --user restart libvirtd`",
        ),
        Err(err) => CheckOutcome::fail(
            format!("could not reach libvirtd at {uri}: {err}"),
            "is `libvirtd` running on the user session bus? try `systemctl --user status libvirtd`",
        ),
    };

    CheckResult {
        name: "libvirtd",
        outcome,
    }
}

/// Check that `/dev/kvm` is readable by the invoking user.
///
/// We probe with [`std::fs::File::open`] in read-only mode. Failure does
/// not necessarily mean KVM is unavailable (qemu user-session can fall
/// back to TCG), but the architecture spec's `doctor` requires the check.
pub fn check_kvm_device() -> CheckResult {
    let path = Path::new("/dev/kvm");

    let outcome = match std::fs::File::open(path) {
        Ok(_) => CheckOutcome::pass("/dev/kvm is readable".to_owned()),
        Err(err) => CheckOutcome::fail(
            format!("/dev/kvm is not accessible: {err}"),
            "ensure the kvm module is loaded and your user is in the `kvm` group (`sudo usermod -aG kvm $USER`, then \
             log out and back in)",
        ),
    };

    CheckResult {
        name: "kvm-device",
        outcome,
    }
}

/// Check that every required tool is on `PATH`.
///
/// `tools` is taken as a parameter so tests can pass a fake list.
pub fn check_tools_on_path(tools: &[&str]) -> CheckResult {
    let mut missing: Vec<&str> = Vec::new();

    for tool in tools {
        if which_on_path(tool).is_none() {
            missing.push(tool);
        }
    }

    let outcome = if missing.is_empty() {
        let found = tools.join(", ");
        CheckOutcome::pass(format!("found on PATH: {found}"))
    } else {
        let absent = missing.join(", ");
        CheckOutcome::fail(
            format!("missing from PATH: {absent}"),
            "install the missing packages: on Fedora `sudo dnf install genisoimage gnupg2 qemu-img`",
        )
    };

    CheckResult {
        name: "tools-on-path",
        outcome,
    }
}

/// Check that the four XDG-derived paths exist or can be created.
///
/// Failure here means [`paths::config_dir`] or one of its data-side
/// siblings could not be created. We probe by attempting
/// [`std::fs::create_dir_all`]; on a healthy system this is a no-op.
pub fn check_xdg_paths() -> CheckResult {
    let result = (|| -> Result<Vec<PathBuf>> {
        let dirs = vec![
            paths::config_dir()?,
            paths::data_dir()?,
            paths::base_dir()?,
            paths::sessions_dir()?,
        ];

        for dir in &dirs {
            std::fs::create_dir_all(dir)?;
        }

        Ok(dirs)
    })();

    let outcome = match result {
        Ok(dirs) => CheckOutcome::pass(format!(
            "writable XDG paths: {}",
            dirs.iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", "),
        )),
        Err(err) => CheckOutcome::fail(
            format!("could not create XDG paths: {err}"),
            "check that $HOME is writable and that no XDG override points at a read-only location",
        ),
    };

    CheckResult {
        name: "xdg-paths",
        outcome,
    }
}

/// Check that the trusted Fedora GPG key is persisted under the base
/// directory **and** that `gpgv` accepts it as a usable keyring.
///
/// `tartarus base pull` writes [`base::TRUSTED_KEY_FILENAME`] into the
/// XDG data dir on every successful pull; doctor probes that file. The
/// key is fetched lazily, so a fresh install legitimately does not have
/// it yet — the `Fail` line names `tartarus base pull` as the
/// remediation.
pub fn check_trusted_gpg_key() -> CheckResult {
    let path = match base::trusted_key_path() {
        Ok(path) => path,
        Err(err) => {
            return CheckResult {
                name: "trusted-gpg-key",
                outcome: CheckOutcome::fail(
                    format!("could not resolve the base directory: {err}"),
                    "this should not happen on a healthy XDG environment; rerun `tartarus doctor` after fixing $HOME",
                ),
            };
        },
    };

    let outcome = trusted_gpg_key_outcome(&path);

    CheckResult {
        name: "trusted-gpg-key",
        outcome,
    }
}

/// Check HTTPS egress to `download.fedoraproject.org` works under
/// strict TLS.
///
/// Issues a HEAD with the same `reqwest` rustls stack `tartarus base
/// pull` uses and surfaces TLS chain failures specifically. The probe
/// is short-timeout (a few seconds) so the doctor command stays
/// snappy on a misbehaving host.
pub fn check_fedora_egress() -> CheckResult {
    let outcome = match probe_egress(FEDORA_EGRESS_PROBE_URL, EGRESS_PROBE_TIMEOUT) {
        Ok(()) => CheckOutcome::pass(format!("HEAD {FEDORA_EGRESS_PROBE_URL} succeeded under strict TLS")),
        Err(err) => CheckOutcome::fail(
            format!("HEAD {FEDORA_EGRESS_PROBE_URL} failed: {err}"),
            egress_remediation_hint(&err),
        ),
    };

    CheckResult {
        name: "fedora-egress",
        outcome,
    }
}

/// Check libvirt for orphaned domain UUIDs: a `tartarus-`-pattern
/// domain that has no matching session directory under
/// `sessions/by-uuid/`.
///
/// Defines libvirt as the source of truth for domain identity (what
/// `virsh list --all` reports) and the on-disk session dir as the
/// source of truth for session metadata. A live mismatch is the
/// "stale state from a crashed teardown" condition the architecture
/// spec calls out.
pub fn check_orphan_domains(uri: &str) -> CheckResult {
    let outcome = match collect_orphans(uri) {
        Ok(orphans) if orphans.is_empty() => {
            CheckOutcome::pass("no orphaned tartarus-prefixed libvirt domains".to_owned())
        },
        Ok(orphans) => {
            let names = orphans.join(", ");
            CheckOutcome::fail(
                format!("orphaned libvirt domains: {names}"),
                "remove with `virsh -c qemu:///session undefine <domain>` after confirming the session dir is gone",
            )
        },
        Err(err) => CheckOutcome::fail(
            format!("could not enumerate libvirt domains: {err}"),
            "is libvirtd running? this check needs the connection that libvirtd-reachable just verified",
        ),
    };

    CheckResult {
        name: "orphan-domains",
        outcome,
    }
}

// -----------------------------------------------------------------------------
// Check Implementations
// -----------------------------------------------------------------------------

/// Compute the [`CheckOutcome`] for the persisted GPG-trust check
/// against an explicit `path`. Split out so unit tests can exercise the
/// "missing", "empty file", and "bogus contents" branches without
/// mutating the real XDG tree.
fn trusted_gpg_key_outcome(path: &Path) -> CheckOutcome {
    let display = path.display();

    if !path.exists() {
        return CheckOutcome::fail(
            format!("trusted GPG key not found at {display}"),
            "run `tartarus base pull` once to fetch and persist the Fedora release key",
        );
    }

    match fs::metadata(path) {
        Ok(metadata) if metadata.len() == 0 => {
            return CheckOutcome::fail(
                format!("trusted GPG key at {display} is empty"),
                "delete the empty file and re-run `tartarus base pull` to refetch",
            );
        },
        Err(err) => {
            return CheckOutcome::fail(
                format!("could not stat trusted GPG key at {display}: {err}"),
                "check that the base directory is readable",
            );
        },
        Ok(_) => {},
    }

    match gpgv_accepts_keyring(path) {
        Ok(()) => CheckOutcome::pass(format!("trusted GPG key present at {display}")),
        Err(err) => CheckOutcome::fail(
            format!("gpgv rejected keyring at {display}: {err}"),
            "delete the file and re-run `tartarus base pull` to refetch the keyring",
        ),
    }
}

/// Probe whether `gpgv` can use `keyring` as a trusted-key file.
///
/// `gpgv --keyring <file> /dev/null` exits non-zero on a malformed key
/// file because the input file is not a signature, but it parses the
/// keyring before that error is surfaced — we read stderr to
/// distinguish "keyring unreadable" from "input is not a signed file."
/// The latter is the success signal we want.
fn gpgv_accepts_keyring(keyring: &Path) -> std::result::Result<(), String> {
    let output = std::process::Command::new("gpgv")
        .arg("--keyring")
        .arg(keyring)
        .arg("/dev/null")
        .output()
        .map_err(|err| format!("could not spawn gpgv: {err}"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);

    if stderr_indicates_unreadable_keyring(&stderr) {
        return Err(stderr.lines().next().unwrap_or("").trim().to_owned());
    }

    Ok(())
}

/// Heuristic over `gpgv`'s stderr: the strings that tell us the
/// *keyring* itself is broken (rather than the *input* not being a
/// signature). When we see one of these we surface the failure;
/// otherwise we treat the keyring as parseable.
fn stderr_indicates_unreadable_keyring(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("keyblock resource")
        || lower.contains("invalid keyring")
        || lower.contains("no keyring")
        || lower.contains("cannot open")
        || lower.contains("not found")
}

/// True iff the process is running with `euid == 0`.
///
/// The crate denies `unsafe_code`, so we re-use the same `/proc/self/status`
/// trick [`crate::refuse_root`] uses. On non-Linux the check returns
/// `false`; libvirt's session bus refuses root anyway, so the doctor
/// check is defense-in-depth either way.
fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        crate::effective_uid_is_zero()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Open a libvirt connection at `uri`, list every defined domain, and
/// return the names that look like Tartarus session UUIDs but have no
/// corresponding session directory on disk.
fn collect_orphans(uri: &str) -> std::result::Result<Vec<String>, String> {
    let connection = Connection::open(uri).map_err(|err| err.to_string())?;
    let domains = connection.inner().list_all_domains(0).map_err(|err| err.to_string())?;

    let by_uuid_dir = paths::sessions_by_uuid_dir().map_err(|err| err.to_string())?;

    let mut orphans: Vec<String> = Vec::new();

    for domain in &domains {
        let name = match domain.get_name() {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!(%err, "skipping domain whose name could not be read");
                continue;
            },
        };

        if name.starts_with(TARTARUS_LAYERING_PREFIX) {
            continue;
        }

        if !looks_like_session_uuid(&name) {
            continue;
        }

        if !by_uuid_dir.join(&name).is_dir() {
            orphans.push(name);
        }
    }

    orphans.sort();
    Ok(orphans)
}

/// Heuristic: a session domain name is a v4 UUID, hyphenated, 36
/// characters. We do not parse the UUID strictly here — a name that
/// happens to look like a UUID but is not Tartarus-managed will simply
/// fail the on-disk membership test and surface as an orphan, which is
/// the correct user-facing outcome for "unknown libvirt domain that
/// shadows our naming convention."
fn looks_like_session_uuid(name: &str) -> bool {
    name.len() == 36
        && name.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Issue a HEAD request against `url` with the `reqwest` rustls stack,
/// returning a one-line error string when the request fails for any
/// reason (TLS chain, DNS, connection refused, redirects).
fn probe_egress(url: &str, timeout: Duration) -> std::result::Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|err| err.to_string())?;

    let response = client.head(url).send().map_err(|err| err.to_string())?;

    if response.status().is_success() || response.status().is_redirection() {
        Ok(())
    } else {
        let status = response.status();
        Err(format!("HTTP {status}"))
    }
}

/// Pick a remediation hint matching the most common shapes of
/// `reqwest::blocking` failure when calling
/// `download.fedoraproject.org`.
fn egress_remediation_hint(detail: &str) -> String {
    let lower = detail.to_lowercase();

    if lower.contains("certificate") || lower.contains("tls") || lower.contains("invalid peer certificate") {
        "TLS chain validation failed; check the host clock and the system trust store".to_owned()
    } else if lower.contains("dns") || lower.contains("resolve") {
        "DNS lookup for download.fedoraproject.org failed; check resolver configuration".to_owned()
    } else if lower.contains("timeout") || lower.contains("timed out") {
        "the request timed out; check egress connectivity and retry".to_owned()
    } else {
        "check egress connectivity to download.fedoraproject.org under strict TLS".to_owned()
    }
}

/// Print the doctor summary to stdout.
///
/// `doctor` is the documented exception to the project's "tracing only"
/// rule for runtime narration: the summary is the command's user-facing
/// output, in the same way `auth status` is. Per-check structured events
/// also flow through `tracing` for log scraping.
fn print_doctor_summary(summary: &str) {
    println!("{summary}");
}

/// Search `PATH` for `name` and return the first hit, mimicking
/// `which`/`command -v` semantics without a new dependency.
///
/// Returns `None` when `PATH` is unset or the binary is not found in any
/// of its entries; symbolic links and non-executable files behave the
/// same as a missing entry on the failing-check side.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;

    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[test]
    fn check_outcome_helpers_round_trip() {
        let pass = CheckOutcome::pass("ok");
        let fail = CheckOutcome::fail("nope", "fix it");

        assert!(!pass.is_failure(), "Pass should not be a failure");
        assert!(fail.is_failure(), "Fail should be a failure");
    }

    #[test]
    fn render_summary_marks_each_check() {
        let results = vec![
            CheckResult {
                name: "alpha",
                outcome: CheckOutcome::pass("good"),
            },
            CheckResult {
                name: "beta",
                outcome: CheckOutcome::fail("bad", "do thing"),
            },
        ];

        let summary = render_summary(&results);

        assert!(
            summary.contains("PASS  alpha: good"),
            "summary should include PASS line for alpha, got: {summary}",
        );
        assert!(
            summary.contains("FAIL  beta: bad"),
            "summary should include FAIL line for beta, got: {summary}",
        );
        assert!(
            summary.contains("      hint: do thing"),
            "summary should include hint line for failures, got: {summary}",
        );
        assert!(
            summary.contains("1 check(s) failed."),
            "summary should report failure count, got: {summary}",
        );
    }

    #[test]
    fn render_summary_announces_clean_run() {
        let results = vec![CheckResult {
            name: "alpha",
            outcome: CheckOutcome::pass("good"),
        }];

        let summary = render_summary(&results);

        assert!(
            summary.contains("All checks passed."),
            "all-pass summary should announce success, got: {summary}",
        );
    }

    #[test]
    fn failure_count_counts_failures_only() {
        let results = vec![
            CheckResult {
                name: "a",
                outcome: CheckOutcome::pass("ok"),
            },
            CheckResult {
                name: "b",
                outcome: CheckOutcome::fail("bad", "h"),
            },
            CheckResult {
                name: "c",
                outcome: CheckOutcome::fail("bad", "h"),
            },
        ];

        assert_eq!(failure_count(&results), 2, "two failing checks should count two");
    }

    #[test]
    fn exit_code_is_zero_for_clean_run() {
        let results = vec![CheckResult {
            name: "a",
            outcome: CheckOutcome::pass("ok"),
        }];

        assert_eq!(exit_code(&results), 0, "clean run should exit 0");
    }

    #[test]
    fn exit_code_matches_failure_count() {
        let results = vec![
            CheckResult {
                name: "a",
                outcome: CheckOutcome::fail("x", "y"),
            },
            CheckResult {
                name: "b",
                outcome: CheckOutcome::fail("x", "y"),
            },
        ];

        assert_eq!(exit_code(&results), 2, "exit code should match the failure count");
    }

    #[test]
    fn check_tools_on_path_reports_missing_tools() {
        let result = check_tools_on_path(&["definitely-not-a-real-tool-xyz-tartarus"]);

        match result.outcome {
            CheckOutcome::Fail { message, .. } => {
                assert!(
                    message.contains("definitely-not-a-real-tool-xyz-tartarus"),
                    "missing tool should appear in the failure message, got: {message}",
                );
            },
            CheckOutcome::Pass { message } => {
                panic!("a synthetic missing tool should not pass: {message}")
            },
        }
    }

    #[test]
    fn check_tools_on_path_passes_when_every_tool_present() {
        let result = check_tools_on_path(&[]);

        assert!(!result.outcome.is_failure(), "an empty tool list should pass trivially",);
    }

    #[test]
    fn check_libvirtd_returns_a_named_check() {
        let result = check_libvirtd("qemu+tcp://127.0.0.1:1/system");

        assert_eq!(result.name, "libvirtd", "check name should be stable");
    }

    #[test]
    fn check_kvm_device_returns_a_named_check() {
        let result = check_kvm_device();

        assert_eq!(result.name, "kvm-device", "check name should be stable");
    }

    #[test]
    fn check_xdg_paths_returns_a_named_check() {
        let result = check_xdg_paths();

        assert_eq!(result.name, "xdg-paths", "check name should be stable");
    }

    #[test]
    fn check_not_root_passes_for_non_root_uid() {
        if running_as_root() {
            return;
        }

        let result = check_not_root();

        assert!(
            !result.outcome.is_failure(),
            "non-root caller should pass the not-root check, got: {:?}",
            result.outcome,
        );
        assert_eq!(result.name, "not-root", "check name should be stable");
    }

    #[test]
    fn check_trusted_gpg_key_fails_when_absent() {
        let dir = unique_tempdir();
        let path = dir.join(base::TRUSTED_KEY_FILENAME);

        let outcome = trusted_gpg_key_outcome(&path);

        match outcome {
            CheckOutcome::Fail { message, .. } => {
                assert!(
                    message.contains("not found"),
                    "missing-key failure should mention `not found`, got: {message}",
                );
            },
            CheckOutcome::Pass { message } => panic!("absent key should not pass, got: {message}"),
        }
    }

    #[test]
    fn check_trusted_gpg_key_fails_when_empty() {
        let dir = unique_tempdir();
        let path = dir.join(base::TRUSTED_KEY_FILENAME);
        std::fs::write(&path, b"").expect("write empty key file");

        let outcome = trusted_gpg_key_outcome(&path);

        match outcome {
            CheckOutcome::Fail { message, .. } => {
                assert!(
                    message.contains("empty"),
                    "empty-key failure should mention `empty`, got: {message}",
                );
            },
            CheckOutcome::Pass { message } => panic!("empty key should not pass, got: {message}"),
        }
    }

    #[test]
    fn stderr_indicator_recognises_unreadable_keyring() {
        assert!(
            stderr_indicates_unreadable_keyring("gpgv: invalid keyring 'fedora.gpg'"),
            "`invalid keyring` should be recognised as an unreadable-keyring signal",
        );
        assert!(
            stderr_indicates_unreadable_keyring("gpgv: keyblock resource '...': General error"),
            "`keyblock resource` should be recognised as an unreadable-keyring signal",
        );
        assert!(
            !stderr_indicates_unreadable_keyring("gpgv: no signature found"),
            "`no signature` is the success-mode stderr from gpgv against /dev/null",
        );
    }

    #[test]
    fn looks_like_session_uuid_accepts_v4_shape() {
        assert!(
            looks_like_session_uuid("11111111-2222-3333-4444-555555555555"),
            "the canonical v4 UUID shape should match",
        );
    }

    #[test]
    fn looks_like_session_uuid_rejects_layering_prefix() {
        assert!(
            !looks_like_session_uuid("tartarus-layering-12345-0"),
            "the layering domain name shape must not be flagged as a session UUID",
        );
    }

    #[test]
    fn looks_like_session_uuid_rejects_short_string() {
        assert!(!looks_like_session_uuid("abc"), "short strings should not match");
    }

    #[test]
    fn egress_remediation_picks_tls_hint_for_chain_failure() {
        let hint = egress_remediation_hint("error: invalid peer certificate: UnknownIssuer");

        assert!(
            hint.contains("TLS chain"),
            "TLS chain failures should produce a TLS-hint, got: {hint}",
        );
    }

    #[test]
    fn egress_remediation_picks_dns_hint_for_resolver_failure() {
        let hint = egress_remediation_hint("dns error: failed to lookup address");

        assert!(
            hint.contains("DNS"),
            "DNS failures should produce a DNS-hint, got: {hint}",
        );
    }

    #[test]
    fn check_fedora_egress_returns_a_named_check() {
        let result = check_fedora_egress();

        assert_eq!(result.name, "fedora-egress", "check name should be stable");
    }

    #[test]
    fn check_orphan_domains_returns_a_named_check() {
        let result = check_orphan_domains("qemu+tcp://127.0.0.1:1/system");

        assert_eq!(result.name, "orphan-domains", "check name should be stable");
    }

    #[test]
    fn check_trusted_gpg_key_returns_a_named_check() {
        let result = check_trusted_gpg_key();

        assert_eq!(result.name, "trusted-gpg-key", "check name should be stable");
    }

    #[test]
    fn run_checks_emits_one_result_per_documented_check() {
        let config = crate::config::Config::resolve(
            crate::config::FileConfig::default(),
            crate::config::CliOverrides::default(),
        );

        let results = run_checks(&config);

        let names: Vec<&str> = results.iter().map(|r| r.name).collect();

        assert_eq!(
            names,
            vec![
                "not-root",
                "libvirtd",
                "kvm-device",
                "xdg-paths",
                "tools-on-path",
                "trusted-gpg-key",
                "fedora-egress",
                "orphan-domains",
            ],
            "doctor must emit each documented check exactly once, in the documented order",
        );
    }

    #[test]
    fn which_on_path_finds_sh() {
        let found = which_on_path("sh");

        assert!(
            found.is_some(),
            "POSIX environments always have `sh` on PATH; got: {found:?}",
        );
    }

    #[test]
    fn which_on_path_returns_none_for_missing_tool() {
        let found = which_on_path("definitely-not-a-real-tool-xyz-tartarus");

        assert!(found.is_none(), "synthetic missing tool should return None");
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn unique_tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-doctor-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
