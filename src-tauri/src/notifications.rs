#[tauri::command]
pub(crate) async fn is_macos_debug_build() -> bool {
    cfg!(all(target_os = "macos", debug_assertions))
}

#[tauri::command]
pub(crate) async fn app_build_type() -> String {
    if cfg!(debug_assertions) {
        "debug".to_string()
    } else {
        "release".to_string()
    }
}

/// macOS dev-mode fallback for system notifications.
///
/// In `tauri dev` (debug assertions enabled), the app is typically run as a
/// bare binary instead of a bundled `.app`. macOS notifications can silently
/// fail in that mode because the process does not have a stable bundle
/// identifier registered with the system notification center.
///
/// This fallback uses AppleScript via `osascript` so the developer still gets
/// a visible notification during local development.
#[tauri::command]
pub(crate) async fn send_notification_fallback(title: String, body: String) -> Result<(), String> {
    crate::shared::notifications_core::send_notification_fallback_core(title, body)
}
