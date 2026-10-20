use super::*;

use crate::daemon_binary::resolve_daemon_binary_path;
use crate::shared::managed_daemon_core as managed;

fn app_data_dir(state: &AppState) -> Result<std::path::PathBuf, String> {
    state
        .settings_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| "Unable to resolve app data directory".to_string())
}

pub(super) async fn tailscale_daemon_command_preview(
    state: State<'_, AppState>,
) -> Result<TailscaleDaemonCommandPreview, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Err(UNSUPPORTED_MESSAGE.to_string());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let daemon_path = resolve_daemon_binary_path()?;
        let data_dir = app_data_dir(&state)?;
        let settings = state.app_settings.lock().await.clone();
        Ok(managed::daemon_command_preview(
            &settings,
            &daemon_path,
            &data_dir,
        ))
    }
}

pub(super) async fn tailscale_daemon_start(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Err("Tailscale daemon start is only supported on desktop.".to_string());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let settings = state.app_settings.lock().await.clone();
        let data_dir = app_data_dir(&state)?;
        let daemon_binary = resolve_daemon_binary_path()?;
        let mut runtime = state.tcp_daemon.lock().await;
        managed::daemon_start(&settings, &data_dir, &daemon_binary, &mut runtime).await
    }
}

pub(super) async fn tailscale_daemon_stop(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Err("Tailscale daemon stop is only supported on desktop.".to_string());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let settings = state.app_settings.lock().await.clone();
        let mut runtime = state.tcp_daemon.lock().await;
        managed::daemon_stop(&settings, &mut runtime).await
    }
}

pub(super) async fn tailscale_daemon_status(
    state: State<'_, AppState>,
) -> Result<TcpDaemonStatus, String> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    return Err("Tailscale daemon status is only supported on desktop.".to_string());

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let settings = state.app_settings.lock().await.clone();
        let mut runtime = state.tcp_daemon.lock().await;
        managed::daemon_status(&settings, &mut runtime).await
    }
}
