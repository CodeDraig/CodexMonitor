use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::shared::ui_state_core::{write_legacy_export, LegacyUiStateSnapshot};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LegacyUiStateExportResult {
    pub(crate) path: String,
}

#[tauri::command]
pub(crate) fn export_legacy_ui_state(
    app: AppHandle,
    snapshot: LegacyUiStateSnapshot,
) -> Result<LegacyUiStateExportResult, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("resolve app data directory: {error}"))?;
    let path = write_legacy_export(data_dir, &snapshot).map_err(|error| error.to_string())?;
    Ok(LegacyUiStateExportResult {
        path: path.to_string_lossy().into_owned(),
    })
}
