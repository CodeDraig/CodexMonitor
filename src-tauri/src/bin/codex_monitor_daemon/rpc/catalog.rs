use super::{parse_optional_string, parse_optional_u32, parse_string};
use crate::shared::codex_aux_core;
use crate::shared::codex_core;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "get_config_model" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.get_config_model(workspace_id).await)
        }
        "list_mcp_server_status" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let cursor = parse_optional_string(params, "cursor");
            let limit = parse_optional_u32(params, "limit");
            Some(
                state
                    .list_mcp_server_status(workspace_id, cursor, limit)
                    .await,
            )
        }
        "model_list" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.model_list(workspace_id).await)
        }
        "experimental_feature_list" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let cursor = parse_optional_string(params, "cursor");
            let limit = parse_optional_u32(params, "limit");
            Some(
                state
                    .experimental_feature_list(workspace_id, cursor, limit)
                    .await,
            )
        }
        "collaboration_mode_list" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.collaboration_mode_list(workspace_id).await)
        }
        "skills_list" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.skills_list(workspace_id).await)
        }
        "apps_list" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let cursor = parse_optional_string(params, "cursor");
            let limit = parse_optional_u32(params, "limit");
            let thread_id = parse_optional_string(params, "threadId");
            Some(
                state
                    .apps_list(workspace_id, cursor, limit, thread_id)
                    .await,
            )
        }
        "codex_doctor" => {
            let codex_bin = parse_optional_string(params, "codexBin");
            let codex_args = parse_optional_string(params, "codexArgs");
            Some(state.codex_doctor(codex_bin, codex_args).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn list_mcp_server_status(
        &self,
        workspace_id: String,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<Value, String> {
        codex_core::mcp::list_mcp_server_status_core(&self.sessions, workspace_id, cursor, limit)
            .await
    }

    pub(crate) async fn model_list(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::models::model_list_core(&self.sessions, workspace_id).await
    }

    pub(crate) async fn experimental_feature_list(
        &self,
        workspace_id: String,
        cursor: Option<String>,
        limit: Option<u32>,
    ) -> Result<Value, String> {
        codex_core::features::experimental_feature_list_core(
            &self.sessions,
            workspace_id,
            cursor,
            limit,
        )
        .await
    }

    pub(crate) async fn collaboration_mode_list(
        &self,
        workspace_id: String,
    ) -> Result<Value, String> {
        codex_core::models::collaboration_mode_list_core(&self.sessions, workspace_id).await
    }

    pub(crate) async fn skills_list(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::skills::skills_list_core(&self.sessions, &self.workspaces, workspace_id).await
    }

    pub(crate) async fn apps_list(
        &self,
        workspace_id: String,
        cursor: Option<String>,
        limit: Option<u32>,
        thread_id: Option<String>,
    ) -> Result<Value, String> {
        codex_core::apps::apps_list_core(&self.sessions, workspace_id, cursor, limit, thread_id)
            .await
    }

    pub(crate) async fn get_config_model(&self, workspace_id: String) -> Result<Value, String> {
        codex_core::models::get_config_model_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn codex_doctor(
        &self,
        codex_bin: Option<String>,
        codex_args: Option<String>,
    ) -> Result<Value, String> {
        codex_aux_core::codex_doctor_core(&self.app_settings, codex_bin, codex_args).await
    }
}
