pub(crate) mod command;
mod event_router;
pub(crate) mod session;
mod thread_identity;
mod thread_list;
mod thread_source;
mod workspace_paths;

use command::{build_codex_command_with_bin, check_codex_installation};
use session::WorkspaceSession;
use workspace_paths::normalize_root_path;

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::backend::events::{AppServerEvent, EventSink};
use crate::shared::process_core::kill_child_process_tree;
use crate::types::WorkspaceEntry;

fn build_initialize_params(client_version: &str) -> Value {
    json!({
        "clientInfo": {
            "name": "codex_monitor",
            "title": "Codex Monitor",
            "version": client_version
        },
        "capabilities": {
            "experimentalApi": true
        }
    })
}

fn validate_initialize_response(response: &Value) -> Result<(), String> {
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Unknown server error");
        return Err(format!(
            "Codex app-server rejected initialization: {message}"
        ));
    }
    if !response.get("result").is_some_and(Value::is_object) {
        return Err("Codex app-server returned an invalid initialization response".to_string());
    }
    Ok(())
}

pub(crate) async fn spawn_workspace_session<E: EventSink>(
    entry: WorkspaceEntry,
    default_codex_bin: Option<String>,
    codex_args: Option<String>,
    codex_home: Option<PathBuf>,
    client_version: String,
    event_sink: E,
) -> Result<Arc<WorkspaceSession>, String> {
    let codex_bin = default_codex_bin;
    let _ = check_codex_installation(codex_bin.clone()).await?;

    let mut command = build_codex_command_with_bin(
        codex_bin,
        codex_args.as_deref(),
        vec!["app-server".to_string()],
    )?;
    command.current_dir(&entry.path);
    if let Some(path) = codex_home.as_ref() {
        command.env("CODEX_HOME", path);
    }
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());

    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("missing stdin")?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;

    let session = Arc::new(WorkspaceSession {
        codex_args,
        child: Mutex::new(child),
        stdin: Mutex::new(stdin),
        activity: Mutex::new(Default::default()),
        pending: Mutex::new(HashMap::new()),
        request_context: Mutex::new(HashMap::new()),
        thread_workspace: Mutex::new(HashMap::new()),
        hidden_thread_ids: Mutex::new(HashSet::new()),
        next_id: AtomicU64::new(1),
        run_recorders: Mutex::new(HashMap::new()),
        background_thread_callbacks: Mutex::new(HashMap::new()),
        owner_workspace_id: entry.id.clone(),
        workspace_ids: Mutex::new(HashSet::from([entry.id.clone()])),
        workspace_roots: Mutex::new(HashMap::from([(
            entry.id.clone(),
            normalize_root_path(&entry.path),
        )])),
    });

    tokio::spawn(event_router::read_stdout(
        stdout,
        Arc::clone(&session),
        entry.id.clone(),
        event_sink.clone(),
    ));

    let workspace_id = entry.id.clone();
    let event_sink_clone = event_sink.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            let payload = AppServerEvent {
                workspace_id: workspace_id.clone(),
                message: json!({
                    "method": "codex/stderr",
                    "params": { "message": line },
                }),
            };
            event_sink_clone.emit_app_server_event(payload);
        }
    });

    let init_params = build_initialize_params(&client_version);
    let init_result = timeout(
        Duration::from_secs(15),
        session.send_request("initialize", init_params),
    )
    .await;
    let init_response = match init_result {
        Ok(response) => response,
        Err(_) => {
            let mut child = session.child.lock().await;
            kill_child_process_tree(&mut child).await;
            return Err(
                "Codex app-server did not respond to initialize. Check that `codex app-server` works in Terminal."
                    .to_string(),
            );
        }
    };
    if let Err(error) = init_response.and_then(|response| validate_initialize_response(&response)) {
        let mut child = session.child.lock().await;
        kill_child_process_tree(&mut child).await;
        return Err(error);
    }
    session.send_notification("initialized", None).await?;

    let payload = AppServerEvent {
        workspace_id: entry.id.clone(),
        message: json!({
            "method": "codex/connected",
            "params": { "workspaceId": entry.id.clone() }
        }),
    };
    event_sink.emit_app_server_event(payload);

    if let Some(path) = event_sink.run_store_path() {
        crate::shared::runs_core::recording::restore(session.clone(), path);
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn initialization_requires_a_successful_rpc_result() {
        assert!(validate_initialize_response(&json!({"result": {"userAgent": "codex"}})).is_ok());
        let error = validate_initialize_response(
            &json!({"error": {"code": -32600, "message": "Unsupported client"}}),
        )
        .unwrap_err();
        assert!(error.contains("Unsupported client"));
        assert!(validate_initialize_response(&json!({"id": 1})).is_err());
    }

    #[test]
    fn build_initialize_params_enables_experimental_api() {
        let params = build_initialize_params("1.2.3");
        assert_eq!(
            params
                .get("capabilities")
                .and_then(|caps| caps.get("experimentalApi"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );
    }
}
