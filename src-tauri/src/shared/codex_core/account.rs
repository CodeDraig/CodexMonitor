use super::workspace_context::get_session_clone;
use super::workspace_context::resolve_workspace_and_parent;
use crate::backend::app_server::session::WorkspaceSession;
use crate::codex::home::{resolve_default_codex_home, resolve_workspace_codex_home};
use crate::shared::account::{build_account_response, read_auth_account};
use crate::types::WorkspaceEntry;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn account_rate_limits_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    session
        .send_request_for_workspace(&workspace_id, "account/rateLimits/read", Value::Null)
        .await
}

pub(crate) async fn account_read_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
) -> Result<Value, String> {
    let session = {
        let sessions = sessions.lock().await;
        sessions.get(&workspace_id).cloned()
    };
    let response = if let Some(session) = session {
        session
            .send_request_for_workspace(&workspace_id, "account/read", Value::Null)
            .await
            .ok()
    } else {
        None
    };

    let (entry, parent_entry) = resolve_workspace_and_parent(workspaces, &workspace_id).await?;
    let codex_home = resolve_workspace_codex_home(&entry, parent_entry.as_ref())
        .or_else(resolve_default_codex_home);
    let fallback = read_auth_account(codex_home);

    Ok(build_account_response(response, fallback))
}
