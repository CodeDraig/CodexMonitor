use super::{parse_string, parse_string_array};
use crate::shared::codex_core;
use crate::DaemonState;
use serde_json::json;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "respond_to_server_request" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let map = match params.as_object().ok_or("missing requestId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err.to_string())),
            };
            let request_id = match map
                .get("requestId")
                .cloned()
                .filter(|value| value.is_number() || value.is_string())
                .ok_or("missing requestId")
            {
                Ok(value) => value,
                Err(err) => return Some(Err(err.to_string())),
            };
            let result = match map.get("result").cloned().ok_or("missing `result`") {
                Ok(value) => value,
                Err(err) => return Some(Err(err.to_string())),
            };
            Some(
                state
                    .respond_to_server_request(workspace_id, request_id, result)
                    .await,
            )
        }
        "remember_approval_rule" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let command = match parse_string_array(params, "command") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.remember_approval_rule(workspace_id, command).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn respond_to_server_request(
        &self,
        workspace_id: String,
        request_id: Value,
        result: Value,
    ) -> Result<Value, String> {
        codex_core::approvals::respond_to_server_request_core(
            &self.sessions,
            workspace_id,
            request_id,
            result,
        )
        .await?;
        Ok(json!({ "ok": true }))
    }

    pub(crate) async fn remember_approval_rule(
        &self,
        workspace_id: String,
        command: Vec<String>,
    ) -> Result<Value, String> {
        codex_core::approvals::remember_approval_rule_core(&self.workspaces, workspace_id, command)
            .await
    }
}
