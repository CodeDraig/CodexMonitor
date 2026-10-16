use super::workspace_context::get_session_clone;
use super::workspace_context::resolve_codex_home_for_workspace_core;
use crate::backend::app_server::session::WorkspaceSession;
use crate::rules;
use crate::types::WorkspaceEntry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn respond_to_server_request_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    request_id: Value,
    result: Value,
) -> Result<(), String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    session.send_response(request_id, result).await
}

pub(crate) async fn remember_approval_rule_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
    command: Vec<String>,
) -> Result<Value, String> {
    let command = command
        .into_iter()
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if command.is_empty() {
        return Err("empty command".to_string());
    }

    let codex_home = resolve_codex_home_for_workspace_core(workspaces, &workspace_id).await?;
    let rules_path = rules::default_rules_path(&codex_home);
    rules::append_prefix_rule(&rules_path, &command)?;

    Ok(json!({
        "ok": true,
        "rulesPath": rules_path,
    }))
}
