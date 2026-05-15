//! [`SessionProvider`]: trait every backend (libvirt, Hetzner) implements.
//!
//! The trait carries the per-session lifecycle entry points the CLI
//! dispatches into. Provider-agnostic request and outcome types live
//! alongside it so multiple impls can share a single CLI surface.
//!
//! Each impl picks its own `Error` type; the binary's top-level
//! `Error` provides `From` impls for the variants it understands.

use crate::session::run_mode::RunMode;

// -----------------------------------------------------------------------------
// RunRequest
// -----------------------------------------------------------------------------

/// Caller-supplied parameters for [`SessionProvider::run`].
///
/// Carries the union of fields every provider might need; impls
/// silently ignore fields they cannot honour (e.g. Hetzner has no
/// libvirt URI, so the libvirt-flavoured `privileged_libvirt` field
/// is meaningless there).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunRequest {
    /// Optional alias the user passed via `--name`.
    pub name: Option<String>,

    /// Background mode flag (mutually exclusive with `detach`).
    pub background: bool,

    /// Slug to promote to the session's default repo.
    pub default_repo: Option<String>,

    /// Detach mode flag (mutually exclusive with `background`).
    pub detach: bool,

    /// `--ephemeral`: delete overlay on session destroy.
    pub ephemeral: bool,

    /// GPU passthrough request from `--gpu` (`None`, `"auto"`, or a
    /// BDF). Libvirt-only.
    pub gpu: Option<String>,

    /// `--privileged-libvirt`: use `qemu:///system` instead of
    /// `qemu:///session` (weaker isolation). Libvirt-only.
    pub privileged_libvirt: bool,

    /// Repository slugs to clone at first boot (empty = use config).
    pub repos: Vec<String>,
}

impl RunRequest {
    /// Resolve the [`RunMode`] from the parsed flag pair.
    pub fn run_mode(&self) -> RunMode {
        RunMode::from_flags(self.detach, self.background)
    }
}

// -----------------------------------------------------------------------------
// RunOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`SessionProvider::run`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutcome {
    /// Alias from `--name`, if set.
    pub alias: Option<String>,

    /// Run mode the session was started in.
    pub mode: RunMode,

    /// Remote-connect URL for background mode, else `None`.
    pub remote_url: Option<String>,

    /// Session UUID.
    pub uuid: String,
}

// -----------------------------------------------------------------------------
// ResumeOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`SessionProvider::resume`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeOutcome {
    /// Whether the session was actually started (vs. already running).
    pub started_from_shutoff: bool,

    /// Session UUID that was resumed.
    pub uuid: String,
}

// -----------------------------------------------------------------------------
// StopOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`SessionProvider::stop`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StopOutcome {
    /// True when graceful shutdown timed out and force-destroy was used.
    pub force_stopped: bool,

    /// Session identifier for the success message (alias or UUID).
    pub name: String,
}

// -----------------------------------------------------------------------------
// DestroyOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`SessionProvider::destroy`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestroyOutcome {
    /// UUID of the destroyed session.
    pub uuid: String,
}

// -----------------------------------------------------------------------------
// ListEntry
// -----------------------------------------------------------------------------

/// One row of the session list table.
///
/// All numeric fields are already rendered as display strings (with
/// `?` placeholders when unknown) so that the binary's formatter
/// stays provider-agnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListEntry {
    /// Alias when set, else `(unnamed)`.
    pub alias: String,

    /// Base image filename (libvirt) or image slug (Hetzner).
    pub base: String,

    /// vCPU count as a string, or `?` if unknown.
    pub cpu: String,

    /// Comma-joined envs (e.g. `rust,go,python`).
    pub envs: String,

    /// Memory as `<mib>M`, or `?` if unknown.
    pub mem: String,

    /// `yes` / `no` rendering of the persist flag.
    pub persist: String,

    /// Overlay/volume virtual size as `<gib>G`, or `?` if unknown.
    pub size: String,

    /// Provider-specific status string (libvirt domain state /
    /// Hetzner server state).
    pub status: String,

    /// First 8 chars of the UUID.
    pub uuid_short: String,
}

// -----------------------------------------------------------------------------
// RenameOutcome
// -----------------------------------------------------------------------------

/// Outcome of a successful [`SessionProvider::rename`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameOutcome {
    /// Alias the session is now reachable as.
    pub alias: String,

    /// UUID the alias resolves to.
    pub uuid: String,
}

// -----------------------------------------------------------------------------
// SessionProvider
// -----------------------------------------------------------------------------

/// Per-backend session-lifecycle entry points.
///
/// The CLI binary builds a concrete provider (today, only
/// `tartarus_libvirt::LibvirtProvider`) and dispatches into it via
/// this trait. Each impl picks its own [`SessionProvider::Error`];
/// the binary's `Error` flattens those at the boundary.
pub trait SessionProvider {
    /// Error type the impl raises out of its lifecycle methods.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Create + start a fresh session.
    ///
    /// Equivalent to `tartarus run` at the CLI surface.
    fn run(&self, request: &RunRequest) -> std::result::Result<RunOutcome, Self::Error>;

    /// Re-attach to an existing session, starting it if shut off.
    ///
    /// Equivalent to `tartarus resume <alias|uuid>`.
    fn resume(&self, target: &str) -> std::result::Result<ResumeOutcome, Self::Error>;

    /// Stop a running session gracefully, force-killing if needed.
    ///
    /// Equivalent to `tartarus stop <alias|uuid>`. Metadata and
    /// overlay survive on disk.
    fn stop(&self, target: &str) -> std::result::Result<StopOutcome, Self::Error>;

    /// Tear down a session entirely: domain, overlay, alias, dir.
    ///
    /// Equivalent to `tartarus destroy <alias|uuid>`.
    fn destroy(&self, target: &str) -> std::result::Result<DestroyOutcome, Self::Error>;

    /// Enumerate every session known to this provider.
    ///
    /// Equivalent to `tartarus list`. Rendering the table is the
    /// binary's responsibility.
    fn list(&self) -> std::result::Result<Vec<ListEntry>, Self::Error>;

    /// Create or move a session alias symlink.
    ///
    /// Equivalent to `tartarus rename <uuid> <name>`. Refuses when
    /// `alias` is already pointed at a different UUID.
    fn rename(&self, uuid: &str, alias: &str) -> std::result::Result<RenameOutcome, Self::Error>;
}
