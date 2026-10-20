#[cfg(any(target_os = "android", target_os = "ios"))]
mod core;
mod daemon_commands;

use tauri::State;

use crate::state::AppState;
use crate::types::{TailscaleDaemonCommandPreview, TailscaleStatus, TcpDaemonStatus};

#[cfg(any(target_os = "android", target_os = "ios"))]
const UNSUPPORTED_MESSAGE: &str = "Tailscale integration is only available on desktop.";

#[tauri::command]
pub(crate) async fn tailscale_status() -> Result<TailscaleStatus, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Ok(core::unavailable_status(
            None,
            UNSUPPORTED_MESSAGE.to_string(),
        ))
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        crate::shared::managed_daemon_core::tailscale_status().await
    }
}

#[tauri::command]
pub(crate) async fn tailscale_daemon_command_preview(
    state: State<'_, AppState>,
) -> Result<TailscaleDaemonCommandPreview, String> {
    daemon_commands::tailscale_daemon_command_preview(state).await
}

#[tauri::command]
pub(crate) async fn tailscale_daemon_start(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    daemon_commands::tailscale_daemon_start(state).await
}

#[tauri::command]
pub(crate) async fn tailscale_daemon_stop(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    daemon_commands::tailscale_daemon_stop(state).await
}

#[tauri::command]
pub(crate) async fn tailscale_daemon_status(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    daemon_commands::tailscale_daemon_status(state).await
}
