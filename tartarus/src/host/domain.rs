//! Domain XML templating and lifecycle (define / start / shutdown /
//! destroy / undefine).

use std::time::{Duration, Instant};

use crate::{
    error::Result,
    host::{connect::Connection, error::HostError},
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Polling interval while waiting for graceful shutdown.
const SHUTDOWN_POLL_INTERVAL_MS: u64 = 200;

/// Default memory (MiB) for a trivial domain.
const DEFAULT_MEMORY_MIB: u32 = 512;

/// Default vCPU count for a trivial domain.
const DEFAULT_VCPUS: u32 = 1;

/// Default memory (MiB) for a session domain.
const SESSION_MEMORY_MIB: u32 = 4_096;

/// Default vCPU count for a session domain.
const SESSION_VCPUS: u32 = 2;

/// Canonical `qemu-guest-agent` channel name.
const QEMU_GA_CHANNEL: &str = "org.qemu.guest_agent.0";

/// Memory (MiB) for the layering boot.
const LAYERING_MEMORY_MIB: u32 = 2_048;

/// vCPU count for the layering boot.
const LAYERING_VCPUS: u32 = 2;

/// Domain type: KVM full-virt.
const DOMAIN_TYPE: &str = "kvm";

/// OS type for hardware virtualisation.
const OS_TYPE: &str = "hvm";

/// Guest architecture.
const OS_ARCH: &str = "x86_64";

// ---------------------------------------------------------------------------
// Domain Lifecycle
// ---------------------------------------------------------------------------

/// Caller-supplied parameters for a Tartarus-managed domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainSpec {
    /// libvirt domain name (UUID-as-name in production usage).
    pub name: String,

    /// Memory in MiB.
    pub memory_mib: u32,

    /// Number of vCPUs.
    pub vcpus: u32,
}

/// Parameters for the one-shot layering domain that
/// `tartarus base pull` boots to apply the Tartarus layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LayeringDomainSpec {
    /// libvirt domain name.
    pub name: String,

    /// Path to the qcow2 base image (writable disk for layering).
    pub base_image: std::path::PathBuf,

    /// Memory in MiB.
    pub memory_mib: u32,

    /// Path to the layering seed ISO (cloud-init NoCloud).
    pub seed_iso: std::path::PathBuf,

    /// Number of vCPUs.
    pub vcpus: u32,
}

impl LayeringDomainSpec {
    /// Build a [`LayeringDomainSpec`] with default memory and vCPU
    /// counts.
    pub fn new(
        name: impl Into<String>,
        base_image: impl Into<std::path::PathBuf>,
        seed_iso: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            base_image: base_image.into(),
            memory_mib: LAYERING_MEMORY_MIB,
            seed_iso: seed_iso.into(),
            vcpus: LAYERING_VCPUS,
        }
    }

    /// Render the spec to libvirt domain XML.
    pub fn to_xml(&self) -> String {
        let name = xml_escape(&self.name);
        let base_image = xml_escape(&self.base_image.display().to_string());
        let seed_iso = xml_escape(&self.seed_iso.display().to_string());

        let mut xml = String::new();
        xml.push_str(&format!("<domain type='{DOMAIN_TYPE}'>\n"));
        xml.push_str(&format!("  <name>{name}</name>\n"));
        let memory_mib = self.memory_mib;
        let vcpus = self.vcpus;
        xml.push_str(&format!("  <memory unit='MiB'>{memory_mib}</memory>\n"));
        xml.push_str(&format!("  <currentMemory unit='MiB'>{memory_mib}</currentMemory>\n"));
        xml.push_str(&format!("  <vcpu placement='static'>{vcpus}</vcpu>\n"));
        xml.push_str("  <os>\n");
        xml.push_str(&format!("    <type arch='{OS_ARCH}'>{OS_TYPE}</type>\n"));
        xml.push_str("    <boot dev='hd'/>\n");
        xml.push_str("  </os>\n");
        xml.push_str("  <features>\n");
        xml.push_str("    <acpi/>\n");
        xml.push_str("    <apic/>\n");
        xml.push_str("  </features>\n");
        xml.push_str("  <devices>\n");

        xml.push_str("    <disk type='file' device='disk'>\n");
        xml.push_str("      <driver name='qemu' type='qcow2'/>\n");
        xml.push_str(&format!("      <source file='{base_image}'/>\n"));
        xml.push_str("      <target dev='vda' bus='virtio'/>\n");
        xml.push_str("    </disk>\n");

        xml.push_str("    <disk type='file' device='cdrom'>\n");
        xml.push_str("      <driver name='qemu' type='raw'/>\n");
        xml.push_str(&format!("      <source file='{seed_iso}'/>\n"));
        xml.push_str("      <target dev='sda' bus='sata'/>\n");
        xml.push_str("      <readonly/>\n");
        xml.push_str("    </disk>\n");

        xml.push_str("    <interface type='user'>\n");
        xml.push_str("      <model type='virtio'/>\n");
        xml.push_str("    </interface>\n");

        xml.push_str("    <serial type='pty'>\n");
        xml.push_str("      <target port='0'/>\n");
        xml.push_str("    </serial>\n");
        xml.push_str("    <console type='pty'>\n");
        xml.push_str("      <target type='serial' port='0'/>\n");
        xml.push_str("    </console>\n");

        xml.push_str("    <rng model='virtio'>\n");
        xml.push_str("      <backend model='random'>/dev/urandom</backend>\n");
        xml.push_str("    </rng>\n");

        xml.push_str("  </devices>\n");
        xml.push_str("</domain>\n");

        xml
    }
}

/// Parameters for a per-session domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDomainSpec {
    /// libvirt domain name (the session UUID, in production usage).
    pub name: String,

    /// PCI address of a borrowed GPU for passthrough, if any.
    pub gpu_passthrough: Option<crate::gpu::PciAddress>,

    /// Vendor-specific quirks for the borrowed GPU.
    pub gpu_quirks: crate::gpu::quirks::VendorQuirks,

    /// Loopback host port forwarded to guest port 22 for SSH.
    pub ssh_hostfwd_port: Option<u16>,

    /// Memory in MiB.
    pub memory_mib: u32,

    /// Path to the per-session qcow2 overlay.
    pub overlay: std::path::PathBuf,

    /// Path to the per-session seed ISO (cloud-init NoCloud).
    pub seed_iso: std::path::PathBuf,

    /// Number of vCPUs.
    pub vcpus: u32,
}

impl SessionDomainSpec {
    /// Build a [`SessionDomainSpec`] with default memory and vCPU
    /// counts.
    pub fn new(
        name: impl Into<String>,
        overlay: impl Into<std::path::PathBuf>,
        seed_iso: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self {
            name: name.into(),
            gpu_passthrough: None,
            gpu_quirks: crate::gpu::quirks::VendorQuirks::default(),
            memory_mib: SESSION_MEMORY_MIB,
            overlay: overlay.into(),
            seed_iso: seed_iso.into(),
            ssh_hostfwd_port: None,
            vcpus: SESSION_VCPUS,
        }
    }

    /// Builder: enable SSH port forwarding to the guest.
    pub fn with_ssh_hostfwd(mut self, host_port: u16) -> Self {
        self.ssh_hostfwd_port = Some(host_port);
        self
    }

    /// Builder: attach a borrowed GPU for passthrough.
    pub fn with_gpu(mut self, address: crate::gpu::PciAddress, quirks: crate::gpu::quirks::VendorQuirks) -> Self {
        self.gpu_passthrough = Some(address);
        self.gpu_quirks = quirks;
        self
    }

    /// Render the session spec to libvirt domain XML.
    pub fn to_xml(&self) -> String {
        let name = xml_escape(&self.name);
        let overlay = xml_escape(&self.overlay.display().to_string());
        let seed_iso = xml_escape(&self.seed_iso.display().to_string());

        let mut xml = String::new();
        push_session_header(&mut xml, &name, self.memory_mib, self.vcpus, &self.gpu_quirks);
        push_session_devices(
            &mut xml,
            &overlay,
            &seed_iso,
            self.gpu_passthrough.as_ref(),
            self.ssh_hostfwd_port,
        );
        xml.push_str("</domain>\n");

        xml
    }
}

impl DomainSpec {
    /// Build a trivial spec with default memory and vCPU count.
    pub fn trivial(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            memory_mib: DEFAULT_MEMORY_MIB,
            vcpus: DEFAULT_VCPUS,
        }
    }

    /// Render the spec to libvirt domain XML.
    pub fn to_xml(&self) -> String {
        let name = xml_escape(&self.name);
        let memory = self.memory_mib;
        let vcpus = self.vcpus;

        let mut xml = String::new();
        xml.push_str(&format!("<domain type='{DOMAIN_TYPE}'>\n"));
        xml.push_str(&format!("  <name>{name}</name>\n"));
        xml.push_str(&format!("  <memory unit='MiB'>{memory}</memory>\n"));
        xml.push_str(&format!("  <currentMemory unit='MiB'>{memory}</currentMemory>\n"));
        xml.push_str(&format!("  <vcpu placement='static'>{vcpus}</vcpu>\n"));
        xml.push_str("  <os>\n");
        xml.push_str(&format!("    <type arch='{OS_ARCH}'>{OS_TYPE}</type>\n"));
        xml.push_str("  </os>\n");
        xml.push_str("</domain>\n");

        xml
    }
}

/// RAII guard that undefines (and force-destroys if active) a libvirt
/// domain on `Drop` unless explicitly disarmed.
pub struct DomainGuard<'a> {
    /// True while the guard is responsible for cleanup.
    armed: bool,

    /// Connection used to look up the domain at drop time.
    connection: &'a Connection,

    /// Name of the guarded libvirt domain.
    name: String,
}

impl<'a> DomainGuard<'a> {
    /// Adopt cleanup responsibility for the domain `name` on `connection`.
    pub fn adopt(connection: &'a Connection, name: impl Into<String>) -> Self {
        Self {
            armed: true,
            connection,
            name: name.into(),
        }
    }

    /// Release cleanup responsibility (the domain should outlive the
    /// guard).
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DomainGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        match lookup(self.connection, &self.name) {
            Ok(domain) => {
                let active = domain.is_active().unwrap_or(false);
                if active && let Err(err) = domain.destroy() {
                    tracing::warn!(
                        name = %self.name,
                        %err,
                        "failed to force-destroy libvirt domain during rollback",
                    );
                }
                if let Err(err) = domain.undefine() {
                    tracing::warn!(
                        name = %self.name,
                        %err,
                        "failed to undefine libvirt domain during rollback; manual `virsh undefine` may be required",
                    );
                } else {
                    tracing::debug!(name = %self.name, "rolled back libvirt domain");
                }
            },
            Err(err) => tracing::debug!(
                name = %self.name,
                %err,
                "libvirt domain not found at rollback time; assuming already cleaned up",
            ),
        }
    }
}

/// Define a persistent domain on `connection` from `spec`.
pub fn define(connection: &Connection, spec: &DomainSpec) -> Result<virt::domain::Domain> {
    let xml = spec.to_xml();

    tracing::debug!(name = %spec.name, "defining libvirt domain");

    let domain =
        virt::domain::Domain::define_xml(connection.inner(), &xml).map_err(|source| HostError::DomainOperation {
            operation: "define",
            source,
        })?;

    Ok(domain)
}

/// Look up a previously-defined domain by name.
pub fn lookup(connection: &Connection, name: &str) -> Result<virt::domain::Domain> {
    let domain = virt::domain::Domain::lookup_by_name(connection.inner(), name).map_err(|source| {
        HostError::DomainOperation {
            operation: "lookup_by_name",
            source,
        }
    })?;

    Ok(domain)
}

/// Start a previously-defined domain by name.
pub fn start(connection: &Connection, name: &str) -> Result<()> {
    let domain = lookup(connection, name)?;

    tracing::debug!(name, "starting libvirt domain");

    domain.create().map_err(|source| HostError::DomainOperation {
        operation: "create",
        source,
    })?;

    Ok(())
}

/// Request a graceful shutdown and wait up to `timeout` for shut-off.
///
/// Returns [`HostError::ShutdownTimeout`] without forcing; the caller
/// decides whether to fall back to [`destroy`].
pub fn shutdown(connection: &Connection, name: &str, timeout: Duration) -> Result<()> {
    let domain = lookup(connection, name)?;

    tracing::debug!(name, ?timeout, "requesting graceful libvirt shutdown");

    domain.shutdown().map_err(|source| HostError::DomainOperation {
        operation: "shutdown",
        source,
    })?;

    wait_for_shutoff_inner(&domain, name, timeout)
}

/// Force-stop the domain immediately. Equivalent to `virsh destroy`.
pub fn destroy(connection: &Connection, name: &str) -> Result<()> {
    let domain = lookup(connection, name)?;

    tracing::debug!(name, "force-destroying libvirt domain");

    domain.destroy().map_err(|source| HostError::DomainOperation {
        operation: "destroy",
        source,
    })?;

    Ok(())
}

/// Undefine a domain by name. The domain must be inactive first.
pub fn undefine(connection: &Connection, name: &str) -> Result<()> {
    let domain = lookup(connection, name)?;

    tracing::debug!(name, "undefining libvirt domain");

    domain.undefine().map_err(|source| HostError::DomainOperation {
        operation: "undefine",
        source,
    })?;

    Ok(())
}

/// Define a layering domain on `connection` from `spec`.
pub fn define_layering(connection: &Connection, spec: &LayeringDomainSpec) -> Result<virt::domain::Domain> {
    let xml = spec.to_xml();

    tracing::debug!(name = %spec.name, "defining libvirt layering domain");

    let domain =
        virt::domain::Domain::define_xml(connection.inner(), &xml).map_err(|source| HostError::DomainOperation {
            operation: "define-layering",
            source,
        })?;

    Ok(domain)
}

/// Block until `name` reaches shut-off or `timeout` elapses.
pub fn wait_for_shutoff(connection: &Connection, name: &str, timeout: Duration) -> Result<()> {
    let domain = lookup(connection, name)?;
    wait_for_shutoff_inner(&domain, name, timeout)
}

/// Define a session domain on `connection` from `spec`.
pub fn define_session(connection: &Connection, spec: &SessionDomainSpec) -> Result<virt::domain::Domain> {
    let xml = spec.to_xml();

    tracing::debug!(name = %spec.name, "defining libvirt session domain");

    let domain =
        virt::domain::Domain::define_xml(connection.inner(), &xml).map_err(|source| HostError::DomainOperation {
            operation: "define-session",
            source,
        })?;

    Ok(domain)
}

// ---------------------------------------------------------------------------
// XML Construction
// ---------------------------------------------------------------------------

/// Push the `<domain>` header, OS, features, and CPU sections.
fn push_session_header(
    xml: &mut String,
    name: &str,
    memory_mib: u32,
    vcpus: u32,
    quirks: &crate::gpu::quirks::VendorQuirks,
) {
    xml.push_str(&format!("<domain type='{DOMAIN_TYPE}'>\n"));
    xml.push_str(&format!("  <name>{name}</name>\n"));
    xml.push_str(&format!("  <memory unit='MiB'>{memory_mib}</memory>\n"));
    xml.push_str(&format!("  <currentMemory unit='MiB'>{memory_mib}</currentMemory>\n"));
    xml.push_str(&format!("  <vcpu placement='static'>{vcpus}</vcpu>\n"));
    xml.push_str("  <os>\n");
    xml.push_str(&format!("    <type arch='{OS_ARCH}'>{OS_TYPE}</type>\n"));
    xml.push_str("    <boot dev='hd'/>\n");
    xml.push_str("  </os>\n");
    xml.push_str("  <features>\n");
    xml.push_str("    <acpi/>\n");
    xml.push_str("    <apic/>\n");
    if quirks.apply_nvidia_hide_kvm {
        push_nvidia_code43_features(xml);
    }
    xml.push_str("  </features>\n");
    if quirks.apply_nvidia_hide_kvm {
        push_nvidia_code43_cpu(xml);
    }
}

/// Push the `<features>` children that hide KVM from an NVIDIA driver
/// (Code 43 workaround).
fn push_nvidia_code43_features(xml: &mut String) {
    xml.push_str("    <kvm>\n");
    xml.push_str("      <hidden state='on'/>\n");
    xml.push_str("    </kvm>\n");
    xml.push_str("    <hyperv mode='custom'>\n");
    xml.push_str("      <vendor_id state='on' value='tartarusvfio'/>\n");
    xml.push_str("    </hyperv>\n");
}

/// Push the `<cpu>` block that disables the `hypervisor` CPUID flag
/// for NVIDIA passthrough.
fn push_nvidia_code43_cpu(xml: &mut String) {
    xml.push_str("  <cpu mode='host-passthrough' check='none'>\n");
    xml.push_str("    <feature policy='disable' name='hypervisor'/>\n");
    xml.push_str("  </cpu>\n");
}

/// Push the `<devices>` section for a session domain.
fn push_session_devices(
    xml: &mut String,
    overlay: &str,
    seed_iso: &str,
    gpu: Option<&crate::gpu::PciAddress>,
    ssh_hostfwd_port: Option<u16>,
) {
    xml.push_str("  <devices>\n");
    push_session_disks(xml, overlay, seed_iso);
    push_session_network(xml, ssh_hostfwd_port);
    push_session_console(xml);
    push_session_qemu_ga_channel(xml);
    push_session_rng(xml);
    if let Some(addr) = gpu {
        push_session_hostdev_pci(xml, addr);
    }
    xml.push_str("  </devices>\n");
}

/// Push a `<hostdev>` block for PCI passthrough (`managed='no'`).
fn push_session_hostdev_pci(xml: &mut String, address: &crate::gpu::PciAddress) {
    xml.push_str("    <hostdev mode='subsystem' type='pci' managed='no'>\n");
    xml.push_str("      <source>\n");
    xml.push_str(&format!(
        "        <address domain='0x{:04x}' bus='0x{:02x}' slot='0x{:02x}' function='0x{:01x}'/>\n",
        address.domain, address.bus, address.device, address.function,
    ));
    xml.push_str("      </source>\n");
    xml.push_str("    </hostdev>\n");
}

/// Push the overlay disk + seed ISO cdrom subset of `<devices>`.
fn push_session_disks(xml: &mut String, overlay: &str, seed_iso: &str) {
    xml.push_str("    <disk type='file' device='disk'>\n");
    xml.push_str("      <driver name='qemu' type='qcow2' discard='unmap'/>\n");
    xml.push_str(&format!("      <source file='{overlay}'/>\n"));
    xml.push_str("      <target dev='vda' bus='virtio'/>\n");
    xml.push_str("    </disk>\n");

    xml.push_str("    <disk type='file' device='cdrom'>\n");
    xml.push_str("      <driver name='qemu' type='raw'/>\n");
    xml.push_str(&format!("      <source file='{seed_iso}'/>\n"));
    xml.push_str("      <target dev='sda' bus='sata'/>\n");
    xml.push_str("      <readonly/>\n");
    xml.push_str("    </disk>\n");
}

/// Push the SLIRP network interface, optionally with SSH port
/// forwarding.
fn push_session_network(xml: &mut String, ssh_hostfwd_port: Option<u16>) {
    xml.push_str("    <interface type='user'>\n");
    xml.push_str("      <model type='virtio'/>\n");
    if let Some(host_port) = ssh_hostfwd_port {
        xml.push_str("      <portForward proto='tcp' address='127.0.0.1'>\n");
        xml.push_str(&format!("        <range start='{host_port}' to='22'/>\n"));
        xml.push_str("      </portForward>\n");
    }
    xml.push_str("    </interface>\n");
}

/// Push the serial console PTY pair.
fn push_session_console(xml: &mut String) {
    xml.push_str("    <serial type='pty'>\n");
    xml.push_str("      <target port='0'/>\n");
    xml.push_str("    </serial>\n");
    xml.push_str("    <console type='pty'>\n");
    xml.push_str("      <target type='serial' port='0'/>\n");
    xml.push_str("    </console>\n");
}

/// Push the `qemu-guest-agent` virtio-serial channel device.
fn push_session_qemu_ga_channel(xml: &mut String) {
    xml.push_str("    <channel type='unix'>\n");
    xml.push_str("      <source mode='bind'/>\n");
    xml.push_str(&format!("      <target type='virtio' name='{QEMU_GA_CHANNEL}'/>\n",));
    xml.push_str("    </channel>\n");
}

/// Push the virtio-rng device.
fn push_session_rng(xml: &mut String) {
    xml.push_str("    <rng model='virtio'>\n");
    xml.push_str("      <backend model='random'>/dev/urandom</backend>\n");
    xml.push_str("    </rng>\n");
}

/// Block until `domain` reports the shut-off state or `timeout` elapses.
fn wait_for_shutoff_inner(domain: &virt::domain::Domain, name: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(SHUTDOWN_POLL_INTERVAL_MS);

    loop {
        let active = domain.is_active().map_err(|source| HostError::DomainOperation {
            operation: "is_active",
            source,
        })?;

        if !active {
            return Ok(());
        }

        if Instant::now() >= deadline {
            return Err(HostError::ShutdownTimeout {
                name: name.to_owned(),
                seconds: timeout.as_secs(),
            }
            .into());
        }

        std::thread::sleep(poll);
    }
}

/// Escape XML-significant characters for domain XML embedding.
fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());

    for ch in input.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_spec_uses_documented_defaults() {
        let spec = DomainSpec::trivial("test-domain");

        assert_eq!(spec.name, "test-domain", "name should round-trip from the constructor");
        assert_eq!(
            spec.memory_mib, DEFAULT_MEMORY_MIB,
            "trivial spec should use the documented memory default",
        );
        assert_eq!(
            spec.vcpus, DEFAULT_VCPUS,
            "trivial spec should use the documented vcpu default",
        );
    }

    #[test]
    fn xml_contains_required_elements() {
        let xml = DomainSpec::trivial("alpha").to_xml();

        assert!(
            xml.contains("<domain type='kvm'>"),
            "domain element should declare type kvm, got: {xml}",
        );
        assert!(
            xml.contains("<name>alpha</name>"),
            "name element should round-trip, got: {xml}",
        );
        assert!(
            xml.contains("<memory unit='MiB'>512</memory>"),
            "memory element should match defaults, got: {xml}",
        );
        assert!(
            xml.contains("<vcpu placement='static'>1</vcpu>"),
            "vcpu element should match defaults, got: {xml}",
        );
        assert!(
            xml.contains("<type arch='x86_64'>hvm</type>"),
            "os/type element should be hvm on x86_64, got: {xml}",
        );
    }

    #[test]
    fn xml_escapes_unsafe_characters_in_name() {
        let spec = DomainSpec {
            name: "evil<&>name".to_owned(),
            memory_mib: DEFAULT_MEMORY_MIB,
            vcpus: DEFAULT_VCPUS,
        };

        let xml = spec.to_xml();

        assert!(
            xml.contains("evil&lt;&amp;&gt;name"),
            "XML-significant characters in the name should be escaped, got: {xml}",
        );
        assert!(
            !xml.contains("evil<&>name"),
            "raw XML-significant characters should not appear, got: {xml}",
        );
    }

    #[test]
    fn xml_round_trips_custom_memory_and_vcpus() {
        let spec = DomainSpec {
            name: "beta".to_owned(),
            memory_mib: 1024,
            vcpus: 4,
        };

        let xml = spec.to_xml();

        assert!(
            xml.contains("<memory unit='MiB'>1024</memory>"),
            "memory should reflect the spec value, got: {xml}",
        );
        assert!(
            xml.contains("<vcpu placement='static'>4</vcpu>"),
            "vcpu count should reflect the spec value, got: {xml}",
        );
    }

    #[test]
    fn xml_escape_handles_all_xml_significant_characters() {
        let escaped = xml_escape("<>&\"'");

        assert_eq!(
            escaped, "&lt;&gt;&amp;&quot;&apos;",
            "every XML-significant character should be escaped",
        );
    }

    #[test]
    fn layering_spec_uses_documented_defaults() {
        let spec = LayeringDomainSpec::new("layering-test", "/tmp/base.qcow2", "/tmp/seed.iso");

        assert_eq!(spec.name, "layering-test", "name should round-trip");
        assert_eq!(
            spec.memory_mib, LAYERING_MEMORY_MIB,
            "layering spec should use the documented memory default",
        );
        assert_eq!(
            spec.vcpus, LAYERING_VCPUS,
            "layering spec should use the documented vcpu default",
        );
        assert_eq!(
            spec.base_image,
            std::path::PathBuf::from("/tmp/base.qcow2"),
            "base image path should round-trip",
        );
        assert_eq!(
            spec.seed_iso,
            std::path::PathBuf::from("/tmp/seed.iso"),
            "seed iso path should round-trip",
        );
    }

    #[test]
    fn layering_xml_attaches_base_disk_as_virtio_vda() {
        let spec = LayeringDomainSpec::new("layering-test", "/tmp/base.qcow2", "/tmp/seed.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<source file='/tmp/base.qcow2'/>"),
            "layering XML should reference the base image path, got: {xml}",
        );
        assert!(
            xml.contains("<target dev='vda' bus='virtio'/>"),
            "base disk must ride on virtio vda for the layering boot, got: {xml}",
        );
        assert!(
            xml.contains("<driver name='qemu' type='qcow2'/>"),
            "base disk driver should be qcow2, got: {xml}",
        );
    }

    #[test]
    fn layering_xml_attaches_seed_iso_as_cdrom() {
        let spec = LayeringDomainSpec::new("layering-test", "/tmp/base.qcow2", "/tmp/seed.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<disk type='file' device='cdrom'>"),
            "seed must ride on a cdrom-typed disk, got: {xml}",
        );
        assert!(
            xml.contains("<source file='/tmp/seed.iso'/>"),
            "cdrom source should point at the seed ISO, got: {xml}",
        );
        assert!(
            xml.contains("<readonly/>"),
            "cdrom must be readonly so cloud-init treats it as authoritative, got: {xml}",
        );
    }

    #[test]
    fn layering_xml_includes_serial_console_rng_and_slirp() {
        let spec = LayeringDomainSpec::new("layering-test", "/tmp/base.qcow2", "/tmp/seed.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<serial type='pty'>"),
            "layering XML should expose a serial console for boot diagnostics, got: {xml}",
        );
        assert!(
            xml.contains("<console type='pty'>"),
            "layering XML should expose the matching console device, got: {xml}",
        );
        assert!(
            xml.contains("<rng model='virtio'>"),
            "layering XML should include a virtio-rng device for entropy, got: {xml}",
        );
        assert!(
            xml.contains("<interface type='user'>"),
            "layering XML should use SLIRP usermode networking so cloud-init can reach dnf mirrors, got: {xml}",
        );
    }

    #[test]
    fn session_spec_uses_documented_defaults() {
        let spec = SessionDomainSpec::new("session-test", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");

        assert_eq!(spec.name, "session-test", "name should round-trip");
        assert_eq!(
            spec.memory_mib, SESSION_MEMORY_MIB,
            "session spec should use the documented session memory default",
        );
        assert_eq!(
            spec.vcpus, SESSION_VCPUS,
            "session spec should use the documented session vcpu default",
        );
    }

    #[test]
    fn session_xml_attaches_overlay_as_virtio_vda() {
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<source file='/tmp/overlay.qcow2'/>"),
            "session XML should reference the overlay path, got: {xml}",
        );
        assert!(
            xml.contains("<target dev='vda' bus='virtio'/>"),
            "overlay must ride on virtio vda, got: {xml}",
        );
        assert!(
            xml.contains("<driver name='qemu' type='qcow2' discard='unmap'/>"),
            "overlay driver should enable discard so guest fstrim flows back, got: {xml}",
        );
    }

    #[test]
    fn session_xml_attaches_seed_iso_as_readonly_cdrom() {
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<disk type='file' device='cdrom'>"),
            "session XML should expose the seed ISO as a cdrom, got: {xml}",
        );
        assert!(
            xml.contains("<source file='/tmp/cloud-init.iso'/>"),
            "session XML should reference the seed ISO path, got: {xml}",
        );
        assert!(
            xml.contains("<readonly/>"),
            "seed cdrom must be readonly so cloud-init treats it as authoritative, got: {xml}",
        );
    }

    #[test]
    fn session_xml_includes_required_extra_devices() {
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<serial type='pty'>"),
            "session XML should expose a serial console for P6 console attach, got: {xml}",
        );
        assert!(
            xml.contains("<console type='pty'>"),
            "session XML should expose the matching console device, got: {xml}",
        );
        assert!(
            xml.contains("<rng model='virtio'>"),
            "session XML should include a virtio-rng device, got: {xml}",
        );
        assert!(
            xml.contains("<interface type='user'>"),
            "session XML should use SLIRP usermode networking, got: {xml}",
        );
    }

    #[test]
    fn session_xml_omits_hostdev_when_no_gpu_borrowed() {
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");
        let xml = spec.to_xml();

        assert!(
            !xml.contains("<hostdev"),
            "session XML must not emit a hostdev block when no GPU is borrowed, got: {xml}",
        );
    }

    #[test]
    fn session_xml_emits_hostdev_with_managed_no_when_gpu_borrowed() {
        let address: crate::gpu::PciAddress = "0000:01:00.0".parse().expect("parse");
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso")
            .with_gpu(address, crate::gpu::quirks::VendorQuirks::default());
        let xml = spec.to_xml();

        assert!(
            xml.contains("<hostdev mode='subsystem' type='pci' managed='no'>"),
            "hostdev must be managed='no' so libvirt does not race our driver detach, got: {xml}",
        );
        assert!(
            xml.contains("domain='0x0000'") && xml.contains("bus='0x01'") && xml.contains("slot='0x00'"),
            "hostdev address must reflect the borrowed BDF, got: {xml}",
        );
    }

    #[test]
    fn session_xml_emits_kvm_hidden_when_nvidia_quirk_active() {
        let address: crate::gpu::PciAddress = "0000:01:00.0".parse().expect("parse");
        let quirks = crate::gpu::quirks::VendorQuirks {
            apply_nvidia_hide_kvm: true,
        };
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso").with_gpu(address, quirks);
        let xml = spec.to_xml();

        assert!(
            xml.contains("<kvm>") && xml.contains("<hidden state='on'/>"),
            "NVIDIA quirk must hide KVM via <kvm><hidden state='on'/></kvm>, got: {xml}",
        );
        assert!(
            xml.contains("<hyperv mode='custom'>") && xml.contains("<vendor_id state='on'"),
            "NVIDIA quirk must spoof a non-KVM hyperv vendor_id, got: {xml}",
        );
        assert!(
            xml.contains("<feature policy='disable' name='hypervisor'/>"),
            "NVIDIA quirk must scrub the hypervisor CPUID flag, got: {xml}",
        );
    }

    #[test]
    fn session_xml_omits_nvidia_quirk_when_inactive() {
        let address: crate::gpu::PciAddress = "0000:01:00.0".parse().expect("parse");
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso")
            .with_gpu(address, crate::gpu::quirks::VendorQuirks::default());
        let xml = spec.to_xml();

        assert!(
            !xml.contains("<kvm>") && !xml.contains("<hyperv"),
            "non-NVIDIA borrow must not emit the Code 43 quirk, got: {xml}",
        );
    }

    #[test]
    fn session_xml_carries_qemu_guest_agent_channel() {
        let spec = SessionDomainSpec::new("s", "/tmp/overlay.qcow2", "/tmp/cloud-init.iso");
        let xml = spec.to_xml();

        assert!(
            xml.contains("<channel type='unix'>"),
            "session XML must expose a unix-typed channel device, got: {xml}",
        );
        assert!(
            xml.contains("name='org.qemu.guest_agent.0'"),
            "channel must carry the canonical qemu-guest-agent name, got: {xml}",
        );
    }

    #[test]
    fn layering_xml_escapes_unsafe_characters_in_paths() {
        let spec = LayeringDomainSpec::new(
            "layering-test",
            std::path::PathBuf::from("/tmp/path<with>chars/base.qcow2"),
            std::path::PathBuf::from("/tmp/seed.iso"),
        );

        let xml = spec.to_xml();

        assert!(
            xml.contains("path&lt;with&gt;chars/base.qcow2"),
            "XML-significant characters in disk paths must be escaped, got: {xml}",
        );
    }
}
