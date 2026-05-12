//! Command-line interface definitions and dispatch.

use clap::{ArgAction, Parser, Subcommand};

use crate::{
    config::{CliOverrides, Config},
    error::{Error, Result},
    logging::Verbosity,
};

// -----------------------------------------------------------------------------
// Cli
// -----------------------------------------------------------------------------

/// Top-level Tartarus CLI.
#[derive(Debug, Parser)]
#[command(
    name = "tartarus",
    about = "Security sandbox for AI coding agents on QEMU/KVM via libvirt.",
    version,
    propagate_version = true
)]
pub struct Cli {
    /// Suppress all output below `error` level.
    #[arg(long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Increase log verbosity. Repeatable: `-v` is info, `-vv` is debug.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Subcommand to dispatch.
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Resolve the user's verbosity selection.
    pub fn verbosity(&self) -> Verbosity {
        if self.quiet {
            return Verbosity::Quiet;
        }

        match self.verbose {
            0 => Verbosity::Default,
            1 => Verbosity::Info,
            _ => Verbosity::Debug,
        }
    }
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage Tartarus credentials and config defaults.
    #[command(subcommand)]
    Auth(AuthCommand),

    /// Manage the base image library.
    #[command(subcommand)]
    Base(BaseCommand),

    /// Manage programming environments inside a session.
    #[command(subcommand)]
    Env(EnvCommand),

    /// Run diagnostic checks against the host.
    Doctor,

    /// Grow a session's overlay online by the configured increment.
    Grow {
        /// Alias or UUID identifying the session.
        target: String,
    },

    /// List all known sessions.
    List,

    /// Rename (or create) the alias symlink for a session UUID.
    Rename {
        /// UUID of the session to rename.
        uuid: String,

        /// New alias for the session.
        name: String,
    },

    /// Re-attach to an existing session.
    Resume {
        /// Alias or UUID identifying the session.
        target: String,
    },

    /// Start a new session.
    Run {
        /// Repository slug (`owner/name`) to clone into the session.
        /// Repeatable: pass `--repo` once per repo to clone into the
        /// session. When omitted, `[base] repos` from the config is
        /// used.
        #[arg(long)]
        repo: Vec<String>,

        /// Slug to mark as the session's default working directory.
        /// Must match one of the slugs supplied via `--repo` (or
        /// `[base] repos` when no `--repo` is given). Defaults to the
        /// config-side `[base] default_repo`, then to the
        /// `default = true`-flagged config entry, then to the first
        /// listed slug.
        #[arg(long)]
        default_repo: Option<String>,

        /// Delete the overlay and aliases when the session exits.
        #[arg(long)]
        ephemeral: bool,

        /// Friendly alias for the session.
        #[arg(long)]
        name: Option<String>,

        /// Programming environments to install at first boot
        /// (comma-separated; e.g. `rust,go,python`). Overrides `[base] envs`
        /// in `config.toml`. Also reads `TARTARUS_BASE_ENVS`.
        #[arg(long, value_delimiter = ',', env = "TARTARUS_BASE_ENVS")]
        env: Vec<String>,

        /// Read the GitHub PAT from stdin instead of the config.
        #[arg(long)]
        github_token_stdin: bool,

        /// Read the Anthropic API key from stdin instead of the config.
        #[arg(long)]
        anthropic_key_stdin: bool,

        /// Detached mode: start the session and return without
        /// attaching the console. Re-attach later via `tartarus
        /// resume <alias|uuid>`.
        #[arg(long, conflicts_with = "background")]
        detach: bool,

        /// Background mode: detached **plus** Claude remote-connect.
        /// The host captures the URL printed by Claude during boot
        /// and reports it on stdout / persists it in `metadata.json`.
        #[arg(long, conflicts_with = "detach")]
        background: bool,

        /// Borrow a host PCI device (typically a GPU) for the session.
        ///
        /// Accepts `auto` (M2 Phase 3 — picks the first clean GPU IOMMU
        /// group) or a literal `DDDD:BB:DD.F` PCI address. Phase 1
        /// validates the address and runs the host-side gate
        /// (`HostPreCheck`) but does not yet perform the driver detach
        /// or domain-XML attach.
        #[arg(long, value_name = "auto|BDF")]
        gpu: Option<String>,

        /// Session memory in MiB. Overrides `[vm] memory_mib` in
        /// `config.toml`. Also reads `TARTARUS_MEMORY`.
        #[arg(long, value_name = "MIB", env = "TARTARUS_MEMORY")]
        memory: Option<u32>,

        /// Session vCPU count. Overrides `[vm] vcpus` in
        /// `config.toml`. Also reads `TARTARUS_VCPUS`.
        #[arg(long, value_name = "COUNT", env = "TARTARUS_VCPUS")]
        vcpus: Option<u32>,

        /// Run this single session under `qemu:///system` (root libvirtd)
        /// instead of `qemu:///session`.
        ///
        /// Required when GPU passthrough's VFIO group nodes are not
        /// readable by the invoking user (the default M2 path expects
        /// `tartarus host setup-gpu` to have installed udev rules).
        /// Documented as a weaker isolation posture; only set this when
        /// you understand the implication.
        #[arg(long)]
        privileged_libvirt: bool,
    },

    /// Inspect or recover host-wide Tartarus state (GPU device borrows,
    /// future host-level configuration). All sub-commands are
    /// host-affecting; none of them touch a specific session.
    #[command(subcommand)]
    Host(HostCommand),

    /// Attach to a running session over SSH.
    ///
    /// Use `tartarus ssh <alias|uuid>` for an interactive shell.
    /// Anything after the literal `--` separator is forwarded
    /// verbatim to `ssh`, e.g. `tartarus ssh foo -- -L
    /// 8080:localhost:8080` for a port forward.
    Ssh {
        /// Alias or UUID identifying the session.
        target: String,

        /// Verbatim arguments forwarded to `ssh`. Everything after
        /// `--` lands here.
        #[arg(last = true)]
        ssh_args: Vec<String>,
    },

    /// Shut a session down cleanly.
    Stop {
        /// Alias or UUID identifying the session.
        target: String,
    },

    /// Destroy a session: overlay, alias, and libvirt domain.
    Destroy {
        /// Alias or UUID identifying the session.
        target: String,
    },

    /// Update packages and Claude inside a session.
    Update {
        /// Alias or UUID identifying the session.
        target: String,
    },

    /// Change compute resources on an existing session.
    ///
    /// For stopped sessions the domain XML is regenerated with the
    /// new values. For running sessions the change is applied live
    /// (best-effort) and persisted for the next boot.
    Set {
        /// Alias or UUID identifying the session.
        target: String,

        /// New memory in MiB.
        #[arg(long, value_name = "MIB")]
        memory: Option<u32>,

        /// New vCPU count.
        #[arg(long, value_name = "COUNT")]
        vcpus: Option<u32>,
    },
}

/// `tartarus auth ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Bootstrap or refresh the config file.
    ///
    /// Bare `auth init` walks the GitHub + Anthropic flows. `auth init google`
    /// is the dedicated subcommand for the Vertex (Google Cloud) backend.
    Init {
        /// Overwrite an existing `config.toml` instead of refusing.
        #[arg(long, global = true)]
        force: bool,

        /// Optional flow selector. When omitted, the GitHub + Anthropic
        /// interactive flow runs.
        #[command(subcommand)]
        flow: Option<AuthInitCommand>,
    },

    /// Print which credentials are configured (redacted).
    Status,

    /// Replace one or more credentials in place.
    Rotate,
}

/// Flow selector for `tartarus auth init`.
#[derive(Debug, Subcommand)]
pub enum AuthInitCommand {
    /// Bootstrap the Vertex (Google Cloud) credential bundle.
    Google,
}

/// `tartarus base ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum BaseCommand {
    /// Fetch, verify, and apply the Tartarus layer to the latest base image.
    Pull {
        /// Override the Fedora release pulled (e.g. `41`). Defaults to
        /// [`crate::disk::base::DEFAULT_FEDORA_RELEASE`].
        #[arg(long)]
        release: Option<String>,
    },

    /// List available bases and the current pointer.
    List,

    /// Delete unreferenced base images.
    Prune {
        /// Print what would be deleted without touching disk.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `tartarus env ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum EnvCommand {
    /// Install a programming environment into a session (idempotent).
    Add {
        /// Alias or UUID identifying the session.
        target: String,

        /// Environment name (e.g. `rust`, `go`, `python`).
        name: String,
    },

    /// Update installed programming environments in a session (idempotent).
    Update {
        /// Alias or UUID identifying the session.
        target: String,
    },
}

/// `tartarus host ...` subcommands. Host-wide state (GPU device borrows,
/// future host config); never operates on a specific session.
#[derive(Debug, Subcommand)]
pub enum HostCommand {
    /// Manage host-side GPU device borrows.
    #[command(subcommand)]
    Gpu(HostGpuCommand),
}

/// `tartarus host gpu ...` subcommands.
#[derive(Debug, Subcommand)]
pub enum HostGpuCommand {
    /// Print the udev rule that lets the invoking user open
    /// `/dev/vfio/<group>` so `qemu:///session` can use VFIO without
    /// `--privileged-libvirt`.
    ///
    /// Tartarus does not write the rule itself (that requires root
    /// and Tartarus refuses to run as root); the output includes the
    /// exact `install` command to run as root.
    SetupGpu,

    /// List every GPU on the host with vendor/device names and
    /// IOMMU-group annotations.
    List,

    /// Print the host-side GPU pre-check report. With `--bdf`,
    /// includes the per-device IOMMU-group cleanliness check plus
    /// the device's vendor/device names from `pci.ids`.
    Status {
        /// Optional PCI address to inspect (e.g. `0000:01:00.0`).
        #[arg(long, value_name = "BDF")]
        bdf: Option<String>,
    },

    /// Force-release a borrowed device.
    ///
    /// Walks the session metadata index to find which session
    /// recorded the borrow, replays the saved [`crate::gpu::driver::Receipt`]
    /// against the kernel, and clears the borrow record. Used when a
    /// session crashed without releasing the device.
    Release {
        /// PCI address of the device to release (e.g. `0000:01:00.0`).
        bdf: String,
    },
}

/// Dispatch a parsed [`Cli`] to its handler.
pub fn run(cli: Cli, config: Option<Config>) -> Result<()> {
    match cli.command {
        Command::Auth(cmd) => dispatch_auth(cmd, config.as_ref()),
        Command::Base(cmd) => dispatch_base(cmd),
        Command::Env(cmd) => dispatch_env(cmd, config),
        Command::Destroy { target } => dispatch_destroy(config, &target),
        Command::Doctor => dispatch_doctor(config.as_ref()),
        Command::Grow { target } => dispatch_grow(config, &target),
        Command::List => dispatch_list(config),
        Command::Rename { uuid, name } => dispatch_rename(&uuid, &name),
        Command::Resume { target } => dispatch_resume(config, &target),
        Command::Host(cmd) => dispatch_host(cmd),
        Command::Run {
            repo,
            default_repo,
            ephemeral,
            name,
            detach,
            background,
            gpu,
            privileged_libvirt,
            ..
        } => dispatch_run(
            config,
            crate::session::run::RunRequest {
                background,
                default_repo,
                detach,
                ephemeral,
                gpu,
                name,
                privileged_libvirt,
                repos: repo,
            },
        ),
        Command::Set { target, memory, vcpus } => dispatch_set(config, &target, memory, vcpus),
        Command::Ssh { target, ssh_args } => dispatch_ssh(config, target, ssh_args),
        Command::Stop { target } => dispatch_stop(config, &target),
        Command::Update { target } => dispatch_update(config, &target),
    }
}

/// Dispatch the `tartarus ssh <alias|uuid> [-- <args>]` subcommand.
fn dispatch_ssh(config: Option<Config>, target: String, ssh_args: Vec<String>) -> Result<()> {
    let config = require_config(config)?;
    let request = crate::session::ssh_attach::AttachRequest {
        target,
        trailing_ssh_args: ssh_args,
    };
    let outcome = crate::session::ssh_attach::run(&config, &request)?;
    tracing::info!(uuid = %outcome.uuid, port = outcome.host_port, "ssh attach finished");
    Ok(())
}

/// Dispatch a `tartarus host ...` subcommand.
fn dispatch_host(cmd: HostCommand) -> Result<()> {
    match cmd {
        HostCommand::Gpu(HostGpuCommand::List) => dispatch_host_gpu_list(),
        HostCommand::Gpu(HostGpuCommand::Release { bdf }) => dispatch_host_gpu_release(&bdf),
        HostCommand::Gpu(HostGpuCommand::SetupGpu) => dispatch_host_gpu_setup(),
        HostCommand::Gpu(HostGpuCommand::Status { bdf }) => dispatch_host_gpu_status(bdf.as_deref()),
    }
}

/// Render `tartarus host gpu setup-gpu`: print the udev rule plus
/// the `install` command the operator should run as root.
fn dispatch_host_gpu_setup() -> Result<()> {
    let user = crate::host_user::current()?;
    let rule = crate::gpu::setup::build_udev_rule(&user.username);

    println!("# udev rule for {user}", user = user.username);
    print!("{body}", body = rule.body);
    println!();
    println!("# install with:");
    println!(
        "#   sudo install -m 0644 -o root -g root /dev/stdin {path} <<'EOF'",
        path = rule.install_path.display(),
    );
    print!("{body}", body = rule.body);
    println!("# EOF");
    println!("#   sudo udevadm control --reload-rules");
    println!("#   sudo udevadm trigger /dev/vfio/*");

    Ok(())
}

/// Render `tartarus host gpu list` output to stdout.
///
/// One line per discovered GPU: PCI address, human label from
/// `pci.ids`, IOMMU group id, and a `[clean]` / `[shared with N]`
/// marker so the operator can spot at a glance which devices are
/// safely poolable.
fn dispatch_host_gpu_list() -> Result<()> {
    let gpus = crate::gpu::pci::list_gpus()?;

    if gpus.is_empty() {
        println!("no display-class PCI devices found");
        return Ok(());
    }

    for gpu in &gpus {
        let group = crate::gpu::iommu::group_for(&gpu.address).ok();
        let group_label = match group {
            Some(g) if g.is_clean_for_passthrough(&gpu.address) => {
                let id = g.id;
                format!("group={id} [clean]")
            },
            Some(g) => {
                let id = g.id;
                let shared = g.members.len() - 1;
                format!("group={id} [shared with {shared}]")
            },
            None => "group=?".to_owned(),
        };
        println!(
            "{addr}  {label}  {group}",
            addr = gpu.address,
            label = gpu.label(),
            group = group_label
        );
    }

    Ok(())
}

/// Run [`crate::gpu::driver::release_with_receipt`] manually for a
/// device a crashed session left borrowed.
fn dispatch_host_gpu_release(bdf: &str) -> Result<()> {
    let address: crate::gpu::PciAddress = bdf.parse()?;
    let receipt = crate::session::gpu_index::lookup_receipt(&address)?;

    let io = crate::gpu::driver::KernelSysfs;
    crate::gpu::driver::release_with_receipt(&io, &receipt)?;
    crate::session::gpu_index::clear_receipt(&address)?;

    println!("released {address}");
    Ok(())
}

/// Render `tartarus host gpu status` output to stdout.
///
/// Calls [`crate::gpu::HostPreCheck::probe`] and prints one line per
/// check. With `bdf`, the IOMMU-group section also lists every device
/// in the target's group so the operator can decide whether passing it
/// through is safe.
fn dispatch_host_gpu_status(bdf: Option<&str>) -> Result<()> {
    let target = match bdf {
        Some(s) => Some(s.parse::<crate::gpu::PciAddress>()?),
        None => None,
    };

    let outcome = crate::gpu::HostPreCheck::probe(target.as_ref())?;

    if let Some(addr) = target.as_ref()
        && let Ok(device) = crate::gpu::PciDevice::at(addr.clone())
    {
        println!("device:              {}", device.label());
        println!("pci_address:         {addr}");
    }
    println!("iommu_enabled:       {}", outcome.iommu_enabled);
    println!("vfio_pci_loaded:     {}", outcome.vfio_pci_loaded);
    if let Some(group) = outcome.iommu_group.as_ref() {
        println!("iommu_group_id:      {}", group.id);
        println!("iommu_group_clean:   {}", outcome.iommu_group_clean.unwrap_or(false));
        println!("iommu_group_members:");
        for member in &group.members {
            let label = crate::gpu::PciDevice::at(member.clone())
                .map(|d| d.label())
                .unwrap_or_else(|_| String::new());
            if label.is_empty() {
                println!("  - {member}");
            } else {
                println!("  - {member}  {label}");
            }
        }
    }

    Ok(())
}

/// Dispatch the `tartarus set <alias|uuid>` subcommand.
fn dispatch_set(config: Option<Config>, target: &str, memory: Option<u32>, vcpus: Option<u32>) -> Result<()> {
    let config = require_config(config)?;
    let request = crate::session::set::SetRequest { memory, vcpus };
    let outcome = crate::session::set::run(&config, target, &request)?;
    println!(
        "session {uuid} updated: memory={mem}M vcpus={cpu}",
        uuid = outcome.uuid,
        mem = outcome.memory_mib,
        cpu = outcome.vcpus,
    );
    Ok(())
}

/// Dispatch the `tartarus update <alias|uuid>` subcommand.
fn dispatch_update(config: Option<Config>, target: &str) -> Result<()> {
    let config = require_config(config)?;

    let outcome = crate::session::update::run(&config, target)?;
    tracing::info!(uuid = %outcome.uuid, mode = ?outcome.mode, "update complete");
    println!("session {uuid} updated", uuid = outcome.uuid);

    Ok(())
}

/// Build a [`CliOverrides`] from the parsed [`Cli`].
pub fn cli_overrides(cli: &Cli) -> CliOverrides {
    let mut overrides = CliOverrides::default();

    if let Command::Run {
        env,
        repo,
        default_repo,
        memory,
        vcpus,
        ..
    } = &cli.command
    {
        overrides.vm_memory_mib = *memory;
        overrides.vm_vcpus = *vcpus;
        if !env.is_empty() {
            overrides.base_envs = Some(env.clone());
        }
        if !repo.is_empty() {
            overrides.base_repos = Some(
                repo.iter()
                    .map(|slug| crate::config::RepoEntry {
                        default: false,
                        slug: slug.clone(),
                    })
                    .collect(),
            );
        }
        if let Some(slug) = default_repo {
            overrides.base_default_repo = Some(slug.clone());
        }
    }

    overrides
}

/// Whether a subcommand can run without a successful config load.
pub fn tolerates_missing_config(cli: &Cli) -> bool {
    matches!(
        &cli.command,
        Command::Auth(AuthCommand::Init { .. } | AuthCommand::Status) | Command::Doctor,
    )
}

// -----------------------------------------------------------------------------
// Command Dispatch
// -----------------------------------------------------------------------------

/// Dispatch a parsed [`AuthCommand`].
fn dispatch_auth(cmd: AuthCommand, config: Option<&Config>) -> Result<()> {
    match cmd {
        AuthCommand::Init { flow: None, force } => crate::auth::run_init(force),
        AuthCommand::Init {
            flow: Some(AuthInitCommand::Google),
            force: _,
        } => crate::auth::run_init_google(),
        AuthCommand::Rotate => stub("auth rotate", None),
        AuthCommand::Status => {
            let path = crate::paths::config_file()?;
            let file = crate::auth::load_file_config_optional(&path)?;
            crate::auth::run_status(config, file.as_ref())
        },
    }
}

/// Dispatch the `tartarus doctor` subcommand.
fn dispatch_doctor(config: Option<&Config>) -> Result<()> {
    let owned_default;
    let resolved = match config {
        Some(c) => c,
        None => {
            owned_default = crate::config::Config::resolve(
                crate::config::FileConfig::default(),
                crate::config::CliOverrides::default(),
            );
            &owned_default
        },
    };

    let failures = crate::doctor::run(resolved)?;

    if failures == 0 {
        Ok(())
    } else {
        Err(Error::DoctorFailures(failures))
    }
}

/// Dispatch a parsed [`BaseCommand`].
fn dispatch_base(cmd: BaseCommand) -> Result<()> {
    match cmd {
        BaseCommand::List => {
            let library = crate::disk::base::list()?;
            print!("{}", crate::disk::base::render_list(&library));
            Ok(())
        },
        BaseCommand::Prune { dry_run } => {
            let rendered = crate::disk::base::prune(dry_run)?;
            print!("{rendered}");
            Ok(())
        },
        BaseCommand::Pull { release } => {
            let release = release.unwrap_or_else(|| crate::disk::base::DEFAULT_FEDORA_RELEASE.to_owned());
            let base = crate::disk::base::pull(&release)?;
            tracing::info!(name = %base.name, "base pull complete");
            println!("base/current -> {name}", name = base.name);
            Ok(())
        },
    }
}

/// Dispatch the `tartarus run` subcommand.
fn dispatch_run(config: Option<Config>, request: crate::session::run::RunRequest) -> Result<()> {
    let config = require_config(config)?;
    config.validate_for_run()?;

    let outcome = crate::session::run::run(&config, &request)?;

    let label = outcome.alias.as_deref().unwrap_or("(unnamed)");
    println!("session {uuid} started ({label})", uuid = outcome.uuid);

    Ok(())
}

/// Dispatch the `tartarus grow <alias|uuid>` subcommand.
fn dispatch_grow(config: Option<Config>, target: &str) -> Result<()> {
    let config = require_config(config)?;

    let outcome = crate::disk::grow::run(&config, target)?;
    tracing::info!(
        uuid = %outcome.uuid,
        before_gib = outcome.before_gib,
        after_gib = outcome.after_gib,
        marker_was_present = outcome.marker_was_present,
        "grow complete",
    );
    println!(
        "session {uuid}: grown {before}G -> {after}G",
        uuid = outcome.uuid,
        before = outcome.before_gib,
        after = outcome.after_gib,
    );

    Ok(())
}

/// Dispatch the `tartarus resume <alias|uuid>` subcommand.
fn dispatch_resume(config: Option<Config>, target: &str) -> Result<()> {
    let config = require_config(config)?;

    let outcome = crate::session::resume::run(&config, target)?;
    tracing::info!(uuid = %outcome.uuid, started = outcome.started_from_shutoff, "resume complete");

    Ok(())
}

/// Require a config, surfacing `ConfigError::NotFound` when absent.
fn require_config(config: Option<Config>) -> Result<Config> {
    config.ok_or_else(|| {
        Error::Config(crate::config::ConfigError::NotFound {
            path: crate::paths::config_file().unwrap_or_default(),
        })
    })
}

/// Dispatch the `tartarus destroy <alias|uuid>` subcommand.
fn dispatch_destroy(config: Option<Config>, target: &str) -> Result<()> {
    let config = require_config(config)?;
    let outcome = crate::session::destroy::run(&config, target)?;
    println!("session {uuid} destroyed", uuid = outcome.uuid);
    Ok(())
}

/// Dispatch the `tartarus list` subcommand.
fn dispatch_list(config: Option<Config>) -> Result<()> {
    let config = require_config(config)?;
    let table = crate::session::list::run(&config)?;
    print!("{table}");
    Ok(())
}

/// Dispatch the `tartarus rename <uuid> <name>` subcommand.
fn dispatch_rename(uuid: &str, alias: &str) -> Result<()> {
    let outcome = crate::session::rename::run(uuid, alias)?;
    println!("alias '{alias}' -> {uuid}", alias = outcome.alias, uuid = outcome.uuid,);
    Ok(())
}

/// Dispatch the `tartarus stop <alias|uuid>` subcommand.
fn dispatch_stop(config: Option<Config>, target: &str) -> Result<()> {
    let config = require_config(config)?;
    let outcome = crate::session::stop::run(&config, target)?;
    if outcome.force_stopped {
        println!(
            "session {name} force-stopped (graceful shutdown timed out)",
            name = outcome.name,
        );
    } else {
        println!("session {name} stopped", name = outcome.name);
    }
    Ok(())
}

/// Dispatch a parsed [`EnvCommand`].
fn dispatch_env(cmd: EnvCommand, config: Option<Config>) -> Result<()> {
    let config = require_config(config)?;
    match cmd {
        EnvCommand::Add { target, name } => {
            let outcome = crate::session::env::add(&config, &target, &name)?;
            tracing::info!(uuid = %outcome.uuid, env = %outcome.env, "env add complete");
            println!(
                "session {uuid}: env {env} ready",
                uuid = outcome.uuid,
                env = outcome.env,
            );
            Ok(())
        },
        EnvCommand::Update { target } => {
            let outcome = crate::session::env::update(&config, &target)?;
            tracing::info!(uuid = %outcome.uuid, "env update complete");
            println!("session {uuid}: envs updated", uuid = outcome.uuid);
            Ok(())
        },
    }
}

/// Log and return [`Error::NotImplemented`] for stub commands.
fn stub(label: &'static str, arg: Option<&str>) -> Result<()> {
    match arg {
        Some(arg) => tracing::info!(command = label, arg, "stub command invoked"),
        None => tracing::info!(command = label, "stub command invoked"),
    }

    Err(Error::NotImplemented(label))
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_subcommand_parses_minimum_args() {
        let cli = Cli::try_parse_from(["tartarus", "run", "--repo", "owner/name"]).expect("run --repo should parse");

        match cli.command {
            Command::Run { repo, ephemeral, .. } => {
                assert_eq!(
                    repo,
                    vec!["owner/name".to_owned()],
                    "repo should round-trip into a list"
                );
                assert!(!ephemeral, "ephemeral should default to false");
            },
            other => panic!("expected Command::Run, got {other:?}"),
        }
    }

    #[test]
    fn run_subcommand_accepts_repeated_repo_flags() {
        let cli = Cli::try_parse_from([
            "tartarus",
            "run",
            "--repo",
            "owner/one",
            "--repo",
            "owner/two",
            "--default-repo",
            "owner/two",
        ])
        .expect("repeated --repo should parse");

        match cli.command {
            Command::Run { repo, default_repo, .. } => {
                assert_eq!(
                    repo,
                    vec!["owner/one".to_owned(), "owner/two".to_owned()],
                    "repeated --repo should round-trip in order",
                );
                assert_eq!(
                    default_repo.as_deref(),
                    Some("owner/two"),
                    "--default-repo should round-trip",
                );
            },
            other => panic!("expected Command::Run, got {other:?}"),
        }
    }

    #[test]
    fn cli_overrides_picks_up_repos_and_default_repo() {
        let cli = Cli::try_parse_from([
            "tartarus",
            "run",
            "--repo",
            "owner/one",
            "--repo",
            "owner/two",
            "--default-repo",
            "owner/two",
        ])
        .expect("multi-repo run should parse");

        let overrides = cli_overrides(&cli);

        let repos = overrides.base_repos.expect("repos override should be populated");
        assert_eq!(
            repos.iter().map(|r| r.slug.as_str()).collect::<Vec<_>>(),
            vec!["owner/one", "owner/two"],
            "CLI repos should round-trip into base_repos in order",
        );
        assert!(
            repos.iter().all(|r| !r.default),
            "CLI repos do not carry a default flag — that is selected via --default-repo",
        );
        assert_eq!(
            overrides.base_default_repo.as_deref(),
            Some("owner/two"),
            "--default-repo should populate base_default_repo",
        );
    }

    #[test]
    fn quiet_and_verbose_are_mutually_exclusive() {
        let result = Cli::try_parse_from(["tartarus", "--quiet", "-v", "list"]);

        assert!(result.is_err(), "--quiet and -v should be rejected together",);
    }

    #[test]
    fn verbose_count_maps_to_verbosity() {
        let cli = Cli::try_parse_from(["tartarus", "-vv", "list"]).expect("-vv list should parse");

        assert_eq!(cli.verbosity(), Verbosity::Debug, "-vv should map to Debug");
    }

    #[test]
    fn auth_init_google_is_a_distinct_subcommand() {
        let cli = Cli::try_parse_from(["tartarus", "auth", "init", "google"]).expect("auth init google should parse");

        match cli.command {
            Command::Auth(AuthCommand::Init {
                flow: Some(AuthInitCommand::Google),
                ..
            }) => {},
            other => panic!("expected auth init google, got {other:?}"),
        }
    }

    #[test]
    fn auth_init_with_no_flow_uses_default() {
        let cli = Cli::try_parse_from(["tartarus", "auth", "init"]).expect("auth init (no flow) should parse");

        match cli.command {
            Command::Auth(AuthCommand::Init {
                flow: None,
                force: false,
            }) => {},
            other => panic!("expected bare auth init, got {other:?}"),
        }
    }

    #[test]
    fn auth_init_force_flag_is_recognised() {
        let cli = Cli::try_parse_from(["tartarus", "auth", "init", "--force"]).expect("auth init --force should parse");

        match cli.command {
            Command::Auth(AuthCommand::Init {
                flow: None,
                force: true,
            }) => {},
            other => panic!("expected auth init with force, got {other:?}"),
        }
    }

    #[test]
    fn run_dispatch_for_resume_requires_config() {
        let cli = Cli::try_parse_from(["tartarus", "resume", "alpha"]).expect("resume should parse");

        let err = run(cli, None).expect_err("resume without a config should fail");

        match err {
            Error::Config(crate::config::ConfigError::NotFound { .. }) => {},
            other => panic!("expected Config::NotFound for resume without a config, got {other:?}"),
        }
    }

    #[test]
    fn run_subcommand_parses_detach_flag() {
        let cli = Cli::try_parse_from(["tartarus", "run", "--repo", "owner/name", "--detach"])
            .expect("--detach should parse");

        match cli.command {
            Command::Run { detach, background, .. } => {
                assert!(detach, "--detach should round-trip into the flag");
                assert!(!background, "--background should remain false");
            },
            other => panic!("expected Command::Run, got {other:?}"),
        }
    }

    #[test]
    fn run_subcommand_rejects_detach_with_background() {
        let result = Cli::try_parse_from(["tartarus", "run", "--repo", "owner/name", "--detach", "--background"]);

        assert!(
            result.is_err(),
            "--detach and --background must be mutually exclusive at the clap level",
        );
    }

    #[test]
    fn cli_overrides_picks_up_run_envs() {
        let cli = Cli::try_parse_from(["tartarus", "run", "--repo", "owner/name", "--env", "rust,python"])
            .expect("run --env list should parse");

        let overrides = cli_overrides(&cli);

        assert_eq!(
            overrides.base_envs,
            Some(vec!["rust".to_owned(), "python".to_owned()]),
            "comma-separated --env should populate base_envs",
        );
    }

    #[test]
    fn cli_overrides_is_empty_for_non_run_commands() {
        let cli = Cli::try_parse_from(["tartarus", "list"]).expect("list should parse");

        let overrides = cli_overrides(&cli);

        assert_eq!(
            overrides,
            CliOverrides::default(),
            "non-run commands should not contribute overrides",
        );
    }

    #[test]
    fn grow_subcommand_parses_target() {
        let cli = Cli::try_parse_from(["tartarus", "grow", "alpha"]).expect("grow target should parse");

        match cli.command {
            Command::Grow { target } => {
                assert_eq!(target, "alpha", "grow should round-trip the target");
            },
            other => panic!("expected Command::Grow, got {other:?}"),
        }
    }

    #[test]
    fn grow_requires_config() {
        let cli = Cli::try_parse_from(["tartarus", "grow", "alpha"]).expect("grow should parse");

        let err = run(cli, None).expect_err("grow without a config should fail");

        match err {
            Error::Config(crate::config::ConfigError::NotFound { .. }) => {},
            other => panic!("expected Config::NotFound for grow without a config, got {other:?}"),
        }
    }

    #[test]
    fn env_add_parses_target_and_env_name() {
        let cli =
            Cli::try_parse_from(["tartarus", "env", "add", "alpha", "rust"]).expect("env add target env should parse");

        match cli.command {
            Command::Env(EnvCommand::Add { target, name }) => {
                assert_eq!(target, "alpha", "env add should round-trip the target");
                assert_eq!(name, "rust", "env add should round-trip the env name");
            },
            other => panic!("expected Command::Env(Add), got {other:?}"),
        }
    }

    #[test]
    fn env_update_parses_target() {
        let cli = Cli::try_parse_from(["tartarus", "env", "update", "alpha"]).expect("env update target should parse");

        match cli.command {
            Command::Env(EnvCommand::Update { target }) => {
                assert_eq!(target, "alpha", "env update should round-trip the target");
            },
            other => panic!("expected Command::Env(Update), got {other:?}"),
        }
    }

    #[test]
    fn env_add_with_unknown_env_is_rejected_before_libvirt() {
        let cli =
            Cli::try_parse_from(["tartarus", "env", "add", "alpha", "haskell"]).expect("env add bogus should parse");

        let err = run(cli, None).expect_err("env add bogus should fail");

        match err {
            Error::Config(crate::config::ConfigError::NotFound { .. }) => {},
            Error::Session(crate::session::error::SessionError::UnknownEnv { env, .. }) => {
                assert_eq!(env, "haskell", "the rejected env name should round-trip into the error");
            },
            other => panic!("expected Config::NotFound or Session::UnknownEnv, got {other:?}"),
        }
    }

    #[test]
    fn env_add_requires_config() {
        let cli = Cli::try_parse_from(["tartarus", "env", "add", "alpha", "rust"]).expect("env add should parse");

        let err = run(cli, None).expect_err("env add without a config should fail");

        match err {
            Error::Config(crate::config::ConfigError::NotFound { .. }) => {},
            other => panic!("expected Config::NotFound for env add without a config, got {other:?}"),
        }
    }

    #[test]
    fn tolerates_missing_config_for_bootstrap_and_diagnostic_commands() {
        let init = Cli::try_parse_from(["tartarus", "auth", "init"]).expect("auth init should parse");
        let init_google =
            Cli::try_parse_from(["tartarus", "auth", "init", "google"]).expect("auth init google should parse");
        let status = Cli::try_parse_from(["tartarus", "auth", "status"]).expect("auth status should parse");
        let doctor = Cli::try_parse_from(["tartarus", "doctor"]).expect("doctor should parse");
        let list = Cli::try_parse_from(["tartarus", "list"]).expect("list should parse");

        assert!(
            tolerates_missing_config(&init),
            "auth init should tolerate a missing config"
        );
        assert!(
            tolerates_missing_config(&init_google),
            "auth init google should tolerate a missing config",
        );
        assert!(
            tolerates_missing_config(&status),
            "auth status should report `not configured` rather than fail on a fresh install",
        );
        assert!(
            tolerates_missing_config(&doctor),
            "doctor should run on a fresh install before the first auth init",
        );
        assert!(!tolerates_missing_config(&list), "list should require a config");
    }
}
