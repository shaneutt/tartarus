//! Hetzner label keys Tartarus uses to round-trip session state.
//!
//! Servers and volumes Tartarus owns are tagged with these labels so
//! `list` can filter the project, and so the binary can re-discover
//! a session from its alias without reading per-session files.

use std::collections::BTreeMap;

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// Marks any Hetzner resource Tartarus owns.
pub const LABEL_OWNED: &str = "tartarus.owned";

/// Session UUID, mirrored from `metadata.json`.
pub const LABEL_UUID: &str = "tartarus.uuid";

/// Optional alias from `--name`. Absent when the session was started
/// without one.
pub const LABEL_ALIAS: &str = "tartarus.alias";

/// True iff the overlay/volume should survive `destroy`.
pub const LABEL_PERSIST: &str = "tartarus.persist";

/// Label selector that scopes a Hetzner `list` to Tartarus servers.
pub const SELECTOR_ALL: &str = "tartarus.owned=true";

// -----------------------------------------------------------------------------
// Builders
// -----------------------------------------------------------------------------

/// Assemble the label map a fresh server gets tagged with.
pub fn fresh_session(uuid: &str, alias: Option<&str>, persist: bool) -> BTreeMap<String, String> {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert(LABEL_OWNED.to_owned(), "true".to_owned());
    labels.insert(LABEL_UUID.to_owned(), uuid.to_owned());
    if let Some(alias) = alias {
        labels.insert(LABEL_ALIAS.to_owned(), alias.to_owned());
    }
    labels.insert(
        LABEL_PERSIST.to_owned(),
        if persist { "true" } else { "false" }.to_owned(),
    );
    labels
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_session_marks_ownership_and_uuid() {
        let labels = fresh_session("abc-1234", Some("fix-bug"), true);
        assert_eq!(labels.get(LABEL_OWNED).map(String::as_str), Some("true"));
        assert_eq!(labels.get(LABEL_UUID).map(String::as_str), Some("abc-1234"));
        assert_eq!(labels.get(LABEL_ALIAS).map(String::as_str), Some("fix-bug"));
        assert_eq!(labels.get(LABEL_PERSIST).map(String::as_str), Some("true"));
    }

    #[test]
    fn fresh_session_omits_alias_when_none() {
        let labels = fresh_session("abc-1234", None, false);
        assert!(!labels.contains_key(LABEL_ALIAS));
        assert_eq!(labels.get(LABEL_PERSIST).map(String::as_str), Some("false"));
    }
}
