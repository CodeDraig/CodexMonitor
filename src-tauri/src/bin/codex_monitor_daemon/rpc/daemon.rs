use super::parse_string;
use crate::DaemonState;
use crate::DAEMON_NAME;
use serde_json::json;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "ping" => Some(Ok(json!({ "ok": true }))),
        "daemon_info" => Some(Ok(state.daemon_info())),
        "daemon_shutdown" => {
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                std::process::exit(0);
            });
            Some(Ok(json!({ "ok": true })))
        }
        "menu_set_accelerators" => {
            let updates: Vec<Value> = match params {
                Value::Object(map) => match map
                    .get("updates")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                {
                    Ok(value) => value.unwrap_or_default(),
                    Err(err) => return Some(Err(err.to_string())),
                },
                _ => Vec::new(),
            };
            Some(
                state
                    .menu_set_accelerators(updates)
                    .await
                    .map(|_| json!({ "ok": true })),
            )
        }
        "is_macos_debug_build" => {
            let is_debug = state.is_macos_debug_build().await;
            Some(Ok(Value::Bool(is_debug)))
        }
        "send_notification_fallback" => {
            let title = match parse_string(params, "title") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let body = match parse_string(params, "body") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .send_notification_fallback(title, body)
                    .await
                    .map(|_| json!({ "ok": true })),
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) fn daemon_info(&self) -> Value {
        json!({
            "name": DAEMON_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
            "mode": "tcp",
            "binaryPath": self.daemon_binary_path,
        })
    }

    pub(crate) async fn menu_set_accelerators(&self, _updates: Vec<Value>) -> Result<(), String> {
        // Daemon has no native menu runtime; treat as no-op for remote parity.
        Ok(())
    }

    pub(crate) async fn is_macos_debug_build(&self) -> bool {
        cfg!(all(target_os = "macos", debug_assertions))
    }

    pub(crate) async fn send_notification_fallback(
        &self,
        title: String,
        body: String,
    ) -> Result<(), String> {
        crate::shared::notifications_core::send_notification_fallback_core(title, body)
    }
}
