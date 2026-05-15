//! `tartarus stop` against Hetzner Cloud.
//!
//! Graceful first (`POST /servers/{id}/actions/shutdown`); on
//! timeout, fall through to `poweroff` and report `force_stopped`.

use tartarus_provider::StopOutcome;

use crate::{
    Result,
    api::{Client, servers},
    session::lifecycle,
};

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

/// How long to wait for the ACPI shutdown action to finish.
const GRACEFUL_TIMEOUT_SECS: u64 = 60;

// -----------------------------------------------------------------------------
// Entry Point
// -----------------------------------------------------------------------------

/// Run `tartarus stop <alias|uuid>` against Hetzner.
pub fn run(client: &Client, target: &str) -> Result<StopOutcome> {
    let server = lifecycle::find_server(client, target)?;
    let display = server
        .labels
        .get(crate::session::labels::LABEL_ALIAS)
        .cloned()
        .unwrap_or_else(|| target.to_owned());

    let shutdown_action = shutdown(client, server.id)?;
    match lifecycle::wait_for_with_timeout(client, shutdown_action.id, "server shutdown", GRACEFUL_TIMEOUT_SECS) {
        Ok(()) => Ok(StopOutcome {
            force_stopped: false,
            name: display,
        }),
        Err(_) => {
            tracing::warn!(
                server_id = server.id,
                "Hetzner server did not shut down gracefully; forcing poweroff",
            );
            let force = power_off(client, server.id)?;
            lifecycle::wait_for(client, force.id, "server poweroff")?;
            Ok(StopOutcome {
                force_stopped: true,
                name: display,
            })
        },
    }
}

// -----------------------------------------------------------------------------
// Power Actions
// -----------------------------------------------------------------------------

/// Generic action-envelope for `actions/shutdown` and `actions/poweroff`.
#[derive(serde::Deserialize)]
struct PowerResponse {
    action: servers::Action,
}

/// `POST /servers/{id}/actions/shutdown`.
fn shutdown(client: &Client, id: u64) -> Result<servers::Action> {
    let response: PowerResponse = client.post(
        "POST /servers/{id}/actions/shutdown",
        &format!("/servers/{id}/actions/shutdown"),
        &serde_json::json!({}),
    )?;
    Ok(response.action)
}

/// `POST /servers/{id}/actions/poweroff`.
fn power_off(client: &Client, id: u64) -> Result<servers::Action> {
    let response: PowerResponse = client.post(
        "POST /servers/{id}/actions/poweroff",
        &format!("/servers/{id}/actions/poweroff"),
        &serde_json::json!({}),
    )?;
    Ok(response.action)
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

    fn list_response(uuid: &str, alias: Option<&str>) -> String {
        let alias_line = alias
            .map(|a| format!(r#""{key}": "{a}","#, key = labels::LABEL_ALIAS))
            .unwrap_or_default();
        format!(
            r#"{{
                "servers": [{{
                    "id": 21,
                    "name": "tartarus-y",
                    "status": "running",
                    "labels": {{
                        {alias_line}
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
    fn stop_uses_graceful_shutdown_when_it_succeeds() {
        let fake = Server::start(vec![
            CannedResponse::ok(list_response("abcd1111", Some("fix-bug"))),
            CannedResponse::ok(r#"{"action":{"id":31,"status":"running"}}"#),
            CannedResponse::ok(r#"{"action":{"id":31,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "fix-bug").expect("stop should succeed");
        assert!(
            !outcome.force_stopped,
            "graceful path should report force_stopped=false"
        );
        assert_eq!(outcome.name, "fix-bug");

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(paths[1].contains("POST /servers/21/actions/shutdown"));
    }

    #[test]
    fn stop_falls_back_to_poweroff_when_shutdown_errors() {
        // The graceful action immediately reports `error`, so wait_for
        // returns ActionFailed which the stop logic catches and
        // falls through to the poweroff path.
        let fake = Server::start(vec![
            CannedResponse::ok(list_response("abcd2222", None)),
            CannedResponse::ok(r#"{"action":{"id":41,"status":"running"}}"#),
            CannedResponse::ok(
                r#"{"action":{"id":41,"status":"error","error":{"code":"timeout","message":"guest unresponsive"}}}"#,
            ),
            CannedResponse::ok(r#"{"action":{"id":42,"status":"running"}}"#),
            CannedResponse::ok(r#"{"action":{"id":42,"status":"success"}}"#),
        ]);
        let client = Client::with_base_url("tok", &fake.base_url);

        let outcome = run(&client, "abcd2222").expect("force-stop path should succeed");
        assert!(outcome.force_stopped, "fallback path should report force_stopped=true");

        let paths = fake.seen_paths.lock().expect("paths");
        assert!(paths.iter().any(|p| p.contains("/actions/poweroff")));
    }
}
