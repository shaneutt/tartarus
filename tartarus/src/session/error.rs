//! Errors raised by the session subsystem.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// SessionError
// ---------------------------------------------------------------------------

/// Failure modes specific to session identity, metadata, and lifecycle.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Alias already points at a different UUID.
    #[error("alias `{alias}` is already in use by session {existing_uuid}")]
    AliasInUse {
        /// Conflicting alias name.
        alias: String,

        /// UUID the existing alias points at.
        existing_uuid: String,
    },

    /// Alias symlink target does not exist on disk.
    #[error("alias `{alias}` points at {target} but no session lives there")]
    DanglingAlias {
        /// Alias name that resolved to nothing.
        alias: String,

        /// Path the dangling alias points at.
        target: PathBuf,
    },

    /// CLI-supplied `--repo` slug is not a valid `owner/name` shape.
    #[error("invalid repo slug `{slug}`; expected GitHub `owner/name` (charset `[A-Za-z0-9._-]+/[A-Za-z0-9._-]+`)")]
    InvalidRepoSlug {
        /// Slug the user supplied.
        slug: String,
    },

    /// Alias contains characters outside the portable filename charset.
    #[error(
        "invalid alias `{alias}`; aliases must match `[A-Za-z0-9][A-Za-z0-9._-]*` and be at most 64 characters long"
    )]
    InvalidAlias {
        /// Alias the user supplied.
        alias: String,
    },

    /// UUID does not match the canonical v4 hex shape.
    #[error("invalid session UUID `{uuid}`; expected canonical v4 hex form")]
    InvalidUuid {
        /// UUID the caller supplied.
        uuid: String,
    },

    /// Required credentials are missing for session seeding.
    #[error("no GitHub credentials configured. hint: run `tartarus auth init` to set them up.")]
    MissingCredentials,

    /// Schema version in `metadata.json` is unsupported.
    #[error("session metadata version {version} is not supported (loader supports v1 and newer)")]
    MetadataVersion {
        /// Observed schema version.
        version: u32,
    },

    /// `metadata.json` is not valid JSON.
    #[error("session metadata at {path} is not valid JSON: {source}")]
    MetadataParse {
        /// Path that failed to parse.
        path: PathBuf,

        /// Underlying serde_json error.
        source: serde_json::Error,
    },

    /// No base image available for a new session.
    #[error("no base image is current; hint: run `tartarus base pull` first")]
    NoBaseCurrent,

    /// No repository configured for the session.
    #[error("no repository configured; pass `--repo owner/name` (repeatable) or set `[base] repos` in config.toml")]
    NoRepos,

    /// No session matches the given UUID or alias.
    #[error("no session matches `{target}` (looked under by-uuid/ and by-name/)")]
    NotFound {
        /// Identifier the user supplied.
        target: String,
    },

    /// `ssh-keygen` failed during per-session keypair generation.
    #[error("ssh-keygen failed: {detail}")]
    SshKeygenFailed {
        /// Human-readable description (spawn failure / non-zero exit).
        detail: String,
    },

    /// No free loopback port available in the configured SSH range.
    #[error(
        "could not allocate a free loopback port in {start}..={end} for session SSH; \
         hint: stop a stale session, or expand the range."
    )]
    SshPortExhausted {
        /// Inclusive lower bound that was scanned.
        start: u16,

        /// Inclusive upper bound that was scanned.
        end: u16,
    },

    /// Guest SSH host key could not be read via `qemu-guest-agent`.
    #[error("could not read the guest's SSH host key after {attempts} attempts: {detail}")]
    SshHostKeyUnavailable {
        /// Number of guest-agent reads attempted before giving up.
        attempts: u32,

        /// Underlying error or short failure description.
        detail: String,
    },

    /// Env name is not in the supported set (`rust`, `go`, `python`).
    #[error("unknown env `{env}`; supported envs are: {}", supported.join(", "))]
    UnknownEnv {
        /// Name the user supplied.
        env: String,

        /// Canonical supported list.
        supported: &'static [&'static str],
    },
}
