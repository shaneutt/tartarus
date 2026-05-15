//! Base image library: pull, list, prune.
//!
//! Owns `~/.local/share/tartarus/base/`. [`pull`] downloads, GPG-verifies,
//! layers, and updates `current`. [`list`] enumerates versioned bases.
//! [`prune`] deletes bases with no referencing overlays.

pub mod layering_seed;

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde::Deserialize;
use tartarus_provider::paths;

use crate::{
    error::{Error, Result},
    host::{
        connect::Connection,
        domain::{self, LayeringDomainSpec},
    },
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default Fedora release.
pub const DEFAULT_FEDORA_RELEASE: &str = "41";

/// Default Fedora cloud base image filename.
pub const DEFAULT_FEDORA_IMAGE_NAME: &str = "Fedora-Cloud-Base-Generic-41-1.4.x86_64.qcow2";

/// Mirror prefix for Fedora image artefacts.
pub const FEDORA_MIRROR_PREFIX: &str = "https://download.fedoraproject.org/pub/fedora/linux/releases";

/// Filename of the Fedora-published checksum manifest.
pub const FEDORA_CHECKSUM_FILE: &str = "Fedora-Cloud-41-1.4-x86_64-CHECKSUM";

/// Fedora's published release-keys bundle URL.
pub const FEDORA_GPG_KEY_URL: &str = "https://getfedora.org/static/fedora.gpg";

/// Maximum bytes for the GPG key fetch.
const MAX_GPG_KEY_BYTES: u64 = 5_242_880; // 5 MiB

/// Maximum bytes for the CHECKSUM file.
const MAX_CHECKSUM_BYTES: u64 = 65_536; // 64 KiB

/// Maximum bytes for the cloud base image fetch (8 GiB).
const MAX_IMAGE_BYTES: u64 = 8_589_934_592; // 8 GiB

/// HTTP timeout for image download (one hour).
const IMAGE_DOWNLOAD_TIMEOUT_SECS: u64 = 3_600;

/// HTTP timeout for small auxiliary fetches.
const AUX_DOWNLOAD_TIMEOUT_SECS: u64 = 60;

/// Time budget for the layering boot (30 minutes).
const LAYERING_BOOT_TIMEOUT_SECS: u64 = 1_800;

/// Filename of the in-place symlink pointing at the active base.
const CURRENT_SYMLINK_NAME: &str = "current";

/// Filename of the persisted Fedora GPG keyring.
pub const TRUSTED_KEY_FILENAME: &str = "fedora.gpg";

/// Filename of the sidecar fingerprint pin (TOFU anchor).
pub const KEY_FINGERPRINTS_FILENAME: &str = "fedora.gpg.fingerprints";

/// Counter feeding the per-process suffix on the temp `current` symlink.
static SYMLINK_COUNTER: AtomicU64 = AtomicU64::new(0);

// -----------------------------------------------------------------------------
// Base Library
// -----------------------------------------------------------------------------

/// Failure modes specific to the base library.
#[derive(Debug, thiserror::Error)]
pub enum BaseError {
    /// Image SHA-256 does not match the signed manifest entry.
    #[error(
        "image at {path} failed CHECKSUM binding: expected {expected}, got {actual}. \
         hint: re-run `tartarus base pull`; if the failure persists the upstream mirror is compromised."
    )]
    ChecksumMismatch {
        /// SHA-256 hex digest computed from the downloaded image.
        actual: String,

        /// SHA-256 hex digest taken from the signed manifest entry.
        expected: String,

        /// Path of the downloaded image that failed verification.
        path: PathBuf,
    },

    /// Download failed.
    #[error(
        "download of {url} failed: {source}. hint: check network access and TLS chain to download.fedoraproject.org."
    )]
    Download {
        /// Underlying transport or IO error.
        #[source]
        source: std::io::Error,

        /// URL that was being fetched.
        url: String,
    },

    /// GPG verification failed.
    #[error(
        "GPG verification failed for {artifact}: {detail}. hint: confirm the trusted key is current; \
         re-run `tartarus base pull` to refetch artefacts."
    )]
    GpgVerification {
        /// Artifact that failed verification (e.g. the CHECKSUM file).
        artifact: PathBuf,

        /// Short human-readable detail extracted from `gpgv`'s exit
        /// status / output.
        detail: String,
    },

    /// Keyring fingerprint does not match the TOFU pin.
    #[error(
        "Fedora GPG keyring fingerprint mismatch: observed {observed}, pinned {pinned}. \
         hint: if Fedora rotated the release key, delete `~/.local/share/tartarus/base/fedora.gpg*` and re-run \
         `tartarus base pull`. Otherwise the trust anchor was tampered with."
    )]
    GpgFingerprintMismatch {
        /// Comma-separated fingerprint(s) extracted from the keyring at hand.
        observed: String,

        /// Comma-separated fingerprint(s) recorded in the sidecar pin.
        pinned: String,
    },

    /// Base directory inaccessible.
    #[error("base library at {path} could not be read: {source}. hint: run `tartarus base pull` first.")]
    Inaccessible {
        /// Path that was probed.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Base file name does not match `fedora-N-YYYY-MM-DD.qcow2`.
    #[error("base file name {name} does not match the `fedora-N-YYYY-MM-DD.qcow2` pattern")]
    InvalidBaseName {
        /// Offending file name.
        name: String,
    },

    /// `qemu-img info` could not be parsed for an overlay.
    #[error("could not parse `qemu-img info --output=json` for {overlay}: {detail}")]
    OverlayInfo {
        /// Short detail extracted from the JSON parser or process error.
        detail: String,

        /// Path of the overlay whose info could not be parsed.
        overlay: PathBuf,
    },

    /// External tool exited non-zero.
    #[error("tool `{tool}` exited non-zero ({status}): {source}")]
    Tool {
        /// Underlying error.
        #[source]
        source: std::io::Error,

        /// Exit status, or `"spawn-failed"`.
        status: String,

        /// Name of the tool that failed (e.g. `genisoimage`).
        tool: &'static str,
    },
}

/// One versioned base image present in the library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Base {
    /// File name without the directory component, e.g.
    /// `fedora-41-2026-05-01.qcow2`.
    pub name: String,

    /// ISO-8601 release date parsed from [`Self::name`] (e.g. `2026-05-01`).
    pub date: String,

    /// Absolute path to the qcow2 file.
    pub path: PathBuf,

    /// Fedora release number parsed from [`Self::name`] (e.g. `"41"`).
    pub release: String,
}

impl Base {
    /// Parse a base file name into a [`Base`] anchored at `base_dir`.
    pub fn from_name(base_dir: &Path, name: &str) -> Result<Self> {
        let parsed = parse_base_name(name).ok_or_else(|| BaseError::InvalidBaseName { name: name.to_owned() })?;

        Ok(Self {
            name: name.to_owned(),
            date: parsed.date,
            path: base_dir.join(name),
            release: parsed.release,
        })
    }
}

/// Snapshot of the base library.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BaseLibrary {
    /// Versioned bases, sorted by file name (newest last).
    pub bases: Vec<Base>,

    /// Filename `current` resolves to, or `None`.
    pub current: Option<String>,
}

/// What the prune planner decided to do.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PrunePlan {
    /// Bases scheduled for deletion (no overlays reference them, and
    /// they are not the `current` target).
    pub deleted: Vec<Base>,

    /// Bases retained because they are still referenced by an overlay or
    /// are the `current` target. Each entry carries its retention reason
    /// for the rendered output.
    pub kept: Vec<KeptBase>,
}

/// One retained base, annotated with the reason it was kept.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeptBase {
    /// The retained base.
    pub base: Base,

    /// True iff the base is the `current` symlink target.
    pub is_current: bool,

    /// Number of overlays that name this base as their backing file.
    pub overlay_refcount: usize,
}

/// Path the trusted Fedora GPG key is persisted at after a successful
/// `tartarus base pull`.
///
/// Written from inside [`pull`] once `gpgv` accepts the manifest, so
/// `tartarus doctor` can re-verify the trust anchor without going to
/// the network. `Ok(path)` is returned regardless of whether the file
/// already exists — callers (today only `doctor`) probe with
/// [`std::path::Path::exists`] themselves.
pub fn trusted_key_path() -> Result<PathBuf> {
    Ok(paths::base_dir()?.join(TRUSTED_KEY_FILENAME))
}

/// External-tool dependencies the [`pull`] flow drives.
///
/// Production wires this trait to live HTTPS, `gpg`/`gpgv`/`sha256sum`/
/// `genisoimage`, and a real libvirt connection via [`RealDeps`]. Unit
/// tests substitute a mock so the orchestration shape (download order,
/// verification gate, on-disk layout) is exercisable without a live
/// network, those tools on `PATH`, or `libvirtd`.
pub trait Deps {
    /// Apply the Tartarus layer to `image_path` by booting Fedora once
    /// with `seed_iso_path` attached as a CD-ROM. Production routes this
    /// through libvirt; tests skip the boot.
    fn apply_layer(&self, image_path: &Path, seed_iso_path: &Path) -> Result<()>;

    /// Stream `url` to `dest` with TLS-strict HTTPS, returning a typed
    /// error on transport, status, or cap-overflow failure.
    fn download(&self, url: &str, dest: &Path, timeout_secs: u64, max_bytes: Option<u64>) -> Result<()>;

    /// Build a NoCloud seed ISO at `iso_path` from the `user-data` and
    /// `meta-data` files in `workdir`.
    fn genisoimage(&self, workdir: &Path, iso_path: &Path) -> Result<()>;

    /// Extract the canonical 40-hex-char fingerprint(s) from `keyring`
    /// using an isolated `gpg` homedir under `workdir`.
    fn gpg_fingerprints(&self, workdir: &Path, keyring: &Path) -> Result<Vec<String>>;

    /// Run `gpgv` against `signed` with `keyring` as the trust anchor.
    fn gpgv(&self, keyring: &Path, signed: &Path) -> Result<()>;

    /// Compute the SHA-256 hex digest of `path`.
    fn sha256(&self, path: &Path) -> Result<String>;
}

/// Production [`Deps`] implementation: real network, real `gpg`/`gpgv`/
/// `sha256sum`/`genisoimage`, real libvirt connection.
#[derive(Debug, Default)]
pub struct RealDeps;

impl Deps for RealDeps {
    fn apply_layer(&self, image_path: &Path, seed_iso_path: &Path) -> Result<()> {
        apply_layer_via_boot(image_path, seed_iso_path)
    }

    fn download(&self, url: &str, dest: &Path, timeout_secs: u64, max_bytes: Option<u64>) -> Result<()> {
        download_to_file(url, dest, timeout_secs, max_bytes)
    }

    fn genisoimage(&self, workdir: &Path, iso_path: &Path) -> Result<()> {
        genisoimage_build(workdir, iso_path)
    }

    fn gpg_fingerprints(&self, workdir: &Path, keyring: &Path) -> Result<Vec<String>> {
        extract_keyring_fingerprints(workdir, keyring)
    }

    fn gpgv(&self, keyring: &Path, signed: &Path) -> Result<()> {
        gpgv_verify(keyring, signed)
    }

    fn sha256(&self, path: &Path) -> Result<String> {
        sha256sum(path)
    }
}

/// Pull the latest Fedora cloud base, GPG-verify it, apply the Tartarus
/// layer via boot + cloud-init, and update `current`.
///
/// Thin wrapper over [`pull_with`] backed by [`RealDeps`]. The
/// orchestration shape (download order, verification gate, on-disk
/// layout) lives in [`pull_with`] so the unit tests can drive it
/// against a mock implementation of [`Deps`].
pub fn pull(release: &str) -> Result<Base> {
    pull_with(release, &RealDeps)
}

/// Same as [`pull`] but takes its tool-shaped dependencies via a
/// [`Deps`] trait object. Each step narrates its progress via
/// `tracing::info!` because `base pull` is long-running and the user
/// wants to see where it is.
///
/// The layering boot itself requires a running `libvirtd` and `/dev/kvm`;
/// the integration test that exercises this end-to-end is `#[ignore]`'d.
/// The host-side orchestration (download, GPG verify, version+symlink
/// layout, layering-seed authoring, shell-out to `qemu-img` / `genisoimage`
/// / `gpgv`) is exercisable without libvirt via the `Deps` mock used by
/// the unit tests below.
pub fn pull_with<D: Deps + ?Sized>(release: &str, deps: &D) -> Result<Base> {
    let base_dir = paths::base_dir()?;
    pull_with_in(release, deps, &base_dir)
}

/// [`pull_with`] applied to an explicit `base_dir`. Test-friendly because
/// the caller can route the side-effecting steps into a tempdir without
/// touching the XDG-derived path.
fn pull_with_in<D: Deps + ?Sized>(release: &str, deps: &D, base_dir: &Path) -> Result<Base> {
    fs::create_dir_all(base_dir).map_err(|source| BaseError::Inaccessible {
        path: base_dir.to_path_buf(),
        source,
    })?;

    let workdir_path = workdir_for_pull(base_dir);
    fs::create_dir_all(&workdir_path).map_err(|source| BaseError::Inaccessible {
        path: workdir_path.clone(),
        source,
    })?;

    // RAII guard so that any early return below — network failure, GPG
    // failure, layering boot failure — leaves no half-finished artefacts
    // behind in `base_dir`.
    let mut workdir_guard = WorkdirGuard::adopt(workdir_path.clone());
    let workdir = workdir_path;

    tracing::info!(?workdir, release, "starting base pull");

    let image_url = fedora_image_url(release);
    let checksum_url = fedora_checksum_url(release);
    let key_url = FEDORA_GPG_KEY_URL.to_owned();

    let downloaded_image = workdir.join(DEFAULT_FEDORA_IMAGE_NAME);
    let downloaded_checksum = workdir.join(FEDORA_CHECKSUM_FILE);
    let trusted_key = workdir.join("fedora.gpg");

    tracing::info!(url = %image_url, "downloading Fedora cloud base image (TLS strict)");
    deps.download(
        &image_url,
        &downloaded_image,
        IMAGE_DOWNLOAD_TIMEOUT_SECS,
        Some(MAX_IMAGE_BYTES),
    )?;

    tracing::info!(url = %checksum_url, "downloading signed CHECKSUM manifest (TLS strict)");
    deps.download(
        &checksum_url,
        &downloaded_checksum,
        AUX_DOWNLOAD_TIMEOUT_SECS,
        Some(MAX_CHECKSUM_BYTES),
    )?;

    let persisted_keyring = base_dir.join(TRUSTED_KEY_FILENAME);
    if persisted_keyring.exists() {
        tracing::info!(
            keyring = %persisted_keyring.display(),
            "reusing persisted Fedora GPG key (TOFU pin)",
        );
        warn_if_keyring_world_writable(&persisted_keyring);
        fs::copy(&persisted_keyring, &trusted_key).map_err(|source| BaseError::Inaccessible {
            path: trusted_key.clone(),
            source,
        })?;
    } else {
        tracing::info!(url = %key_url, "downloading Fedora GPG key (TLS strict; first-use trust anchor)");
        deps.download(
            &key_url,
            &trusted_key,
            AUX_DOWNLOAD_TIMEOUT_SECS,
            Some(MAX_GPG_KEY_BYTES),
        )?;
    }

    let observed_fingerprints = deps.gpg_fingerprints(&workdir, &trusted_key)?;
    enforce_pinned_fingerprints(base_dir, &observed_fingerprints)?;

    tracing::info!(checksum = %downloaded_checksum.display(), key = %trusted_key.display(), "verifying signature with gpgv");
    deps.gpgv(&trusted_key, &downloaded_checksum)?;

    persist_trusted_key(base_dir, &trusted_key)?;
    persist_pinned_fingerprints(base_dir, &observed_fingerprints)?;

    tracing::info!(image = %downloaded_image.display(), "binding image to verified manifest via SHA-256");
    verify_image_against_manifest(deps, &downloaded_image, &downloaded_checksum, DEFAULT_FEDORA_IMAGE_NAME)?;

    tracing::info!("authoring layering cloud-init seed");
    layering_seed::write_seed_files(&workdir)?;
    let seed_iso = workdir.join("layering-seed.iso");
    deps.genisoimage(&workdir, &seed_iso)?;

    tracing::info!(image = %downloaded_image.display(), seed = %seed_iso.display(), "booting Fedora base for layering");
    deps.apply_layer(&downloaded_image, &seed_iso)?;

    let date = today_iso();
    let final_name = format!("fedora-{release}-{date}.qcow2");
    let final_path = base_dir.join(&final_name);

    tracing::info!(from = %downloaded_image.display(), to = %final_path.display(), "moving layered image into place");
    fs::rename(&downloaded_image, &final_path).map_err(|source| BaseError::Inaccessible {
        path: final_path.clone(),
        source,
    })?;

    tracing::info!(target = %final_name, "atomically updating `current` symlink");
    update_current_symlink(base_dir, &final_name)?;

    tracing::info!(?workdir, "cleaning layering workdir");
    workdir_guard.disarm();
    let _ = std::fs::remove_dir_all(&workdir);
    drop(workdir_guard);

    Base::from_name(base_dir, &final_name)
}

/// Read the base library off disk and return a [`BaseLibrary`] snapshot.
///
/// Versioned bases are matched against the `fedora-N-YYYY-MM-DD.qcow2`
/// pattern; non-matching entries are ignored (so a stray temp file does
/// not abort the listing). The `current` symlink is read but not
/// followed beyond reading its target name.
pub fn list() -> Result<BaseLibrary> {
    let base_dir = paths::base_dir()?;

    if !base_dir.exists() {
        return Ok(BaseLibrary::default());
    }

    list_in(&base_dir)
}

/// [`list`] applied to a specific directory; the public entry point uses
/// the XDG-derived path. Test-friendly because callers can build a fake
/// base library in a tempdir.
pub fn list_in(base_dir: &Path) -> Result<BaseLibrary> {
    let read = fs::read_dir(base_dir).map_err(|source| BaseError::Inaccessible {
        path: base_dir.to_path_buf(),
        source,
    })?;

    let mut bases: Vec<Base> = Vec::new();

    for entry in read {
        let entry = entry.map_err(|source| BaseError::Inaccessible {
            path: base_dir.to_path_buf(),
            source,
        })?;

        let name = match entry.file_name().to_str() {
            Some(s) => s.to_owned(),
            None => continue,
        };

        if name == CURRENT_SYMLINK_NAME {
            continue;
        }

        if let Ok(base) = Base::from_name(base_dir, &name) {
            bases.push(base);
        }
    }

    bases.sort_by(|a, b| a.name.cmp(&b.name));

    let current = read_current_target(base_dir)?;

    Ok(BaseLibrary { bases, current })
}

/// Render a [`BaseLibrary`] as the user-facing `tartarus base list` table.
pub fn render_list(library: &BaseLibrary) -> String {
    let mut out = String::new();

    if library.bases.is_empty() {
        out.push_str("(no bases pulled yet — run `tartarus base pull`)\n");
        return out;
    }

    out.push_str("NAME                              RELEASE  DATE        CURRENT\n");

    for base in &library.bases {
        let marker = if library.current.as_deref().is_some_and(|c| c == base.name.as_str()) {
            "yes"
        } else {
            ""
        };

        out.push_str(&format!(
            "{name:<32}  {release:<7}  {date:<10}  {marker}\n",
            name = base.name,
            release = base.release,
            date = base.date,
            marker = marker,
        ));
    }

    out
}

/// Compute the prune plan: which bases would be deleted, which kept.
///
/// The plan does not touch disk. [`prune`] applies it (or [`render_prune`]
/// renders it). Pure data → tests and the CLI's `--dry-run` branch share
/// one entry point.
pub fn plan_prune() -> Result<PrunePlan> {
    let base_dir = paths::base_dir()?;
    let library = list_in(&base_dir)?;

    let sessions_dir = paths::sessions_by_uuid_dir()?;
    let backings = if sessions_dir.exists() {
        collect_overlay_backings(&sessions_dir)?
    } else {
        Vec::new()
    };

    Ok(plan_prune_with(&library, &backings))
}

/// Same as [`plan_prune`] but takes its inputs explicitly, for tests and
/// for callers that want to compute against a synthesised library.
pub fn plan_prune_with(library: &BaseLibrary, overlay_backings: &[PathBuf]) -> PrunePlan {
    let mut plan = PrunePlan::default();

    for base in &library.bases {
        let refcount = overlay_backings
            .iter()
            .filter(|b| b.file_name().and_then(|n| n.to_str()) == Some(base.name.as_str()))
            .count();
        let is_current = library.current.as_deref().is_some_and(|c| c == base.name.as_str());

        if refcount > 0 || is_current {
            plan.kept.push(KeptBase {
                base: base.clone(),
                is_current,
                overlay_refcount: refcount,
            });
        } else {
            plan.deleted.push(base.clone());
        }
    }

    plan
}

/// Apply a [`PrunePlan`]: delete every base in `plan.deleted`.
///
/// Returns the count of bytes freed on success. Each deletion is
/// independent: a failure on one base does not abort the others, but the
/// first error is surfaced once the plan finishes.
pub fn apply_prune(plan: &PrunePlan) -> Result<u64> {
    let mut freed: u64 = 0;
    let mut first_error: Option<Error> = None;

    for base in &plan.deleted {
        match fs::metadata(&base.path) {
            Ok(metadata) => {
                let size = metadata.len();
                if let Err(err) = fs::remove_file(&base.path) {
                    tracing::warn!(path = %base.path.display(), %err, "failed to delete base; continuing");
                    if first_error.is_none() {
                        first_error = Some(err.into());
                    }
                } else {
                    freed = freed.saturating_add(size);
                    tracing::info!(name = %base.name, bytes = size, "deleted unreferenced base");
                }
            },
            Err(err) => {
                tracing::warn!(path = %base.path.display(), %err, "could not stat base before delete; continuing");
                if first_error.is_none() {
                    first_error = Some(err.into());
                }
            },
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(freed)
}

/// Run the full prune flow: plan + apply + report.
///
/// `dry_run = true` produces the same human-readable report as a live
/// run but does not touch disk.
pub fn prune(dry_run: bool) -> Result<String> {
    let plan = plan_prune()?;

    if dry_run {
        return Ok(render_prune(&plan, true, 0));
    }

    let freed = apply_prune(&plan)?;

    Ok(render_prune(&plan, false, freed))
}

/// Render a [`PrunePlan`] for the user. `freed` is bytes deleted in a
/// live run; ignored when `dry_run` is true.
pub fn render_prune(plan: &PrunePlan, dry_run: bool, freed: u64) -> String {
    let mut out = String::new();

    if plan.deleted.is_empty() && plan.kept.is_empty() {
        out.push_str("no bases pulled yet — nothing to prune.\n");
        return out;
    }

    for kept in &plan.kept {
        let reason = if kept.is_current {
            format!(
                "{name} ... in use by {n} overlay(s){current}, kept",
                name = kept.base.name,
                n = kept.overlay_refcount,
                current = if kept.overlay_refcount == 0 {
                    " (current)"
                } else {
                    " (current, also referenced)"
                },
            )
        } else {
            format!(
                "{name} ... in use by {n} overlay(s), kept",
                name = kept.base.name,
                n = kept.overlay_refcount,
            )
        };
        out.push_str(&reason);
        out.push('\n');
    }

    for base in &plan.deleted {
        let action = if dry_run { "would remove" } else { "removed" };
        out.push_str(&format!("{name} ... unreferenced, {action}\n", name = base.name));
    }

    if !dry_run && freed > 0 {
        out.push_str(&format!("freed {freed} bytes\n"));
    } else if dry_run {
        out.push_str("(dry-run: no files deleted)\n");
    }

    out
}

// -----------------------------------------------------------------------------
// Pull Orchestration
// -----------------------------------------------------------------------------

/// RAII guard that removes a layering pull workdir on drop unless the
/// caller [disarms][`Self::disarm`] it first.
///
/// `tartarus base pull` writes the freshly-downloaded image, CHECKSUM,
/// GPG key, and seed files into a per-pull working directory under
/// `base/`. On the success path the caller [`Self::disarm`]s the guard
/// and the dir is removed normally; on any early return (TLS failure,
/// GPG mismatch, layering boot timeout) the guard's `Drop` is what
/// reclaims the partial artefacts so `base/` is never left littered.
struct WorkdirGuard {
    /// Whether the guard is responsible for removing `path` on drop.
    /// Constructed `true`; flipped to `false` by [`Self::disarm`] on
    /// the success path.
    armed: bool,

    /// Per-pull working directory the guard owns the cleanup for.
    path: PathBuf,
}

impl WorkdirGuard {
    /// Take ownership of `path` so that — unless [`Self::disarm`]ed first —
    /// it is removed when the guard drops.
    fn adopt(path: PathBuf) -> Self {
        Self { armed: true, path }
    }

    /// Mark the guard as not-responsible for cleanup. The caller has
    /// already finished with the workdir and is removing it explicitly.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for WorkdirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        match fs::remove_dir_all(&self.path) {
            Ok(()) => tracing::debug!(path = %self.path.display(), "removed layering pull workdir"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(err) => tracing::warn!(
                path = %self.path.display(),
                %err,
                "failed to clean layering pull workdir; manual `rm -rf` may be required",
            ),
        }
    }
}

/// Build the canonical Fedora image URL for a release.
pub(crate) fn fedora_image_url(release: &str) -> String {
    format!(
        "{prefix}/{release}/Cloud/x86_64/images/{file}",
        prefix = FEDORA_MIRROR_PREFIX,
        file = DEFAULT_FEDORA_IMAGE_NAME,
    )
}

/// Build the canonical CHECKSUM URL for a release.
pub(crate) fn fedora_checksum_url(release: &str) -> String {
    format!(
        "{prefix}/{release}/Cloud/x86_64/images/{file}",
        prefix = FEDORA_MIRROR_PREFIX,
        file = FEDORA_CHECKSUM_FILE,
    )
}

/// Per-pull working directory under the base dir. Holds the freshly
/// downloaded artefacts until the layering boot finishes; cleaned up on
/// success.
fn workdir_for_pull(base_dir: &Path) -> PathBuf {
    let pid = std::process::id();
    let n = SYMLINK_COUNTER.fetch_add(1, Ordering::Relaxed);
    base_dir.join(format!(".tartarus-pull-{pid}-{n}"))
}

/// Result of parsing a base file name.
struct ParsedBaseName {
    /// The `YYYY-MM-DD` date stem extracted from the file name.
    date: String,

    /// The Fedora release number (e.g. `41`) extracted from the file
    /// name.
    release: String,
}

/// Parse a `fedora-N-YYYY-MM-DD.qcow2` file name.
fn parse_base_name(name: &str) -> Option<ParsedBaseName> {
    let stem = name.strip_suffix(".qcow2")?;
    let rest = stem.strip_prefix("fedora-")?;

    let (release, date) = rest.split_once('-')?;

    if release.is_empty() || release.chars().any(|c| !c.is_ascii_digit()) {
        return None;
    }

    if !looks_like_iso_date(date) {
        return None;
    }

    Some(ParsedBaseName {
        date: date.to_owned(),
        release: release.to_owned(),
    })
}

/// Loose check that `s` matches `YYYY-MM-DD` shape with all-digit fields.
fn looks_like_iso_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }

    parts[0].len() == 4
        && parts[1].len() == 2
        && parts[2].len() == 2
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
}

/// Read the `current` symlink target as a file name (no directory).
///
/// Returns `Ok(None)` when the symlink does not exist or its target is
/// not a simple file name (e.g. an absolute path) — callers treat both
/// as "no current pointer."
fn read_current_target(base_dir: &Path) -> Result<Option<String>> {
    let link = base_dir.join(CURRENT_SYMLINK_NAME);

    match fs::read_link(&link) {
        Ok(target) => Ok(target.file_name().and_then(|n| n.to_str()).map(str::to_owned)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(BaseError::Inaccessible {
            path: link,
            source: err,
        }
        .into()),
    }
}

/// Atomically update `current` to point at `target_name` inside `base_dir`.
///
/// Same atomic pattern the auth subsystem uses for the config file:
/// create the new symlink under a temp name in the same directory, then
/// `rename` over `current`. `rename(2)` is atomic within one filesystem,
/// so the temp must live in the same directory.
fn update_current_symlink(base_dir: &Path, target_name: &str) -> Result<()> {
    let pid = std::process::id();
    let n = SYMLINK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = base_dir.join(format!(".current.tartarus-{pid}-{n}"));

    if tmp.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&tmp);
    }

    create_symlink(target_name, &tmp)?;

    let final_link = base_dir.join(CURRENT_SYMLINK_NAME);

    if let Err(err) = fs::rename(&tmp, &final_link) {
        let _ = fs::remove_file(&tmp);
        return Err(BaseError::Inaccessible {
            path: final_link,
            source: err,
        }
        .into());
    }

    Ok(())
}

/// Create a symlink. We deny `unsafe_code`, but `std::os::unix::fs::symlink`
/// is safe Rust and Unix is the only platform Tartarus targets.
#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|source| {
        BaseError::Inaccessible {
            path: link.to_path_buf(),
            source,
        }
        .into()
    })
}

/// Non-Unix shim. Tartarus does not target non-Unix in MVP, but the
/// build stays portable.
#[cfg(not(unix))]
fn create_symlink(_target: &str, link: &Path) -> Result<()> {
    Err(BaseError::Inaccessible {
        path: link.to_path_buf(),
        source: std::io::Error::other("symlinks are only supported on Unix"),
    }
    .into())
}

/// Stream a URL to disk with strict TLS, an optional max-bytes cap, and a
/// timeout. The HTTP client is built fresh per call because the surface
/// of `pull` is small (three downloads) and a long-lived client buys
/// nothing here.
fn download_to_file(url: &str, dest: &Path, timeout_secs: u64, max_bytes: Option<u64>) -> Result<()> {
    use std::io::{Read, Write};

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .https_only(true)
        .use_rustls_tls()
        .build()
        .map_err(|err| BaseError::Download {
            source: std::io::Error::other(err),
            url: url.to_owned(),
        })?;

    let mut response = client.get(url).send().map_err(|err| BaseError::Download {
        source: std::io::Error::other(err),
        url: url.to_owned(),
    })?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(BaseError::Download {
            source: std::io::Error::other(format!("HTTP {status}")),
            url: url.to_owned(),
        }
        .into());
    }

    let mut file = fs::File::create(dest).map_err(|source| BaseError::Inaccessible {
        path: dest.to_path_buf(),
        source,
    })?;

    let mut buf = [0_u8; 8_192];
    let mut total: u64 = 0;

    loop {
        let n = response.read(&mut buf).map_err(|source| BaseError::Download {
            source,
            url: url.to_owned(),
        })?;

        if n == 0 {
            break;
        }

        total = total.saturating_add(n as u64);

        if let Some(cap) = max_bytes
            && total > cap
        {
            return Err(BaseError::Download {
                source: std::io::Error::other(format!("response exceeded {cap}-byte cap")),
                url: url.to_owned(),
            }
            .into());
        }

        file.write_all(&buf[..n]).map_err(|source| BaseError::Inaccessible {
            path: dest.to_path_buf(),
            source,
        })?;
    }

    file.sync_all().map_err(|source| BaseError::Inaccessible {
        path: dest.to_path_buf(),
        source,
    })?;

    Ok(())
}

/// Copy the freshly-downloaded keyring out of the per-pull workdir
/// into a stable home under `base_dir`.
///
/// The pull flow GPG-verifies the CHECKSUM manifest with `keyring`
/// before this function runs, so the bytes we persist are already
/// known-good. Persisting the key makes the trust anchor available to
/// the doctor diagnostic (and any future re-verification path) without
/// re-fetching from the network. The destination is overwritten on
/// each pull so a key rotation upstream lands cleanly.
///
/// Warn if the persisted Fedora keyring is group/world-writable.
///
/// The keyring is the trust anchor for every subsequent base pull; if a
/// host-side compromise relaxed its mode the
/// [`enforce_pinned_fingerprints`] check would still catch a fingerprint
/// swap, but a permissions regression deserves a louder log line so the
/// operator notices the trust anchor's posture changed.
#[cfg(unix)]
fn warn_if_keyring_world_writable(keyring: &Path) {
    use std::os::unix::fs::MetadataExt;

    let Ok(meta) = fs::metadata(keyring) else {
        return;
    };
    let mode = meta.mode() & 0o777;
    if mode & 0o022 != 0 {
        tracing::warn!(
            path = %keyring.display(),
            mode = format!("{mode:o}"),
            "persisted Fedora keyring is group/world-writable; trust anchor may be compromised",
        );
    }
}

/// Non-Unix shim for [`warn_if_keyring_world_writable`]; modes are not
/// portable so the check is unix-only.
#[cfg(not(unix))]
fn warn_if_keyring_world_writable(_keyring: &Path) {}

/// Same atomic pattern as [`update_current_symlink`]: write to a
/// per-process temp name in the same directory, then `rename` over the
/// final destination. A pull interrupted mid-copy never leaves a
/// truncated keyring under [`TRUSTED_KEY_FILENAME`] for the doctor
/// check to misread.
fn persist_trusted_key(base_dir: &Path, keyring: &Path) -> Result<()> {
    let dest = base_dir.join(TRUSTED_KEY_FILENAME);
    let pid = std::process::id();
    let n = SYMLINK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = base_dir.join(format!(".{TRUSTED_KEY_FILENAME}.tartarus-{pid}-{n}"));

    if tmp.symlink_metadata().is_ok() {
        let _ = fs::remove_file(&tmp);
    }

    fs::copy(keyring, &tmp).map_err(|source| BaseError::Inaccessible {
        path: tmp.clone(),
        source,
    })?;

    if let Err(err) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        return Err(BaseError::Inaccessible {
            path: dest,
            source: err,
        }
        .into());
    }

    tracing::debug!(path = %dest.display(), "persisted Fedora trusted GPG key for doctor");
    Ok(())
}

/// Extract the canonical 40-hex-char fingerprint(s) from `keyring`.
///
/// Shells to `gpg --with-colons --list-keys` against an isolated GPG
/// homedir under `workdir`, parses `fpr:` lines, and returns the
/// fingerprints as uppercase hex strings.
fn extract_keyring_fingerprints(workdir: &Path, keyring: &Path) -> Result<Vec<String>> {
    let homedir = workdir.join(".gnupg-fpr");
    fs::create_dir_all(&homedir).map_err(|source| BaseError::Inaccessible {
        path: homedir.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&homedir, fs::Permissions::from_mode(0o700));
    }

    let output = Command::new("gpg")
        .arg("--homedir")
        .arg(&homedir)
        .arg("--no-default-keyring")
        .arg("--batch")
        .arg("--no-tty")
        .arg("--keyring")
        .arg(keyring)
        .arg("--with-colons")
        .arg("--list-keys")
        .output()
        .map_err(|source| BaseError::Tool {
            source,
            status: "spawn-failed".to_owned(),
            tool: "gpg",
        })?;

    if !output.status.success() {
        return Err(BaseError::Tool {
            source: std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            status: output.status.to_string(),
            tool: "gpg",
        }
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let fingerprints: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("fpr:"))
        .filter_map(|tail| {
            tail.split(':')
                .find(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
                .map(|s| s.to_ascii_uppercase())
        })
        .collect();

    if fingerprints.is_empty() {
        return Err(BaseError::Tool {
            source: std::io::Error::other("gpg --with-colons --list-keys returned no fpr: lines"),
            status: "empty-keyring".to_owned(),
            tool: "gpg",
        }
        .into());
    }

    Ok(fingerprints)
}

/// Compare `observed` against the fingerprint pin recorded in
/// `base_dir/fedora.gpg.fingerprints`, when present.
///
/// The pin is treated as the authoritative trust anchor: every observed
/// fingerprint must already appear in the persisted set, and the
/// persisted set must not be empty (defensive against a truncated
/// sidecar). Returns Ok when either the sidecar is absent (first-use)
/// or every observed fingerprint matches.
fn enforce_pinned_fingerprints(base_dir: &Path, observed: &[String]) -> Result<()> {
    let pin_path = base_dir.join(KEY_FINGERPRINTS_FILENAME);
    let pinned = match fs::read_to_string(&pin_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(
                fingerprints = ?observed,
                "no Fedora GPG fingerprint pin yet; persisting first-observed values (TOFU)",
            );
            return Ok(());
        },
        Err(source) => {
            return Err(BaseError::Inaccessible { path: pin_path, source }.into());
        },
    };

    let pinned_set: Vec<String> = pinned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_ascii_uppercase)
        .collect();

    if pinned_set.is_empty() {
        return Err(BaseError::GpgFingerprintMismatch {
            observed: observed.join(","),
            pinned: "<empty pin file>".to_owned(),
        }
        .into());
    }

    if !observed.iter().all(|fpr| pinned_set.contains(fpr)) {
        return Err(BaseError::GpgFingerprintMismatch {
            observed: observed.join(","),
            pinned: pinned_set.join(","),
        }
        .into());
    }

    tracing::debug!(
        fingerprints = ?observed,
        "Fedora GPG keyring fingerprint matches the persisted TOFU pin",
    );
    Ok(())
}

/// Persist `fingerprints` into the sidecar pin file under `base_dir`.
///
/// Idempotent: if the pin file already exists with the same contents,
/// the rewrite is still safe (atomic write-then-rename). The file lives
/// next to the keyring so the trust anchor and its fingerprint pin are
/// administered together.
fn persist_pinned_fingerprints(base_dir: &Path, fingerprints: &[String]) -> Result<()> {
    let dest = base_dir.join(KEY_FINGERPRINTS_FILENAME);
    let pid = std::process::id();
    let n = SYMLINK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = base_dir.join(format!(".{KEY_FINGERPRINTS_FILENAME}.tartarus-{pid}-{n}"));

    let body = fingerprints.join("\n") + "\n";
    fs::write(&tmp, body.as_bytes()).map_err(|source| BaseError::Inaccessible {
        path: tmp.clone(),
        source,
    })?;

    if let Err(err) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        return Err(BaseError::Inaccessible {
            path: dest,
            source: err,
        }
        .into());
    }

    tracing::debug!(path = %dest.display(), "persisted Fedora GPG fingerprint pin");
    Ok(())
}

/// Verify `signed` against `keyring` via `gpgv`.
///
/// `gpgv` exits 0 on a valid signature and non-zero otherwise; the
/// canonical (and only) signal is the exit code, which is what we check.
fn gpgv_verify(keyring: &Path, signed: &Path) -> Result<()> {
    let output = Command::new("gpgv")
        .arg("--keyring")
        .arg(keyring)
        .arg(signed)
        .output()
        .map_err(|source| BaseError::Tool {
            source,
            status: "spawn-failed".to_owned(),
            tool: "gpgv",
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BaseError::GpgVerification {
            artifact: signed.to_path_buf(),
            detail: stderr.trim().to_owned(),
        }
        .into());
    }

    Ok(())
}

/// Bind a downloaded image to a GPG-verified manifest by recomputing
/// its SHA-256 (via [`Deps::sha256`]) and comparing against the matching
/// entry parsed from the manifest.
///
/// The caller is responsible for having GPG-verified the manifest
/// before invoking this function. The image filename (without
/// directory) is used as the lookup key against the manifest.
///
/// Returns [`BaseError::ChecksumMismatch`] when the digests do not
/// match, [`BaseError::Tool`] when `sha256sum` cannot be executed or
/// returns malformed output, and [`BaseError::GpgVerification`]
/// (re-used for the "manifest exists but does not list this image"
/// case) when the manifest does not carry an entry for the image.
fn verify_image_against_manifest<D: Deps + ?Sized>(
    deps: &D,
    image: &Path,
    manifest: &Path,
    image_name: &str,
) -> Result<()> {
    let manifest_text = fs::read_to_string(manifest).map_err(|source| BaseError::Inaccessible {
        path: manifest.to_path_buf(),
        source,
    })?;

    let expected = parse_manifest_sha256(&manifest_text, image_name).ok_or_else(|| BaseError::GpgVerification {
        artifact: manifest.to_path_buf(),
        detail: format!("verified manifest carries no SHA256 entry for {image_name}"),
    })?;

    let actual = deps.sha256(image)?;

    if actual.eq_ignore_ascii_case(&expected) {
        Ok(())
    } else {
        Err(BaseError::ChecksumMismatch {
            actual,
            expected,
            path: image.to_path_buf(),
        }
        .into())
    }
}

/// Run `sha256sum` against `path` and return the lowercase hex digest.
fn sha256sum(path: &Path) -> Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|source| BaseError::Tool {
            source,
            status: "spawn-failed".to_owned(),
            tool: "sha256sum",
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BaseError::Tool {
            source: std::io::Error::other(stderr.trim().to_owned()),
            status: output.status.to_string(),
            tool: "sha256sum",
        }
        .into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let digest = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| BaseError::Tool {
            source: std::io::Error::other(format!("could not parse sha256sum output: {stdout:?}")),
            status: output.status.to_string(),
            tool: "sha256sum",
        })?
        .to_owned();

    Ok(digest)
}

/// Extract the SHA-256 hex digest for `image_name` from a Fedora-style
/// CHECKSUM manifest. Handles both the `BSD`-style line
/// `SHA256 (FILE) = HEX` and the `coreutils`-style `HEX  FILE`.
///
/// Case-insensitive on the algorithm name; the digest is returned
/// verbatim (the caller compares with [`str::eq_ignore_ascii_case`]).
fn parse_manifest_sha256(manifest: &str, image_name: &str) -> Option<String> {
    for line in manifest.lines() {
        let trimmed = line.trim();
        if let Some(digest) = parse_bsd_line(trimmed, image_name) {
            return Some(digest);
        }
        if let Some(digest) = parse_coreutils_line(trimmed, image_name) {
            return Some(digest);
        }
    }
    None
}

/// Parse a single BSD-style manifest line: `SHA256 (FILE) = HEX`.
fn parse_bsd_line(line: &str, image_name: &str) -> Option<String> {
    let rest = line.strip_prefix("SHA256 ").or_else(|| line.strip_prefix("sha256 "))?;
    let rest = rest.strip_prefix('(')?;
    let (file, rest) = rest.split_once(')')?;
    if file.trim() != image_name {
        return None;
    }
    let rest = rest.trim_start();
    let digest = rest.strip_prefix('=')?.trim();
    if digest.is_empty() {
        None
    } else {
        Some(digest.to_owned())
    }
}

/// Parse a single coreutils-style manifest line: `HEX  FILE`.
fn parse_coreutils_line(line: &str, image_name: &str) -> Option<String> {
    let mut parts = line.splitn(2, char::is_whitespace);
    let digest = parts.next()?;
    let file = parts.next()?.trim_start();
    if file != image_name || digest.len() < 32 {
        return None;
    }
    if digest.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(digest.to_owned())
    } else {
        None
    }
}

/// Build a NoCloud seed ISO at `iso_path` from the `user-data` and
/// `meta-data` files in `workdir`.
///
/// Thin shim over [`crate::seed::iso::run_genisoimage`] so the layering
/// seed ISO and the per-session NoCloud ISO share a single shell-out.
fn genisoimage_build(workdir: &Path, iso_path: &Path) -> Result<()> {
    crate::seed::iso::run_genisoimage(&crate::seed::genisoimage::KernelGenisoimage, workdir, iso_path)
}

/// Boot the freshly-downloaded base with the layering seed attached as a
/// CD-ROM, wait for cloud-init's `poweroff` to take effect, then
/// undefine the transient domain.
///
/// Requires a running `libvirtd` and `/dev/kvm`. The matching integration
/// test is `#[ignore]`'d because the build environment lacks both.
///
/// A [`domain::DomainGuard`] owns the post-define cleanup: the guard
/// undefines (and force-destroys when active) the layering domain on
/// any early return from this function, so a failed `start` or a
/// timed-out shutdown can no longer leak an inactive domain into
/// libvirt's database.
fn apply_layer_via_boot(image_path: &Path, seed_iso_path: &Path) -> Result<()> {
    // TODO(M2): plumb config.network_uri through `base::pull` so a
    // user with a non-default URI is honoured here as well. MVP is
    // local-only, so the default URI is the only reachable target.
    let connection = Connection::open(crate::host::connect::DEFAULT_URI)?;

    let pid = std::process::id();
    let n = SYMLINK_COUNTER.fetch_add(1, Ordering::Relaxed);
    let domain_name = format!("tartarus-layering-{pid}-{n}");

    let spec = LayeringDomainSpec::new(&domain_name, image_path, seed_iso_path);

    let _domain = domain::define_layering(&connection, &spec)?;
    let mut guard = domain::DomainGuard::adopt(&connection, &domain_name);

    domain::start(&connection, &domain_name)?;

    let timeout = Duration::from_secs(LAYERING_BOOT_TIMEOUT_SECS);
    let wait_result = domain::wait_for_shutoff(&connection, &domain_name, timeout);

    if wait_result.is_err() {
        let _ = domain::destroy(&connection, &domain_name);
    }

    domain::undefine(&connection, &domain_name)?;
    guard.disarm();

    wait_result
}

/// Walk `sessions_dir` and collect each overlay's backing-file path.
fn collect_overlay_backings(sessions_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut backings: Vec<PathBuf> = Vec::new();

    let read = fs::read_dir(sessions_dir).map_err(|source| BaseError::Inaccessible {
        path: sessions_dir.to_path_buf(),
        source,
    })?;

    for entry in read {
        let entry = entry.map_err(|source| BaseError::Inaccessible {
            path: sessions_dir.to_path_buf(),
            source,
        })?;

        let overlay = entry.path().join("overlay.qcow2");
        if !overlay.exists() {
            continue;
        }

        match qemu_img_backing_file(&overlay) {
            Ok(Some(backing)) => backings.push(backing),
            Ok(None) => {
                tracing::debug!(overlay = %overlay.display(), "overlay has no backing file pointer");
            },
            Err(err) => {
                tracing::warn!(overlay = %overlay.display(), %err, "could not read overlay info; skipping");
            },
        }
    }

    Ok(backings)
}

/// Run `qemu-img info --output=json` against `overlay` and return the
/// `backing-filename` field if present.
fn qemu_img_backing_file(overlay: &Path) -> Result<Option<PathBuf>> {
    let output = Command::new("qemu-img")
        .arg("info")
        .arg("--output=json")
        .arg(overlay)
        .output()
        .map_err(|source| BaseError::Tool {
            source,
            status: "spawn-failed".to_owned(),
            tool: "qemu-img",
        })?;

    if !output.status.success() {
        return Err(BaseError::Tool {
            source: std::io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
            status: output.status.to_string(),
            tool: "qemu-img",
        }
        .into());
    }

    tracing::debug!(
        stdout = %String::from_utf8_lossy(&output.stdout),
        "qemu-img info stdout",
    );
    parse_qemu_img_backing(&output.stdout, overlay)
}

/// Subset of the `qemu-img info --output=json` shape we care about.
#[derive(Debug, Deserialize)]
struct QemuImgInfo {
    #[serde(rename = "backing-filename")]
    backing_filename: Option<String>,
}

/// Parse the JSON document `qemu-img info --output=json` emits.
///
/// Exposed so unit tests can hand it golden JSON without invoking the
/// real `qemu-img`.
pub(crate) fn parse_qemu_img_backing(json: &[u8], overlay: &Path) -> Result<Option<PathBuf>> {
    let info: QemuImgInfo = serde_json::from_slice(json).map_err(|err| BaseError::OverlayInfo {
        detail: err.to_string(),
        overlay: overlay.to_path_buf(),
    })?;

    Ok(info.backing_filename.map(PathBuf::from))
}

/// Compute today's date as `YYYY-MM-DD` from the system clock.
///
/// Thin wrapper around [`tartarus_provider::time::today_iso`] kept here because
/// the only caller is the base-pull file-name composer.
fn today_iso() -> String {
    tartarus_provider::time::today_iso()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn fedora_image_url_uses_documented_pattern() {
        let url = fedora_image_url("41");

        assert!(
            url.starts_with("https://download.fedoraproject.org/"),
            "image URL must use the TLS-strict Fedora download host, got: {url}",
        );
        assert!(
            url.contains("/releases/41/Cloud/x86_64/images/"),
            "image URL must follow the Fedora releases path pattern, got: {url}",
        );
        assert!(
            url.ends_with(DEFAULT_FEDORA_IMAGE_NAME),
            "image URL must end with the cloud-base file name, got: {url}",
        );
    }

    #[test]
    fn fedora_checksum_url_uses_same_directory_as_image() {
        let image = fedora_image_url("41");
        let checksum = fedora_checksum_url("41");

        let image_dir = image
            .rsplit_once('/')
            .map(|(d, _)| d)
            .expect("image url must have a directory part");
        assert!(
            checksum.starts_with(image_dir),
            "checksum URL should live in the same directory as the image, got image={image} checksum={checksum}",
        );
        assert!(
            checksum.ends_with(FEDORA_CHECKSUM_FILE),
            "checksum URL must point at the published CHECKSUM file, got: {checksum}",
        );
    }

    #[test]
    fn parse_base_name_accepts_documented_pattern() {
        let parsed = parse_base_name("fedora-41-2026-05-01.qcow2").expect("documented pattern should parse");

        assert_eq!(parsed.release, "41", "release should round-trip");
        assert_eq!(parsed.date, "2026-05-01", "date should round-trip");
    }

    #[test]
    fn parse_base_name_rejects_extension_mismatch() {
        assert!(
            parse_base_name("fedora-41-2026-05-01.raw").is_none(),
            ".raw should not parse as a base file name",
        );
    }

    #[test]
    fn parse_base_name_rejects_non_iso_date() {
        assert!(
            parse_base_name("fedora-41-may-2026.qcow2").is_none(),
            "non-ISO date should not parse",
        );
    }

    #[test]
    fn parse_base_name_rejects_non_numeric_release() {
        assert!(
            parse_base_name("fedora-N-2026-05-01.qcow2").is_none(),
            "non-numeric release should not parse",
        );
    }

    #[test]
    fn base_from_name_anchors_path() {
        let dir = tempdir();

        let base = Base::from_name(&dir, "fedora-41-2026-05-01.qcow2").expect("documented name should parse");

        assert_eq!(base.name, "fedora-41-2026-05-01.qcow2", "name should round-trip");
        assert_eq!(
            base.path,
            dir.join("fedora-41-2026-05-01.qcow2"),
            "path should be anchored at base_dir"
        );
    }

    #[test]
    fn list_in_skips_unrelated_files() {
        let dir = tempdir();

        std::fs::write(dir.join("fedora-41-2026-05-01.qcow2"), b"a").expect("write base file");
        std::fs::write(dir.join("README"), b"unrelated").expect("write unrelated file");
        std::fs::write(dir.join("fedora-41-bad.qcow2"), b"a").expect("write malformed file");

        let library = list_in(&dir).expect("list_in should succeed in tempdir");

        assert_eq!(library.bases.len(), 1, "only the well-formed base should be listed");
        assert_eq!(
            library.bases[0].name, "fedora-41-2026-05-01.qcow2",
            "the listed base should be the well-formed one",
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_in_reports_current_target() {
        let dir = tempdir();

        std::fs::write(dir.join("fedora-41-2026-05-01.qcow2"), b"a").expect("write base file");
        std::os::unix::fs::symlink("fedora-41-2026-05-01.qcow2", dir.join("current")).expect("symlink current");

        let library = list_in(&dir).expect("list_in should succeed in tempdir");

        assert_eq!(
            library.current.as_deref(),
            Some("fedora-41-2026-05-01.qcow2"),
            "current target should round-trip",
        );
    }

    #[cfg(unix)]
    #[test]
    fn update_current_symlink_replaces_existing_target() {
        let dir = tempdir();

        std::fs::write(dir.join("fedora-41-2026-05-01.qcow2"), b"a").expect("write old base");
        std::fs::write(dir.join("fedora-41-2026-06-01.qcow2"), b"a").expect("write new base");

        update_current_symlink(&dir, "fedora-41-2026-05-01.qcow2").expect("first symlink update");

        let after_first = std::fs::read_link(dir.join("current")).expect("current should exist");
        assert_eq!(
            after_first.file_name().and_then(|n| n.to_str()),
            Some("fedora-41-2026-05-01.qcow2"),
            "first update should point at the older base",
        );

        update_current_symlink(&dir, "fedora-41-2026-06-01.qcow2").expect("second symlink update");

        let after_second = std::fs::read_link(dir.join("current")).expect("current should exist after second update");
        assert_eq!(
            after_second.file_name().and_then(|n| n.to_str()),
            Some("fedora-41-2026-06-01.qcow2"),
            "second update should point at the newer base",
        );

        assert!(
            dir.join("fedora-41-2026-05-01.qcow2").exists(),
            "old base file should still exist after the symlink swap",
        );
    }

    #[test]
    fn render_list_marks_current_target() {
        let library = BaseLibrary {
            bases: vec![
                Base {
                    name: "fedora-41-2026-04-01.qcow2".to_owned(),
                    date: "2026-04-01".to_owned(),
                    path: PathBuf::from("/tmp/fedora-41-2026-04-01.qcow2"),
                    release: "41".to_owned(),
                },
                Base {
                    name: "fedora-41-2026-05-01.qcow2".to_owned(),
                    date: "2026-05-01".to_owned(),
                    path: PathBuf::from("/tmp/fedora-41-2026-05-01.qcow2"),
                    release: "41".to_owned(),
                },
            ],
            current: Some("fedora-41-2026-05-01.qcow2".to_owned()),
        };

        let rendered = render_list(&library);

        assert!(
            rendered.contains("fedora-41-2026-04-01.qcow2"),
            "older base should appear in the rendered list, got: {rendered}",
        );
        assert!(
            rendered.contains("fedora-41-2026-05-01.qcow2"),
            "newer base should appear in the rendered list, got: {rendered}",
        );
        let current_line = rendered
            .lines()
            .find(|l| l.contains("fedora-41-2026-05-01.qcow2"))
            .expect("current base should appear on its own line");
        assert!(
            current_line.contains("yes"),
            "current marker `yes` should appear on the current base's line, got: {current_line}",
        );
    }

    #[test]
    fn render_list_handles_empty_library() {
        let rendered = render_list(&BaseLibrary::default());

        assert!(
            rendered.contains("no bases pulled yet"),
            "empty library should print the help-bearing line, got: {rendered}",
        );
    }

    #[test]
    fn parse_qemu_img_backing_extracts_filename() {
        let json = br#"{"virtual-size": 107374182400, "filename": "overlay.qcow2", "backing-filename": "/data/base/fedora-41-2026-05-01.qcow2", "format": "qcow2"}"#;
        let overlay = PathBuf::from("overlay.qcow2");

        let backing = parse_qemu_img_backing(json, &overlay)
            .expect("documented qemu-img info shape should parse")
            .expect("backing should be present");

        assert_eq!(
            backing,
            PathBuf::from("/data/base/fedora-41-2026-05-01.qcow2"),
            "backing-filename should round-trip into a PathBuf",
        );
    }

    #[test]
    fn parse_qemu_img_backing_handles_missing_backing_field() {
        let json = br#"{"virtual-size": 107374182400, "filename": "raw.qcow2", "format": "qcow2"}"#;
        let overlay = PathBuf::from("raw.qcow2");

        let backing = parse_qemu_img_backing(json, &overlay).expect("missing backing field should not error");

        assert!(backing.is_none(), "absent backing-filename should map to None");
    }

    #[test]
    fn parse_qemu_img_backing_returns_typed_error_for_garbage() {
        let json = b"not json";
        let overlay = PathBuf::from("garbage.qcow2");

        let err = parse_qemu_img_backing(json, &overlay).expect_err("garbage JSON should fail");

        match err {
            Error::Base(BaseError::OverlayInfo { overlay: reported, .. }) => {
                assert_eq!(reported, overlay, "reported overlay path should round-trip");
            },
            other => panic!("expected BaseError::OverlayInfo, got {other:?}"),
        }
    }

    #[test]
    fn plan_prune_keeps_referenced_and_current_bases() {
        let library = BaseLibrary {
            bases: vec![
                make_base("fedora-41-2026-04-01.qcow2"),
                make_base("fedora-41-2026-05-01.qcow2"),
                make_base("fedora-41-2026-06-01.qcow2"),
            ],
            current: Some("fedora-41-2026-06-01.qcow2".to_owned()),
        };

        let referencing = vec![PathBuf::from("/data/base/fedora-41-2026-05-01.qcow2")];

        let plan = plan_prune_with(&library, &referencing);

        let kept_names: Vec<_> = plan.kept.iter().map(|k| k.base.name.as_str()).collect();
        let deleted_names: Vec<_> = plan.deleted.iter().map(|b| b.name.as_str()).collect();

        assert!(
            kept_names.contains(&"fedora-41-2026-05-01.qcow2"),
            "an overlay-referenced base should be kept, got kept={kept_names:?}",
        );
        assert!(
            kept_names.contains(&"fedora-41-2026-06-01.qcow2"),
            "the current base should always be kept, got kept={kept_names:?}",
        );
        assert_eq!(
            deleted_names,
            vec!["fedora-41-2026-04-01.qcow2"],
            "the unreferenced non-current base should be the only deletion, got: {deleted_names:?}",
        );
    }

    #[test]
    fn plan_prune_treats_current_as_kept_even_without_overlays() {
        let library = BaseLibrary {
            bases: vec![make_base("fedora-41-2026-05-01.qcow2")],
            current: Some("fedora-41-2026-05-01.qcow2".to_owned()),
        };

        let plan = plan_prune_with(&library, &[]);

        assert!(
            plan.deleted.is_empty(),
            "current must never be pruned, got: {:?}",
            plan.deleted
        );
        let kept = plan.kept.first().expect("current should appear in kept");
        assert!(kept.is_current, "current marker should be set on the kept entry");
        assert_eq!(kept.overlay_refcount, 0, "no overlays reference this base");
    }

    #[test]
    fn render_prune_dry_run_does_not_promise_deletion() {
        let library = BaseLibrary {
            bases: vec![
                make_base("fedora-41-2026-04-01.qcow2"),
                make_base("fedora-41-2026-05-01.qcow2"),
            ],
            current: Some("fedora-41-2026-05-01.qcow2".to_owned()),
        };
        let plan = plan_prune_with(&library, &[]);

        let rendered = render_prune(&plan, true, 0);

        assert!(
            rendered.contains("would remove"),
            "dry-run output should say `would remove`, got: {rendered}",
        );
        assert!(
            rendered.contains("dry-run: no files deleted"),
            "dry-run output should announce that nothing was deleted, got: {rendered}",
        );
    }

    #[test]
    fn render_prune_live_announces_freed_bytes() {
        let library = BaseLibrary {
            bases: vec![
                make_base("fedora-41-2026-04-01.qcow2"),
                make_base("fedora-41-2026-05-01.qcow2"),
            ],
            current: Some("fedora-41-2026-05-01.qcow2".to_owned()),
        };
        let plan = plan_prune_with(&library, &[]);

        let rendered = render_prune(&plan, false, 1_024);

        assert!(
            rendered.contains("removed"),
            "live render should say `removed`, got: {rendered}",
        );
        assert!(
            rendered.contains("freed 1024 bytes"),
            "live render should announce freed bytes, got: {rendered}",
        );
    }

    #[test]
    fn apply_prune_dry_run_does_not_delete() {
        let dir = tempdir();
        let path = dir.join("fedora-41-2026-04-01.qcow2");
        std::fs::write(&path, b"x").expect("write base file");

        let plan = PrunePlan {
            deleted: vec![Base {
                name: "fedora-41-2026-04-01.qcow2".to_owned(),
                date: "2026-04-01".to_owned(),
                path: path.clone(),
                release: "41".to_owned(),
            }],
            kept: vec![],
        };

        let rendered = render_prune(&plan, true, 0);
        assert!(rendered.contains("would remove"), "dry-run should say would remove");

        assert!(path.exists(), "dry-run must not delete the file");
    }

    #[test]
    fn apply_prune_deletes_planned_files() {
        let dir = tempdir();
        let path = dir.join("fedora-41-2026-04-01.qcow2");
        let mut file = std::fs::File::create(&path).expect("create base file");
        file.write_all(b"hello").expect("write bytes");
        drop(file);

        let plan = PrunePlan {
            deleted: vec![Base {
                name: "fedora-41-2026-04-01.qcow2".to_owned(),
                date: "2026-04-01".to_owned(),
                path: path.clone(),
                release: "41".to_owned(),
            }],
            kept: vec![],
        };

        let freed = apply_prune(&plan).expect("apply_prune should succeed");

        assert!(!path.exists(), "the planned file should be deleted");
        assert_eq!(freed, 5, "freed bytes should match the file size, got: {freed}");
    }

    #[test]
    fn workdir_guard_removes_path_on_drop_when_armed() {
        let dir = tempdir();
        let workdir = dir.join("pull-workdir");
        std::fs::create_dir_all(&workdir).expect("create workdir");
        std::fs::write(workdir.join("downloaded.qcow2"), b"partial").expect("seed partial download");

        {
            let _guard = WorkdirGuard::adopt(workdir.clone());
        }

        assert!(
            !workdir.exists(),
            "armed WorkdirGuard must clean up the path on drop, got: {}",
            workdir.display(),
        );
    }

    #[test]
    fn workdir_guard_leaves_path_alone_after_disarm() {
        let dir = tempdir();
        let workdir = dir.join("pull-workdir-disarmed");
        std::fs::create_dir_all(&workdir).expect("create workdir");

        {
            let mut guard = WorkdirGuard::adopt(workdir.clone());
            guard.disarm();
        }

        assert!(
            workdir.exists(),
            "disarmed WorkdirGuard must not delete the path; the caller owns cleanup, got: {}",
            workdir.display(),
        );

        std::fs::remove_dir_all(&workdir).expect("test cleanup");
    }

    #[test]
    fn parse_manifest_sha256_handles_bsd_style() {
        let manifest = "\
            # Header line\n\
            SHA256 (Fedora-Cloud-Base-Generic.x86_64-41-1.4.x86_64.qcow2) = abc123def\n\
            SHA256 (Some-Other-File) = ffffffff\n";
        let digest = parse_manifest_sha256(manifest, "Fedora-Cloud-Base-Generic.x86_64-41-1.4.x86_64.qcow2");
        assert_eq!(digest.as_deref(), Some("abc123def"));
    }

    #[test]
    fn parse_manifest_sha256_handles_coreutils_style() {
        let manifest = "abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567  some-image.qcow2\n";
        let digest = parse_manifest_sha256(manifest, "some-image.qcow2");
        assert_eq!(
            digest.as_deref(),
            Some("abc123def0123456789abcdef0123456789abcdef0123456789abcdef01234567"),
        );
    }

    #[test]
    fn parse_manifest_sha256_returns_none_for_unrelated_filenames() {
        let manifest = "SHA256 (other-file.qcow2) = abc123def\n";
        assert!(parse_manifest_sha256(manifest, "wanted-file.qcow2").is_none());
    }

    #[test]
    fn parse_manifest_sha256_rejects_non_hex_coreutils_lines() {
        let manifest = "GHIJKLMNOP01234567890123456789012  some-image.qcow2\n";
        assert!(parse_manifest_sha256(manifest, "some-image.qcow2").is_none());
    }

    #[test]
    fn verify_image_against_manifest_accepts_matching_digest() {
        let dir = tempdir();
        let image_path = dir.join("image.qcow2");
        fs::write(&image_path, b"hello world").expect("write test image");

        let actual = sha256sum(&image_path).expect("sha256sum should run");
        let manifest_path = dir.join("CHECKSUM");
        fs::write(&manifest_path, format!("SHA256 (image.qcow2) = {actual}\n")).expect("write manifest");

        verify_image_against_manifest(&RealDeps, &image_path, &manifest_path, "image.qcow2")
            .expect("matching digest should pass verification");
    }

    #[test]
    fn verify_image_against_manifest_rejects_tampered_image() {
        let dir = tempdir();
        let image_path = dir.join("image.qcow2");
        fs::write(&image_path, b"hello world").expect("write test image");

        let manifest_path = dir.join("CHECKSUM");
        let bogus_digest = "0000000000000000000000000000000000000000000000000000000000000000";
        fs::write(&manifest_path, format!("SHA256 (image.qcow2) = {bogus_digest}\n")).expect("write manifest");

        let err = verify_image_against_manifest(&RealDeps, &image_path, &manifest_path, "image.qcow2")
            .expect_err("digest mismatch should error");
        assert!(matches!(err, crate::Error::Base(BaseError::ChecksumMismatch { .. }),));
    }

    #[test]
    fn pull_with_in_drives_steps_in_documented_order_against_mock_deps() {
        let base_dir = tempdir().join("base");

        let image_bytes = b"hello-image".to_vec();
        let expected_digest = run_real_sha256(&image_bytes);

        let mock = MockDeps {
            checksum_payload: format!("SHA256 ({DEFAULT_FEDORA_IMAGE_NAME}) = {expected_digest}\n").into_bytes(),
            events: std::cell::RefCell::new(Vec::new()),
            fingerprints: vec!["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()],
            image_payload: image_bytes,
            keyring_payload: b"keyring-bytes".to_vec(),
        };

        let pulled = pull_with_in(DEFAULT_FEDORA_RELEASE, &mock, &base_dir)
            .expect("pull_with_in should succeed against the mock deps");

        let events = mock.events.borrow().clone();
        let mut idx = 0;
        let mut expect_event = |needle: &str| {
            let pos = events
                .iter()
                .skip(idx)
                .position(|e| e.contains(needle))
                .unwrap_or_else(|| panic!("expected event containing {needle:?} after position {idx}, got {events:?}"));
            idx += pos + 1;
        };
        expect_event("download:image");
        expect_event("download:checksum");
        expect_event("download:key");
        expect_event("gpg_fingerprints");
        expect_event("gpgv");
        expect_event("genisoimage");
        expect_event("apply_layer");

        assert_eq!(pulled.release, DEFAULT_FEDORA_RELEASE, "release should round-trip");
        assert!(pulled.path.exists(), "final base file should exist on disk");
        assert!(
            pulled.path.starts_with(&base_dir),
            "base file must land under the supplied base_dir"
        );
    }

    #[test]
    fn pull_with_in_propagates_gpgv_failure_and_skips_apply_layer() {
        let base_dir = tempdir().join("base");

        let mock = FailingGpgvDeps {
            inner: MockDeps {
                checksum_payload: format!("SHA256 ({DEFAULT_FEDORA_IMAGE_NAME}) = abc\n").into_bytes(),
                events: std::cell::RefCell::new(Vec::new()),
                fingerprints: vec!["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned()],
                image_payload: b"x".to_vec(),
                keyring_payload: b"k".to_vec(),
            },
        };

        let err = pull_with_in(DEFAULT_FEDORA_RELEASE, &mock, &base_dir).expect_err("gpgv failure should propagate");

        match err {
            Error::Base(BaseError::GpgVerification { .. }) => {},
            other => panic!("expected BaseError::GpgVerification, got {other:?}"),
        }
        let events = mock.inner.events.borrow().clone();
        assert!(
            !events.iter().any(|e| e.contains("apply_layer")),
            "apply_layer must not run after gpgv fails, got events: {events:?}",
        );
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn run_real_sha256(bytes: &[u8]) -> String {
        let dir = tempdir();
        let path = dir.join("payload.bin");
        fs::write(&path, bytes).expect("write payload");
        sha256sum(&path).expect("real sha256 must run for the mock fixture")
    }

    /// Mock [`Deps`] used by the orchestration tests.
    ///
    /// Records each call into `events` so the tests can assert ordering;
    /// `download` writes the supplied payload into the requested `dest`
    /// so subsequent verification steps see the bytes the production
    /// flow would have streamed off the wire.
    struct MockDeps {
        checksum_payload: Vec<u8>,
        events: std::cell::RefCell<Vec<String>>,
        fingerprints: Vec<String>,
        image_payload: Vec<u8>,
        keyring_payload: Vec<u8>,
    }

    impl Deps for MockDeps {
        fn apply_layer(&self, image_path: &Path, _seed_iso_path: &Path) -> Result<()> {
            self.events
                .borrow_mut()
                .push(format!("apply_layer:{}", image_path.display()));
            Ok(())
        }

        fn download(&self, url: &str, dest: &Path, _timeout_secs: u64, _max_bytes: Option<u64>) -> Result<()> {
            let label = if url.contains("CHECKSUM") {
                "download:checksum"
            } else if url.ends_with(".gpg") {
                "download:key"
            } else {
                "download:image"
            };
            self.events.borrow_mut().push(label.to_owned());
            let payload = if url.contains("CHECKSUM") {
                &self.checksum_payload
            } else if url.ends_with(".gpg") {
                &self.keyring_payload
            } else {
                &self.image_payload
            };
            fs::write(dest, payload).map_err(|source| BaseError::Inaccessible {
                path: dest.to_path_buf(),
                source,
            })?;
            Ok(())
        }

        fn genisoimage(&self, _workdir: &Path, iso_path: &Path) -> Result<()> {
            self.events.borrow_mut().push("genisoimage".to_owned());
            fs::write(iso_path, b"iso").map_err(|source| BaseError::Inaccessible {
                path: iso_path.to_path_buf(),
                source,
            })?;
            Ok(())
        }

        fn gpg_fingerprints(&self, _workdir: &Path, _keyring: &Path) -> Result<Vec<String>> {
            self.events.borrow_mut().push("gpg_fingerprints".to_owned());
            Ok(self.fingerprints.clone())
        }

        fn gpgv(&self, _keyring: &Path, _signed: &Path) -> Result<()> {
            self.events.borrow_mut().push("gpgv".to_owned());
            Ok(())
        }

        fn sha256(&self, path: &Path) -> Result<String> {
            self.events.borrow_mut().push("sha256".to_owned());
            let bytes = fs::read(path).map_err(|source| BaseError::Inaccessible {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(run_real_sha256(&bytes))
        }
    }

    /// Wrapper that delegates every method except `gpgv` (which fails)
    /// to its inner [`MockDeps`].
    struct FailingGpgvDeps {
        inner: MockDeps,
    }

    impl Deps for FailingGpgvDeps {
        fn apply_layer(&self, image_path: &Path, seed_iso_path: &Path) -> Result<()> {
            self.inner.apply_layer(image_path, seed_iso_path)
        }

        fn download(&self, url: &str, dest: &Path, timeout_secs: u64, max_bytes: Option<u64>) -> Result<()> {
            self.inner.download(url, dest, timeout_secs, max_bytes)
        }

        fn genisoimage(&self, workdir: &Path, iso_path: &Path) -> Result<()> {
            self.inner.genisoimage(workdir, iso_path)
        }

        fn gpg_fingerprints(&self, workdir: &Path, keyring: &Path) -> Result<Vec<String>> {
            self.inner.gpg_fingerprints(workdir, keyring)
        }

        fn gpgv(&self, signed: &Path, _keyring: &Path) -> Result<()> {
            Err(BaseError::GpgVerification {
                artifact: signed.to_path_buf(),
                detail: "simulated gpgv failure".to_owned(),
            }
            .into())
        }

        fn sha256(&self, path: &Path) -> Result<String> {
            self.inner.sha256(path)
        }
    }

    fn make_base(name: &str) -> Base {
        let parsed = parse_base_name(name).expect("test name should parse");
        Base {
            name: name.to_owned(),
            date: parsed.date,
            path: PathBuf::from(format!("/data/base/{name}")),
            release: parsed.release,
        }
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-base-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir");

        path
    }
}
