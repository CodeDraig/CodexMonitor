use crate::backend::events::AppServerEvent;
use crate::remote_backend;
use crate::shared::codex_core;
use crate::state::AppState;
use serde_json::json;
use serde_json::Value;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::State;

fn emit_thread_live_event(app: &AppHandle, workspace_id: &str, method: &str, params: Value) {
    let _ = app.emit(
        "app-server-event",
        AppServerEvent {
            workspace_id: workspace_id.to_string(),
            message: json!({
                "method": method,
                "params": params,
            }),
        },
    );
}

#[tauri::command]
pub(crate) async fn thread_live_subscribe(
    workspace_id: String,
    thread_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "thread_live_subscribe",
            json!({ "workspaceId": workspace_id, "threadId": thread_id }),
        )
        .await;
    }

    codex_core::subscriptions::thread_live_subscribe_core(
        &state.sessions,
        workspace_id.clone(),
        thread_id.clone(),
    )
    .await?;
    let subscription_id = format!("{}:{}", workspace_id, thread_id);
    emit_thread_live_event(
        &app,
        &workspace_id,
        "thread/live_attached",
        json!({
            "workspaceId": workspace_id,
            "threadId": thread_id,
            "subscriptionId": subscription_id,
        }),
    );
    Ok(json!({
        "subscriptionId": subscription_id,
        "state": "live",
    }))
}

#[tauri::command]
pub(crate) async fn thread_live_unsubscribe(
    workspace_id: String,
    thread_id: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, String> {
    if remote_backend::is_remote_mode(&*state).await {
        return remote_backend::call_remote(
            &*state,
            app,
            "thread_live_unsubscribe",
            json!({ "workspaceId": workspace_id, "threadId": thread_id }),
        )
        .await;
    }

    codex_core::subscriptions::thread_live_unsubscribe_core(
        &state.sessions,
        workspace_id.clone(),
        thread_id.clone(),
    )
    .await?;
    emit_thread_live_event(
        &app,
        &workspace_id,
        "thread/live_detached",
        json!({
            "workspaceId": workspace_id,
            "threadId": thread_id,
            "reason": "manual",
        }),
    );
    Ok(json!({ "ok": true }))
}
