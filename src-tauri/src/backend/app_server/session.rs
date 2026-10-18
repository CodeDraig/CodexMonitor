use super::thread_identity::extract_thread_id;
use super::workspace_paths::normalize_root_path;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::timeout;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone)]
pub(crate) struct RequestContext {
    pub(super) workspace_id: String,
    pub(super) method: String,
    pub(super) runtime_events: Option<mpsc::UnboundedSender<Value>>,
}

pub(crate) struct WorkspaceSession {
    pub(crate) codex_args: Option<String>,
    pub(crate) launch_identity: Option<Value>,
    pub(crate) initialization: Mutex<Value>,
    pub(crate) activity: Mutex<crate::shared::session_activity::SessionActivity>,
    pub(crate) child: Mutex<Child>,
    pub(crate) stdin: Mutex<ChildStdin>,
    pub(crate) pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    pub(crate) request_context: Mutex<HashMap<u64, RequestContext>>,
    pub(crate) thread_workspace: Mutex<HashMap<String, String>>,
    pub(crate) hidden_thread_ids: Mutex<HashSet<String>>,
    pub(crate) next_id: AtomicU64,
    pub(crate) runtime_subscribers: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    /// Ordered run evidence queues; consumers perform storage and history work.
    pub(crate) run_recorders: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    /// Callbacks for hidden background threads.
    pub(crate) background_thread_callbacks: Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>,
    pub(crate) owner_workspace_id: String,
    pub(crate) workspace_ids: Mutex<HashSet<String>>,
    pub(crate) workspace_roots: Mutex<HashMap<String, String>>,
}

impl WorkspaceSession {
    pub(crate) async fn register_workspace(&self, workspace_id: &str) {
        self.register_workspace_with_path(workspace_id, None).await;
    }

    pub(crate) async fn register_workspace_with_path(
        &self,
        workspace_id: &str,
        workspace_path: Option<&str>,
    ) {
        self.workspace_ids
            .lock()
            .await
            .insert(workspace_id.to_string());
        if let Some(path) = workspace_path {
            let normalized = normalize_root_path(path);
            if !normalized.is_empty() {
                self.workspace_roots
                    .lock()
                    .await
                    .insert(workspace_id.to_string(), normalized);
            }
        }
    }

    pub(crate) async fn unregister_workspace(&self, workspace_id: &str) {
        self.workspace_ids.lock().await.remove(workspace_id);
        self.workspace_roots.lock().await.remove(workspace_id);
    }

    pub(crate) async fn workspace_ids_snapshot(&self) -> Vec<String> {
        self.workspace_ids.lock().await.iter().cloned().collect()
    }

    async fn write_message(&self, value: Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let mut line = serde_json::to_string(&value).map_err(|e| e.to_string())?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn send_request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.send_request_for_workspace(self.owner_workspace_id.as_str(), method, params)
            .await
    }

    pub(crate) async fn send_request_for_workspace(
        &self,
        workspace_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.send_routed_request(workspace_id, method, params, None).await
    }

    pub(crate) async fn send_runtime_request_for_workspace(
        &self,
        workspace_id: &str,
        method: &str,
        params: Value,
        events: mpsc::UnboundedSender<Value>,
    ) -> Result<Value, String> {
        self.send_routed_request(workspace_id, method, params, Some(events)).await
    }

    async fn send_routed_request(
        &self,
        workspace_id: &str,
        method: &str,
        params: Value,
        runtime_events: Option<mpsc::UnboundedSender<Value>>,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        // This reservation shares a lock with runtime replacement. Keep it even
        // after a timeout/write failure: the server may have accepted the request.
        self.activity.lock().await.begin_request(
            id,
            method,
            extract_thread_id(&json!({ "params": &params })),
        )?;
        let (tx, rx) = oneshot::channel();
        self.register_workspace(workspace_id).await;
        self.pending.lock().await.insert(id, tx);
        self.request_context.lock().await.insert(
            id,
            RequestContext {
                workspace_id: workspace_id.to_string(),
                method: method.to_string(),
                runtime_events,
            },
        );
        if let Some(thread_id) = extract_thread_id(&json!({ "params": params.clone() })) {
            self.thread_workspace
                .lock()
                .await
                .insert(thread_id, workspace_id.to_string());
        }
        if let Err(error) = self
            .write_message(json!({ "id": id, "method": method, "params": params }))
            .await
        {
            self.pending.lock().await.remove(&id);
            self.request_context.lock().await.remove(&id);
            return Err(error);
        }
        match timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err("request canceled".to_string()),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                self.request_context.lock().await.remove(&id);
                Err(format!(
                    "request timed out after {} seconds",
                    REQUEST_TIMEOUT.as_secs()
                ))
            }
        }
    }

    pub(crate) async fn send_notification(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<(), String> {
        let value = if let Some(params) = params {
            json!({ "method": method, "params": params })
        } else {
            json!({ "method": method })
        };
        self.write_message(value).await
    }

    pub(crate) async fn send_response(&self, id: Value, result: Value) -> Result<(), String> {
        self.write_message(json!({ "id": id, "result": result })).await?;
        for subscriber in self.runtime_subscribers.lock().await.values() {
            let _=subscriber.send(json!({"method":"runtime/native_permission_settled","params":{"nativeRequestId":id}}));
        }
        Ok(())
    }
}
