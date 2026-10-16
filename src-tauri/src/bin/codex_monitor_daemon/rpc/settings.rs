use super::{parse_optional_bool, parse_string, serialize_result, serialize_value};
use crate::shared::codex_core;
use crate::shared::settings_core;
use crate::types::AppSettings;
use crate::DaemonState;
use serde_json::json;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "get_app_settings" => Some(serialize_value(state.get_app_settings().await)),
        "update_app_settings" => {
            let settings_value = match params {
                Value::Object(map) => map.get("settings").cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            };
            let settings: AppSettings = match serde_json::from_value(settings_value) {
                Ok(value) => value,
                Err(err) => return Some(Err(err.to_string())),
            };
            Some(serialize_result(state.update_app_settings(settings)).await)
        }
        "get_codex_config_path" => {
            let path = match settings_core::get_codex_config_path_core() {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(Ok(Value::String(path)))
        }
        "set_codex_feature_flag" => {
            let feature_key = match parse_string(params, "featureKey") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let enabled = match parse_optional_bool(params, "enabled") {
                Some(value) => value,
                None => return Some(Err("missing or invalid `enabled`".to_string())),
            };
            Some(
                state
                    .set_codex_feature_flag(feature_key, enabled)
                    .await
                    .map(|_| json!({ "ok": true })),
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn get_app_settings(&self) -> AppSettings {
        settings_core::get_app_settings_core(&self.app_settings).await
    }

    pub(crate) async fn update_app_settings(
        &self,
        settings: AppSettings,
    ) -> Result<AppSettings, String> {
        settings_core::update_app_settings_core(settings, &self.app_settings, &self.settings_path)
            .await
    }

    pub(crate) async fn set_codex_feature_flag(
        &self,
        feature_key: String,
        enabled: bool,
    ) -> Result<(), String> {
        codex_core::features::set_feature_flag_core(&self.sessions, feature_key.as_str(), enabled)
            .await
    }
}
