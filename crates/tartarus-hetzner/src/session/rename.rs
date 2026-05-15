//! `tartarus rename` against Hetzner Cloud.
//!
//! Updates the server's `tartarus.alias` label so the next
//! `list`/`stop`/`destroy` finds it by the new name. The Hetzner
//! display name is left as `tartarus-<short uuid>` so the operator's
//! Hetzner Cloud Console UI keeps its predictable layout.

use std::collections::BTreeMap;

use tartarus_provider::{RenameOutcome, session::SessionError};

use crate::{
    Result,
    api::Client,
    session::{labels, lifecycle},
};

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus rename <uuid> <alias>` against Hetzner.
pub fn run(client: &Client, uuid: &str, alias: &str) -> Result<RenameOutcome> {
    let server = lifecycle::find_server(client, uuid)?;
    let stored_uuid = server
        .labels
        .get(labels::LABEL_UUID)
        .cloned()
        .ok_or_else(|| SessionError::NotFound {
            target: uuid.to_owned(),
        })?;

    let mut new_labels = server.labels.clone();
    new_labels.insert(labels::LABEL_ALIAS.to_owned(), alias.to_owned());

    set_labels(client, server.id, &new_labels)?;

    Ok(RenameOutcome {
        alias: alias.to_owned(),
        uuid: stored_uuid,
    })
}

// -----------------------------------------------------------------------------
// PUT Labels
// -----------------------------------------------------------------------------

/// Payload for `PUT /servers/{id}` when only labels change.
#[derive(serde::Serialize)]
struct UpdateLabelsRequest<'a> {
    labels: &'a BTreeMap<String, String>,
}

/// `PUT /servers/{id}` with the full label map. Hetzner replaces
/// the existing label set so we have to send everything.
fn set_labels(client: &Client, id: u64, labels: &BTreeMap<String, String>) -> Result<()> {
    let _: serde_json::Value = client.post(
        "PUT /servers/{id}",
        &format!("/servers/{id}"),
        &UpdateLabelsRequest { labels },
    )?;
    Ok(())
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::tests_fake_server::{CannedResponse, Server},
        session::labels::{LABEL_ALIAS, LABEL_OWNED, LABEL_PERSIST, LABEL_UUID},
    };

    #[test]
    fn rename_sends_full_label_map_with_new_alias() {
        let body = format!(
            r#"{{
                "servers": [{{
                    "id": 51,
                    "name": "tartarus-q",
                    "status": "running",
                    "labels": {{
                        "{LABEL_OWNED}": "true",
                        "{LABEL_UUID}": "abcd-uuid",
                        "{LABEL_PERSIST}": "true"
                    }}
                }}]
            }}"#
        );
        let fake = Server::start(vec![
            CannedResponse::ok(body),
            CannedResponse::ok(r#"{"server":{"id":51,"name":"tartarus-q","status":"running","labels":{}}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "abcd-uuid", "new-alias").expect("rename should succeed");
        assert_eq!(outcome.alias, "new-alias");
        assert_eq!(outcome.uuid, "abcd-uuid");

        let bodies = fake.seen_bodies.lock().expect("bodies");
        let put_body = &bodies[1];
        assert!(
            put_body.contains(LABEL_ALIAS) && put_body.contains("new-alias"),
            "PUT body should include the new alias label: {put_body}",
        );
        assert!(
            put_body.contains(LABEL_OWNED),
            "PUT body should include the preserved ownership label: {put_body}",
        );
    }

    #[test]
    fn rename_errors_when_target_is_missing() {
        let fake = Server::start(vec![CannedResponse::ok(r#"{"servers":[]}"#)]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let err = run(&client, "no-such", "new-alias").expect_err("missing target should error");
        match err {
            crate::Error::Lifecycle(lifecycle::LifecycleError::SessionNotFound { target }) => {
                assert_eq!(target, "no-such");
            },
            other => panic!("expected SessionNotFound, got {other:?}"),
        }
    }
}
