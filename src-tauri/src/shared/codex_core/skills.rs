use super::workspace_context::get_session_clone;
use super::workspace_context::resolve_workspace_path_core;
use crate::backend::app_server::session::WorkspaceSession;
use crate::types::WorkspaceEntry;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn skills_list_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let workspace_path = resolve_workspace_path_core(workspaces, &workspace_id).await?;

    // Codex can discover project-scoped skills from `<workspace>/.agents/skills`.
    // Some environments don't surface those reliably in CodexMonitor unless we
    // pass the default project skills path explicitly.
    let mut source_paths: Vec<String> = vec![];
    let project_skills_dir = Path::new(&workspace_path).join(".agents").join("skills");
    if project_skills_dir.is_dir() {
        if let Some(p) = project_skills_dir.to_str() {
            source_paths.push(p.to_string());
        }
    }

    let params = if source_paths.is_empty() {
        json!({ "cwd": workspace_path })
    } else {
        json!({ "cwd": workspace_path, "skillsPaths": source_paths })
    };

    let mut response = session
        .send_request_for_workspace(&workspace_id, "skills/list", params)
        .await?;

    // Attach diagnostics for the UI (non-breaking: keep original response fields).
    if let Value::Object(ref mut obj) = response {
        obj.insert("sourcePaths".to_string(), json!(source_paths));
        obj.insert("sourceErrors".to_string(), json!([]));
    }

    Ok(response)
}
