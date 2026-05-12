//! `tracing-subscriber` initialization. Precedence: CLI flags >
//! `TARTARUS_LOG` env var > default (`warn`). Output goes to stderr.

use tracing_subscriber::{EnvFilter, fmt};

use crate::error::Result;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Default filter directive.
const DEFAULT_FILTER: &str = "warn";

/// Environment variable consulted when no CLI verbosity flag is given.
const ENV_VAR: &str = "TARTARUS_LOG";

// -----------------------------------------------------------------------------
// Verbosity
// -----------------------------------------------------------------------------

/// Verbosity selected by the CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verbosity {
    /// `--quiet`: errors only.
    Quiet,

    /// No flag set: defer to `TARTARUS_LOG`, falling back to `warn`.
    Default,

    /// `-v`: info-level events from the `tartarus` crate.
    Info,

    /// `-vv`: debug-level events from the `tartarus` crate.
    Debug,
}

impl Verbosity {
    /// Filter directive, or `None` for [`Verbosity::Default`].
    fn filter_directive(self) -> Option<&'static str> {
        match self {
            Verbosity::Quiet => Some("error"),
            Verbosity::Default => None,
            Verbosity::Info => Some("tartarus=info,warn"),
            Verbosity::Debug => Some("tartarus=debug,info"),
        }
    }
}

/// Install the global `tracing` subscriber.
pub fn init(verbosity: Verbosity) -> Result<()> {
    let filter = build_filter(verbosity);

    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init()
        .map_err(|err| std::io::Error::other(err.to_string()))?;

    Ok(())
}

// -----------------------------------------------------------------------------
// Filter Resolution
// -----------------------------------------------------------------------------

/// Resolve a filter according to the documented precedence: CLI > env > default.
fn build_filter(verbosity: Verbosity) -> EnvFilter {
    if let Some(directive) = verbosity.filter_directive() {
        return EnvFilter::new(directive);
    }

    EnvFilter::try_from_env(ENV_VAR).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_verbosity_overrides_env() {
        let filter = build_filter(Verbosity::Info);

        assert!(
            filter.to_string().contains("tartarus=info"),
            "explicit -v should produce a tartarus=info directive, got {filter}",
        );
    }

    #[test]
    fn default_verbosity_yields_a_filter() {
        let filter = build_filter(Verbosity::Default);

        assert!(!filter.to_string().is_empty(), "default filter should not be empty",);
    }

    #[test]
    fn quiet_emits_error_only_directive() {
        let filter = build_filter(Verbosity::Quiet);

        assert!(
            filter.to_string().contains("error"),
            "--quiet should produce an error-only directive, got {filter}",
        );
    }
}
