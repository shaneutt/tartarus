//! `tartarus resume` against Hetzner Cloud.
//!
//! Hetzner does not preserve console state across `poweroff`, so
//! "resume" just powers the server back on and returns. A future
//! revision can attach to the cloud-init console stream.

use tartarus_provider::ResumeOutcome;

use crate::{
    Result,
    api::{Client, servers},
    session::lifecycle,
};

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus resume <alias|uuid>` against Hetzner.
pub fn run(client: &Client, target: &str) -> Result<ResumeOutcome> {
    let server = lifecycle::find_server(client, target)?;
    let uuid = uuid_from(&server, target);

    let started_from_shutoff = matches!(server.status.as_str(), "off" | "stopped");

    if started_from_shutoff {
        let action = power_on(client, server.id)?;
        lifecycle::wait_for(client, action.id, "server power-on")?;
    }

    Ok(ResumeOutcome {
        started_from_shutoff,
        uuid,
    })
}

// -----------------------------------------------------------------------------
// Power Actions
// -----------------------------------------------------------------------------

/// Response envelope for `POST /servers/{id}/actions/poweron`.
#[derive(serde::Deserialize)]
struct PowerOnResponse {
    action: servers::Action,
}

/// Issue `POST /servers/{id}/actions/poweron`.
fn power_on(client: &Client, server_id: u64) -> Result<servers::Action> {
    let response: PowerOnResponse = client.post(
        "POST /servers/{id}/actions/poweron",
        &format!("/servers/{server_id}/actions/poweron"),
        &serde_json::json!({}),
    )?;
    Ok(response.action)
}

/// Extract the session UUID from the Hetzner label, falling back to
/// the caller-supplied `target` when (somehow) absent.
fn uuid_from(server: &servers::Server, target: &str) -> String {
    server
        .labels
        .get(crate::session::labels::LABEL_UUID)
        .cloned()
        .unwrap_or_else(|| target.to_owned())
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

    fn list_response(uuid: &str, status: &str) -> String {
        format!(
            r#"{{
                "servers": [{{
                    "id": 17,
                    "name": "tartarus-x",
                    "status": "{status}",
                    "labels": {{
                        "{owned}": "true",
                        "{uuid_key}": "{uuid}",
                        "{persist_key}": "true"
                    }}
                }}]
            }}"#,
            owned = labels::LABEL_OWNED,
            uuid_key = labels::LABEL_UUID,
            persist_key = labels::LABEL_PERSIST,
        )
    }

    #[test]
    fn resume_powers_on_when_server_is_off() {
        let fake = Server::start(vec![
            CannedResponse::ok(list_response("abcd1234", "off")),
            CannedResponse::ok(r#"{"action":{"id":11,"status":"running"}}"#),
            CannedResponse::ok(r#"{"action":{"id":11,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "abcd1234").expect("resume should succeed");
        assert!(
            outcome.started_from_shutoff,
            "off state should mean a power-on was issued"
        );
        assert_eq!(outcome.uuid, "abcd1234");

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(
            paths[1].contains("POST /servers/17/actions/poweron"),
            "second call should be the power-on: {paths:?}",
        );
    }

    #[test]
    fn resume_is_a_noop_when_server_is_already_running() {
        let fake = Server::start(vec![CannedResponse::ok(list_response("abcd5678", "running"))]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "abcd5678").expect("resume should succeed");
        assert!(!outcome.started_from_shutoff, "running state should skip the power-on");

        let paths = fake.seen_paths.lock().expect("paths");
        assert_eq!(paths.len(), 1, "only the server list should have been hit: {paths:?}");
    }
}
