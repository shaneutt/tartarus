//! [`RunMode`] enum: foreground, detached, or background.

// ---------------------------------------------------------------------------
// RunMode
// ---------------------------------------------------------------------------

/// Selected run mode for `tartarus run`. `Foreground` is the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RunMode {
    /// Start + capture remote-connect URL, drivable from claude.ai.
    Background,

    /// Start without console attach; re-attach via `tartarus resume`.
    Detached,

    /// Start and attach host TTY to guest serial console.
    #[default]
    Foreground,
}

impl RunMode {
    /// True iff the mode attaches the host TTY after start.
    pub fn attaches_console(&self) -> bool {
        matches!(self, Self::Foreground)
    }

    /// True iff the mode captures the remote-connect URL after start.
    pub fn captures_remote_url(&self) -> bool {
        matches!(self, Self::Background)
    }

    /// True iff the seed should enable Claude remote-connectivity.
    pub fn enables_remote_connect(&self) -> bool {
        matches!(self, Self::Background)
    }

    /// Resolve from the two boolean flags. Panics if both are true
    /// (clap prevents this).
    pub fn from_flags(detach: bool, background: bool) -> Self {
        match (detach, background) {
            (false, false) => Self::Foreground,
            (true, false) => Self::Detached,
            (false, true) => Self::Background,
            (true, true) => panic!(
                "RunMode::from_flags called with both --detach and --background; clap should have rejected this combination at parse time",
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_foreground() {
        assert_eq!(
            RunMode::default(),
            RunMode::Foreground,
            "default run mode should be foreground per spec",
        );
    }

    #[test]
    fn from_flags_with_both_false_is_foreground() {
        assert_eq!(RunMode::from_flags(false, false), RunMode::Foreground);
    }

    #[test]
    fn from_flags_detach_only_is_detached() {
        assert_eq!(RunMode::from_flags(true, false), RunMode::Detached);
    }

    #[test]
    fn from_flags_background_only_is_background() {
        assert_eq!(RunMode::from_flags(false, true), RunMode::Background);
    }

    #[test]
    #[should_panic(expected = "clap should have rejected")]
    fn from_flags_with_both_true_panics() {
        let _ = RunMode::from_flags(true, true);
    }

    #[test]
    fn foreground_attaches_console() {
        assert!(RunMode::Foreground.attaches_console());
        assert!(!RunMode::Detached.attaches_console());
        assert!(!RunMode::Background.attaches_console());
    }

    #[test]
    fn only_background_enables_remote_connect() {
        assert!(!RunMode::Foreground.enables_remote_connect());
        assert!(!RunMode::Detached.enables_remote_connect());
        assert!(RunMode::Background.enables_remote_connect());
    }

    #[test]
    fn only_background_captures_remote_url() {
        assert!(!RunMode::Foreground.captures_remote_url());
        assert!(!RunMode::Detached.captures_remote_url());
        assert!(RunMode::Background.captures_remote_url());
    }
}
