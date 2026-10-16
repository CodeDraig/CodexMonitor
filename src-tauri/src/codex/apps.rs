use crate::remote_backend;
use crate::shared::codex_core;
use crate::state::AppState;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub(crate) async fn apps_list(
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    thread_id: Option<String>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "apps_list",
            json!({
                "workspaceId": workspace_id,
                "cursor": cursor,
                "limit": limit,
                "threadId": thread_id
            }),
        )
        .await;
    }

    codex_core::apps::apps_list_core(&state.sessions, workspace_id, cursor, limit, thread_id).await
}
