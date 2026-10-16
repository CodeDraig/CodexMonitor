use super::{parse_optional_string, parse_optional_u32, parse_string};
use crate::backend::events::AppServerEvent;
use crate::backend::events::EventSink;
use crate::shared::codex_core;
use crate::shared::runs_core::types::RunContext;
use crate::shared::runs_rpc;
use crate::DaemonState;
use serde_json::json;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "start_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .start_thread(
                        workspace_id,
                        match serde_json::from_value(
                            params.get("runContext").cloned().unwrap_or(Value::Null),
                        ) {
                            Ok(value) => value,
                            Err(e) => return Some(Err(e.to_string())),
                        },
                    )
                    .await,
            )
        }
        "resume_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.resume_thread(workspace_id, thread_id).await)
        }
        "read_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.read_thread(workspace_id, thread_id).await)
        }
        "thread_live_subscribe" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.thread_live_subscribe(workspace_id, thread_id).await)
        }
        "thread_live_unsubscribe" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.thread_live_unsubscribe(workspace_id, thread_id).await)
        }
        "fork_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.fork_thread(workspace_id, thread_id).await)
        }
        "list_threads" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let cursor = parse_optional_string(params, "cursor");
            let limit = parse_optional_u32(params, "limit");
            let sort_key = parse_optional_string(params, "sortKey");
            Some(
                state
                    .list_threads(workspace_id, cursor, limit, sort_key)
                    .await,
            )
        }
        "archive_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.archive_thread(workspace_id, thread_id).await)
        }
        "compact_thread" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.compact_thread(workspace_id, thread_id).await)
        }
        "set_thread_name" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let name = match parse_string(params, "name") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.set_thread_name(workspace_id, thread_id, name).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn start_thread(
        &self,
        workspace_id: String,
        run_context: Option<RunContext>,
    ) -> Result<Value, String> {
        codex_core::threads::start_thread_core(
            runs_rpc::tracking(&self.storage_path, run_context),
            &self.sessions,
            &self.workspaces,
            workspace_id,
        )
        .await
    }

    pub(crate) async fn resume_thread(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::threads::resume_thread_core(&self.sessions, workspace_id, thread_id).await
    }

    pub(crate) async fn read_thread(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::threads::read_thread_core(&self.sessions, workspace_id, thread_id).await
    }

    pub(crate) async fn thread_live_subscribe(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::subscriptions::thread_live_subscribe_core(
            &self.sessions,
            workspace_id.clone(),
            thread_id.clone(),
        )
        .await?;
        let subscription_id = format!("{}:{}", workspace_id, thread_id);
        self.event_sink.emit_app_server_event(AppServerEvent {
            workspace_id: workspace_id.clone(),
            message: json!({
                "method": "thread/live_attached",
                "params": {
                    "workspaceId": workspace_id,
                    "threadId": thread_id,
                    "subscriptionId": subscription_id,
                }
            }),
        });
        Ok(json!({
            "subscriptionId": subscription_id,
            "state": "live",
        }))
    }

    pub(crate) async fn thread_live_unsubscribe(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::subscriptions::thread_live_unsubscribe_core(
            &self.sessions,
            workspace_id.clone(),
            thread_id.clone(),
        )
        .await?;
        self.event_sink.emit_app_server_event(AppServerEvent {
            workspace_id: workspace_id.clone(),
            message: json!({
                "method": "thread/live_detached",
                "params": {
                    "workspaceId": workspace_id,
                    "threadId": thread_id,
                    "reason": "manual",
                }
            }),
        });
        Ok(json!({ "ok": true }))
    }

    pub(crate) async fn fork_thread(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::threads::fork_thread_core(&self.sessions, workspace_id, thread_id).await
    }

    pub(crate) async fn list_threads(
        &self,
        workspace_id: String,
        cursor: Option<String>,
        limit: Option<u32>,
        sort_key: Option<String>,
    ) -> Result<Value, String> {
        codex_core::threads::list_threads_core(
            &self.sessions,
            workspace_id,
            cursor,
            limit,
            sort_key,
        )
        .await
    }

    pub(crate) async fn archive_thread(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::threads::archive_thread_core(&self.sessions, workspace_id, thread_id).await
    }

    pub(crate) async fn compact_thread(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Result<Value, String> {
        codex_core::threads::compact_thread_core(&self.sessions, workspace_id, thread_id).await
    }

    pub(crate) async fn set_thread_name(
        &self,
        workspace_id: String,
        thread_id: String,
        name: String,
    ) -> Result<Value, String> {
        codex_core::threads::set_thread_name_core(&self.sessions, workspace_id, thread_id, name)
            .await
    }
}
