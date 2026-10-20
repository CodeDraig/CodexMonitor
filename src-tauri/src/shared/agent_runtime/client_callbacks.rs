//! Backend-owned ACP callbacks. Authentication interactions remain transient.

use super::{
    client_files::AcpFileService,
    client_store::{AcpCallbackState, AcpTerminalSnapshot},
    client_terminals::AcpTerminalService,
    registry::persist,
    types::{AcpCommandExecutionPolicy, AcpFilesystemPolicy, RuntimeProfile},
};
use crate::backend::acp::{AcpRequestHandler, AcpWireError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{oneshot, Mutex};

struct Pending {
    summary: Value,
    reply: oneshot::Sender<Value>,
}

#[derive(Clone)]
pub(crate) struct ClientCallbacks {
    pub connection_id: String,
    pub generation: u64,
    pub profile: RuntimeProfile,
    pub path: PathBuf,
    pub cwd: PathBuf,
    pub discovery: bool,
    pub session_id: Arc<Mutex<Option<String>>>,
    pub native_id: Arc<Mutex<Option<String>>>,
    pub terminals: Arc<AcpTerminalService>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    elicitation_ids: Arc<Mutex<HashSet<String>>>,
    terminal_close: Arc<Mutex<()>>,
    closed: Arc<AtomicBool>,
    files: Option<Arc<AcpFileService>>,
    workspace_change_events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>>,
    allowed_roots: Vec<PathBuf>,
    pub auth_state: Arc<Mutex<String>>,
}

fn wire_error(message: impl Into<String>) -> AcpWireError {
    AcpWireError {
        code: -32001,
        message: message.into(),
        data: None,
    }
}

impl ClientCallbacks {
    pub fn new(
        connection_id: String,
        generation: u64,
        profile: RuntimeProfile,
        path: PathBuf,
        cwd: PathBuf,
        discovery: bool,
    ) -> Result<Self, String> {
        let canonical_cwd = cwd
            .canonicalize()
            .map_err(|error| format!("invalid_input: ACP workspace root: {error}"))?;
        if !canonical_cwd.is_dir() {
            return Err("invalid_input: ACP workspace root is not a directory".into());
        }
        let mut allowed_roots = vec![canonical_cwd.clone()];
        for root in &profile.client_policy.additional_roots {
            let path = PathBuf::from(root);
            if !path.is_absolute() {
                return Err(format!(
                    "invalid_input: ACP additional root must be absolute: {root}"
                ));
            }
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("invalid_input: ACP additional root {root}: {error}"))?;
            if !canonical.is_dir() {
                return Err(format!(
                    "invalid_input: ACP additional root is not a directory: {root}"
                ));
            }
            if !allowed_roots.contains(&canonical) {
                allowed_roots.push(canonical);
            }
        }
        let files = if profile.client_policy.filesystem == AcpFilesystemPolicy::Disabled
            || discovery
        {
            None
        } else {
            let backup_path = path.with_file_name("acp-file-backups");
            let is_inside_root = allowed_roots
                .iter()
                .any(|root| backup_path.starts_with(root));
            let backup_path = if profile.client_policy.filesystem == AcpFilesystemPolicy::ReadOnly
                || is_inside_root
            {
                if is_inside_root
                    && profile.client_policy.filesystem == AcpFilesystemPolicy::ReadWrite
                {
                    return Err("invalid_input: read-write ACP callbacks require the run store and file backups to be outside approved roots".into());
                }
                // Read-only mode never writes backups. Give its file service an
                // isolated non-workspace lock directory instead of blocking
                // legacy workspaces whose app-data path lies under the root.
                std::env::temp_dir()
                    .join("codexmonitor-acp-readonly-locks")
                    .join(&connection_id)
            } else {
                backup_path
            };
            Some(Arc::new(AcpFileService::new(
                canonical_cwd.clone(),
                allowed_roots.iter().skip(1).cloned().collect(),
                backup_path,
            )?))
        };
        Ok(Self {
            connection_id,
            generation,
            profile,
            path,
            cwd: canonical_cwd,
            discovery,
            session_id: Default::default(),
            native_id: Default::default(),
            terminals: Arc::new(AcpTerminalService::new()),
            pending: Default::default(),
            elicitation_ids: Default::default(),
            terminal_close: Default::default(),
            closed: Default::default(),
            files,
            workspace_change_events: Default::default(),
            allowed_roots,
            auth_state: Arc::new(Mutex::new("unknown".into())),
        })
    }

    /// Advertise only callbacks implemented on this build and allowed by the
    /// profile, using each ACP capability's defined marker shape.
    pub fn capabilities(&self) -> Value {
        let filesystem_enabled = cfg!(unix)
            && !self.discovery
            && self.profile.client_policy.filesystem != AcpFilesystemPolicy::Disabled;
        let mut capabilities = json!({
            "fs": {
                "readTextFile": filesystem_enabled,
                "writeTextFile": filesystem_enabled
                    && !self.discovery
                    && self.profile.client_policy.filesystem == AcpFilesystemPolicy::ReadWrite
            },
            // Interactive authentication is provided by ConnectionManager's
            // separate login PTY service, not the command-terminal callbacks.
            "auth": {"terminal": cfg!(all(
                feature = "acp-host-services",
                not(any(target_os = "ios", target_os = "android"))
            ))},
            "elicitation": {"url": {}},
            "session": {"compaction": {}}
        });
        if command_terminal_callbacks_enabled()
            && !self.discovery
            && self.profile.client_policy.command_execution != AcpCommandExecutionPolicy::Disabled
        {
            capabilities["terminal"] = json!(true);
        }
        capabilities
    }

    /// Route successful filesystem writes through the foreground runtime event
    /// recorder. Discovery and pre-session connections never receive this sender.
    pub(crate) async fn set_workspace_change_events(
        &self,
        sender: tokio::sync::mpsc::UnboundedSender<Value>,
    ) {
        *self.workspace_change_events.lock().await = Some(sender);
    }

    pub async fn pending(&self) -> Vec<Value> {
        self.pending
            .lock()
            .await
            .values()
            .map(|pending| pending.summary.clone())
            .collect()
    }

    pub async fn respond(&self, id: &str, decision: Value) -> Result<Value, String> {
        let pending = self
            .pending
            .lock()
            .await
            .remove(id)
            .ok_or("invalid_input: pending client request not found")?;
        pending
            .reply
            .send(decision)
            .map_err(|_| "transport_lost: client request no longer pending".to_string())?;
        Ok(json!({"acknowledged": true}))
    }

    async fn decision(&self, method: &str, params: &Value) -> Result<Value, AcpWireError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(wire_error("transport_lost: connection closed"));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let summary = if method == "elicitation/create" {
            json!({
                "clientRequestId": id,
                "kind": "url",
                "method": method,
                "url": params["url"],
                "message": params["message"],
                "elicitationId": params["elicitationId"]
            })
        } else {
            json!({
                "clientRequestId": id,
                "kind": "approval",
                "method": method,
                "command": params["command"],
                "args": params["args"],
                "cwd": params["cwd"]
            })
        };
        self.pending
            .lock()
            .await
            .insert(id.clone(), Pending { summary, reply: tx });
        let result = tokio::time::timeout(std::time::Duration::from_secs(600), rx).await;
        self.pending.lock().await.remove(&id);
        match result {
            Ok(Ok(value)) => Ok(value),
            _ => Err(wire_error("Client interaction cancelled or expired")),
        }
    }

    pub async fn close(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        self.pending.lock().await.clear();
        self.elicitation_ids.lock().await.clear();
        let terminal_result = self.close_terminals().await;
        let connection_id = self.connection_id.clone();
        let generation = self.generation;
        let uncertain_result = persist(self.path.clone(), move |store| {
            store.mark_acp_callbacks_uncertain(&connection_id, generation, "connection closed")
        })
        .await;
        terminal_result?;
        uncertain_result?;
        Ok(())
    }

    async fn validate_session(&self, params: &Value) -> Result<String, AcpWireError> {
        let native = params["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| wire_error("invalid_input: missing native sessionId"))?;
        let mut expected = self.native_id.lock().await;
        if let Some(expected) = expected.as_ref() {
            if expected != native {
                return Err(wire_error(
                    "invalid_input: callback belongs to another session",
                ));
            }
        } else if self.discovery {
            return Err(wire_error(
                "unsupported: discovery connection has no session",
            ));
        } else {
            // A normal connection is provisional until session/new has returned.
            // The first callback may bind only that connection, and all later
            // callbacks must keep the same native id.
            *expected = Some(native.to_owned());
        }
        Ok(self
            .session_id
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| self.connection_id.clone()))
    }

    async fn snapshot(&self, session_id: &str, terminal_id: &str) -> Result<Value, String> {
        let payload = if let Some(snapshot) =
            self.terminals
                .read_retained(session_id, self.generation, terminal_id)
        {
            json!({
                "output": snapshot.output,
                "truncated": snapshot.truncated,
                "exitCode": snapshot.exit_code,
                "signal": snapshot.signal.map(|signal| signal.to_string()),
                "released": snapshot.released
            })
        } else {
            self.terminals
                .output(
                    session_id,
                    self.generation,
                    &json!({"terminalId": terminal_id}),
                )
                .await?
        };
        let record = AcpTerminalSnapshot {
            session_id: session_id.into(),
            generation: self.generation,
            terminal_id: terminal_id.into(),
            payload: payload.clone(),
        };
        persist(self.path.clone(), move |store| {
            store.save_acp_terminal_snapshot(&record)
        })
        .await?;
        Ok(payload)
    }

    /// Stop all command terminals owned by this ACP conversation generation
    /// and durably retain each final bounded output and exit result. This must
    /// be used for cancellation and session shutdown, including handles the
    /// agent never polled for output.
    pub(crate) async fn close_terminals(&self) -> Result<(), String> {
        self.stop_terminals(true).await
    }

    /// Cancel currently running command terminals without sealing the ACP
    /// connection generation, so later turns can create new terminals.
    pub(crate) async fn cancel_terminals(&self) -> Result<(), String> {
        self.stop_terminals(false).await
    }

    async fn stop_terminals(&self, seal: bool) -> Result<(), String> {
        // `on_disconnected`, ConnectionManager shutdown, and cancellation can
        // converge. Serialize them so every caller sees and persists the
        // retained snapshots after the first closer has reaped active handles.
        let _close_guard = self.terminal_close.lock().await;
        let session_id = self
            .session_id
            .lock()
            .await
            .clone()
            .unwrap_or_else(|| self.connection_id.clone());
        let snapshots = if seal {
            self.terminals
                .close_generation(&session_id, self.generation)
                .await
        } else {
            self.terminals
                .cancel_generation(&session_id, self.generation)
                .await
        };
        let records = snapshots
            .into_iter()
            .map(|snapshot| AcpTerminalSnapshot {
                session_id: snapshot.session_id,
                generation: snapshot.generation,
                terminal_id: snapshot.terminal_id,
                payload: json!({
                    "output": snapshot.output,
                    "truncated": snapshot.truncated,
                    "exitCode": snapshot.exit_code,
                    "signal": snapshot.signal.map(|signal| signal.to_string()),
                    "released": snapshot.released
                }),
            })
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Ok(());
        }
        persist(self.path.clone(), move |store| {
            for record in &records {
                store.save_acp_terminal_snapshot(record)?;
            }
            Ok(())
        })
        .await
    }

    async fn begin_mutation(
        &self,
        request_id: &Value,
        method: &str,
        params: &Value,
        session_id: &str,
    ) -> Result<MutationIntent, AcpWireError> {
        let connection_id = self.connection_id.clone();
        let generation = self.generation;
        let method = method.to_owned();
        let request_id = request_id.clone();
        let session_id = session_id.to_owned();
        let hash = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(params).map_err(|e| {
                wire_error(format!(
                    "invalid_input: cannot encode callback parameters: {e}"
                ))
            })?)
        );
        let (record, created) = persist(self.path.clone(), move |store| {
            store.begin_acp_callback(
                &connection_id,
                generation,
                &request_id,
                Some(&session_id),
                &method,
                &hash,
            )
        })
        .await
        .map_err(wire_error)?;
        if created {
            Ok(MutationIntent::New(record.id))
        } else {
            match record.state {
                AcpCallbackState::Succeeded => {
                    let result = record.result.unwrap_or_else(|| json!({}));
                    Ok(MutationIntent::Replay(
                        result.get("wireResult").cloned().unwrap_or(result),
                    ))
                }
                AcpCallbackState::Failed => Err(wire_error(
                    record.error.unwrap_or_else(|| "callback failed".into()),
                )),
                AcpCallbackState::Pending | AcpCallbackState::Uncertain => Err(wire_error(
                    "transport_lost: callback outcome is uncertain; not repeated",
                )),
            }
        }
    }

    async fn finish_mutation(
        &self,
        operation_id: &str,
        wire_result: &Value,
        receipt: Option<Value>,
    ) -> Result<(), AcpWireError> {
        let operation_id = operation_id.to_owned();
        let mut result = json!({"wireResult": wire_result});
        if let Some(receipt) = receipt {
            result["receipt"] = receipt;
        }
        persist(self.path.clone(), move |store| {
            store.finish_acp_callback(&operation_id, Some(&result), None)
        })
        .await
        .map(|_| ())
        .map_err(|error| {
            wire_error(format!(
                "transport_lost: callback result could not be durably recorded; outcome is uncertain and will not be repeated: {error}"
            ))
        })
    }

    async fn fail_mutation(&self, operation_id: &str, error: &str) {
        let operation_id = operation_id.to_owned();
        let error = error.to_owned();
        let _ = persist(self.path.clone(), move |store| {
            store.finish_acp_callback(&operation_id, None, Some(&error))
        })
        .await;
    }

    async fn elicitation(&self, params: &Value) -> Result<Value, AcpWireError> {
        if params["mode"] != "url" {
            return Err(AcpWireError {
                code: -32602,
                message: "Only URL elicitation is supported".into(),
                data: None,
            });
        }
        if let Some(session_id) = params["sessionId"].as_str() {
            self.validate_session(&json!({"sessionId": session_id}))
                .await?;
        } else if params.get("requestId").is_none_or(Value::is_null) {
            return Err(AcpWireError {
                code: -32602,
                message: "URL elicitation requires requestId or sessionId scope".into(),
                data: None,
            });
        }
        let elicitation_id = params["elicitationId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| AcpWireError {
                code: -32602,
                message: "URL elicitation requires elicitationId".into(),
                data: None,
            })?;
        let url = params["url"]
            .as_str()
            .filter(|url| safe_auth_url(url))
            .ok_or_else(|| wire_error("invalid_input: authentication URL is not allowed"))?;
        {
            let mut known = self.elicitation_ids.lock().await;
            if !known.insert(elicitation_id.to_owned()) {
                return Err(wire_error(
                    "conflict: elicitationId is already outstanding on this connection",
                ));
            }
        }
        let decision = match self.decision("elicitation/create", params).await {
            Ok(decision) => decision,
            Err(error) => {
                self.elicitation_ids.lock().await.remove(elicitation_id);
                return Err(error);
            }
        };
        let action = decision
            .as_str()
            .or_else(|| decision["action"].as_str())
            .unwrap_or("cancel");
        let action = match action {
            "accept" | "decline" | "cancel" => action,
            _ => "cancel",
        };
        if action == "accept" {
            // Keep only the opaque id in process memory. The URL (which can
            // accidentally contain secrets) is never journaled or logged.
            let _ = url;
        } else {
            self.elicitation_ids.lock().await.remove(elicitation_id);
        }
        Ok(json!({"action": action}))
    }

    async fn call(
        &self,
        method: String,
        params: Value,
        request_id: Value,
        history: Option<String>,
    ) -> Result<Value, AcpWireError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(wire_error("transport_lost: stale connection"));
        }
        if method == "elicitation/create" {
            if self.discovery || history.is_some() {
                return Err(wire_error(
                    "unsupported: discovery and replay cannot start authentication interactions",
                ));
            }
            return self.elicitation(&params).await;
        }
        if ![
            "fs/read_text_file",
            "fs/write_text_file",
            "terminal/create",
            "terminal/output",
            "terminal/wait_for_exit",
            "terminal/kill",
            "terminal/release",
        ]
        .contains(&method.as_str())
        {
            return Err(AcpWireError {
                code: -32601,
                message: "Client capability unavailable".into(),
                data: None,
            });
        }

        let is_terminal = method.starts_with("terminal/");
        if is_terminal && !command_terminal_callbacks_enabled() {
            return Err(wire_error(
                "unsupported: ACP command-terminal callbacks are unavailable on this host",
            ));
        }
        if self.discovery && (is_terminal || method.starts_with("fs/")) {
            return Err(wire_error(
                "unsupported: discovery connections cannot use filesystem or terminal callbacks",
            ));
        }
        let bound_session_id = if is_terminal || method.starts_with("fs/") {
            Some(self.session_id.lock().await.clone().ok_or_else(|| {
                wire_error(
                    "permission_denied: filesystem and terminal callbacks require a bound Monitor session",
                )
            })?)
        } else {
            None
        };
        // Do not let a callback rejected for missing Monitor ownership bind a
        // provisional native ID and poison the eventual conversation session.
        let validated_session_id = self.validate_session(&params).await?;
        let session_id = bound_session_id.unwrap_or(validated_session_id);
        let mutation = matches!(
            method.as_str(),
            "fs/write_text_file" | "terminal/create" | "terminal/kill" | "terminal/release"
        );
        if mutation && (self.discovery || history.is_some()) {
            return Err(wire_error(
                "unsupported: replay and discovery cannot mutate backend state",
            ));
        }
        if method.starts_with("fs/") {
            if !cfg!(unix) {
                return Err(wire_error(
                    "unsupported: ACP filesystem callbacks are unavailable on this host",
                ));
            }
            if self.profile.client_policy.filesystem == AcpFilesystemPolicy::Disabled
                || (mutation
                    && self.profile.client_policy.filesystem != AcpFilesystemPolicy::ReadWrite)
            {
                return Err(wire_error(
                    "permission_denied: filesystem callback disabled",
                ));
            }
        } else if self.profile.client_policy.command_execution
            == AcpCommandExecutionPolicy::Disabled
        {
            return Err(wire_error("permission_denied: terminal callback disabled"));
        }

        let mut effective_params = params.clone();
        if method == "terminal/create" {
            let requested = params["cwd"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_else(|| self.cwd.clone());
            if !requested.is_absolute() {
                return Err(wire_error(
                    "invalid_input: terminal directory must be absolute",
                ));
            }
            let canonical = requested.canonicalize().map_err(|error| {
                wire_error(format!("invalid_input: terminal directory: {error}"))
            })?;
            if !canonical.is_dir()
                || !self
                    .allowed_roots
                    .iter()
                    .any(|root| canonical.starts_with(root))
            {
                return Err(wire_error(
                    "permission_denied: terminal directory outside allowed roots",
                ));
            }
            effective_params["cwd"] = json!(canonical);
        }

        // Record the intent before showing approval. A repeated request id can
        // therefore never create a second prompt or repeat a mutation.
        let intent = if mutation {
            match self
                .begin_mutation(&request_id, &method, &params, &session_id)
                .await?
            {
                MutationIntent::Replay(result) => return Ok(result),
                intent => Some(intent),
            }
        } else {
            None
        };

        let operation_id = match intent {
            Some(MutationIntent::New(id)) => Some(id),
            Some(MutationIntent::Replay(_)) => unreachable!(),
            None => None,
        };

        if method == "terminal/create"
            && self.profile.client_policy.command_execution == AcpCommandExecutionPolicy::Ask
        {
            let decision = match self.decision(&method, &effective_params).await {
                Ok(decision) => decision,
                Err(error) => {
                    if let Some(operation_id) = operation_id.as_deref() {
                        self.fail_mutation(operation_id, &error.message).await;
                    }
                    return Err(error);
                }
            };
            let action = decision
                .as_str()
                .or_else(|| decision["action"].as_str())
                .unwrap_or("deny");
            if !["allow_once", "accept"].contains(&action) {
                let error = wire_error("permission_denied: command declined");
                if let Some(operation_id) = operation_id.as_deref() {
                    self.fail_mutation(operation_id, &error.message).await;
                }
                return Err(error);
            }
        }

        let (result, receipt): (Result<Value, String>, Option<Value>) = match method.as_str() {
            "fs/read_text_file" => {
                let Some(files) = self.files.as_ref() else {
                    return Err(wire_error(
                        "unsupported: filesystem callbacks are not configured",
                    ));
                };
                (files.read(&params).await, None)
            }
            "fs/write_text_file" => {
                let Some(files) = self.files.as_ref() else {
                    if let Some(operation_id) = operation_id.as_deref() {
                        self.fail_mutation(
                            operation_id,
                            "unsupported: filesystem callbacks are not configured",
                        )
                        .await;
                    }
                    return Err(wire_error(
                        "unsupported: filesystem callbacks are not configured",
                    ));
                };
                match files.write(&params).await {
                    Ok(receipt) => (Ok(json!({})), Some(receipt)),
                    Err(error) => (Err(error), None),
                }
            }
            "terminal/create" => (
                self.terminals
                    .create(&session_id, self.generation, &effective_params)
                    .await,
                None,
            ),
            "terminal/output" => (
                self.terminals
                    .output(&session_id, self.generation, &params)
                    .await,
                None,
            ),
            "terminal/wait_for_exit" => (
                self.terminals
                    .wait_for_exit(&session_id, self.generation, &params)
                    .await,
                None,
            ),
            "terminal/kill" => {
                let killed = self
                    .terminals
                    .kill(&session_id, self.generation, &params)
                    .await;
                let result = match killed {
                    Ok(value) => self
                        .terminals
                        .wait_for_exit(&session_id, self.generation, &params)
                        .await
                        .map(|_| value),
                    Err(error) => Err(error),
                };
                (result, None)
            }
            "terminal/release" => (
                self.terminals
                    .release(&session_id, self.generation, &params)
                    .await,
                None,
            ),
            _ => unreachable!(),
        };

        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Some(operation_id) = operation_id.as_deref() {
                    self.fail_mutation(operation_id, &error).await;
                }
                return Err(wire_error(error));
            }
        };
        if is_terminal {
            let terminal_id = result
                .get("terminalId")
                .and_then(Value::as_str)
                .or_else(|| params["terminalId"].as_str());
            if let Some(terminal_id) = terminal_id {
                self.snapshot(&session_id, terminal_id).await.map_err(|error| {
                    wire_error(format!(
                        "transport_lost: terminal effect occurred but its evidence could not be saved; outcome is uncertain: {error}"
                    ))
                })?;
            }
        }
        if let Some(operation_id) = operation_id {
            self.finish_mutation(&operation_id, &result, receipt.clone())
                .await?;
        }
        if method == "fs/write_text_file" {
            self.emit_workspace_change(&receipt).await;
        }
        Ok(result)
    }

    async fn emit_workspace_change(&self, receipt: &Option<Value>) {
        let Some(sender) = self.workspace_change_events.lock().await.clone() else {
            return;
        };
        let relative_path = receipt
            .as_ref()
            .and_then(|receipt| receipt["path"].as_str())
            .and_then(|path| {
                PathBuf::from(path)
                    .strip_prefix(&self.cwd)
                    .ok()
                    .map(PathBuf::from)
            })
            .filter(|path| {
                !path.as_os_str().is_empty()
                    && path
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
            })
            .and_then(|path| path.to_str().map(|path| path.replace('\\', "/")));
        let paths = relative_path.into_iter().collect::<Vec<_>>();
        // This is an internal nudge consumed by RuntimeRegistry::drive. It is
        // not protocol evidence and contains no file content or credentials.
        let _ = sender.send(json!({
            "method":"runtime/workspace_files_changed",
            "params":{"paths":paths}
        }));
    }
}

fn command_terminal_callbacks_enabled() -> bool {
    cfg!(not(any(target_os = "ios", target_os = "android")))
}

enum MutationIntent {
    New(String),
    Replay(Value),
}

fn safe_auth_url(value: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(value) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => {
            let Some(host) = url.host_str() else {
                return false;
            };
            host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        }
        _ => false,
    }
}

impl AcpRequestHandler for ClientCallbacks {
    fn handle(
        &self,
        method: String,
        params: Value,
        id: Value,
        history_load_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>> {
        let callbacks = self.clone();
        Box::pin(async move { callbacks.call(method, params, id, history_load_id).await })
    }

    fn on_notification(&self, method: &str, params: &Value) {
        match method {
            "runtime/auth_required" => {
                let state = self.auth_state.clone();
                tokio::spawn(async move {
                    *state.lock().await = "required".into();
                });
            }
            "_auth/status_update" => {
                let state = self.auth_state.clone();
                let value = match params["authenticated"].as_bool() {
                    Some(true) => "authenticated",
                    // An absent or malformed field must never be treated as
                    // an authenticated result; preserve the unknown state.
                    Some(false) | None => "unknown",
                };
                tokio::spawn(async move {
                    *state.lock().await = value.into();
                });
            }
            "elicitation/complete" => {
                let Some(id) = params["elicitationId"].as_str().map(str::to_owned) else {
                    return;
                };
                let known = self.elicitation_ids.clone();
                tokio::spawn(async move {
                    // Each ClientCallbacks belongs to one ACP connection; an
                    // unknown or duplicate completion is ignored without logging.
                    known.lock().await.remove(&id);
                });
            }
            _ => {}
        }
    }

    fn on_disconnected(&self) {
        let callbacks = self.clone();
        tokio::spawn(async move {
            let _ = callbacks.close().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::AcpClientPolicy;
    use super::*;
    use std::{fs, sync::OnceLock, time::Duration};
    use tokio::runtime::{Builder, Runtime};

    fn runtime() -> &'static Runtime {
        static RUNTIME: OnceLock<Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| Builder::new_multi_thread().enable_all().build().unwrap())
    }

    struct Fixture {
        base: PathBuf,
        root: PathBuf,
        database: PathBuf,
        callbacks: ClientCallbacks,
    }

    impl Fixture {
        async fn new(policy: AcpClientPolicy) -> Self {
            let base = std::env::temp_dir()
                .join(format!("codex-acp-callback-test-{}", uuid::Uuid::new_v4()));
            let root = base.join("workspace");
            let data = base.join("data");
            fs::create_dir_all(&root).unwrap();
            fs::create_dir_all(&data).unwrap();
            let database = data.join("runs.sqlite3");
            let profile = RuntimeProfile {
                id: "fixture".into(),
                revision: 1,
                name: "Fixture".into(),
                kind: super::super::types::RuntimeKind::Acp,
                executable: "fixture".into(),
                args: Vec::new(),
                working_directory: None,
                preset_id: None,
                environment: Default::default(),
                client_policy: policy,
            };
            let callbacks = ClientCallbacks::new(
                "connection-fixture".into(),
                1,
                profile,
                database.clone(),
                root.clone(),
                false,
            )
            .unwrap();
            *callbacks.session_id.lock().await = Some("monitor-session".into());
            *callbacks.native_id.lock().await = Some("native-session".into());
            Self {
                base,
                root,
                database,
                callbacks,
            }
        }

        async fn call(
            &self,
            method: &str,
            params: Value,
            id: Value,
        ) -> Result<Value, AcpWireError> {
            self.callbacks
                .call(method.to_owned(), params, id, None)
                .await
        }

        fn cleanup(self) {
            drop(self.callbacks);
            let _ = fs::remove_dir_all(self.base);
        }
    }

    fn base_params() -> Value {
        json!({"sessionId":"native-session"})
    }

    #[test]
    fn advertised_callbacks_match_host_support_and_profile_policy() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy::default()).await;
            let capabilities = fixture.callbacks.capabilities();
            assert_eq!(capabilities["fs"]["readTextFile"], false);
            assert_eq!(capabilities["fs"]["writeTextFile"], false);
            assert!(capabilities.get("terminal").is_none());
            assert_eq!(
                capabilities["auth"]["terminal"],
                cfg!(all(
                    feature = "acp-host-services",
                    not(any(target_os = "ios", target_os = "android"))
                ))
            );
            fixture.cleanup();
        });
    }

    #[test]
    fn enabled_command_callbacks_are_desktop_only_and_dispatch_matches_advertisement() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                command_execution: AcpCommandExecutionPolicy::Allow,
                ..Default::default()
            })
            .await;
            let advertised = fixture.callbacks.capabilities().get("terminal").is_some();
            assert_eq!(advertised, command_terminal_callbacks_enabled());
            if !command_terminal_callbacks_enabled() {
                let error = fixture
                    .call(
                        "terminal/create",
                        json!({
                            "sessionId":"native-session",
                            "command":"not-run"
                        }),
                        json!(1),
                    )
                    .await
                    .unwrap_err();
                assert!(error.message.contains("unavailable on this host"));
            }
            fixture.cleanup();
        });
    }

    #[test]
    fn callback_file_write_persists_receipt_and_wire_duplicate_is_idempotent() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                filesystem: AcpFilesystemPolicy::ReadWrite,
                ..Default::default()
            }).await;
            let (events_tx, mut events_rx) = tokio::sync::mpsc::unbounded_channel();
            fixture
                .callbacks
                .set_workspace_change_events(events_tx)
                .await;
            let target = fixture.root.join("created.txt");
            fs::write(&target, "before").unwrap();
            let mut params = base_params();
            params["path"] = json!(target);
            params["content"] = json!("first");
            let first = fixture
                .call("fs/write_text_file", params.clone(), json!(44))
                .await
                .unwrap();
            assert_eq!(first, json!({}));
            let change = tokio::time::timeout(Duration::from_secs(1), events_rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(change["method"], "runtime/workspace_files_changed");
            assert_eq!(change["params"]["paths"], json!(["created.txt"]));
            assert_eq!(fs::read_to_string(&target).unwrap(), "first");
            let raw_result: String = rusqlite::Connection::open(&fixture.database)
                .unwrap()
                .query_row(
                    "SELECT result_json FROM acp_callback_operations WHERE connection_id=? AND generation=? AND request_id_json=?",
                    rusqlite::params!["connection-fixture", 1, "44"],
                    |row| row.get(0),
                )
                .unwrap();
            let stored: Value = serde_json::from_str(&raw_result).unwrap();
            assert_eq!(stored["wireResult"], json!({}));
            assert_eq!(stored["receipt"]["beforeSha256"].as_str().unwrap().len(), 64);
            let backup = PathBuf::from(stored["receipt"]["backupRef"].as_str().unwrap());
            assert_eq!(fs::read_to_string(backup).unwrap(), "before");
            // Duplicate wire ids are returned from the callback journal without
            // executing a second write; same content here also checks receipt retention.
            let duplicate = fixture
                .call("fs/write_text_file", params, json!(44))
                .await
                .unwrap();
            assert_eq!(duplicate, json!({}));
            assert!(events_rx.try_recv().is_err());
            assert_eq!(fs::read_to_string(&target).unwrap(), "first");
            let backups = fixture.database.parent().unwrap().join("acp-file-backups");
            assert!(backups.exists());
            fixture.cleanup();
        });
    }

    #[test]
    fn duplicate_command_request_is_rejected_before_a_second_approval_prompt() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                command_execution: AcpCommandExecutionPolicy::Ask,
                ..Default::default()
            })
            .await;
            let params = json!({
                "sessionId":"native-session",
                "command":"/bin/sh",
                "args":["-c","touch should-not-exist"],
                "cwd":fixture.root
            });
            let callbacks = fixture.callbacks.clone();
            let first_params = params.clone();
            let first = tokio::spawn(async move {
                callbacks
                    .call("terminal/create".into(), first_params, json!(71), None)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if !fixture.callbacks.pending().await.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let duplicate = fixture
                .call("terminal/create", params, json!(71))
                .await
                .unwrap_err();
            assert!(duplicate.message.contains("not repeated"));
            assert_eq!(fixture.callbacks.pending().await.len(), 1);
            let pending = fixture.callbacks.pending().await.remove(0);
            let request_id = pending["clientRequestId"].as_str().unwrap();
            fixture
                .callbacks
                .respond(request_id, json!({"action":"decline"}))
                .await
                .unwrap();
            assert!(first.await.unwrap().is_err());
            assert!(!fixture.root.join("should-not-exist").exists());
            fixture.cleanup();
        });
    }

    #[test]
    fn discovery_replay_and_cross_session_cannot_launch_or_mutate() {
        runtime().block_on(async {
            let mut fixture = Fixture::new(AcpClientPolicy {
                filesystem: AcpFilesystemPolicy::ReadWrite,
                command_execution: AcpCommandExecutionPolicy::Allow,
                ..Default::default()
            })
            .await;
            fixture.callbacks.discovery = true;
            assert!(fixture
                .call(
                    "terminal/create",
                    json!({"sessionId":"native-session","command":"/bin/true"}),
                    json!(1)
                )
                .await
                .is_err());
            fixture.callbacks.discovery = false;
            let mut params = base_params();
            params["path"] = json!(fixture.root.join("denied.txt"));
            params["content"] = json!("denied");
            assert!(fixture
                .callbacks
                .call(
                    "fs/write_text_file".into(),
                    params.clone(),
                    json!(2),
                    Some("history-load".into())
                )
                .await
                .is_err());
            params["sessionId"] = json!("other-native-session");
            assert!(fixture
                .call("fs/write_text_file", params, json!(3))
                .await
                .is_err());
            assert!(!fixture.root.join("denied.txt").exists());
            fixture.cleanup();
        });
    }

    #[test]
    #[cfg(unix)]
    fn closing_callbacks_persists_final_snapshot_without_agent_polling() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                command_execution: AcpCommandExecutionPolicy::Allow,
                ..Default::default()
            })
            .await;
            let created = fixture
                .callbacks
                .terminals
                .create(
                    "monitor-session",
                    1,
                    &json!({
                        "command":"/bin/sh",
                        "args":["-c", "printf unpolled-final; touch output-ready; sleep 30"],
                        "cwd":fixture.root
                    }),
                )
                .await
                .unwrap();
            let terminal_id = created["terminalId"].as_str().unwrap().to_owned();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !fixture.root.join("output-ready").exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("terminal should emit its output before session close");

            // The agent never calls terminal/output, wait_for_exit or release.
            fixture.callbacks.close_terminals().await.unwrap();
            let session_id = "monitor-session".to_owned();
            let terminal_id_for_read = terminal_id.clone();
            let durable = persist(fixture.database.clone(), move |store| {
                store.read_acp_terminal_snapshot(&session_id, 1, &terminal_id_for_read)
            })
            .await
            .unwrap()
            .expect("final terminal snapshot should be durable");
            assert_eq!(durable.payload["output"], "unpolled-final");
            assert_eq!(durable.payload["signal"], "9");
            assert_eq!(durable.payload["released"], true);
            assert_eq!(durable.payload["truncated"], false);
            assert!(fixture
                .callbacks
                .terminals
                .output("monitor-session", 1, &json!({"terminalId":terminal_id}))
                .await
                .is_err());
            assert!(fixture
                .callbacks
                .terminals
                .create("monitor-session", 1, &json!({"command":"/usr/bin/true"}))
                .await
                .is_err());
            fixture.cleanup();
        });
    }

    #[test]
    #[cfg(unix)]
    fn terminal_cancellation_persists_output_but_keeps_generation_open() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                command_execution: AcpCommandExecutionPolicy::Allow,
                ..Default::default()
            })
            .await;
            let first = fixture
                .callbacks
                .terminals
                .create(
                    "monitor-session",
                    1,
                    &json!({
                        "command":"/bin/sh",
                        "args":["-c", "printf before-cancel; touch cancel-ready; sleep 30"],
                        "cwd":fixture.root
                    }),
                )
                .await
                .unwrap();
            let first_id = first["terminalId"].as_str().unwrap().to_owned();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !fixture.root.join("cancel-ready").exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("terminal should emit output before cancellation");

            fixture.callbacks.cancel_terminals().await.unwrap();
            let first_for_read = first_id.clone();
            let snapshot = persist(fixture.database.clone(), move |store| {
                store.read_acp_terminal_snapshot("monitor-session", 1, &first_for_read)
            })
            .await
            .unwrap()
            .expect("cancellation should save terminal evidence");
            assert_eq!(snapshot.payload["output"], "before-cancel");
            assert_eq!(snapshot.payload["signal"], "9");
            assert_eq!(snapshot.payload["released"], true);

            let second = fixture
                .call(
                    "terminal/create",
                    json!({
                        "sessionId":"native-session",
                        "command":"/usr/bin/printf",
                        "args":["after-cancel"],
                        "cwd":fixture.root
                    }),
                    json!(85),
                )
                .await
                .expect("cancelled turn must leave the connection generation usable");
            let second_id = second["terminalId"].as_str().unwrap().to_owned();
            assert_ne!(second_id, first_id);

            fixture.callbacks.close_terminals().await.unwrap();
            assert!(fixture
                .callbacks
                .terminals
                .create("monitor-session", 1, &json!({"command":"/usr/bin/true"}))
                .await
                .is_err());
            let second_for_read = second_id.clone();
            let final_snapshot = persist(fixture.database.clone(), move |store| {
                store.read_acp_terminal_snapshot("monitor-session", 1, &second_for_read)
            })
            .await
            .unwrap()
            .expect("final close should persist the later turn terminal");
            assert_eq!(final_snapshot.payload["released"], true);
            fixture.cleanup();
        });
    }

    #[test]
    #[cfg(unix)]
    fn enabled_callbacks_wait_for_monitor_session_binding_then_use_bound_identity() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy {
                command_execution: AcpCommandExecutionPolicy::Allow,
                ..Default::default()
            })
            .await;
            *fixture.callbacks.session_id.lock().await = None;
            *fixture.callbacks.native_id.lock().await = None;
            let rejected_params = json!({
                "sessionId":"native-session-a",
                "command":"/usr/bin/printf",
                "args":["bound-only"]
            });
            let rejected = fixture
                .call("terminal/create", rejected_params, json!(83))
                .await
                .unwrap_err();
            assert!(rejected.message.contains("bound Monitor session"));
            assert!(!fixture.database.exists());
            assert!(fixture.callbacks.native_id.lock().await.is_none());

            // This models ConnectionManager binding the Monitor UUID before
            // session/new or session/load has supplied the provisional native ID.
            *fixture.callbacks.session_id.lock().await = Some("monitor-bound".into());
            let bound_params = json!({
                "sessionId":"native-session-b",
                "command":"/usr/bin/printf",
                "args":["bound-only"]
            });
            let created = fixture
                .call("terminal/create", bound_params, json!(84))
                .await
                .unwrap();
            assert_eq!(
                fixture.callbacks.native_id.lock().await.as_deref(),
                Some("native-session-b")
            );
            let terminal_id = created["terminalId"].as_str().unwrap().to_owned();
            fixture.callbacks.close_terminals().await.unwrap();
            let snapshot = persist(fixture.database.clone(), move |store| {
                store.read_acp_terminal_snapshot("monitor-bound", 1, &terminal_id)
            })
            .await
            .unwrap()
            .expect("bound terminal snapshot should persist under Monitor identity");
            assert_eq!(snapshot.session_id, "monitor-bound");
            assert_eq!(snapshot.payload["released"], true);
            fixture.cleanup();
        });
    }

    #[test]
    fn url_validation_disallows_host_prefix_and_credentials() {
        assert!(safe_auth_url("https://login.example.test/oauth"));
        assert!(safe_auth_url("http://localhost:3000/callback"));
        assert!(safe_auth_url("http://127.0.0.1:3000/callback"));
        assert!(!safe_auth_url("http://localhost.evil.test/callback"));
        assert!(!safe_auth_url("https://user:password@example.test/login"));
        assert!(!safe_auth_url("file:///etc/passwd"));
    }

    #[test]
    fn accepted_url_elicitation_is_connection_scoped_and_completion_only_settles_known_id() {
        runtime().block_on(async {
            let fixture = Fixture::new(AcpClientPolicy::default()).await;
            let callbacks = fixture.callbacks.clone();
            let request = json!({
                "mode":"url",
                "requestId":17,
                "elicitationId":"opaque-login-id",
                "url":"https://login.example.test/authorize?state=private-state",
                "message":"Sign in"
            });
            let task = tokio::spawn(async move {
                callbacks
                    .call("elicitation/create".into(), request, json!(901), None)
                    .await
            });
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if !fixture.callbacks.pending().await.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let pending = fixture.callbacks.pending().await.remove(0);
            assert_eq!(pending["kind"], "url");
            assert_eq!(pending["elicitationId"], "opaque-login-id");
            assert!(pending["url"]
                .as_str()
                .unwrap()
                .contains("state=private-state"));
            let client_request_id = pending["clientRequestId"].as_str().unwrap();
            fixture
                .callbacks
                .respond(client_request_id, json!({"action":"accept"}))
                .await
                .unwrap();
            assert_eq!(task.await.unwrap().unwrap(), json!({"action":"accept"}));
            assert!(fixture
                .callbacks
                .elicitation_ids
                .lock()
                .await
                .contains("opaque-login-id"));

            fixture.callbacks.on_notification(
                "elicitation/complete",
                &json!({"elicitationId":"unknown-id"}),
            );
            tokio::task::yield_now().await;
            assert!(fixture
                .callbacks
                .elicitation_ids
                .lock()
                .await
                .contains("opaque-login-id"));
            fixture.callbacks.on_notification(
                "elicitation/complete",
                &json!({"elicitationId":"opaque-login-id"}),
            );
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    if fixture.callbacks.elicitation_ids.lock().await.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // The URL may appear in the in-memory approval card, but no ACP
            // callback journal is created for elicitation or its completion.
            assert!(!fixture.database.exists());
            fixture.cleanup();
        });
    }

    #[test]
    fn disabled_file_policy_does_not_block_or_create_a_workspace_backup_directory() {
        let root =
            std::env::temp_dir().join(format!("codex-acp-disabled-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let profile = RuntimeProfile {
            id: "fixture".into(),
            revision: 1,
            name: "Fixture".into(),
            kind: super::super::types::RuntimeKind::Acp,
            executable: "fixture".into(),
            args: Vec::new(),
            working_directory: None,
            preset_id: None,
            environment: Default::default(),
            client_policy: AcpClientPolicy::default(),
        };
        let path = root.join("runs.sqlite3");
        let callbacks = ClientCallbacks::new(
            "disabled-fs".into(),
            1,
            profile,
            path.clone(),
            root.clone(),
            false,
        )
        .unwrap();
        assert!(callbacks.files.is_none());
        assert!(!root.join("acp-file-backups").exists());
        drop(callbacks);
        let _ = fs::remove_dir_all(root);
    }
}
