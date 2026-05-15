//! `tartarus destroy` against Hetzner Cloud.
//!
//! Deletes the server. When `tartarus.persist=false` is set in the
//! server labels (the `--ephemeral` path) we also drop the attached
//! volume; otherwise the volume survives so the operator can attach
//! it to a fresh session.

use tartarus_provider::DestroyOutcome;

use crate::{
    Result,
    api::{Client, servers, volumes},
    session::lifecycle,
};

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus destroy <alias|uuid>` against Hetzner.
pub fn run(client: &Client, target: &str) -> Result<DestroyOutcome> {
    let server = lifecycle::find_server(client, target)?;
    let uuid = server
        .labels
        .get(crate::session::labels::LABEL_UUID)
        .cloned()
        .unwrap_or_else(|| target.to_owned());

    let ephemeral = server
        .labels
        .get(crate::session::labels::LABEL_PERSIST)
        .map(|v| v == "false")
        .unwrap_or(false);

    let delete = servers::delete(client, server.id)?;
    if let Some(envelope) = delete {
        lifecycle::wait_for(client, envelope.action.id, "server delete")?;
    }

    if ephemeral {
        drop_attached_volumes(client, server.id)?;
    }

    tracing::info!(server_id = server.id, %uuid, ephemeral, "Hetzner session destroyed");

    Ok(DestroyOutcome { uuid })
}

// -----------------------------------------------------------------------------
// Volume Cleanup
// -----------------------------------------------------------------------------

/// List every volume that was attached to `server_id` and delete it.
fn drop_attached_volumes(client: &Client, server_id: u64) -> Result<()> {
    let response = list_volumes(client)?;
    for volume in response.volumes {
        if volume.server == Some(server_id) {
            tracing::info!(
                volume_id = volume.id,
                server_id,
                "deleting attached volume because session was ephemeral",
            );
            volumes::delete(client, volume.id)?;
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct VolumesList {
    volumes: Vec<volumes::Volume>,
}

/// `GET /volumes` (Tartarus does not paginate; project sizes are small).
fn list_volumes(client: &Client) -> Result<VolumesList> {
    Ok(client.get("GET /volumes", "/volumes")?)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::tests_fake_server::{CannedResponse, Server},
        session::labels,
    };

    fn server_with_session_payload(uuid: &str, persist: bool, server_id: u64) -> String {
        let persist_str = if persist { "true" } else { "false" };
        format!(
            r#"{{
                "servers": [{{
                    "id": {server_id},
                    "name": "tartarus-{short}",
                    "status": "running",
                    "labels": {{
                        "{owned}": "true",
                        "{uuid_key}": "{uuid}",
                        "{persist_key}": "{persist_str}"
                    }}
                }}]
            }}"#,
            short = &uuid[..uuid.len().min(8)],
            owned = labels::LABEL_OWNED,
            uuid_key = labels::LABEL_UUID,
            persist_key = labels::LABEL_PERSIST,
        )
    }

    #[test]
    fn destroy_finds_session_deletes_server_and_polls_action() {
        let fake = Server::start(vec![
            CannedResponse::ok(server_with_session_payload("abcd1234-uuid", true, 7)),
            CannedResponse::ok(r#"{"action":{"id":99,"status":"running"}}"#),
            CannedResponse::ok(r#"{"action":{"id":99,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "abcd1234-uuid").expect("destroy should succeed");
        assert_eq!(outcome.uuid, "abcd1234-uuid");

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(
            paths[0].contains("GET /servers?label_selector="),
            "first call should be the labelled list: {paths:?}",
        );
        assert!(
            paths[1].contains("DELETE /servers/7"),
            "second call should be the server delete: {paths:?}",
        );
    }

    #[test]
    fn destroy_errors_when_no_session_matches() {
        let fake = Server::start(vec![CannedResponse::ok(r#"{"servers":[]}"#)]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let err = run(&client, "no-such").expect_err("missing session should error");
        match err {
            crate::Error::Lifecycle(lifecycle::LifecycleError::SessionNotFound { target }) => {
                assert_eq!(target, "no-such");
            },
            other => panic!("expected SessionNotFound, got {other:?}"),
        }
    }
}
