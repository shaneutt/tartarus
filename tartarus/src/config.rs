//! Configuration: deserialise, merge, validate.
//!
//! Precedence: CLI flags > env vars > `config.toml` > built-in defaults.
//! See [`docs/spec.md`] for the full schema.
//!
//! [`docs/spec.md`]: https://github.com/the-lost-art-of-programming/tartarus/blob/main/docs/spec.md

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{error::Result, paths};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Mask for standard Unix permission bits (owner/group/other).
#[cfg(unix)]
const PERMISSION_MASK: u32 = 0o777;

/// Required mode for the config file (holds API keys and PATs).
#[cfg(unix)]
const REQUIRED_MODE: u32 = 0o600;

/// Default Claude model.
const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-4-7";

/// Default Claude effort tier.
const DEFAULT_CLAUDE_EFFORT: &str = "high";

/// Default programming environments installed at first boot.
const DEFAULT_BASE_ENVS: &[&str] = &["rust", "go", "python"];

/// Default `rustup` components.
const DEFAULT_RUST_COMPONENTS: &[&str] = &["rustfmt", "clippy", "rust-analyzer", "llvm-tools-preview", "rust-src"];

/// Default `rustup` toolchains.
const DEFAULT_RUST_TOOLCHAINS: &[&str] = &["stable", "nightly"];

/// Default cargo-installed tools.
const DEFAULT_RUST_CARGO_TOOLS: &[&str] = &[
    "cargo-audit",
    "cargo-deny",
    "cargo-llvm-cov",
    "cargo-fuzz",
    "cargo-nextest",
];

/// Default libvirt URI (local-only in MVP).
const DEFAULT_NETWORK_URI: &str = "qemu:///session";

/// Default per-session overlay virtual size, in GiB (sparse).
const DEFAULT_DISK_VIRTUAL_SIZE_GIB: u32 = 100;

/// Auto-grow watermark, percentage of current virtual size.
const DEFAULT_DISK_GROW_THRESHOLD_PCT: u8 = 85;

/// Auto-grow increment in GiB per watermark trip.
const DEFAULT_DISK_GROW_INCREMENT_GIB: u32 = 100;

/// Minimum overlay virtual size, in GiB.
const MIN_DISK_VIRTUAL_SIZE_GIB: u32 = 8;

// ---------------------------------------------------------------------------
// Configuration Types
// ---------------------------------------------------------------------------

/// All configuration errors.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `bedrock` backend is not supported in MVP (milestone 2).
    #[error(
        "[claude] backend = \"bedrock\" is not supported in MVP; AWS Bedrock arrives in milestone 2. \
         Use `anthropic` or `vertex` for now."
    )]
    BedrockNotSupported,

    /// Config file is not valid TOML.
    #[error("config file at {path} is not valid TOML: {source}")]
    Deserialize {
        /// Path that failed to parse.
        path: PathBuf,

        /// Underlying TOML error.
        source: toml::de::Error,
    },

    /// Config file is more permissive than mode `0600`.
    #[error("config file at {path} has insecure mode {mode:#o}; required mode is 0600")]
    InsecurePermissions {
        /// Observed mode bits (owner/group/other).
        mode: u32,

        /// Path that was checked.
        path: PathBuf,
    },

    /// Semantic validation failure.
    #[error("invalid configuration: {0}")]
    Invalid(String),

    /// Selected backend is missing its required credentials.
    #[error("[claude] backend = \"{backend}\" but {missing} is not configured")]
    MissingBackendCredentials {
        /// Backend name.
        backend: String,

        /// Missing field description.
        missing: &'static str,
    },

    /// No GitHub token available for `tartarus run`.
    #[error("no GitHub credentials configured. hint: run `tartarus auth init` to set them up.")]
    MissingGithubToken,

    /// Config file does not exist.
    #[error("config file not found at {path}. hint: run `tartarus auth init` to create it.")]
    NotFound {
        /// Path that was checked.
        path: PathBuf,
    },
}

/// Claude backend selector.
///
/// MVP supports `Anthropic` and `Vertex`. `Bedrock` is recognised
/// at parse time so validation can reject it cleanly (milestone 2).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// Direct Anthropic API calls.
    Anthropic,

    /// AWS Bedrock. Not implemented in MVP.
    Bedrock,

    /// Google Cloud Vertex AI with a service-account file.
    Vertex,
}

/// One entry in `[base] repos`.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RepoEntry {
    /// Whether this entry is the session's default repo.
    #[serde(default)]
    pub default: bool,

    /// `owner/name` slug, e.g. `the-lost-art-of-programming/tartarus`.
    pub slug: String,
}

/// Resolved configuration consumed by the rest of the crate.
///
/// Produced by [`Config::resolve`]; all optional fields are filled
/// from defaults.
#[derive(Clone, Eq, PartialEq)]
pub struct Config {
    /// Default repo slug, or `None` (falls back to first listed).
    pub base_default_repo: Option<String>,

    /// Base-image environments installed at first boot.
    pub base_envs: Vec<String>,

    /// Repos from `[base]`, empty when none configured.
    pub base_repos: Vec<RepoEntry>,

    /// Selected Claude backend.
    pub claude_backend: Backend,

    /// Anthropic API key (when backend is [`Backend::Anthropic`]).
    pub claude_anthropic_api_key: Option<String>,

    /// Claude effort tier (e.g. `"low"`, `"high"`).
    pub claude_effort: String,

    /// Claude model identifier (e.g. `"claude-opus-4-7"`).
    pub claude_model: String,

    /// Vertex service-account JSON file path.
    pub claude_vertex_credentials_file: Option<PathBuf>,

    /// Vertex GCP project ID.
    pub claude_vertex_project_id: Option<String>,

    /// Vertex GCP region (e.g. `"us-east5"`).
    pub claude_vertex_region: Option<String>,

    /// Auto-grow increment in GiB.
    pub disk_grow_increment_gib: u32,

    /// Auto-grow watermark (percentage of virtual size).
    pub disk_grow_threshold_pct: u8,

    /// Per-session overlay virtual size, in GiB.
    pub disk_virtual_size_gib: u32,

    /// GitHub PAT for in-guest `gh` and `git clone`.
    pub github_token: Option<String>,

    /// libvirt connection URI.
    pub network_uri: String,

    /// Cargo-installed tools.
    pub rust_cargo_tools: Vec<String>,

    /// `rustup` components.
    pub rust_components: Vec<String>,

    /// `rustup` toolchains.
    pub rust_toolchains: Vec<String>,

    /// User GID inside the guest.
    pub user_gid: Option<u32>,

    /// User UID inside the guest.
    pub user_uid: Option<u32>,

    /// Username inside the guest.
    pub user_username: Option<String>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_default_repo", &self.base_default_repo)
            .field("base_envs", &self.base_envs)
            .field("base_repos", &self.base_repos)
            .field("claude_backend", &self.claude_backend)
            .field(
                "claude_anthropic_api_key",
                &self.claude_anthropic_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("claude_effort", &self.claude_effort)
            .field("claude_model", &self.claude_model)
            .field("claude_vertex_credentials_file", &self.claude_vertex_credentials_file)
            .field("claude_vertex_project_id", &self.claude_vertex_project_id)
            .field("claude_vertex_region", &self.claude_vertex_region)
            .field("disk_grow_increment_gib", &self.disk_grow_increment_gib)
            .field("disk_grow_threshold_pct", &self.disk_grow_threshold_pct)
            .field("disk_virtual_size_gib", &self.disk_virtual_size_gib)
            .field("github_token", &self.github_token.as_ref().map(|_| "[REDACTED]"))
            .field("network_uri", &self.network_uri)
            .field("rust_cargo_tools", &self.rust_cargo_tools)
            .field("rust_components", &self.rust_components)
            .field("rust_toolchains", &self.rust_toolchains)
            .field("user_gid", &self.user_gid)
            .field("user_uid", &self.user_uid)
            .field("user_username", &self.user_username)
            .finish()
    }
}

impl Config {
    /// Merge a [`FileConfig`] with [`CliOverrides`] into a resolved
    /// [`Config`], applying the CLI > file > default precedence.
    pub fn resolve(file: FileConfig, cli: CliOverrides) -> Self {
        let claude = file.claude;
        let anthropic = claude.anthropic;
        let vertex = claude.vertex;
        let disk = file.disk;
        let network = file.network;
        let rust = file.rust;
        let user = file.user;

        Self {
            base_default_repo: cli.base_default_repo.or(file.base.default_repo),
            base_envs: cli.base_envs.or(file.base.envs).unwrap_or_else(default_base_envs),
            base_repos: cli.base_repos.or(file.base.repos).unwrap_or_default(),
            claude_backend: cli.claude_backend.or(claude.backend).unwrap_or(Backend::Anthropic),
            claude_anthropic_api_key: cli.claude_anthropic_api_key.or(anthropic.api_key),
            claude_effort: cli
                .claude_effort
                .or(claude.effort)
                .unwrap_or_else(|| DEFAULT_CLAUDE_EFFORT.to_owned()),
            claude_model: cli
                .claude_model
                .or(claude.model)
                .unwrap_or_else(|| DEFAULT_CLAUDE_MODEL.to_owned()),
            claude_vertex_credentials_file: cli.claude_vertex_credentials_file.or(vertex.credentials_file),
            claude_vertex_project_id: cli.claude_vertex_project_id.or(vertex.project_id),
            claude_vertex_region: cli.claude_vertex_region.or(vertex.region),
            disk_grow_increment_gib: cli
                .disk_grow_increment_gib
                .or(disk.grow_increment_gib)
                .unwrap_or(DEFAULT_DISK_GROW_INCREMENT_GIB),
            disk_grow_threshold_pct: cli
                .disk_grow_threshold_pct
                .or(disk.grow_threshold_pct)
                .unwrap_or(DEFAULT_DISK_GROW_THRESHOLD_PCT),
            disk_virtual_size_gib: cli
                .disk_virtual_size_gib
                .or(disk.virtual_size_gib)
                .unwrap_or(DEFAULT_DISK_VIRTUAL_SIZE_GIB),
            github_token: cli.github_token.or(file.github.token),
            network_uri: cli
                .network_uri
                .or(network.uri)
                .unwrap_or_else(|| DEFAULT_NETWORK_URI.to_owned()),
            rust_cargo_tools: rust.cargo_tools.unwrap_or_else(default_rust_cargo_tools),
            rust_components: rust.components.unwrap_or_else(default_rust_components),
            rust_toolchains: rust.toolchains.unwrap_or_else(default_rust_toolchains),
            user_gid: user.gid,
            user_uid: user.uid,
            user_username: user.username,
        }
    }

    /// Validate cross-field invariants on the resolved config.
    pub fn validate(&self) -> Result<()> {
        if self.disk_virtual_size_gib < MIN_DISK_VIRTUAL_SIZE_GIB {
            return Err(ConfigError::Invalid(format!(
                "[disk] virtual_size_gib = {} is below the minimum of {MIN_DISK_VIRTUAL_SIZE_GIB} GiB",
                self.disk_virtual_size_gib,
            ))
            .into());
        }

        if self.disk_grow_threshold_pct == 0 || self.disk_grow_threshold_pct > 100 {
            return Err(ConfigError::Invalid(format!(
                "[disk] grow_threshold_pct = {} must be between 1 and 100",
                self.disk_grow_threshold_pct,
            ))
            .into());
        }

        if self.disk_grow_increment_gib == 0 {
            return Err(ConfigError::Invalid("[disk] grow_increment_gib must be a positive integer".to_owned()).into());
        }

        validate_user_identity(self)?;
        validate_backend_credentials(self)?;
        validate_repos(self)?;
        validate_seed_input_strings(self)?;

        Ok(())
    }

    /// Run-time validation: [`Self::validate`] plus checks that a
    /// GitHub token and backend credentials are present.
    pub fn validate_for_run(&self) -> Result<()> {
        self.validate()?;

        if self.github_token.as_deref().unwrap_or("").is_empty() {
            return Err(ConfigError::MissingGithubToken.into());
        }

        Ok(())
    }
}

/// Top-level shape of `config.toml`. All sections and fields are
/// optional; [`Config::resolve`] supplies defaults.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    /// `[base]` section.
    pub base: BaseSection,

    /// `[claude]` section.
    pub claude: ClaudeSection,

    /// `[disk]` section.
    pub disk: DiskSection,

    /// `[github]` section.
    pub github: GithubSection,

    /// `[network]` section.
    pub network: NetworkSection,

    /// `[rust]` section.
    pub rust: RustSection,

    /// `[user]` section.
    pub user: UserSection,
}

impl fmt::Debug for FileConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileConfig")
            .field("base", &self.base)
            .field("claude", &self.claude)
            .field("disk", &self.disk)
            .field("github", &self.github)
            .field("network", &self.network)
            .field("rust", &self.rust)
            .field("user", &self.user)
            .finish()
    }
}

/// `[github]` config section.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GithubSection {
    /// GitHub personal access token (`ghp_...` or `github_pat_...`).
    pub token: Option<String>,
}

impl fmt::Debug for GithubSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GithubSection")
            .field("token", &self.token.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// `[claude]` config section.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeSection {
    /// `[claude.anthropic]` sub-section.
    pub anthropic: ClaudeAnthropicSection,

    /// Backend selector.
    pub backend: Option<Backend>,

    /// Effort tier (e.g. `"low"`, `"high"`).
    pub effort: Option<String>,

    /// Model identifier (e.g. `"claude-opus-4-7"`).
    pub model: Option<String>,

    /// `[claude.vertex]` sub-section.
    pub vertex: ClaudeVertexSection,
}

impl fmt::Debug for ClaudeSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeSection")
            .field("anthropic", &self.anthropic)
            .field("backend", &self.backend)
            .field("effort", &self.effort)
            .field("model", &self.model)
            .field("vertex", &self.vertex)
            .finish()
    }
}

/// `[claude.anthropic]` config section.
#[derive(Clone, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeAnthropicSection {
    /// Anthropic API key (`sk-ant-...`).
    pub api_key: Option<String>,
}

impl fmt::Debug for ClaudeAnthropicSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeAnthropicSection")
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

/// `[claude.vertex]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClaudeVertexSection {
    /// Service-account JSON file path.
    pub credentials_file: Option<PathBuf>,

    /// GCP project ID.
    pub project_id: Option<String>,

    /// GCP region (e.g. `"us-east5"`).
    pub region: Option<String>,
}

/// `[base]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BaseSection {
    /// Slug override for the default repo.
    pub default_repo: Option<String>,

    /// Programming environments installed at first boot.
    pub envs: Option<Vec<String>>,

    /// Repos cloned at first boot. At most one may be `default = true`.
    pub repos: Option<Vec<RepoEntry>>,
}

/// `[rust]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RustSection {
    /// Cargo-installed binaries.
    pub cargo_tools: Option<Vec<String>>,

    /// `rustup` components.
    pub components: Option<Vec<String>>,

    /// `rustup` toolchains.
    pub toolchains: Option<Vec<String>>,
}

/// `[user]` config section: optional overrides for the in-guest identity.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UserSection {
    /// Override GID inside the guest.
    pub gid: Option<u32>,

    /// Override UID inside the guest.
    pub uid: Option<u32>,

    /// Override username inside the guest.
    pub username: Option<String>,
}

/// `[disk]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiskSection {
    /// Auto-grow increment, in GiB.
    pub grow_increment_gib: Option<u32>,

    /// Auto-grow watermark, percentage of current virtual size (1..=100).
    pub grow_threshold_pct: Option<u8>,

    /// Per-session overlay virtual size, in GiB.
    pub virtual_size_gib: Option<u32>,
}

/// `[network]` config section.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkSection {
    /// libvirt connection URI.
    pub uri: Option<String>,
}

/// CLI / env overrides folded into [`Config`] by [`Config::resolve`].
#[derive(Clone, Default, Eq, PartialEq)]
pub struct CliOverrides {
    /// Override for `[base] default_repo`.
    pub base_default_repo: Option<String>,

    /// Override for `[base] envs`.
    pub base_envs: Option<Vec<String>>,

    /// Override for `[base] repos`.
    pub base_repos: Option<Vec<RepoEntry>>,

    /// Override for `[claude.anthropic] api_key`.
    pub claude_anthropic_api_key: Option<String>,

    /// Override for `[claude] backend`.
    pub claude_backend: Option<Backend>,

    /// Override for `[claude] effort`.
    pub claude_effort: Option<String>,

    /// Override for `[claude] model`.
    pub claude_model: Option<String>,

    /// Override for `[claude.vertex] credentials_file`.
    pub claude_vertex_credentials_file: Option<PathBuf>,

    /// Override for `[claude.vertex] project_id`.
    pub claude_vertex_project_id: Option<String>,

    /// Override for `[claude.vertex] region`.
    pub claude_vertex_region: Option<String>,

    /// Override for `[disk] grow_increment_gib`.
    pub disk_grow_increment_gib: Option<u32>,

    /// Override for `[disk] grow_threshold_pct`.
    pub disk_grow_threshold_pct: Option<u8>,

    /// Override for `[disk] virtual_size_gib`.
    pub disk_virtual_size_gib: Option<u32>,

    /// Override for `[github] token`.
    pub github_token: Option<String>,

    /// Override for `[network] uri`.
    pub network_uri: Option<String>,
}

impl fmt::Debug for CliOverrides {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CliOverrides")
            .field("base_default_repo", &self.base_default_repo)
            .field("base_envs", &self.base_envs)
            .field("base_repos", &self.base_repos)
            .field(
                "claude_anthropic_api_key",
                &self.claude_anthropic_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("claude_backend", &self.claude_backend)
            .field("claude_effort", &self.claude_effort)
            .field("claude_model", &self.claude_model)
            .field("claude_vertex_credentials_file", &self.claude_vertex_credentials_file)
            .field("claude_vertex_project_id", &self.claude_vertex_project_id)
            .field("claude_vertex_region", &self.claude_vertex_region)
            .field("disk_grow_increment_gib", &self.disk_grow_increment_gib)
            .field("disk_grow_threshold_pct", &self.disk_grow_threshold_pct)
            .field("disk_virtual_size_gib", &self.disk_virtual_size_gib)
            .field("github_token", &self.github_token.as_ref().map(|_| "[REDACTED]"))
            .field("network_uri", &self.network_uri)
            .finish()
    }
}

/// Load `config.toml` from the standard XDG location.
pub fn load() -> Result<FileConfig> {
    load_from(&paths::config_file()?)
}

/// Load `config.toml` from an explicit path, enforcing mode `0600`.
pub fn load_from(path: &Path) -> Result<FileConfig> {
    if !path.exists() {
        return Err(ConfigError::NotFound {
            path: path.to_path_buf(),
        }
        .into());
    }

    enforce_owner_only_mode(path)?;

    let raw = fs::read_to_string(path)?;
    let parsed: FileConfig = toml::from_str(&raw).map_err(|source| ConfigError::Deserialize {
        path: path.to_path_buf(),
        source,
    })?;

    tracing::debug!(?path, "loaded config file");

    Ok(parsed)
}

/// [`load`] + [`Config::resolve`] + [`Config::validate`].
pub fn load_and_resolve(cli: CliOverrides) -> Result<Config> {
    let file = load()?;
    let resolved = Config::resolve(file, cli);

    resolved.validate()?;

    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Reject configs more permissive than mode `0600`.
#[cfg(unix)]
fn enforce_owner_only_mode(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)?;
    let mode = metadata.mode() & PERMISSION_MASK;

    if mode != REQUIRED_MODE {
        return Err(ConfigError::InsecurePermissions {
            mode,
            path: path.to_path_buf(),
        }
        .into());
    }

    Ok(())
}

/// Non-Unix no-op for build portability.
#[cfg(not(unix))]
fn enforce_owner_only_mode(_path: &Path) -> Result<()> {
    Ok(())
}

/// Materialise [`DEFAULT_BASE_ENVS`] into owned strings.
fn default_base_envs() -> Vec<String> {
    DEFAULT_BASE_ENVS.iter().copied().map(str::to_owned).collect()
}

/// Materialise [`DEFAULT_RUST_COMPONENTS`] into owned strings.
fn default_rust_components() -> Vec<String> {
    DEFAULT_RUST_COMPONENTS.iter().copied().map(str::to_owned).collect()
}

/// Materialise [`DEFAULT_RUST_TOOLCHAINS`] into owned strings.
fn default_rust_toolchains() -> Vec<String> {
    DEFAULT_RUST_TOOLCHAINS.iter().copied().map(str::to_owned).collect()
}

/// Materialise [`DEFAULT_RUST_CARGO_TOOLS`] into owned strings.
fn default_rust_cargo_tools() -> Vec<String> {
    DEFAULT_RUST_CARGO_TOOLS.iter().copied().map(str::to_owned).collect()
}

/// Reject UID/GID 0 and empty usernames.
fn validate_user_identity(config: &Config) -> Result<()> {
    if let Some(uid) = config.user_uid
        && uid == 0
    {
        return Err(ConfigError::Invalid("[user] uid = 0 is not permitted".to_owned()).into());
    }

    if let Some(gid) = config.user_gid
        && gid == 0
    {
        return Err(ConfigError::Invalid("[user] gid = 0 is not permitted".to_owned()).into());
    }

    if let Some(username) = &config.user_username
        && username.trim().is_empty()
    {
        return Err(ConfigError::Invalid("[user] username must not be empty".to_owned()).into());
    }

    if let Some(username) = &config.user_username
        && !crate::host_user::is_valid_username(username)
    {
        return Err(ConfigError::Invalid(format!(
            "[user] username `{username}` is not a POSIX portable identifier; expected `[a-z_][a-z0-9_-]{{0,31}}`",
        ))
        .into());
    }

    Ok(())
}

/// Validate that the selected backend has its credentials populated.
fn validate_backend_credentials(config: &Config) -> Result<()> {
    match config.claude_backend {
        Backend::Anthropic => {
            if config.claude_anthropic_api_key.as_deref().unwrap_or("").is_empty() {
                return Err(ConfigError::MissingBackendCredentials {
                    backend: "anthropic".to_owned(),
                    missing: "[claude.anthropic] api_key",
                }
                .into());
            }
        },
        Backend::Bedrock => {
            return Err(ConfigError::BedrockNotSupported.into());
        },
        Backend::Vertex => {
            let Some(file) = config.claude_vertex_credentials_file.as_ref() else {
                return Err(ConfigError::MissingBackendCredentials {
                    backend: "vertex".to_owned(),
                    missing: "[claude.vertex] credentials_file",
                }
                .into());
            };

            if !file.exists() {
                return Err(ConfigError::Invalid(format!(
                    "[claude.vertex] credentials_file = {} does not exist on the host",
                    file.display(),
                ))
                .into());
            }
        },
    }

    Ok(())
}

/// Validate that credential strings and repo slugs are safe to embed
/// in cloud-init YAML and shell env files (no control chars, valid
/// `owner/name` shape).
fn validate_seed_input_strings(config: &Config) -> Result<()> {
    let single_line = |label: &str, value: &str| -> Result<()> {
        if !crate::seed::input::is_safe_single_line(value) {
            return Err(ConfigError::Invalid(format!(
                "{label} contains a control character or exceeds the 4 KiB single-line credential limit",
            ))
            .into());
        }
        Ok(())
    };

    if let Some(token) = &config.github_token {
        single_line("[github] token", token)?;
    }
    if let Some(api_key) = &config.claude_anthropic_api_key {
        single_line("[claude.anthropic] api_key", api_key)?;
    }
    if let Some(project_id) = &config.claude_vertex_project_id {
        single_line("[claude.vertex] project_id", project_id)?;
    }
    if let Some(region) = &config.claude_vertex_region {
        single_line("[claude.vertex] region", region)?;
    }
    single_line("[claude] model", &config.claude_model)?;
    single_line("[claude] effort", &config.claude_effort)?;

    for slug in config.base_repos.iter().map(|r| r.slug.as_str()) {
        if !crate::seed::input::is_valid_repo_slug(slug) {
            return Err(ConfigError::Invalid(format!(
                "[base] repos slug `{slug}` is not a valid GitHub `owner/name` (charset `[A-Za-z0-9._-]+/[A-Za-z0-9._-]+`)",
            ))
            .into());
        }
    }

    if let Some(slug) = &config.base_default_repo
        && !crate::seed::input::is_valid_repo_slug(slug)
    {
        return Err(ConfigError::Invalid(format!(
            "[base] default_repo `{slug}` is not a valid GitHub `owner/name`",
        ))
        .into());
    }

    Ok(())
}

/// Validate multi-repo invariants: at most one default, and
/// `default_repo` must name a listed slug.
fn validate_repos(config: &Config) -> Result<()> {
    let flagged = config.base_repos.iter().filter(|r| r.default).count();
    if flagged > 1 {
        return Err(ConfigError::Invalid(format!(
            "[base] repos lists {flagged} entries with default = true; at most one entry may be marked default",
        ))
        .into());
    }

    if let Some(slug) = config.base_default_repo.as_deref()
        && !config.base_repos.is_empty()
        && !config.base_repos.iter().any(|r| r.slug == slug)
    {
        return Err(ConfigError::Invalid(format!(
            "[base] default_repo = \"{slug}\" does not match any slug in [base] repos",
        ))
        .into());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::error::Error;

    #[test]
    fn defaults_resolve_to_documented_values() {
        let resolved = Config::resolve(FileConfig::default(), CliOverrides::default());

        assert_eq!(
            resolved.claude_backend,
            Backend::Anthropic,
            "default backend should be anthropic"
        );
        assert_eq!(
            resolved.claude_model, DEFAULT_CLAUDE_MODEL,
            "default model should match the constant"
        );
        assert_eq!(
            resolved.claude_effort, DEFAULT_CLAUDE_EFFORT,
            "default effort should match the constant"
        );
        assert_eq!(
            resolved.base_envs,
            DEFAULT_BASE_ENVS.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            "default base envs should be the opinionated set",
        );
        assert_eq!(
            resolved.disk_virtual_size_gib, DEFAULT_DISK_VIRTUAL_SIZE_GIB,
            "default overlay virtual size should be 100 GiB",
        );
        assert_eq!(
            resolved.network_uri, DEFAULT_NETWORK_URI,
            "default URI should be qemu:///session"
        );
    }

    #[test]
    fn cli_beats_env_beats_config() {
        let file = file_config_with_envs(&["from-config"]);
        let cli_layer = file_config_with_envs(&["from-config-when-no-cli"]);

        let result_cli = Config::resolve(
            file.clone(),
            CliOverrides {
                base_envs: Some(vec!["from-cli".to_owned()]),
                ..CliOverrides::default()
            },
        );
        assert_eq!(
            result_cli.base_envs,
            vec!["from-cli".to_owned()],
            "CLI override should beat the config value",
        );

        let result_env = Config::resolve(
            file.clone(),
            CliOverrides {
                base_envs: Some(vec!["from-env".to_owned()]),
                ..CliOverrides::default()
            },
        );
        assert_eq!(
            result_env.base_envs,
            vec!["from-env".to_owned()],
            "an env-sourced override (modeled the same as CLI) should beat the config value",
        );

        let result_config = Config::resolve(cli_layer, CliOverrides::default());
        assert_eq!(
            result_config.base_envs,
            vec!["from-config-when-no-cli".to_owned()],
            "config should win when neither CLI nor env supplies a value",
        );

        let result_default = Config::resolve(FileConfig::default(), CliOverrides::default());
        assert_eq!(
            result_default.base_envs,
            DEFAULT_BASE_ENVS.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            "the built-in default should win when no source supplies a value",
        );
    }

    #[test]
    fn round_trip_serialise_deserialise_merge_validate() {
        let original = sample_file_config();

        let toml_text = toml::to_string(&original).expect("sample config should serialise");
        let parsed: FileConfig = toml::from_str(&toml_text).expect("sample config should round-trip via TOML");

        assert_eq!(parsed, original, "FileConfig should round-trip cleanly through TOML",);

        let cli = CliOverrides {
            base_envs: Some(vec!["rust".to_owned(), "go".to_owned()]),
            claude_model: Some("claude-sonnet-4-7".to_owned()),
            disk_virtual_size_gib: Some(200),
            ..CliOverrides::default()
        };

        let resolved = Config::resolve(parsed, cli);

        resolved.validate().expect("resolved sample config should validate");

        assert_eq!(
            resolved.base_envs,
            vec!["rust".to_owned(), "go".to_owned()],
            "CLI envs should win over config envs",
        );
        assert_eq!(
            resolved.claude_model, "claude-sonnet-4-7",
            "CLI model should win over config model",
        );
        assert_eq!(
            resolved.disk_virtual_size_gib, 200,
            "CLI disk size should win over config",
        );
        assert_eq!(
            resolved.claude_effort, "max",
            "config-only effort field should flow through unchanged",
        );
        assert_eq!(
            resolved.claude_anthropic_api_key.as_deref(),
            Some("sk-ant-test"),
            "config-only API key should flow through unchanged",
        );
        assert_eq!(
            resolved.github_token.as_deref(),
            Some("ghp_test"),
            "config-only github token should flow through unchanged",
        );
        assert_eq!(
            resolved.rust_components,
            vec!["rustfmt".to_owned(), "clippy".to_owned()],
            "config-only rust components should flow through unchanged",
        );
    }

    #[test]
    fn deny_unknown_fields_at_top_level() {
        let bad = "[grithub]\ntoken = \"ghp_x\"\n";

        let err = toml::from_str::<FileConfig>(bad).expect_err("typo'd section should fail to deserialize");

        assert!(
            err.to_string().contains("unknown field"),
            "error should mention the unknown field, got: {err}",
        );
    }

    #[test]
    fn validate_rejects_undersized_overlay() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.disk_virtual_size_gib = 1;

        let err = resolved
            .validate()
            .expect_err("undersized overlay should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => assert!(
                msg.contains("virtual_size_gib"),
                "error should mention virtual_size_gib, got: {msg}",
            ),
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_zero_grow_increment() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.disk_grow_increment_gib = 0;

        let err = resolved
            .validate()
            .expect_err("zero grow increment should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => assert!(
                msg.contains("grow_increment_gib"),
                "error should mention grow_increment_gib, got: {msg}",
            ),
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_zero_uid() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.user_uid = Some(0);

        let err = resolved.validate().expect_err("uid 0 should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert!(msg.contains("uid"), "error should mention uid, got: {msg}");
            },
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn anthropic_backend_requires_api_key() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.claude_anthropic_api_key = None;

        let err = resolved
            .validate()
            .expect_err("anthropic backend with no api_key should fail validation");

        match err {
            Error::Config(ConfigError::MissingBackendCredentials { backend, .. }) => {
                assert_eq!(backend, "anthropic", "error should identify the anthropic backend");
            },
            other => panic!("expected ConfigError::MissingBackendCredentials, got {other:?}"),
        }
    }

    #[test]
    fn vertex_backend_requires_credentials_file() {
        let resolved = Config {
            claude_backend: Backend::Vertex,
            claude_vertex_credentials_file: None,
            ..sample_resolved_with_anthropic()
        };

        let err = resolved
            .validate()
            .expect_err("vertex backend with no credentials_file should fail validation");

        match err {
            Error::Config(ConfigError::MissingBackendCredentials { backend, .. }) => {
                assert_eq!(backend, "vertex", "error should identify the vertex backend");
            },
            other => panic!("expected ConfigError::MissingBackendCredentials, got {other:?}"),
        }
    }

    #[test]
    fn vertex_backend_rejects_missing_credentials_file() {
        let resolved = Config {
            claude_backend: Backend::Vertex,
            claude_vertex_credentials_file: Some(PathBuf::from("/definitely/not/a/real/path.json")),
            ..sample_resolved_with_anthropic()
        };

        let err = resolved
            .validate()
            .expect_err("vertex backend with a non-existent credentials_file should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("does not exist"),
                    "error should mention the missing file, got: {msg}",
                );
            },
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn bedrock_backend_is_rejected_at_validate_time() {
        let resolved = Config {
            claude_backend: Backend::Bedrock,
            ..sample_resolved_with_anthropic()
        };

        let err = resolved
            .validate()
            .expect_err("bedrock backend should be rejected as a milestone-2 feature");

        match err {
            Error::Config(ConfigError::BedrockNotSupported) => {},
            other => panic!("expected ConfigError::BedrockNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn bedrock_error_message_names_milestone_2() {
        let err = ConfigError::BedrockNotSupported.to_string();

        assert!(
            err.contains("milestone 2"),
            "bedrock rejection should name milestone 2, got: {err}",
        );
    }

    #[test]
    fn bedrock_round_trips_through_toml() {
        let body = "[claude]\nbackend = \"bedrock\"\n";
        let parsed: FileConfig = toml::from_str(body).expect("bedrock backend should parse, only validate rejects it");

        assert_eq!(
            parsed.claude.backend,
            Some(Backend::Bedrock),
            "the loader recognises bedrock so validate can reject it cleanly",
        );
    }

    #[test]
    fn validate_rejects_multiple_default_repos() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.base_repos = vec![
            RepoEntry {
                default: true,
                slug: "owner/alpha".to_owned(),
            },
            RepoEntry {
                default: true,
                slug: "owner/beta".to_owned(),
            },
        ];

        let err = resolved
            .validate()
            .expect_err("more than one default-flagged repo should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("default = true"),
                    "error should call out the multi-default conflict, got: {msg}",
                );
            },
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn validate_accepts_zero_default_flagged_repos() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.base_repos = vec![
            RepoEntry {
                default: false,
                slug: "owner/alpha".to_owned(),
            },
            RepoEntry {
                default: false,
                slug: "owner/beta".to_owned(),
            },
        ];

        resolved
            .validate()
            .expect("zero defaults is allowed; first listed wins at seed time");
    }

    #[test]
    fn validate_rejects_default_repo_not_in_repos_list() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.base_repos = vec![RepoEntry {
            default: false,
            slug: "owner/alpha".to_owned(),
        }];
        resolved.base_default_repo = Some("owner/missing".to_owned());

        let err = resolved
            .validate()
            .expect_err("default_repo naming a slug not in the list should fail validation");

        match err {
            Error::Config(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("default_repo"),
                    "error should name the offending field, got: {msg}",
                );
            },
            other => panic!("expected ConfigError::Invalid, got {other:?}"),
        }
    }

    #[test]
    fn multi_repo_section_round_trips_through_toml() {
        let body = "[base]\nrepos = [\
                    {slug = \"owner/alpha\"},\
                    {slug = \"owner/beta\", default = true},\
                  ]\ndefault_repo = \"owner/beta\"\n";

        let parsed: FileConfig = toml::from_str(body).expect("multi-repo TOML should parse");

        let repos = parsed.base.repos.expect("repos should populate");
        assert_eq!(repos.len(), 2, "two entries should round-trip");
        assert_eq!(repos[0].slug, "owner/alpha");
        assert!(!repos[0].default, "alpha entry should default to false");
        assert!(repos[1].default, "beta entry should be flagged default");
        assert_eq!(parsed.base.default_repo.as_deref(), Some("owner/beta"),);
    }

    #[test]
    fn validate_for_run_requires_github_token() {
        let mut resolved = sample_resolved_with_anthropic();
        resolved.github_token = None;

        let err = resolved
            .validate_for_run()
            .expect_err("missing github token should fail run-time validation");

        match err {
            Error::Config(ConfigError::MissingGithubToken) => {},
            other => panic!("expected ConfigError::MissingGithubToken, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_rejects_world_readable_config() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let path = dir.join("config.toml");
        write_config(&path, "[github]\ntoken = \"ghp_test\"\n");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("set_permissions should succeed in test tempdir");

        let err = load_from(&path).expect_err("world-readable config should be rejected");

        match err {
            Error::Config(ConfigError::InsecurePermissions { mode, .. }) => {
                assert_eq!(mode, 0o644, "observed mode should match the one we set");
            },
            other => panic!("expected ConfigError::InsecurePermissions, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_accepts_owner_only_config() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir();
        let path = dir.join("config.toml");
        write_config(&path, "[github]\ntoken = \"ghp_test\"\n");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("set_permissions should succeed in test tempdir");

        let parsed = load_from(&path).expect("0600 config should load cleanly");

        assert_eq!(
            parsed.github.token.as_deref(),
            Some("ghp_test"),
            "github token should round-trip through load_from",
        );
    }

    #[test]
    fn load_returns_not_found_for_missing_file() {
        let dir = tempdir();
        let path = dir.join("does-not-exist.toml");

        let err = load_from(&path).expect_err("missing file should return NotFound");

        match err {
            Error::Config(ConfigError::NotFound { path: reported }) => {
                assert_eq!(reported, path, "reported path should match the requested path",);
            },
            other => panic!("expected ConfigError::NotFound, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test Utilities
    // -----------------------------------------------------------------------

    /// [`FileConfig`] with a single `[base] envs` value.
    fn file_config_with_envs(envs: &[&str]) -> FileConfig {
        FileConfig {
            base: BaseSection {
                default_repo: None,
                envs: Some(envs.iter().map(|s| (*s).to_owned()).collect()),
                repos: None,
            },
            ..FileConfig::default()
        }
    }

    /// Representative [`FileConfig`] with every section populated.
    fn sample_file_config() -> FileConfig {
        FileConfig {
            base: BaseSection {
                default_repo: None,
                envs: Some(vec!["python".to_owned()]),
                repos: None,
            },
            claude: ClaudeSection {
                anthropic: ClaudeAnthropicSection {
                    api_key: Some("sk-ant-test".to_owned()),
                },
                backend: Some(Backend::Anthropic),
                effort: Some("max".to_owned()),
                model: Some("claude-opus-4-7".to_owned()),
                vertex: ClaudeVertexSection::default(),
            },
            disk: DiskSection {
                grow_increment_gib: Some(50),
                grow_threshold_pct: Some(90),
                virtual_size_gib: Some(150),
            },
            github: GithubSection {
                token: Some("ghp_test".to_owned()),
            },
            network: NetworkSection {
                uri: Some("qemu:///session".to_owned()),
            },
            rust: RustSection {
                cargo_tools: Some(vec!["cargo-nextest".to_owned()]),
                components: Some(vec!["rustfmt".to_owned(), "clippy".to_owned()]),
                toolchains: Some(vec!["stable".to_owned()]),
            },
            user: UserSection {
                gid: Some(1000),
                uid: Some(1000),
                username: Some("alice".to_owned()),
            },
        }
    }

    /// Resolved Anthropic-backend [`Config`] for validation tests.
    fn sample_resolved_with_anthropic() -> Config {
        Config::resolve(
            FileConfig {
                claude: ClaudeSection {
                    anthropic: ClaudeAnthropicSection {
                        api_key: Some("sk-ant-test".to_owned()),
                    },
                    backend: Some(Backend::Anthropic),
                    ..ClaudeSection::default()
                },
                github: GithubSection {
                    token: Some("ghp_test".to_owned()),
                },
                ..FileConfig::default()
            },
            CliOverrides::default(),
        )
    }

    /// Unique per-process temp directory.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tartarus-config-test-{pid}-{n}"));

        std::fs::create_dir_all(&path).expect("create_dir_all should succeed in test tempdir root");

        path
    }

    /// Write `body` to `path` with default permissions.
    fn write_config(path: &Path, body: &str) {
        let mut file = std::fs::File::create(path).expect("create should succeed in test tempdir");

        file.write_all(body.as_bytes())
            .expect("write_all should succeed in test tempdir");
    }
}
