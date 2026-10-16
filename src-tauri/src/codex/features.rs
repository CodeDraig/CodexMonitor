use crate::remote_backend;
use crate::shared::codex_core;
use crate::state::AppState;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub(crate) async fn experimental_feature_list(
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "experimental_feature_list",
            json!({
                "workspaceId": workspace_id,
                "cursor": cursor,
                "limit": limit
            }),
        )
        .await;
    }

    codex_core::features::experimental_feature_list_core(
        &state.sessions,
        workspace_id,
        cursor,
        limit,
    )
    .await
}

#[tauri::command]
pub(crate) async fn set_codex_feature_flag(
    feature_key: String,
    enabled: bool,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    if remote_backend::is_remote_mode(&*state).await {
        remote_backend::call_remote(
            &*state,
            app,
            "set_codex_feature_flag",
            json!({
                "featureKey": feature_key,
                "enabled": enabled
            }),
        )
        .await?;
        return Ok(());
    }

    codex_core::features::set_feature_flag_core(&state.sessions, feature_key.as_str(), enabled)
        .await
}
