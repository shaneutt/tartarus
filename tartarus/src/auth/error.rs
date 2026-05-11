//! Errors raised by the `auth` subsystem.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// AuthError
// ---------------------------------------------------------------------------

/// Failure modes specific to credential acquisition and storage.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The Anthropic API key was empty after the browser fallback.
    #[error("no Anthropic API key was provided")]
    AnthropicKeyMissing,

    /// Config file exists and `--force` was not passed.
    #[error(
        "{path} already exists; pass --force to overwrite, or run `tartarus auth init google` to merge a Vertex \
         backend into the existing file"
    )]
    ConfigAlreadyExists {
        /// Path that would have been overwritten.
        path: PathBuf,
    },

    /// User pressed Enter at the GitHub PAT prompt (empty input).
    #[error(
        "no GitHub personal access token was provided. hint: create one at https://github.com/settings/tokens with \
         the `repo` scope, then re-run `tartarus auth init` and paste it at the prompt."
    )]
    GithubTokenMissing,

    /// Stdin read failed.
    #[error("failed to read from stdin: {0}")]
    InteractiveReadFailed(#[source] std::io::Error),

    /// Stdout write failed (e.g. closed pipe).
    #[error("failed to write to stdout: {0}")]
    InteractiveWriteFailed(#[source] std::io::Error),

    /// Path was not absolute.
    #[error("{path} is not an absolute path; tartarus does not expand `~` or environment variables")]
    PathNotAbsolute {
        /// The non-absolute path supplied.
        path: PathBuf,
    },

    /// TOML serialisation of [`crate::config::FileConfig`] failed.
    #[error("failed to serialise config to TOML: {0}")]
    Serialize(#[from] toml::ser::Error),

    /// HTTP request failed.
    #[error("HTTP request failed: {0}")]
    Transport(#[from] reqwest::Error),

    /// Service-account file is not valid JSON.
    #[error("service-account file at {path} is not valid JSON: {source}")]
    VertexFileParse {
        /// Path that failed to parse.
        path: PathBuf,

        /// Underlying serde_json error.
        source: serde_json::Error,
    },

    /// Service-account file could not be read.
    #[error("failed to read service-account file at {path}: {source}")]
    VertexFileRead {
        /// Path that failed to read.
        path: PathBuf,

        /// Underlying I/O error.
        source: std::io::Error,
    },
}
