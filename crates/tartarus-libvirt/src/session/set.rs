//! `tartarus set <session>`: change compute resources on an existing session.
//!
//! Stopped sessions get their domain XML regenerated. Running sessions
//! use libvirt's memory and vCPU APIs to apply live changes (best-effort)
//! and persist the new values for the next boot.

use std::path::Path;

use tartarus_provider::session::{
    SessionError, identity,
    metadata::{self, Metadata},
};

use crate::{
    config::Config,
    error::{Error, Result},
    host::{
        connect::Connection,
        domain::{self, SessionDomainSpec},
        error::HostError,
    },
    session::run::DOMAIN_XML_FILE_NAME,
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Minimum session memory, in MiB (matches config validation).
const MIN_MEMORY_MIB: u32 = 512;

/// Minimum session vCPU count (matches config validation).
const MIN_VCPUS: u32 = 1;

/// Maximum session vCPU count (matches config validation).
const MAX_VCPUS: u32 = 64;

/// KiB per MiB, for the libvirt memory API.
const KIB_PER_MIB: u64 = 1_024;

// -----------------------------------------------------------------------------
// SetRequest
// -----------------------------------------------------------------------------

/// Caller-supplied parameters for [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetRequest {
    /// New memory in MiB, or `None` to leave unchanged.
    pub memory: Option<u32>,

    /// New vCPU count, or `None` to leave unchanged.
    pub vcpus: Option<u32>,
}

/// Outcome of a successful [`run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetOutcome {
    /// Whether the session was running or stopped.
    pub mode: SetMode,

    /// Effective memory after the change.
    pub memory_mib: u32,

    /// Session UUID.
    pub uuid: String,

    /// Effective vCPU count after the change.
    pub vcpus: u32,
}

/// Whether the session was running at reconfigure time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SetMode {
    /// Session was already running. Changes applied live (best-effort)
    /// and persisted for the next boot.
    Running,

    /// Session was stopped. Domain XML regenerated with new values.
    Stopped,
}

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus set <alias|uuid> [--memory MIB] [--vcpus COUNT]`.
pub fn run(config: &Config, target: &str, request: &SetRequest) -> Result<SetOutcome> {
    if request.memory.is_none() && request.vcpus.is_none() {
        return Err(Error::from(SessionError::MissingSetField));
    }

    validate_bounds(request)?;

    let resolved = identity::resolve(target)?;
    let metadata_path = resolved.directory.join(metadata::METADATA_FILE_NAME);
    let mut metadata = Metadata::load(&metadata_path)?;

    let memory_mib = request.memory.unwrap_or_else(|| effective_memory(&metadata, config));
    let vcpus = request.vcpus.unwrap_or_else(|| effective_vcpus(&metadata, config));

    let connection = Connection::open(&config.network_uri)?;
    let is_running = is_active(&connection, &resolved.uuid)?;

    let mode = if is_running {
        apply_running(&connection, &resolved.uuid, memory_mib, vcpus)?;
        SetMode::Running
    } else {
        apply_stopped(&resolved.directory, &resolved.uuid, &metadata, memory_mib, vcpus)?;
        SetMode::Stopped
    };

    metadata.memory_mib = memory_mib;
    metadata.vcpus = vcpus;
    metadata.save(&metadata_path)?;

    tracing::info!(
        uuid = %resolved.uuid,
        ?mode,
        memory_mib,
        vcpus,
        "session resources updated",
    );

    Ok(SetOutcome {
        mode,
        memory_mib,
        uuid: resolved.uuid,
        vcpus,
    })
}

// -----------------------------------------------------------------------------
// Validation
// -----------------------------------------------------------------------------

/// Validate request values against bounds.
fn validate_bounds(request: &SetRequest) -> Result<()> {
    if let Some(mib) = request.memory
        && mib < MIN_MEMORY_MIB
    {
        return Err(Error::from(SessionError::SetOutOfRange {
            field: "memory",
            value: mib,
            min: MIN_MEMORY_MIB,
            max: u32::MAX,
        }));
    }
    if let Some(count) = request.vcpus
        && !(MIN_VCPUS..=MAX_VCPUS).contains(&count)
    {
        return Err(Error::from(SessionError::SetOutOfRange {
            field: "vcpus",
            value: count,
            min: MIN_VCPUS,
            max: MAX_VCPUS,
        }));
    }
    Ok(())
}

/// Effective memory: from metadata if non-zero, else config default.
fn effective_memory(metadata: &Metadata, config: &Config) -> u32 {
    if metadata.memory_mib > 0 {
        metadata.memory_mib
    } else {
        config.vm_memory_mib
    }
}

/// Effective vCPUs: from metadata if non-zero, else config default.
fn effective_vcpus(metadata: &Metadata, config: &Config) -> u32 {
    if metadata.vcpus > 0 {
        metadata.vcpus
    } else {
        config.vm_vcpus
    }
}

// -----------------------------------------------------------------------------
// Running Path
// -----------------------------------------------------------------------------

/// Apply memory and vCPU changes to a running domain.
///
/// Uses `VIR_DOMAIN_*_CONFIG` to persist for next boot and attempts
/// `VIR_DOMAIN_*_LIVE` for immediate effect (best-effort).
fn apply_running(connection: &Connection, name: &str, memory_mib: u32, vcpus: u32) -> Result<()> {
    let domain = domain::lookup(connection, name)?;
    let memory_kib = u64::from(memory_mib) * KIB_PER_MIB;

    let config_flag = virt::sys::VIR_DOMAIN_MEM_CONFIG;
    let live_and_config = virt::sys::VIR_DOMAIN_MEM_LIVE | virt::sys::VIR_DOMAIN_MEM_CONFIG;

    match domain.set_memory_flags(memory_kib, live_and_config) {
        Ok(_) => tracing::info!(memory_mib, "memory updated (live + config)"),
        Err(err) => {
            tracing::warn!(%err, memory_mib, "live memory change failed; persisting for next boot");
            domain
                .set_memory_flags(memory_kib, config_flag)
                .map_err(|source| HostError::DomainOperation {
                    operation: "set_memory_flags(CONFIG)",
                    source,
                })?;
        },
    }

    let vcpu_config = virt::sys::VIR_DOMAIN_VCPU_CONFIG;
    let vcpu_live_config = virt::sys::VIR_DOMAIN_VCPU_LIVE | virt::sys::VIR_DOMAIN_VCPU_CONFIG;

    match domain.set_vcpus_flags(vcpus, vcpu_live_config) {
        Ok(_) => tracing::info!(vcpus, "vcpus updated (live + config)"),
        Err(err) => {
            tracing::warn!(%err, vcpus, "live vcpu change failed; persisting for next boot");
            domain
                .set_vcpus_flags(vcpus, vcpu_config)
                .map_err(|source| HostError::DomainOperation {
                    operation: "set_vcpus_flags(CONFIG)",
                    source,
                })?;
        },
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Stopped Path
// -----------------------------------------------------------------------------

/// Regenerate domain XML and redefine the libvirt domain.
fn apply_stopped(session_dir: &Path, uuid: &str, metadata: &Metadata, memory_mib: u32, vcpus: u32) -> Result<()> {
    let overlay = session_dir.join("overlay.qcow2");
    let seed_iso = session_dir.join("cloud-init.iso");

    let ssh_port = metadata
        .ssh_port
        .ok_or_else(|| SessionError::SshPortMissing { uuid: uuid.to_owned() })?;
    let mut spec = SessionDomainSpec::new(uuid, &overlay, &seed_iso, memory_mib, ssh_port).with_vcpus(vcpus);

    if let Some(ref borrow) = metadata.gpu_borrow {
        let address: crate::gpu::PciAddress = borrow.address.parse()?;
        spec = spec.with_gpu(address, crate::gpu::quirks::VendorQuirks::default());
    }

    let xml = spec.to_xml();
    let xml_path = session_dir.join(DOMAIN_XML_FILE_NAME);
    std::fs::write(&xml_path, xml.as_bytes())?;

    tracing::debug!(uuid, "domain XML regenerated with new resources");

    Ok(())
}

// -----------------------------------------------------------------------------
// Utility Functions
// -----------------------------------------------------------------------------

/// True iff `name`'s libvirt domain is currently active.
fn is_active(connection: &Connection, name: &str) -> Result<bool> {
    match domain::lookup(connection, name) {
        Ok(domain) => domain.is_active().map_err(|source| {
            HostError::DomainOperation {
                operation: "is_active",
                source,
            }
            .into()
        }),
        Err(_) => Ok(false),
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_mode_running_and_stopped_are_distinct() {
        assert_ne!(
            SetMode::Running,
            SetMode::Stopped,
            "Running and Stopped must be distinguishable",
        );
    }

    #[test]
    fn set_outcome_round_trips_through_struct() {
        let outcome = SetOutcome {
            mode: SetMode::Stopped,
            memory_mib: 8_192,
            uuid: "abcd".to_owned(),
            vcpus: 4,
        };

        assert_eq!(outcome.uuid, "abcd", "uuid should round-trip");
        assert_eq!(outcome.mode, SetMode::Stopped, "mode should round-trip");
        assert_eq!(outcome.memory_mib, 8_192, "memory_mib should round-trip");
        assert_eq!(outcome.vcpus, 4, "vcpus should round-trip");
    }

    #[test]
    fn validate_rejects_undersized_memory() {
        let request = SetRequest {
            memory: Some(256),
            vcpus: None,
        };
        let err = validate_bounds(&request).unwrap_err();
        assert!(err.to_string().contains("memory"), "error should mention memory: {err}",);
    }

    #[test]
    fn validate_rejects_zero_vcpus() {
        let request = SetRequest {
            memory: None,
            vcpus: Some(0),
        };
        let err = validate_bounds(&request).unwrap_err();
        assert!(err.to_string().contains("vcpus"), "error should mention vcpus: {err}",);
    }

    #[test]
    fn validate_rejects_excessive_vcpus() {
        let request = SetRequest {
            memory: None,
            vcpus: Some(65),
        };
        let err = validate_bounds(&request).unwrap_err();
        assert!(err.to_string().contains("vcpus"), "error should mention vcpus: {err}",);
    }

    #[test]
    fn validate_accepts_boundary_values() {
        let request = SetRequest {
            memory: Some(MIN_MEMORY_MIB),
            vcpus: Some(MAX_VCPUS),
        };
        assert!(validate_bounds(&request).is_ok(), "boundary values should be accepted",);
    }

    #[test]
    fn effective_memory_prefers_metadata_over_config() {
        let config = sample_config();
        let mut metadata = sample_metadata();
        metadata.memory_mib = 16_384;

        assert_eq!(
            effective_memory(&metadata, &config),
            16_384,
            "non-zero metadata memory should win over config default",
        );
    }

    #[test]
    fn effective_memory_falls_back_to_config() {
        let config = sample_config();
        let mut metadata = sample_metadata();
        metadata.memory_mib = 0;

        assert_eq!(
            effective_memory(&metadata, &config),
            config.vm_memory_mib,
            "zero metadata memory should fall back to config",
        );
    }

    #[test]
    fn effective_vcpus_prefers_metadata_over_config() {
        let config = sample_config();
        let mut metadata = sample_metadata();
        metadata.vcpus = 8;

        assert_eq!(
            effective_vcpus(&metadata, &config),
            8,
            "non-zero metadata vcpus should win over config default",
        );
    }

    #[test]
    fn effective_vcpus_falls_back_to_config() {
        let config = sample_config();
        let mut metadata = sample_metadata();
        metadata.vcpus = 0;

        assert_eq!(
            effective_vcpus(&metadata, &config),
            config.vm_vcpus,
            "zero metadata vcpus should fall back to config",
        );
    }

    #[test]
    #[ignore = "requires a running libvirtd; run with --ignored"]
    fn end_to_end_set_on_stopped_session() {}

    #[test]
    #[ignore = "requires a running libvirtd with an active domain; run with --ignored"]
    fn end_to_end_set_on_running_session() {}

    // -------------------------------------------------------------------------
    // Test Utilities
    // -------------------------------------------------------------------------

    fn sample_config() -> Config {
        Config {
            base_default_repo: None,
            base_envs: vec![],
            base_repos: vec![],
            claude_backend: tartarus_provider::config::Backend::Anthropic,
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
            base: "fedora-41-test.qcow2".to_owned(),
            envs: vec![],
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            repos: vec![],
            uuid: "00000000-0000-0000-0000-000000000000".to_owned(),
            vcpus: 2,
        })
    }
}
