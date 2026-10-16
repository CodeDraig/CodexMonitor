use crate::remote_backend;
use crate::shared::codex_core;
use crate::state::AppState;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub(crate) async fn respond_to_server_request(
    workspace_id: String,
    request_id: Value,
    result: Value,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    if remote_backend::is_remote_mode(&*state).await {
        remote_backend::call_remote(
            &*state,
            app,
            "respond_to_server_request",
            json!({ "workspaceId": workspace_id, "requestId": request_id, "result": result }),
        )
        .await?;
        return Ok(());
    }

    codex_core::approvals::respond_to_server_request_core(
        &state.sessions,
        workspace_id,
        request_id,
        result,
    )
    .await
}

#[tauri::command]
pub(crate) async fn remember_approval_rule(
    workspace_id: String,
    command: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    codex_core::approvals::remember_approval_rule_core(&state.workspaces, workspace_id, command)
        .await
}
