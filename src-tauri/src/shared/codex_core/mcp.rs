use super::workspace_context::get_session_clone;
use crate::backend::app_server::session::WorkspaceSession;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn list_mcp_server_status_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let params = json!({ "cursor": cursor, "limit": limit });
    session
        .send_request_for_workspace(&workspace_id, "mcpServerStatus/list", params)
        .await
}
