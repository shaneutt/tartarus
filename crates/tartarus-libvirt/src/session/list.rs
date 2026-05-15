//! `tartarus list`: enumerate sessions and produce a status table.

use std::{collections::HashMap, path::Path};

use tartarus_provider::{
    ListEntry, paths,
    session::metadata::{METADATA_FILE_NAME, Metadata},
};

use crate::{config::Config, error::Result, host::connect::Connection};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Length of the UUID prefix shown in the table.
const UUID_PREFIX_LEN: usize = 8;

/// Width of the alias column.
const ALIAS_COL_WIDTH: usize = 24;

/// Width of the status column.
const STATUS_COL_WIDTH: usize = 9;

/// Width of the base column.
const BASE_COL_WIDTH: usize = 28;

/// Width of the envs column.
const ENVS_COL_WIDTH: usize = 22;

/// Width of the size column.
const SIZE_COL_WIDTH: usize = 6;

/// Width of the memory column.
const MEM_COL_WIDTH: usize = 7;

/// Width of the CPU column.
const CPU_COL_WIDTH: usize = 4;

/// Read every session under `by-uuid/`, query libvirt for each one's
/// status, and return the assembled rows.
pub fn collect(config: &Config) -> Result<Vec<ListEntry>> {
    let dir = paths::sessions_by_uuid_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let metadatas = scan_metadata(&dir)?;
    let statuses = query_statuses(config, &metadatas).unwrap_or_default();

    Ok(metadatas
        .into_iter()
        .map(|m| build_entry(&m, statuses.get(&m.uuid)))
        .collect())
}

/// Render a list of [`ListEntry`] into the `tartarus list` table.
pub fn render(entries: &[ListEntry]) -> String {
    if entries.is_empty() {
        return "(no sessions yet — run `tartarus run --repo <slug>`)\n".to_owned();
    }

    let mut out = String::new();
    out.push_str(&format!(
        "{a:<aw$}  {u:<uw$}  {s:<sw$}  {b:<bw$}  {e:<ew$}  {z:<zw$}  {m:<mw$}  {c:<cw$}  PERSIST\n",
        a = "ALIAS",
        u = "UUID",
        s = "STATUS",
        b = "BASE",
        e = "ENVS",
        z = "SIZE",
        m = "MEM",
        c = "CPU",
        aw = ALIAS_COL_WIDTH,
        uw = UUID_PREFIX_LEN,
        sw = STATUS_COL_WIDTH,
        bw = BASE_COL_WIDTH,
        ew = ENVS_COL_WIDTH,
        zw = SIZE_COL_WIDTH,
        mw = MEM_COL_WIDTH,
        cw = CPU_COL_WIDTH,
    ));
    for row in entries {
        out.push_str(&format_row(row));
    }
    out
}

/// `tartarus list` entry point. Returns the rendered table string.
pub fn run(config: &Config) -> Result<String> {
    tracing::info!("scanning sessions");
    let entries = collect(config)?;
    Ok(render(&entries))
}

// -----------------------------------------------------------------------------
// Table Rendering
// -----------------------------------------------------------------------------

/// Format one [`ListEntry`] row into the table layout.
fn format_row(entry: &ListEntry) -> String {
    format!(
        "{a:<aw$}  {u:<uw$}  {s:<sw$}  {b:<bw$}  {e:<ew$}  {z:<zw$}  {m:<mw$}  {c:<cw$}  {p}\n",
        a = entry.alias,
        u = entry.uuid_short,
        s = entry.status,
        b = entry.base,
        e = entry.envs,
        z = entry.size,
        m = entry.mem,
        c = entry.cpu,
        p = entry.persist,
        aw = ALIAS_COL_WIDTH,
        uw = UUID_PREFIX_LEN,
        sw = STATUS_COL_WIDTH,
        bw = BASE_COL_WIDTH,
        ew = ENVS_COL_WIDTH,
        zw = SIZE_COL_WIDTH,
        mw = MEM_COL_WIDTH,
        cw = CPU_COL_WIDTH,
    )
}

/// Load every parseable `metadata.json` under `by-uuid/`, skipping
/// malformed entries.
fn scan_metadata(dir: &Path) -> Result<Vec<Metadata>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path().join(METADATA_FILE_NAME);
        if !path.exists() {
            continue;
        }
        match Metadata::load(&path) {
            Ok(metadata) => out.push(metadata),
            Err(err) => tracing::warn!(?path, %err, "skipping malformed session metadata"),
        }
    }
    out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(out)
}

/// Query libvirt for session statuses via a single
/// `virConnectListAllDomains` call. Returns an empty map when
/// libvirtd is unreachable.
fn query_statuses(config: &Config, sessions: &[Metadata]) -> Result<HashMap<String, String>> {
    let connection = Connection::open(&config.network_uri)?;
    let domains =
        connection
            .inner()
            .list_all_domains(0)
            .map_err(|source| crate::host::error::HostError::DomainOperation {
                operation: "list_all_domains",
                source,
            })?;

    let mut by_name: HashMap<String, &virt::domain::Domain> = HashMap::with_capacity(domains.len());
    for domain in &domains {
        match domain.get_name() {
            Ok(name) => {
                by_name.insert(name, domain);
            },
            Err(err) => tracing::debug!(%err, "skipping domain whose name could not be read"),
        }
    }

    let mut out = HashMap::with_capacity(sessions.len());
    for session in sessions {
        let label = match by_name.get(session.uuid.as_str()) {
            Some(domain) => domain_state_label(domain),
            None => {
                tracing::debug!(uuid = %session.uuid, "domain not present in libvirt list");
                "missing".to_owned()
            },
        };
        out.insert(session.uuid.clone(), label);
    }
    Ok(out)
}

/// Map a libvirt domain to its status label.
fn domain_state_label(domain: &virt::domain::Domain) -> String {
    match domain.is_active() {
        Ok(true) => "running".to_owned(),
        Ok(false) => "shutoff".to_owned(),
        Err(_) => "unknown".to_owned(),
    }
}

/// Assemble one [`ListEntry`] from metadata and optional status.
fn build_entry(metadata: &Metadata, status: Option<&String>) -> ListEntry {
    let alias = metadata.alias.clone().unwrap_or_else(|| "(unnamed)".to_owned());
    let uuid_short: String = metadata.uuid.chars().take(UUID_PREFIX_LEN).collect();
    let envs = metadata.envs.join(",");
    let persist = if metadata.persist { "yes" } else { "no" }.to_owned();
    let size = if metadata.overlay_virtual_gib == 0 {
        "?".to_owned()
    } else {
        format!("{n}G", n = metadata.overlay_virtual_gib)
    };
    let mem = if metadata.memory_mib == 0 {
        "?".to_owned()
    } else {
        format!("{n}M", n = metadata.memory_mib)
    };
    let cpu = if metadata.vcpus == 0 {
        "?".to_owned()
    } else {
        metadata.vcpus.to_string()
    };
    let status = status.cloned().unwrap_or_else(|| "unknown".to_owned());

    ListEntry {
        alias,
        base: metadata.base.clone(),
        cpu,
        envs,
        mem,
        persist,
        size,
        status,
        uuid_short,
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tartarus_provider::{seed::input::RepoSpec, session::metadata};

    use super::*;

    #[test]
    fn render_emits_header_and_rows() {
        let entries = vec![
            ListEntry {
                alias: "(unnamed)".to_owned(),
                base: "fedora-41-2026-05-01.qcow2".to_owned(),
                cpu: "2".to_owned(),
                envs: "rust,go".to_owned(),
                mem: "4096M".to_owned(),
                persist: "yes".to_owned(),
                size: "100G".to_owned(),
                status: "running".to_owned(),
                uuid_short: "12345678".to_owned(),
            },
            ListEntry {
                alias: "fix-bug".to_owned(),
                base: "fedora-41-2026-05-01.qcow2".to_owned(),
                cpu: "4".to_owned(),
                envs: "python".to_owned(),
                mem: "8192M".to_owned(),
                persist: "no".to_owned(),
                size: "200G".to_owned(),
                status: "shutoff".to_owned(),
                uuid_short: "abcdef01".to_owned(),
            },
        ];

        let rendered = render(&entries);

        assert!(rendered.contains("ALIAS"), "header should appear, got: {rendered}");
        assert!(rendered.contains("SIZE"), "SIZE header should appear, got: {rendered}");
        assert!(rendered.contains("MEM"), "MEM header should appear, got: {rendered}");
        assert!(rendered.contains("CPU"), "CPU header should appear, got: {rendered}");
        assert!(
            rendered.contains("PERSIST"),
            "PERSIST column should appear, got: {rendered}"
        );
        assert!(rendered.contains("100G"), "size row should appear, got: {rendered}");
        assert!(rendered.contains("4096M"), "memory row should appear, got: {rendered}");
        assert!(rendered.contains("(unnamed)"), "unnamed row should appear");
        assert!(rendered.contains("fix-bug"), "alias row should appear");
        assert!(rendered.contains("shutoff"), "shutoff status should appear");
    }

    #[test]
    fn render_handles_empty_listing() {
        let rendered = render(&[]);
        assert!(
            rendered.contains("no sessions yet"),
            "empty listing should print the help-bearing line, got: {rendered}",
        );
    }

    #[test]
    fn build_entry_uses_documented_unnamed_marker() {
        let metadata = sample_metadata(None);
        let entry = build_entry(&metadata, Some(&"running".to_owned()));

        assert_eq!(entry.alias, "(unnamed)", "absent alias should render as `(unnamed)`");
        assert_eq!(entry.status, "running", "status should round-trip from libvirt");
        assert_eq!(entry.persist, "yes", "persist should render as `yes`");
    }

    #[test]
    fn build_entry_falls_back_to_unknown_status() {
        let metadata = sample_metadata(Some("alpha".to_owned()));
        let entry = build_entry(&metadata, None);

        assert_eq!(
            entry.status,
            "unknown",
            "missing libvirt status should render as `unknown`, got: {status}",
            status = entry.status,
        );
    }

    #[test]
    fn build_entry_renders_size_from_overlay_virtual_gib() {
        let mut metadata = sample_metadata(None);
        metadata.overlay_virtual_gib = 250;

        let entry = build_entry(&metadata, None);

        assert_eq!(
            entry.size,
            "250G",
            "size column should reflect overlay_virtual_gib, got: {size}",
            size = entry.size,
        );
    }

    #[test]
    fn build_entry_renders_size_question_mark_when_overlay_size_unknown() {
        let mut metadata = sample_metadata(None);
        metadata.overlay_virtual_gib = 0;

        let entry = build_entry(&metadata, None);

        assert_eq!(
            entry.size,
            "?",
            "metadata predating P10 (overlay_virtual_gib = 0) should render as `?`, got: {size}",
            size = entry.size,
        );
    }

    #[test]
    fn build_entry_truncates_uuid_to_prefix() {
        let metadata = sample_metadata(None);
        let entry = build_entry(&metadata, None);

        assert_eq!(
            entry.uuid_short.len(),
            UUID_PREFIX_LEN,
            "uuid_short must be exactly {UUID_PREFIX_LEN} chars, got: {short}",
            short = entry.uuid_short,
        );
    }

    // -----------------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------------

    fn sample_metadata(alias: Option<String>) -> Metadata {
        Metadata {
            version: metadata::CURRENT_VERSION,
            alias,
            base: "fedora-41-2026-05-01.qcow2".to_owned(),
            created_at: "2026-05-05T10:00:00Z".to_owned(),
            envs: vec!["rust".to_owned()],
            gpu_borrow: None,
            last_attached_at: None,
            memory_mib: 4_096,
            overlay_virtual_gib: 100,
            persist: true,
            remote_url: None,
            repos: vec![RepoSpec {
                slug: "owner/name".to_owned(),
                default: true,
            }],
            ssh_port: None,
            uuid: "11111111-2222-3333-4444-555555555555".to_owned(),
            vcpus: 2,
        }
    }
}
