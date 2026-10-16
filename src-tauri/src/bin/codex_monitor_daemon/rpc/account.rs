use super::parse_string;
use crate::shared::codex_core;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "account_rate_limits" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.account_rate_limits(workspace_id).await)
        }
        "account_read" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.account_read(workspace_id).await)
        }
        "codex_login" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.codex_login(workspace_id).await)
        }
        "codex_login_cancel" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.codex_login_cancel(workspace_id).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn account_rate_limits(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::account::account_rate_limits_core(&self.sessions, workspace_id).await
    }

    pub(crate) async fn account_read(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::account::account_read_core(&self.sessions, &self.workspaces, workspace_id).await
    }

    pub(crate) async fn codex_login(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::login::codex_login_core(&self.sessions, &self.codex_login_cancels, workspace_id)
            .await
    }

    pub(crate) async fn codex_login_cancel(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::login::codex_login_cancel_core(
            &self.sessions,
            &self.codex_login_cancels,
            workspace_id,
        )
        .await
    }
}
