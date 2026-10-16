use super::history;
use super::workspace_context::get_session_clone;
use crate::backend::app_server::session::WorkspaceSession;
use crate::codex::config as codex_config;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn set_feature_flag_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    feature_key: &str,
    enabled: bool,
) -> Result<(), String> {
    codex_config::write_feature_enabled(feature_key, enabled)?;
    if matches!(feature_key, "apps" | "plugins") {
        let mut seen = HashSet::new();
        let active: Vec<_> = sessions
            .lock()
            .await
            .values()
            .filter(|session| seen.insert(Arc::as_ptr(session) as usize))
            .cloned()
            .collect();
        for session in active {
            let response = session.send_request("experimentalFeature/enablement/set", json!({"enablement": {feature_key: enabled}})).await
                .map_err(|error| format!("Feature saved, but runtime update failed: {error}. Restart CodexMonitor to apply it."))?;
            history::rpc_result(&response).map_err(|error| format!("Feature saved, but runtime update failed: {error}. Restart CodexMonitor to apply it."))?;
        }
    }
    Ok(())
}

pub(crate) async fn experimental_feature_list_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let params = json!({ "cursor": cursor, "limit": limit });
    session
        .send_request_for_workspace(&workspace_id, "experimentalFeature/list", params)
        .await
}
