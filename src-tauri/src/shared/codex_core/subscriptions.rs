use super::history;
use super::workspace_context::get_session_clone;
use crate::backend::app_server::session::WorkspaceSession;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) async fn thread_live_subscribe_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    thread_id: String,
) -> Result<(), String> {
    if thread_id.trim().is_empty() {
        return Err("threadId is required".to_string());
    }
    let session = get_session_clone(sessions, &workspace_id).await?;
    // This stdio connection owns every loaded session. A just-created thread
    // has no rollout yet and must not be resumed before its first user turn.
    let mut cursor = None::<String>;
    let mut seen = HashSet::new();
    loop {
        let response = session
            .send_request_for_workspace(
                &workspace_id,
                "thread/loaded/list",
                json!({"cursor": cursor, "limit": 100}),
            )
            .await?;
        let result = history::rpc_result(&response)?;
        if result
            .get("data")
            .and_then(Value::as_array)
            .is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(thread_id.as_str())))
        {
            return Ok(());
        }
        cursor = result
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(next) = cursor.as_ref() else { break };
        if !seen.insert(next.clone()) {
            return Err("Loaded thread pagination repeated a cursor".into());
        }
    }
    let response = session
        .send_request_for_workspace(
            &workspace_id,
            "thread/resume",
            json!({"threadId": thread_id, "excludeTurns": true}),
        )
        .await?;
    history::rpc_result(&response)?;
    Ok(())
}

pub(crate) async fn thread_live_unsubscribe_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    thread_id: String,
) -> Result<(), String> {
    if thread_id.trim().is_empty() {
        return Err("threadId is required".to_string());
    }
    // Detach the foreground view only. The shared connection must continue
    // receiving background turns and approval requests for every workspace.
    let _ = get_session_clone(sessions, &workspace_id).await?;
    Ok(())
}
