use crate::state::AppState;
use serde_json::Value;
use tauri::State;

#[tauri::command]
pub(crate) async fn codex_doctor(
    codex_bin: Option<String>,
    codex_args: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    crate::shared::codex_aux_core::codex_doctor_core(&state.app_settings, codex_bin, codex_args)
        .await
}

#[tauri::command]
pub(crate) async fn codex_update(
    codex_bin: Option<String>,
    codex_args: Option<String>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    crate::shared::codex_update_core::codex_update_core(&state.app_settings, codex_bin, codex_args)
        .await
}
