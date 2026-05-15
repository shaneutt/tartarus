//! `tartarus list` against Hetzner Cloud.
//!
//! Fetches every Tartarus-owned server from the project and folds
//! each into the provider-shared [`ListEntry`] shape.

use tartarus_provider::ListEntry;

use crate::{
    Result,
    api::{Client, servers},
    session::labels,
};

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus list` against Hetzner.
pub fn collect(client: &Client) -> Result<Vec<ListEntry>> {
    let response = servers::list(client, Some(labels::SELECTOR_ALL))?;
    Ok(response.servers.into_iter().map(build_entry).collect())
}

// -----------------------------------------------------------------------------
// Row Assembly
// -----------------------------------------------------------------------------

/// Fold one Hetzner server into a [`ListEntry`].
fn build_entry(server: servers::Server) -> ListEntry {
    let alias = server
        .labels
        .get(labels::LABEL_ALIAS)
        .cloned()
        .unwrap_or_else(|| "(unnamed)".to_owned());

    let uuid_short = server
        .labels
        .get(labels::LABEL_UUID)
        .map(|u| u.chars().take(8).collect::<String>())
        .unwrap_or_else(|| "?".to_owned());

    let persist = server
        .labels
        .get(labels::LABEL_PERSIST)
        .map(|v| if v == "true" { "yes" } else { "no" })
        .unwrap_or("?")
        .to_owned();

    ListEntry {
        alias,
        base: "(hetzner)".to_owned(),
        cpu: "?".to_owned(),
        envs: String::new(),
        mem: "?".to_owned(),
        persist,
        size: "?".to_owned(),
        status: server.status,
        uuid_short,
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn build_entry_lifts_alias_and_uuid_short() {
        let mut srv_labels: BTreeMap<String, String> = BTreeMap::new();
        srv_labels.insert(labels::LABEL_OWNED.to_owned(), "true".to_owned());
        srv_labels.insert(labels::LABEL_UUID.to_owned(), "12345678-9abc".to_owned());
        srv_labels.insert(labels::LABEL_ALIAS.to_owned(), "fix-bug".to_owned());
        srv_labels.insert(labels::LABEL_PERSIST.to_owned(), "true".to_owned());

        let entry = build_entry(servers::Server {
            id: 1,
            name: "tartarus-1".to_owned(),
            status: "running".to_owned(),
            public_net: None,
            labels: srv_labels,
        });

        assert_eq!(entry.alias, "fix-bug");
        assert_eq!(entry.uuid_short, "12345678");
        assert_eq!(entry.persist, "yes");
        assert_eq!(entry.status, "running");
    }

    #[test]
    fn build_entry_handles_unnamed_session() {
        let mut srv_labels: BTreeMap<String, String> = BTreeMap::new();
        srv_labels.insert(labels::LABEL_UUID.to_owned(), "abcd".to_owned());

        let entry = build_entry(servers::Server {
            id: 1,
            name: "tartarus-abcd".to_owned(),
            status: "off".to_owned(),
            public_net: None,
            labels: srv_labels,
        });

        assert_eq!(entry.alias, "(unnamed)");
        assert_eq!(entry.persist, "?");
    }
}
