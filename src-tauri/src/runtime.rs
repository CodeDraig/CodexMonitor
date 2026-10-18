use crate::{
    event_sink::TauriEventSink,
    remote_backend,
    shared::{agent_runtime::types::RuntimeError, runs_core, runtime_rpc},
    state::AppState,
};
use serde_json::Value;
use tauri::{AppHandle, State};
#[tauri::command]
pub(crate) async fn runtime_request(
    method: String,
    input: Value,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<Value, RuntimeError> {
    if remote_backend::is_remote_mode(&state).await {
        return remote_backend::call_remote(
            &state,
            app,
            "runtime_request",
            serde_json::json!({"method":method,"input":input}),
        )
        .await
        .map_err(Into::into);
    }
    runtime_rpc::dispatch(
        &method,
        input,
        runs_core::db_path(&state.storage_path),
        &state.runtime_registry,
        &state.workspaces,
        &state.sessions,
        &state.app_settings,
        TauriEventSink::new(app),
    )
    .await
    .map_err(Into::into)
}
