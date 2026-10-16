use crate::remote_backend;
use crate::shared::codex_core;
use crate::state::AppState;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub(crate) async fn codex_login(
    workspace_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "codex_login",
            json!({ "workspaceId": workspace_id }),
        )
        .await;
    }

    codex_core::login::codex_login_core(&state.sessions, &state.codex_login_cancels, workspace_id)
        .await
}

#[tauri::command]
pub(crate) async fn codex_login_cancel(
    workspace_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "codex_login_cancel",
            json!({ "workspaceId": workspace_id }),
        )
        .await;
    }

    codex_core::login::codex_login_cancel_core(
        &state.sessions,
        &state.codex_login_cancels,
        workspace_id,
    )
    .await
}
