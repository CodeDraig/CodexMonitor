use super::parse_string;
use crate::events::emit_background_thread_hide;
use crate::shared::codex_aux_core;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "generate_run_metadata" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let prompt = match parse_string(params, "prompt") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.generate_run_metadata(workspace_id, prompt).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn generate_run_metadata(
        &self,
        workspace_id: String,
        prompt: String,
    ) -> Result<Value, String> {
        codex_aux_core::generate_run_metadata_core(
            &self.sessions,
            &self.workspaces,
            workspace_id,
            &prompt,
            |workspace_id, thread_id| {
                emit_background_thread_hide(&self.event_sink, workspace_id, thread_id);
            },
        )
        .await
    }
}
