//! Udev rule generation for VFIO group device permissions.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Filename for the udev rule.
pub const UDEV_RULE_FILENAME: &str = "99-tartarus-vfio.rules";

/// Default install path. Read by [`UdevRule::install_path`].
const UDEV_RULES_INSTALL_DIR: &str = "/etc/udev/rules.d";

// ---------------------------------------------------------------------------
// Udev Rule Generation
// ---------------------------------------------------------------------------

/// Rendered udev rule body and canonical install path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UdevRule {
    /// Rule file contents (single newline-terminated line).
    pub body: String,

    /// Recommended on-disk path: `/etc/udev/rules.d/99-tartarus-vfio.rules`.
    pub install_path: PathBuf,
}

/// Render the udev rule granting `username` read+write on VFIO
/// group nodes.
pub fn build_udev_rule(username: &str) -> UdevRule {
    debug_assert!(
        crate::host_user::is_valid_username(username),
        "username must pass POSIX validation before reaching udev rule generation",
    );
    let body = format!("SUBSYSTEM==\"vfio\", OWNER=\"{username}\", GROUP=\"{username}\", MODE=\"0660\"\n",);
    UdevRule {
        body,
        install_path: PathBuf::from(UDEV_RULES_INSTALL_DIR).join(UDEV_RULE_FILENAME),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_body_matches_vfio_subsystem() {
        let rule = build_udev_rule("alice");
        assert!(
            rule.body.contains("SUBSYSTEM==\"vfio\""),
            "rule must scope to the vfio subsystem, got: {body}",
            body = rule.body,
        );
    }

    #[test]
    fn rule_body_grants_named_user_read_write() {
        let rule = build_udev_rule("alice");
        assert!(
            rule.body.contains("OWNER=\"alice\""),
            "rule must set OWNER to the named user"
        );
        assert!(
            rule.body.contains("GROUP=\"alice\""),
            "rule must set GROUP to the named user"
        );
        assert!(rule.body.contains("MODE=\"0660\""), "rule must set MODE to 0660");
    }

    #[test]
    fn rule_body_is_a_single_terminated_line() {
        let rule = build_udev_rule("alice");
        assert_eq!(
            rule.body.matches('\n').count(),
            1,
            "rule must emit exactly one newline-terminated line, got: {body:?}",
            body = rule.body,
        );
        assert!(
            rule.body.ends_with('\n'),
            "rule must end with a newline so `>>` appends cleanly"
        );
    }

    #[test]
    fn install_path_is_under_etc_udev_rules_d() {
        let rule = build_udev_rule("alice");
        assert!(
            rule.install_path.starts_with("/etc/udev/rules.d"),
            "install path must be the canonical udev directory"
        );
        assert!(
            rule.install_path.ends_with(UDEV_RULE_FILENAME),
            "install path filename should be {UDEV_RULE_FILENAME}",
        );
    }
}
