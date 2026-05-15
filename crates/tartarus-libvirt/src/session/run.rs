//! `tartarus run`: create + start a session, then route per [`RunMode`].
//!
//! Generates a UUID, builds the session directory, creates the overlay
//! and seed ISO, defines + starts the libvirt domain, then routes per
//! [`RunMode`]: foreground attaches the console, detached returns
//! immediately, background captures the remote-connect URL.

#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::{
    fs,
    path::{Path, PathBuf},
};

use tartarus_provider::{
    RunOutcome, RunRequest, host_user, paths,
    seed::input::{Seed, SeedInputs},
    session::{
        SessionError, identity,
        metadata::{self, Metadata},
        run_mode::RunMode,
    },
};

use crate::{
    config::Config,
    disk::overlay::Overlay,
    error::{Error, Result},
    host::{
        connect::Connection,
        domain::{self, SessionDomainSpec},
    },
    seed::iso,
    session::{ssh, ssh_attach},
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// File name of the persisted libvirt domain XML inside the session dir.
pub const DOMAIN_XML_FILE_NAME: &str = "domain.xml";

/// Mode for per-session directories: `0700` because they hold
/// credentials.
#[cfg(unix)]
const SESSION_DIR_MODE: u32 = 0o700;

/// Run the session-start flow.
///
/// Transactional: failures before the domain is running roll back
/// via RAII guards. After `start` succeeds the guards are disarmed
/// and post-start errors (console attach, URL probe) are logged
/// rather than propagated.
pub fn run(config: &Config, request: &RunRequest) -> Result<RunOutcome> {
    let mode = request.run_mode();

    let gpu_bundle = prepare_gpu_borrow(request)?;
    let mut gpu_guard = gpu_bundle.as_ref().map(|b| GpuBorrowGuard::adopt(b.receipt.clone()));

    let user = host_user::current()?;
    let base = current_base_filename()?;
    let base_path = paths::base_dir()?.join(&base);

    let uuid = identity::new_uuid();
    let session_dir = paths::sessions_by_uuid_dir()?.join(&uuid);
    create_session_dir(&session_dir)?;
    let mut session_guard = SessionDirGuard::adopt(session_dir.clone());

    let ssh_layout = crate::session::ssh::SessionSshLayout::for_session(&session_dir);
    crate::session::ssh::ensure_keypair(&ssh_layout)?;
    let ssh_pubkey = crate::session::ssh::read_public_key(&ssh_layout)?;
    let ssh_port = crate::session::ssh_port::allocate_loopback_port()?;

    let seed = build_seed(
        config,
        request.name.as_deref(),
        SeedInputs {
            default_repo: request.default_repo.clone(),
            remote_connect: mode.enables_remote_connect(),
            repos: request.repos.clone(),
            ssh_pubkey: Some(ssh_pubkey),
            user: user.clone(),
            uuid: uuid.clone(),
        },
    )?;

    let overlay = Overlay::create(&session_dir, &base_path, config.disk_virtual_size_gib)?;
    let seed_iso = iso::write_iso(&session_dir, &seed)?;
    let (_domain_xml_path, xml) = write_domain_xml(
        &session_dir,
        DomainXmlInputs {
            gpu: gpu_bundle.as_ref().map(|b| (&b.address, &b.quirks)),
            memory_mib: config.vm_memory_mib,
            overlay: &overlay.path,
            seed_iso: &seed_iso,
            ssh_hostfwd_port: ssh_port,
            uuid: &uuid,
            vcpus: config.vm_vcpus,
        },
    )?;

    let metadata_path = session_dir.join(metadata::METADATA_FILE_NAME);
    persist_metadata(
        &metadata_path,
        request,
        PersistInputs {
            base: &base,
            envs: &config.base_envs,
            gpu_borrow: gpu_bundle.as_ref().map(|b| &b.receipt),
            memory_mib: config.vm_memory_mib,
            overlay_virtual_gib: overlay.virtual_size_gib,
            seed: &seed,
            ssh_port: Some(ssh_port),
            uuid: &uuid,
            vcpus: config.vm_vcpus,
        },
    )?;

    let mut alias_guard = match request.name.as_deref() {
        Some(alias) => {
            identity::set_alias(alias, &uuid)?;
            Some(AliasGuard::adopt(alias.to_owned()))
        },
        None => None,
    };

    let libvirt_uri = resolve_libvirt_uri(config, request);
    let connection = Connection::open(libvirt_uri)?;
    define_session_xml(&connection, &uuid, &xml)?;
    let mut domain_guard = domain::DomainGuard::adopt(&connection, uuid.clone());
    domain::start(&connection, &uuid)?;

    domain_guard.disarm();
    if let Some(guard) = alias_guard.as_mut() {
        guard.disarm();
    }
    session_guard.disarm();
    if let Some(guard) = gpu_guard.as_mut() {
        guard.disarm();
    }

    tracing::info!(uuid = %uuid, alias = ?request.name, ?mode, "session started");

    let remote_url = route_post_start(&PostStartInputs {
        config,
        metadata_path: &metadata_path,
        mode,
        session_dir: &session_dir,
        ssh_port,
        uuid: &uuid,
    });

    Ok(RunOutcome {
        alias: request.name.clone(),
        mode,
        remote_url,
        uuid,
    })
}

/// Pluggable URL probe for background-mode tests.
#[cfg(test)]
type RemoteUrlProbe = dyn Fn(&Config, &str) -> Result<String>;

/// Background-mode post-start routing exposed for unit tests.
#[cfg(test)]
fn capture_remote_url_for_test(
    config: &Config,
    uuid: &str,
    metadata_path: &Path,
    probe: &RemoteUrlProbe,
) -> Result<Option<String>> {
    let url = probe(config, uuid)?;
    println!("[ background ] remote-connect URL: {url}");
    persist_remote_url(metadata_path, &url)?;
    Ok(Some(url))
}

// -----------------------------------------------------------------------------
// Session Setup
// -----------------------------------------------------------------------------

/// Run the GPU host pre-check when `--gpu` is set. No-op when `None`.
fn enforce_gpu_pre_check(request: &RunRequest) -> Result<Option<crate::gpu::PciAddress>> {
    let Some(spec) = request.gpu.as_deref() else {
        return Ok(None);
    };

    let target = if spec == "auto" {
        match crate::gpu::pci::pick_auto_gpu()? {
            Some(device) => {
                tracing::info!(target = %device.address, label = %device.label(), "--gpu auto picked device");
                device.address
            },
            None => {
                return Err(crate::Error::Gpu(crate::gpu::GpuError::NoCleanGpuFound));
            },
        }
    } else {
        spec.parse()?
    };

    let outcome = crate::gpu::HostPreCheck::probe(Some(&target))?;
    outcome.into_result()?;

    Ok(Some(target))
}

/// Pre-check, conflict scan, and driver detach for GPU passthrough.
///
/// Returns `Ok(None)` when `--gpu` was not supplied.
fn prepare_gpu_borrow(request: &RunRequest) -> Result<Option<GpuBundle>> {
    let Some(target) = enforce_gpu_pre_check(request)? else {
        return Ok(None);
    };

    let device = crate::gpu::PciDevice::at(target.clone())?;
    let quirks = crate::gpu::quirks::evaluate(&device)?;

    if let Some(holder) = crate::session::gpu_index::find_borrowing_session(&target)? {
        return Err(tartarus_provider::session::SessionError::AliasInUse {
            alias: format!("gpu {target}"),
            existing_uuid: holder.uuid,
        }
        .into());
    }

    if request.privileged_libvirt {
        tracing::warn!(target = %target, "GPU passthrough with --privileged-libvirt: weaker isolation posture");
    }
    if quirks.apply_nvidia_hide_kvm {
        tracing::info!(target = %target, "NVIDIA detected; emitting Code 43 workaround");
    }

    let io = crate::gpu::driver::KernelSysfs;
    let receipt = crate::gpu::driver::borrow(&io, &target)?;
    tracing::info!(target = %target, previous_driver = ?receipt.previous_driver, "GPU borrowed");

    Ok(Some(GpuBundle {
        address: target,
        quirks,
        receipt,
    }))
}

/// Bundle returned by [`prepare_gpu_borrow`].
struct GpuBundle {
    address: crate::gpu::PciAddress,
    quirks: crate::gpu::quirks::VendorQuirks,
    receipt: crate::gpu::driver::Receipt,
}

/// Choose between `qemu:///session` and `qemu:///system` based on
/// the `--privileged-libvirt` flag.
fn resolve_libvirt_uri<'a>(config: &'a Config, request: &RunRequest) -> &'a str {
    if request.privileged_libvirt {
        "qemu:///system"
    } else {
        &config.network_uri
    }
}

/// RAII guard that releases a GPU borrow on drop unless disarmed.
struct GpuBorrowGuard {
    armed: bool,
    receipt: Option<crate::gpu::driver::Receipt>,
}

impl GpuBorrowGuard {
    fn adopt(receipt: crate::gpu::driver::Receipt) -> Self {
        Self {
            armed: true,
            receipt: Some(receipt),
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for GpuBorrowGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(receipt) = self.receipt.take() {
            let io = crate::gpu::driver::KernelSysfs;
            if let Err(err) = crate::gpu::driver::release_with_receipt(&io, &receipt) {
                tracing::warn!(
                    address = %receipt.address,
                    %err,
                    "GPU borrow rollback failed; run `tartarus host gpu release {addr}` to retry",
                    addr = receipt.address,
                );
            } else {
                tracing::debug!(address = %receipt.address, "rolled back GPU borrow");
            }
        }
    }
}

/// Create the per-session directory at mode `0700`.
fn create_session_dir(session_dir: &Path) -> Result<()> {
    if let Some(parent) = session_dir.parent() {
        fs::create_dir_all(parent)?;
    }

    create_dir_owner_only(session_dir)
}

/// Create `path` (and only `path`) at mode `0700`.
#[cfg(unix)]
fn create_dir_owner_only(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.mode(SESSION_DIR_MODE);

    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(SESSION_DIR_MODE))?;
            Ok(())
        },
        Err(err) => Err(err.into()),
    }
}

/// Non-Unix shim.
#[cfg(not(unix))]
fn create_dir_owner_only(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// Parameters for [`route_post_start`].
struct PostStartInputs<'a> {
    config: &'a Config,
    metadata_path: &'a Path,
    mode: RunMode,
    session_dir: &'a Path,
    ssh_port: u16,
    uuid: &'a str,
}

/// Route control after the domain is running.
///
/// Errors are logged rather than propagated; the session is alive.
fn route_post_start(inputs: &PostStartInputs<'_>) -> Option<String> {
    let PostStartInputs {
        config,
        metadata_path,
        mode,
        session_dir,
        ssh_port,
        uuid,
    } = inputs;
    match mode {
        RunMode::Foreground => {
            let layout = ssh::SessionSshLayout::for_session(session_dir);
            match ssh_attach::capture_host_key(config, uuid, &layout, *ssh_port) {
                Ok(()) => match ssh_attach::exec_ssh(config, &layout, *ssh_port, uuid, &[]) {
                    Ok(_outcome) => None,
                    Err(err) => {
                        tracing::warn!(
                            %uuid,
                            %err,
                            "session is running but SSH attach failed; reattach with `tartarus connect`",
                        );
                        None
                    },
                },
                Err(err) => {
                    tracing::warn!(
                        %uuid,
                        %err,
                        "session is running but SSH host key capture failed; reattach with `tartarus connect`",
                    );
                    None
                },
            }
        },
        RunMode::Detached => None,
        RunMode::Background => match probe_remote_url(config, uuid) {
            Ok(url) => {
                println!("[ background ] remote-connect URL: {url}");
                if let Err(err) = persist_remote_url(metadata_path, &url) {
                    tracing::warn!(
                        %uuid,
                        %err,
                        "captured remote-connect URL but failed to persist into metadata.json",
                    );
                }
                Some(url)
            },
            Err(err) => {
                tracing::warn!(
                    %uuid,
                    %err,
                    "session is running but remote-connect URL probe failed; the session is reachable by uuid",
                );
                None
            },
        },
    }
}

/// Probe the running guest for Claude's remote-connect URL.
///
/// Stub: returns `NotImplemented` until the agent surface lands.
fn probe_remote_url(_config: &Config, _uuid: &str) -> Result<String> {
    Err(Error::NotImplemented("background mode"))
}

/// Persist `remote_url` into the session's `metadata.json`.
fn persist_remote_url(metadata_path: &Path, url: &str) -> Result<()> {
    let mut meta = Metadata::load(metadata_path)?;
    meta.remote_url = Some(url.to_owned());
    Ok(meta.save(metadata_path)?)
}

/// Build the per-session [`Seed`] from the resolved config.
///
/// Validates CLI-supplied repo slugs and checks for missing repos
/// or credentials before building the seed.
fn build_seed(config: &Config, alias: Option<&str>, inputs: SeedInputs) -> Result<Seed> {
    if inputs.repos.is_empty() && config.base_repos.is_empty() {
        return Err(Error::from(SessionError::NoRepos));
    }

    for slug in &inputs.repos {
        if !tartarus_provider::seed::input::is_valid_repo_slug(slug) {
            return Err(Error::from(SessionError::InvalidRepoSlug { slug: slug.clone() }));
        }
    }

    if let Some(slug) = inputs.default_repo.as_deref()
        && !tartarus_provider::seed::input::is_valid_repo_slug(slug)
    {
        return Err(Error::from(SessionError::InvalidRepoSlug { slug: slug.to_owned() }));
    }

    crate::seed::builder::build_seed(config, alias, inputs)?
        .ok_or_else(|| Error::from(SessionError::MissingCredentials))
}

/// Resolve the file name `base/current` resolves to.
fn current_base_filename() -> Result<String> {
    let library = crate::disk::base::list()?;
    library.current.ok_or_else(|| Error::from(SessionError::NoBaseCurrent))
}

/// Build the session domain XML, persist it, and return the string.
fn write_domain_xml(session_dir: &Path, inputs: DomainXmlInputs<'_>) -> Result<(PathBuf, String)> {
    let mut spec = SessionDomainSpec::new(
        inputs.uuid.to_owned(),
        inputs.overlay,
        inputs.seed_iso,
        inputs.memory_mib,
        inputs.ssh_hostfwd_port,
    )
    .with_vcpus(inputs.vcpus);
    if let Some((addr, quirks)) = inputs.gpu {
        spec = spec.with_gpu(addr.clone(), *quirks);
    }
    let xml = spec.to_xml();

    let path = session_dir.join(DOMAIN_XML_FILE_NAME);
    std::fs::write(&path, xml.as_bytes())?;

    Ok((path, xml))
}

/// Inputs for [`write_domain_xml`].
struct DomainXmlInputs<'a> {
    gpu: Option<(&'a crate::gpu::PciAddress, &'a crate::gpu::quirks::VendorQuirks)>,
    memory_mib: u32,
    overlay: &'a Path,
    seed_iso: &'a Path,
    ssh_hostfwd_port: u16,
    uuid: &'a str,
    vcpus: u32,
}

/// Persist `metadata.json` for the session.
fn persist_metadata(metadata_path: &Path, request: &RunRequest, fields: PersistInputs<'_>) -> Result<()> {
    let mut metadata = metadata::fresh(metadata::FreshFields {
        alias: request.name.clone(),
        base: fields.base.to_owned(),
        envs: fields.envs.to_vec(),
        memory_mib: fields.memory_mib,
        overlay_virtual_gib: fields.overlay_virtual_gib,
        persist: !request.ephemeral,
        repos: fields.seed.repos.clone(),
        uuid: fields.uuid.to_owned(),
        vcpus: fields.vcpus,
    });
    metadata.gpu_borrow = fields.gpu_borrow.map(crate::gpu::driver::record_from_receipt);
    metadata.ssh_port = fields.ssh_port;

    Ok(metadata.save(metadata_path)?)
}

/// Inputs for [`persist_metadata`].
struct PersistInputs<'a> {
    base: &'a str,
    envs: &'a [String],
    gpu_borrow: Option<&'a crate::gpu::driver::Receipt>,
    memory_mib: u32,
    overlay_virtual_gib: u32,
    seed: &'a Seed,
    ssh_port: Option<u16>,
    uuid: &'a str,
    vcpus: u32,
}

/// RAII rollback for the per-session directory.
struct SessionDirGuard {
    /// Whether the guard is responsible for cleanup on drop.
    armed: bool,

    /// Path of the session dir to remove on drop.
    path: PathBuf,
}

impl SessionDirGuard {
    /// Adopt cleanup responsibility for `path`.
    fn adopt(path: PathBuf) -> Self {
        Self { armed: true, path }
    }

    /// Mark the guard as not-responsible for cleanup.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SessionDirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        match fs::remove_dir_all(&self.path) {
            Ok(()) => tracing::debug!(path = %self.path.display(), "rolled back session directory"),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
            Err(err) => tracing::warn!(
                path = %self.path.display(),
                %err,
                "failed to clean up session directory during rollback; manual `rm -rf` may be required",
            ),
        }
    }
}

/// RAII rollback for the alias symlink.
struct AliasGuard {
    /// Alias the guard owns the cleanup for.
    alias: String,

    /// Whether the guard is responsible for cleanup on drop.
    armed: bool,
}

impl AliasGuard {
    /// Adopt cleanup responsibility for `alias`.
    fn adopt(alias: String) -> Self {
        Self { alias, armed: true }
    }

    /// Mark the guard as not-responsible for cleanup.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for AliasGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        match identity::unlink_alias(&self.alias) {
            Ok(()) => tracing::debug!(alias = %self.alias, "rolled back alias symlink"),
            Err(err) => tracing::warn!(
                alias = %self.alias,
                %err,
                "failed to remove alias during rollback; manual `tartarus rename` may be required",
            ),
        }
    }
}

/// Define a session domain by calling libvirt with the persisted XML.
fn define_session_xml(connection: &Connection, name: &str, xml: &str) -> Result<()> {
    let _ = virt::domain::Domain::define_xml(connection.inner(), xml).map_err(|source| {
        crate::host::error::HostError::DomainOperation {
            operation: "define-session",
            source,
        }
    })?;

    tracing::debug!(name, "defined session domain via persisted XML");

    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use tartarus_provider::seed::input::RepoSpec;

    use super::*;

    #[test]
    fn enforce_gpu_pre_check_is_noop_when_gpu_is_none() {
        let request = RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        enforce_gpu_pre_check(&request).expect("no --gpu means no pre-check");
    }

    #[test]
    fn enforce_gpu_pre_check_with_auto_falls_back_to_no_clean_gpu_when_host_has_none() {
        let request = RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: Some("auto".to_owned()),
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        let result = enforce_gpu_pre_check(&request);
        match result {
            Err(crate::Error::Gpu(crate::gpu::GpuError::NoCleanGpuFound)) => {
                // Test sandboxes have no display-class devices,
                // so the walker rightly reports "nothing to pick."
            },
            Err(crate::Error::Gpu(_)) => {
                // Pre-check refused before the walker even ran
                // (no IOMMU on the test host) — also acceptable.
            },
            Ok(Some(_)) => {
                // Real CI hosts may have a clean GPU; passing is fine.
            },
            other => panic!("unexpected outcome from --gpu auto on a no-GPU host: {other:?}"),
        }
    }

    #[test]
    fn enforce_gpu_pre_check_rejects_malformed_bdf() {
        let request = RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: Some("not-a-pci-addr".to_owned()),
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        let err = enforce_gpu_pre_check(&request).expect_err("garbage BDF must fail to parse");
        assert!(
            matches!(err, crate::Error::Gpu(crate::gpu::GpuError::InvalidPciAddress { .. })),
            "expected GpuError::InvalidPciAddress, got {err:?}",
        );
    }

    #[test]
    fn run_request_round_trips_through_struct() {
        let request = RunRequest {
            background: true,
            default_repo: Some("owner/name".to_owned()),
            detach: false,
            ephemeral: true,
            gpu: None,
            name: Some("alpha".to_owned()),
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        assert_eq!(request.repos, vec!["owner/name".to_owned()], "repos should round-trip",);
        assert_eq!(
            request.default_repo.as_deref(),
            Some("owner/name"),
            "default_repo should round-trip",
        );
        assert!(request.background, "background flag should round-trip");
        assert!(request.ephemeral, "ephemeral flag should round-trip");
        assert_eq!(
            request.name.as_deref(),
            Some("alpha"),
            "alias should round-trip into the request",
        );
    }

    #[test]
    fn run_outcome_round_trips_through_struct() {
        let outcome = RunOutcome {
            alias: Some("alpha".to_owned()),
            mode: RunMode::Foreground,
            remote_url: None,
            uuid: "abc".to_owned(),
        };

        assert_eq!(outcome.uuid, "abc", "uuid should round-trip into the outcome");
        assert_eq!(
            outcome.alias.as_deref(),
            Some("alpha"),
            "alias should round-trip into the outcome",
        );
    }

    #[test]
    fn run_request_run_mode_is_foreground_by_default() {
        let request = RunRequest {
            background: false,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        assert_eq!(request.run_mode(), RunMode::Foreground);
    }

    #[test]
    fn run_request_run_mode_routes_detach_flag() {
        let request = RunRequest {
            background: false,
            default_repo: None,
            detach: true,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        assert_eq!(request.run_mode(), RunMode::Detached);
    }

    #[test]
    fn run_request_run_mode_routes_background_flag() {
        let request = RunRequest {
            background: true,
            default_repo: None,
            detach: false,
            ephemeral: false,
            gpu: None,
            name: None,
            privileged_libvirt: false,
            repos: vec!["owner/name".to_owned()],
        };

        assert_eq!(request.run_mode(), RunMode::Background);
    }

    #[test]
    fn capture_remote_url_persists_to_metadata() {
        let dir = unique_tempdir();
        let metadata_path = dir.join(metadata::METADATA_FILE_NAME);
        sample_metadata().save(&metadata_path).expect("seed metadata");

        let probe: Box<RemoteUrlProbe> =
            Box::new(|_cfg: &Config, _uuid: &str| Ok("https://claude.ai/remote/test-token".to_owned()));
        let config = sample_config();

        let url = capture_remote_url_for_test(&config, "11111111", &metadata_path, &probe)
            .expect("capture should succeed against a mocked probe");

        assert_eq!(
            url.as_deref(),
            Some("https://claude.ai/remote/test-token"),
            "captured URL should round-trip from the probe",
        );

        let reloaded = Metadata::load(&metadata_path).expect("reload metadata");
        assert_eq!(
            reloaded.remote_url.as_deref(),
            Some("https://claude.ai/remote/test-token"),
            "remote_url should be persisted into metadata.json",
        );
    }

    #[test]
    fn capture_remote_url_propagates_probe_failure() {
        let dir = unique_tempdir();
        let metadata_path = dir.join(metadata::METADATA_FILE_NAME);
        sample_metadata().save(&metadata_path).expect("seed metadata");

        let probe: Box<RemoteUrlProbe> =
            Box::new(|_cfg: &Config, _uuid: &str| Err(Error::from(SessionError::MissingCredentials)));
        let config = sample_config();

        let result = capture_remote_url_for_test(&config, "11111111", &metadata_path, &probe);

        assert!(result.is_err(), "probe failure should propagate to the caller");
    }

    #[test]
    fn persist_remote_url_is_idempotent_across_calls() {
        let dir = unique_tempdir();
        let metadata_path = dir.join(metadata::METADATA_FILE_NAME);
        sample_metadata().save(&metadata_path).expect("seed metadata");

        persist_remote_url(&metadata_path, "first").expect("first persist");
        persist_remote_url(&metadata_path, "second").expect("second persist");

        let reloaded = Metadata::load(&metadata_path).expect("reload metadata");
        assert_eq!(
            reloaded.remote_url.as_deref(),
            Some("second"),
            "later persist should overwrite the prior value",
        );
    }

    #[test]
    #[ignore = "requires a running qemu:///session libvirtd plus /dev/kvm; run with --ignored after setting up locally"]
    fn run_boots_session_against_real_libvirtd() {}

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn sample_config() -> Config {
        Config {
            base_default_repo: None,
            base_envs: vec!["rust".to_owned()],
            base_repos: vec![],
            claude_backend: tartarus_provider::config::Backend::Anthropic,
            claude_enabled: true,
            claude_anthropic_api_key: Some("sk-ant-test".to_owned()),
            claude_effort: "high".to_owned(),
            claude_model: "claude-opus-4-7".to_owned(),
            claude_vertex_credentials_file: None,
            claude_vertex_project_id: None,
            claude_vertex_region: None,
            disk_grow_increment_gib: 100,
            disk_grow_threshold_pct: 85,
            disk_virtual_size_gib: 100,
            github_token: Some("ghp_test".to_owned()),
            network_uri: "qemu:///session".to_owned(),
            rust_cargo_tools: vec![],
            rust_components: vec![],
            rust_toolchains: vec![],
            user_gid: None,
            user_uid: None,
            user_username: None,
            vm_memory_mib: 4_096,
            vm_vcpus: 2,
            provider: tartarus_provider::config::ProviderKind::Libvirt,
            hetzner: None,
        }
    }

    fn sample_metadata() -> Metadata {
        metadata::fresh(metadata::FreshFields {
            alias: None,
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            envs: vec!["rust".to_owned()],
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            vcpus: 2,
        })
    }

    fn unique_tempdir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-session-run-test-{pid}-{n}"));
        std::fs::create_dir_all(&path).expect("tempdir create");
        path
    }
}
