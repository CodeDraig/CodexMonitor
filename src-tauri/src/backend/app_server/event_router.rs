use super::session::WorkspaceSession;
use super::thread_identity::{extract_related_thread_ids, extract_thread_id};
use super::thread_list::extract_thread_entries_from_thread_list_result;
use super::thread_source::thread_started_is_memory_consolidation;
use super::workspace_paths::resolve_workspace_for_cwd;
use crate::backend::events::{AppServerEvent, EventSink};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStdout;

pub(super) fn should_suppress_hidden_thread_event(
    method_name: Option<&str>,
    has_result_or_error: bool,
) -> bool {
    !has_result_or_error
        && !matches!(
            method_name,
            Some("thread/archived") | Some("codex/backgroundThread")
        )
}

fn is_global_workspace_notification(method: &str) -> bool {
    matches!(
        method,
        "account/updated" | "account/rateLimits/updated" | "account/login/completed"
    )
}

fn should_broadcast_global_workspace_notification(
    method_name: Option<&str>,
    thread_id: Option<&String>,
    request_workspace: Option<&str>,
) -> bool {
    method_name.is_some_and(is_global_workspace_notification)
        && thread_id.is_none()
        && request_workspace.is_none()
}

pub(super) async fn read_stdout<E: EventSink>(
    stdout: ChildStdout,
    session: Arc<WorkspaceSession>,
    fallback_workspace_id: String,
    event_sink: E,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(err) => {
                let payload = AppServerEvent {
                    workspace_id: fallback_workspace_id.clone(),
                    message: json!({
                        "method": "codex/parseError",
                        "params": { "error": err.to_string(), "raw": line },
                    }),
                };
                event_sink.emit_app_server_event(payload);
                continue;
            }
        };

        for subscriber in session.runtime_subscribers.lock().await.values() {
            let _ = subscriber.send(value.clone());
        }
        let maybe_id = value.get("id").and_then(|id| id.as_u64());
        let has_method = value.get("method").is_some();
        let has_result_or_error = value.get("result").is_some() || value.get("error").is_some();
        let method_name = value.get("method").and_then(|method| method.as_str());

        // Check if this event is for a background thread
        let thread_id = extract_thread_id(&value);
        session
            .activity
            .lock()
            .await
            .observe(&value, thread_id.clone());
        let mut request_workspace: Option<String> = None;
        let mut request_method: Option<String> = None;
        if let Some(id) = maybe_id {
            if has_result_or_error {
                if let Some(context) = session.request_context.lock().await.remove(&id) {
                    // Bind runtime ownership on the reader before delivering the
                    // reply, so immediately following thread notifications cannot
                    // race into the legacy conversation path.
                    if let (Some(events), Some(tid)) = (context.runtime_events, thread_id.as_ref()) {
                        session.runtime_subscribers.lock().await.insert(tid.clone(), events);
                    }
                    request_workspace = Some(context.workspace_id);
                    request_method = Some(context.method);
                }
            }
        }

        if let Some(ref workspace_id) = request_workspace {
            let related_thread_ids = extract_related_thread_ids(&value);
            if !related_thread_ids.is_empty() {
                let mut thread_workspace = session.thread_workspace.lock().await;
                for tid in related_thread_ids {
                    thread_workspace.insert(tid, workspace_id.clone());
                }
            } else if let Some(ref tid) = thread_id {
                session
                    .thread_workspace
                    .lock()
                    .await
                    .insert(tid.clone(), workspace_id.clone());
            }
        }
        if matches!(request_method.as_deref(), Some("thread/list")) {
            let thread_entries = extract_thread_entries_from_thread_list_result(&value);
            if !thread_entries.is_empty() {
                let workspace_roots = session.workspace_roots.lock().await.clone();
                let mut hidden_thread_ids = Vec::new();
                let mut thread_workspace = session.thread_workspace.lock().await;
                for entry in thread_entries {
                    if entry.is_memory_consolidation {
                        thread_workspace.remove(&entry.thread_id);
                        hidden_thread_ids.push(entry.thread_id);
                        continue;
                    }
                    let mapped_workspace = entry
                        .cwd
                        .as_deref()
                        .and_then(|cwd| resolve_workspace_for_cwd(cwd, &workspace_roots));
                    if let Some(workspace_id) = mapped_workspace {
                        thread_workspace.insert(entry.thread_id, workspace_id);
                    }
                }
                drop(thread_workspace);
                if !hidden_thread_ids.is_empty() {
                    let mut hidden = session.hidden_thread_ids.lock().await;
                    for thread_id in hidden_thread_ids {
                        hidden.insert(thread_id);
                    }
                }
            }
        }

        if matches!(
            method_name,
            Some(
                "turn/started"
                    | "turn/completed"
                    | "item/completed"
                    | "thread/tokenUsage/updated"
                    | "model/rerouted"
            )
        ) {
            if let Some(ref tid) = thread_id {
                if let Some(recorder) = session.run_recorders.lock().await.get(tid) {
                    let _ = recorder.send(value.clone());
                }
            }
        }

        let mapped_thread_workspace = if let Some(ref tid) = thread_id {
            session.thread_workspace.lock().await.get(tid).cloned()
        } else {
            None
        };

        let routed_workspace_id = mapped_thread_workspace
            .or_else(|| request_workspace.clone())
            .unwrap_or_else(|| fallback_workspace_id.clone());

        if let Some(ref tid) = thread_id {
            if method_name == Some("codex/backgroundThread") {
                let action = value
                    .get("params")
                    .and_then(|params| params.get("action"))
                    .and_then(Value::as_str)
                    .unwrap_or("hide");
                if action.eq_ignore_ascii_case("hide") {
                    session.hidden_thread_ids.lock().await.insert(tid.clone());
                }
            } else if method_name == Some("thread/started")
                && thread_started_is_memory_consolidation(&value)
            {
                session.hidden_thread_ids.lock().await.insert(tid.clone());
                let payload = AppServerEvent {
                    workspace_id: routed_workspace_id.clone(),
                    message: json!({
                        "method": "codex/backgroundThread",
                        "params": {
                            "threadId": tid,
                            "action": "hide"
                        }
                    }),
                };
                event_sink.emit_app_server_event(payload);
                continue;
            }

            let should_suppress_hidden_thread = {
                let hidden = session.hidden_thread_ids.lock().await;
                hidden.contains(tid)
            };
            if should_suppress_hidden_thread
                && should_suppress_hidden_thread_event(method_name, has_result_or_error)
            {
                continue;
            }
        }

        if matches!(method_name, Some("item/started") | Some("item/completed")) {
            let related_thread_ids = extract_related_thread_ids(&value);
            if !related_thread_ids.is_empty() {
                let mut thread_workspace = session.thread_workspace.lock().await;
                for related_id in related_thread_ids {
                    thread_workspace
                        .entry(related_id)
                        .or_insert_with(|| routed_workspace_id.clone());
                }
            }
        }

        if method_name == Some("thread/archived") {
            if let Some(ref tid) = thread_id {
                session.thread_workspace.lock().await.remove(tid);
                session.hidden_thread_ids.lock().await.remove(tid);
            }
        }

        if let Some(id) = maybe_id {
            if has_result_or_error || !has_method {
                if let Some(tx) = session.pending.lock().await.remove(&id) {
                    let _ = tx.send(value);
                }
                continue;
            }
        }
        if has_method {
            // Check for background thread callback
            let mut sent_to_background = false;
            if let Some(ref tid) = thread_id {
                let callbacks = session.background_thread_callbacks.lock().await;
                if let Some(tx) = callbacks.get(tid) {
                    let _ = tx.send(value.clone());
                    sent_to_background = true;
                }
            }
            // Runtime conversations consume the durable runtime channel. Keep extension
            // requests outside the common permission contract on the existing Codex UI.
            let runtime_owned = if let Some(ref tid) = thread_id {
                session.runtime_subscribers.lock().await.contains_key(tid)
            } else { false };
            let extension_request = value.get("id").is_some() && !matches!(method_name,
                Some("item/commandExecution/requestApproval") | Some("item/fileChange/requestApproval"));
            if !sent_to_background && (!runtime_owned || extension_request) {
                if should_broadcast_global_workspace_notification(
                    method_name,
                    thread_id.as_ref(),
                    request_workspace.as_deref(),
                ) {
                    let workspace_ids = session.workspace_ids_snapshot().await;
                    if workspace_ids.is_empty() {
                        let payload = AppServerEvent {
                            workspace_id: routed_workspace_id,
                            message: value,
                        };
                        event_sink.emit_app_server_event(payload);
                    } else {
                        for workspace_id in workspace_ids {
                            let payload = AppServerEvent {
                                workspace_id,
                                message: value.clone(),
                            };
                            event_sink.emit_app_server_event(payload);
                        }
                    }
                } else {
                    let payload = AppServerEvent {
                        workspace_id: routed_workspace_id,
                        message: value,
                    };
                    event_sink.emit_app_server_event(payload);
                }
            }
        }
    }

    // Ensure pending foreground requests cannot accumulate after process output ends.
    session.pending.lock().await.clear();
    session.request_context.lock().await.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_thread_suppression_allows_rpc_responses() {
        assert!(!should_suppress_hidden_thread_event(
            Some("thread/archived"),
            true
        ));
        assert!(!should_suppress_hidden_thread_event(
            Some("thread/updated"),
            true
        ));
        assert!(!should_suppress_hidden_thread_event(None, true));
    }

    #[test]
    fn hidden_thread_suppression_still_blocks_non_exempt_notifications() {
        assert!(should_suppress_hidden_thread_event(
            Some("thread/updated"),
            false
        ));
        assert!(!should_suppress_hidden_thread_event(
            Some("thread/archived"),
            false
        ));
        assert!(!should_suppress_hidden_thread_event(
            Some("codex/backgroundThread"),
            false
        ));
    }
}
