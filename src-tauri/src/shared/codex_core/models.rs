use super::history;
use super::workspace_context::get_session_clone;
use super::workspace_context::resolve_codex_home_for_workspace_core;
use crate::backend::app_server::session::WorkspaceSession;
use crate::codex::config as codex_config;
use crate::types::WorkspaceEntry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn model_list_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let mut data = Vec::new();
    let mut cursor = None::<String>;
    let mut seen = HashSet::new();
    loop {
        let response = session
            .send_request_for_workspace(
                &workspace_id,
                "model/list",
                json!({"cursor": cursor, "limit": 100}),
            )
            .await?;
        let result = history::rpc_result(&response)?;
        let page = result
            .get("data")
            .and_then(Value::as_array)
            .ok_or("Codex model list is missing data")?;
        data.extend(page.iter().cloned());
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(next) = cursor.as_ref() else { break };
        if !seen.insert(next.clone()) {
            return Err("Codex model pagination repeated a cursor".into());
        }
    }
    Ok(json!({"result": {"data": data, "nextCursor": null}}))
}

pub(crate) async fn get_config_model_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
) -> Result<Value, String> {
    let codex_home = resolve_codex_home_for_workspace_core(workspaces, &workspace_id).await?;
    let model = codex_config::read_config_model(Some(codex_home))?;
    Ok(json!({ "model": model }))
}

pub(crate) async fn collaboration_mode_list_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    session
        .send_request_for_workspace(&workspace_id, "collaborationMode/list", json!({}))
        .await
}
