use super::workspace_context::get_session_clone;
use crate::backend::app_server::session::WorkspaceSession;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn apps_list_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
    thread_id: Option<String>,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let params = json!({ "cursor": cursor, "limit": limit, "threadId": thread_id });
    session
        .send_request_for_workspace(&workspace_id, "app/list", params)
        .await
}
