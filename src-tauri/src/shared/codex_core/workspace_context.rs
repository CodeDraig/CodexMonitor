use crate::backend::app_server::session::WorkspaceSession;
use crate::codex::home::{resolve_default_codex_home, resolve_workspace_codex_home};
use crate::types::WorkspaceEntry;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(super) async fn get_session_clone(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: &str,
) -> Result<Arc<WorkspaceSession>, String> {
    let sessions = sessions.lock().await;
    sessions
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| "workspace not connected".to_string())
}

pub(super) async fn resolve_workspace_and_parent(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: &str,
) -> Result<(WorkspaceEntry, Option<WorkspaceEntry>), String> {
    let workspaces = workspaces.lock().await;
    let entry = workspaces
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| "workspace not found".to_string())?;
    let parent_entry = entry
        .parent_id
        .as_ref()
        .and_then(|parent_id| workspaces.get(parent_id))
        .cloned();
    Ok((entry, parent_entry))
}

pub(super) async fn resolve_codex_home_for_workspace_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: &str,
) -> Result<PathBuf, String> {
    let (entry, parent_entry) = resolve_workspace_and_parent(workspaces, workspace_id).await?;
    resolve_workspace_codex_home(&entry, parent_entry.as_ref())
        .or_else(resolve_default_codex_home)
        .ok_or_else(|| "Unable to resolve CODEX_HOME".to_string())
}

pub(super) async fn resolve_workspace_path_core(
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: &str,
) -> Result<String, String> {
    let workspaces = workspaces.lock().await;
    let entry = workspaces
        .get(workspace_id)
        .ok_or_else(|| "workspace not found".to_string())?;
    Ok(entry.path.clone())
}
