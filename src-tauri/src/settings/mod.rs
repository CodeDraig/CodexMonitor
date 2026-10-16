use crate::shared::remote_settings::active_remote_backend;
use tauri::{State, Window};

use crate::shared::settings_core::{
    get_app_settings_core, get_codex_config_path_core, update_app_settings_core,
};
use crate::state::AppState;
use crate::types::{AppSettings, BackendMode};
use crate::window;

#[tauri::command]
pub(crate) async fn get_app_settings(
    state: State<'_, AppState>,
    window: Window,
) -> Result<AppSettings, String> {
    let settings = get_app_settings_core(&state.app_settings).await;
    let _ = window::apply_window_appearance(&window, settings.theme.as_str());
    Ok(settings)
}

#[tauri::command]
pub(crate) async fn update_app_settings(
    settings: AppSettings,
    state: State<'_, AppState>,
    window: Window,
) -> Result<AppSettings, String> {
    let previous = state.app_settings.lock().await.clone();
    let updated =
        update_app_settings_core(settings, &state.app_settings, &state.settings_path).await?;
    if should_reset_remote_backend(&previous, &updated) {
        *state.remote_backend.lock().await = None;
    }
    ensure_remote_runtime_for_settings(&updated, state).await;
    let _ = window::apply_window_appearance(&window, updated.theme.as_str());
    Ok(updated)
}

#[tauri::command]
pub(crate) async fn get_codex_config_path() -> Result<String, String> {
    get_codex_config_path_core()
}

fn should_reset_remote_backend(previous: &AppSettings, updated: &AppSettings) -> bool {
    let backend_mode_changed = !matches!(
        (&previous.backend_mode, &updated.backend_mode),
        (
            crate::types::BackendMode::Local,
            crate::types::BackendMode::Local
        ) | (
            crate::types::BackendMode::Remote,
            crate::types::BackendMode::Remote
        )
    );
    let previous_remote = active_remote_backend(previous);
    let updated_remote = active_remote_backend(updated);
    backend_mode_changed
        || previous_remote.provider != updated_remote.provider
        || previous_remote.host != updated_remote.host
        || previous_remote.token != updated_remote.token
}

async fn ensure_remote_runtime_for_settings(settings: &AppSettings, state: State<'_, AppState>) {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return;
    }
    if !matches!(settings.backend_mode, BackendMode::Remote) {
        return;
    }

    let _ = crate::tailscale::tailscale_daemon_start(state).await;
}

#[cfg(test)]
mod tests {
    use super::should_reset_remote_backend;
    use crate::types::{AppSettings, BackendMode};

    #[test]
    fn should_reset_remote_backend_when_host_changes() {
        let previous = AppSettings::default();
        let mut updated = previous.clone();
        updated.remote_backends[0].host = "remote.example:4732".to_string();
        assert!(should_reset_remote_backend(&previous, &updated));
    }

    #[test]
    fn should_reset_remote_backend_when_transport_token_changes() {
        let previous = AppSettings::default();
        let mut updated = previous.clone();
        updated.remote_backends[0].token = Some("token-1".to_string());
        assert!(should_reset_remote_backend(&previous, &updated));
    }

    #[test]
    fn should_not_reset_remote_backend_for_non_transport_setting_changes() {
        let previous = AppSettings::default();
        let mut updated = previous.clone();
        updated.theme = "dark".to_string();
        updated.backend_mode = BackendMode::Local;
        assert!(!should_reset_remote_backend(&previous, &updated));
    }

    #[test]
    fn resets_for_selection_changes_but_not_inactive_entry_or_timestamp_changes() {
        let mut previous = AppSettings::default();
        previous.remote_backends.push(crate::types::RemoteBackendTarget {
            id: "second".into(),
            host: "second:5732".into(),
            token: Some("token".into()),
            ..crate::types::RemoteBackendTarget::default()
        });
        let mut updated = previous.clone();
        updated.remote_backends[1].host = "inactive:6732".into();
        updated.remote_backends[0].last_connected_at_ms = Some(123);
        assert!(!should_reset_remote_backend(&previous, &updated));
        updated.active_remote_backend_id = Some("second".into());
        assert!(should_reset_remote_backend(&previous, &updated));
    }
}
