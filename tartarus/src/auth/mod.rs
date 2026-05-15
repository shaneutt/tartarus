//! Credential acquisition, storage, and status reporting.
//!
//! Entry points: [`run_init`], [`run_init_google`], [`run_status`],
//! wired from [`crate::cli`]. Everything else is internal plumbing.

pub mod error;
pub mod google;
pub mod init;
pub mod prompt;
pub mod redact;
pub mod status;
pub mod vertex;
pub mod write;

use std::path::Path;

use crate::{config, error::Result};

// -----------------------------------------------------------------------------
// Auth Commands
// -----------------------------------------------------------------------------

/// Load [`config::FileConfig`] from `path`, returning `None` when the
/// file is missing rather than erroring.
pub fn load_file_config_optional(path: &Path) -> Result<Option<config::FileConfig>> {
    if !path.exists() {
        return Ok(None);
    }

    Ok(Some(config::load_from(path)?))
}

/// Run `tartarus auth init`: interactive GitHub + Anthropic bootstrap.
///
/// Prompts for a GitHub PAT (paste-only) and an Anthropic API key
/// (paste, or browser fallback). Writes `config.toml` at mode `0600`;
/// refuses to overwrite unless `force` is set.
pub fn run_init(force: bool) -> Result<()> {
    let path = tartarus_provider::paths::config_file()?;

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    init::run(init::InitContext {
        force,
        path: &path,
        reader: &mut reader,
        writer: &mut writer,
    })
}

/// Run `tartarus auth init google`: bootstrap the Vertex credential
/// bundle by prompting for project ID, region, and service-account
/// JSON path. Merges into any existing config at mode `0600`.
pub fn run_init_google() -> Result<()> {
    let path = tartarus_provider::paths::config_file()?;

    google::run(&path, &mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}

/// Run `tartarus auth status`: print configured credentials, each
/// redacted to the last 4 characters. Always returns `Ok`.
pub fn run_status(config: Option<&config::Config>, file: Option<&config::FileConfig>) -> Result<()> {
    status::print(&mut std::io::stdout().lock(), config, file)
}
