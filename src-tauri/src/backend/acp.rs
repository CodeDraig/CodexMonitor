//! ACP v1 bidirectional newline JSON-RPC transport. Reading never waits on storage or a prompt.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
    task::JoinSet,
};

const MAX_ACP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_ACP_CALLBACKS: usize = 128;

/// A protocol error returned to an agent's JSON-RPC request.
#[derive(Debug, Clone)]
pub(crate) struct AcpWireError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}
impl AcpWireError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Unsupported client request: {method}"),
            data: None,
        }
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }
}

/// Handles ACP client requests arriving while the agent is running.
/// Returned futures own their inputs so the transport can service requests concurrently.
pub(crate) trait AcpRequestHandler: Send + Sync + 'static {
    fn handle(
        &self,
        method: String,
        params: Value,
        id: Value,
        history_load_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>;

    /// Notifications are queued synchronously by the caller; implementations should avoid I/O.
    fn on_notification(&self, _method: &str, _params: &Value) {}
    fn on_disconnected(&self) {}
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unterminated ACP message",
                ))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |index| index + 1);
        if bytes.len().saturating_add(count) > MAX_ACP_MESSAGE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "ACP message exceeds 8 MiB",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
            }
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return String::from_utf8(bytes)
                .map(Some)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        }
    }
}

fn wire_error_response(id: Value, error: AcpWireError) -> Value {
    let mut body = json!({"code": error.code, "message": error.message});
    if let Some(data) = error.data {
        body["data"] = data;
    }
    json!({"jsonrpc":"2.0", "id":id, "error":body})
}

fn safe_agent_error(error: &Value) -> String {
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .map(|code| code.to_string())
        .unwrap_or_else(|| "unknown".into());
    let category = if error.get("code").and_then(Value::as_i64) == Some(-32000) {
        "auth_required"
    } else {
        "runtime_error"
    };
    format!("{category}: ACP error code {code}")
}

fn sanitize_response_for_event(response: &Value) -> Value {
    let Some(error) = response.get("error") else {
        return response.clone();
    };
    json!({
        "jsonrpc": response.get("jsonrpc").cloned().unwrap_or_else(|| json!("2.0")),
        "id": response.get("id").cloned().unwrap_or(Value::Null),
        "error": {
            "code": error.get("code").cloned().unwrap_or(Value::Null),
            "message": "ACP request failed"
        }
    })
}

fn is_ephemeral_auth_notification(method: &str) -> bool {
    method.starts_with("_auth/") || method.starts_with("auth/") || method == "elicitation/complete"
}

async fn write_wire_error(stdin: &Arc<Mutex<ChildStdin>>, id: Value, error: AcpWireError) {
    let response = wire_error_response(id, error);
    let mut input = stdin.lock().await;
    let _ = input.write_all(format!("{response}\n").as_bytes()).await;
}

pub(crate) struct AcpClient {
    pub child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<String, (String, oneshot::Sender<Result<Value, String>>)>>>,
    next: AtomicU64,
    loads: Arc<Mutex<HashMap<String, String>>>,
    pub events: mpsc::UnboundedSender<Value>,
    alive: Arc<AtomicBool>,
    pub initialization: Value,
}
impl AcpClient {
    pub async fn spawn(
        executable: &str,
        args: &[String],
        cwd: &str,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Value>), String> {
        struct NoCallbacks;
        impl AcpRequestHandler for NoCallbacks {
            fn handle(
                &self,
                method: String,
                _params: Value,
                _id: Value,
                _history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                Box::pin(async move { Err(AcpWireError::method_not_found(&method)) })
            }
        }
        Self::spawn_with_services(
            executable,
            args,
            cwd,
            &std::collections::BTreeMap::new(),
            json!({"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"session":{"compaction":{}}}),
            Arc::new(NoCallbacks),
        ).await
    }

    pub async fn spawn_with_services(
        executable: &str,
        args: &[String],
        cwd: &str,
        env: &std::collections::BTreeMap<String, String>,
        client_capabilities: Value,
        handler: Arc<dyn AcpRequestHandler>,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Value>), String> {
        let mut child = Command::new(executable)
            .args(args)
            .current_dir(cwd)
            .env("PWD", cwd)
            .envs(env)
            .kill_on_drop(true)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("transport_lost: {e}"))?;
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().ok_or("transport_lost: no ACP stdin")?,
        ));
        let stdout = child.stdout.take().ok_or("transport_lost: no ACP stdout")?;
        let stderr = child.stderr.take().ok_or("transport_lost: no ACP stderr")?;
        let pending: Arc<Mutex<HashMap<String, (String, oneshot::Sender<Result<Value, String>>)>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::unbounded_channel();
        let loads: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
        let read_loads = loads.clone();
        let event_sender = tx.clone();
        let read_pending = pending.clone();
        let read_alive = alive.clone();
        let read_stdin = stdin.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout);
            let mut callbacks = JoinSet::new();
            let failure = loop {
                tokio::select! {
                    joined = callbacks.join_next(), if !callbacks.is_empty() => {
                        let _ = joined;
                    }
                    line = read_bounded_line(&mut lines) => match line {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => {
                        let v: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => break format!("Invalid ACP JSON: {e}"),
                        };
                        if v.get("id").is_some() && v.get("method").is_none() {
                            if let Some((method, sender)) =
                                read_pending.lock().await.remove(&v["id"].to_string())
                            {
                                let event_response = sanitize_response_for_event(&v);
                                if method == "session/prompt" {
                                    let _ = tx.send(json!({"method":"runtime/prompt_response","params":event_response.clone()}));
                                }
                                if let Some(load) = method.strip_prefix("history:") {
                                    read_loads.lock().await.retain(|_, v| v != load);
                                    // This barrier is on the same queue as every preceding update.
                                    let _ = tx.send(json!({"method":"runtime/history_end","params":{"loadId":load,"response":event_response}}));
                                }
                                let result = if let Some(e) = v.get("error") {
                                    if e["code"] == -32000 {
                                        handler.on_notification("runtime/auth_required", &json!({}));
                                    }
                                    // Agent messages and error data can contain credentials or
                                    // user content. Keep only the stable category and numeric code
                                    // in the operation error surfaced to the UI and event log.
                                    Err(safe_agent_error(e))
                                } else {
                                    v.get("result")
                                        .cloned()
                                        .ok_or("runtime_error: missing ACP result".into())
                                };
                                let _ = sender.send(result);
                            }
                        } else if v["method"] == "session/update"
                            || v["method"] == "session/request_permission"
                        {
                            let load = read_loads
                                .lock()
                                .await
                                .get(v["params"]["sessionId"].as_str().unwrap_or(""))
                                .cloned();
                            if let Some(load) = load {
                                if v["method"] == "session/request_permission" {
                                    // A replayed permission is evidence, never an actionable request.
                                    let response = json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"cancelled"}}});
                                    let _ = read_stdin
                                        .lock()
                                        .await
                                        .write_all(format!("{response}\n").as_bytes())
                                        .await;
                                }
                                let _ = tx.send(json!({"method":"runtime/history","params":{"loadId":load,"event":v}}));
                            } else {
                                let _ = tx.send(v);
                            }
                        } else if let Some(method) = v.get("method").and_then(Value::as_str) {
                            let params = v.get("params").cloned().unwrap_or_else(|| json!({}));
                            if let Some(id) = v.get("id").filter(|id| id.is_string() || id.is_number()).cloned() {
                                if !params.is_object() {
                                    write_wire_error(&read_stdin, id, AcpWireError::invalid_params("params must be an object")).await;
                                } else if callbacks.len() >= MAX_ACP_CALLBACKS {
                                    write_wire_error(&read_stdin, id, AcpWireError { code: -32000, message: "Too many concurrent ACP client requests".into(), data: None }).await;
                                } else {
                                    let active_loads = read_loads.lock().await;
                                    let load_id = if let Some(session) = params.get("sessionId").and_then(Value::as_str) {
                                        active_loads.get(session).cloned()
                                    } else {
                                        // Connection-scoped callbacks such as URL elicitation
                                        // may omit sessionId; associate them with active replay.
                                        active_loads.values().next().cloned()
                                    };
                                    drop(active_loads);
                                    let handler = handler.clone();
                                    let write = read_stdin.clone();
                                    let method = method.to_owned();
                                    callbacks.spawn(async move {
                                        let response = match handler.handle(method, params, id.clone(), load_id).await {
                                            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                                            Err(error) => wire_error_response(id, error),
                                        };
                                        let mut input = write.lock().await;
                                        let _ = input.write_all(format!("{response}\n").as_bytes()).await;
                                    });
                                }
                            } else if v.get("id").is_some() {
                                write_wire_error(&read_stdin, v["id"].clone(), AcpWireError::invalid_params("request id must be a string or number")).await;
                            } else {
                                handler.on_notification(method, &params);
                                if !is_ephemeral_auth_notification(method) {
                                    let _ = tx.send(v);
                                }
                            }
                        } else if v.get("id").is_some() {
                            write_wire_error(&read_stdin, v["id"].clone(), AcpWireError::invalid_params("request method must be a string")).await;
                        }
                    }
                    Ok(None) => break "ACP process closed its output".into(),
                    Err(e) => break e.to_string(),
                    }
                }
            };
            callbacks.abort_all();
            while callbacks.join_next().await.is_some() {}
            handler.on_disconnected();
            read_alive.store(false, Ordering::SeqCst);
            for (_, (_, sender)) in read_pending.lock().await.drain() {
                let _ = sender.send(Err(format!("transport_lost: {failure}")));
            }
            let _ = tx.send(json!({"method":"runtime/disconnected","params":{"message":failure}}));
        });
        // Drain diagnostics so a full stderr pipe cannot stall the protocol.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(_)) = lines.next_line().await {}
        });
        let mut client = Self {
            child: Mutex::new(child),
            stdin,
            pending,
            next: AtomicU64::new(1),
            loads,
            alive,
            initialization: Value::Null,
            events: event_sender,
        };
        let initialized = match client.request(
            "initialize",
            json!({"protocolVersion":1,"clientCapabilities":client_capabilities,"clientInfo":{"name":"codex-monitor","version":env!("CARGO_PKG_VERSION")}}),
            Some(Duration::from_secs(30)),
        ).await {
            Ok(initialized) => initialized,
            Err(error) => {
                client.stop().await;
                return Err(error);
            }
        };
        if initialized["protocolVersion"] != 1 {
            client.stop().await;
            return Err("unsupported: ACP protocol version must be 1".into());
        }
        client.initialization = initialized;
        Ok((Arc::new(client), rx))
    }
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
    pub async fn write(&self, v: Value) -> Result<(), String> {
        if !self.alive() {
            return Err("transport_lost: ACP process disconnected".into());
        }
        let mut input = self.stdin.lock().await;
        input
            .write_all(format!("{v}\n").as_bytes())
            .await
            .map_err(|e| format!("transport_lost: {e}"))
    }
    pub async fn start_request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(String, oneshot::Receiver<Result<Value, String>>), String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let key = id.to_string();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .await
            .insert(key.clone(), (method.to_string(), tx));
        let written = tokio::time::timeout(
            Duration::from_secs(15),
            self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})),
        )
        .await
        .map_err(|_| "transport_lost: ACP write timed out; outcome unknown".to_string())
        .and_then(|v| v);
        if let Err(e) = written {
            self.pending.lock().await.remove(&key);
            return Err(e);
        }
        Ok((key, rx))
    }
    pub async fn start_history_load(
        &self,
        load: &str,
        session: &str,
        cwd: &str,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, String> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.loads.lock().await.insert(session.into(), load.into());
        self.pending
            .lock()
            .await
            .insert(id.to_string(), (format!("history:{load}"), tx));
        if let Err(e)=self.write(json!({"jsonrpc":"2.0","id":id,"method":"session/load","params":{"sessionId":session,"cwd":cwd,"mcpServers":[]}})).await {
            self.loads.lock().await.remove(session);self.pending.lock().await.remove(&id.to_string());return Err(e);
        }
        Ok(rx)
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, String> {
        let (key, rx) = self.start_request(method, params).await?;
        if let Some(duration) = timeout {
            match tokio::time::timeout(duration, rx).await {
                Ok(Ok(v)) => v,
                Ok(Err(_)) => Err("transport_lost: ACP request abandoned".into()),
                Err(_) => {
                    self.pending.lock().await.remove(&key);
                    Err("transport_lost: ACP request timed out; outcome unknown".into())
                }
            }
        } else {
            rx.await
                .map_err(|_| "transport_lost: ACP request abandoned".to_string())?
        }
    }
    pub async fn stop(&self) {
        crate::shared::process_core::kill_child_process_tree(&mut *self.child.lock().await).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_reader_rejects_oversized_messages_without_copying_them() {
        let payload = vec![b'x'; MAX_ACP_MESSAGE_BYTES + 1];
        let mut reader = BufReader::new(payload.as_slice());
        let error = read_bounded_line(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("8 MiB"));
    }

    #[test]
    fn wire_error_keeps_code_data_and_original_id_type() {
        let error = AcpWireError {
            code: -32042,
            message: "denied".into(),
            data: Some(json!({"reason":"policy"})),
        };
        let string_id = wire_error_response(json!("9"), error.clone());
        let number_id = wire_error_response(json!(9), error);
        assert_eq!(string_id["id"], json!("9"));
        assert_eq!(number_id["id"], json!(9));
        assert_eq!(string_id["error"]["code"], json!(-32042));
        assert_eq!(string_id["error"]["data"]["reason"], json!("policy"));
    }

    #[test]
    fn agent_error_diagnostic_does_not_expose_message_or_data() {
        let error = json!({
            "code": -32042,
            "message": "provider access token is private",
            "data": {"token": "sentinel-token-value"}
        });
        let diagnostic = safe_agent_error(&error);
        assert_eq!(diagnostic, "runtime_error: ACP error code -32042");
        assert!(!diagnostic.contains("provider access token"));
        assert!(!diagnostic.contains("sentinel-token-value"));
    }

    #[test]
    fn only_auth_and_elicitation_completion_notifications_are_ephemeral() {
        assert!(is_ephemeral_auth_notification("_auth/status_update"));
        assert!(is_ephemeral_auth_notification("auth/status_update"));
        assert!(is_ephemeral_auth_notification("elicitation/complete"));
        assert!(!is_ephemeral_auth_notification("session/update"));
        assert!(!is_ephemeral_auth_notification("notifications/message"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn callbacks_are_concurrent_and_keep_string_and_numeric_ids() {
        struct Echo {
            active: Arc<std::sync::atomic::AtomicU64>,
            max_active: Arc<std::sync::atomic::AtomicU64>,
        }
        impl AcpRequestHandler for Echo {
            fn handle(
                &self,
                method: String,
                _params: Value,
                id: Value,
                _history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                let active = self.active.clone();
                let max_active = self.max_active.clone();
                Box::pin(async move {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(current, Ordering::SeqCst);
                    if method == "fs/read_text_file" {
                        tokio::time::sleep(Duration::from_millis(75)).await;
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(json!({"method":method,"receivedId":id}))
                })
            }
        }
        let script = r#"
import json,sys
def read(): return json.loads(sys.stdin.readline())
def emit(value): print(json.dumps(value),flush=True)
initialization=read()
emit({"jsonrpc":"2.0","id":initialization["id"],"result":{"protocolVersion":1,"agentInfo":{"name":"fixture"}}})
responses=[]
while True:
    message=read()
    if message.get("method")=="probe":
        emit({"jsonrpc":"2.0","id":"read-id","method":"fs/read_text_file","params":{}})
        emit({"jsonrpc":"2.0","id":7,"method":"terminal/output","params":{}})
        while len(responses) < 2:
            responses.append(read())
        emit({"jsonrpc":"2.0","id":message["id"],"result":{}})
    elif message.get("method")=="collect":
        emit({"jsonrpc":"2.0","id":message["id"],"result":{"responses":responses,"clientCapabilities":initialization["params"]["clientCapabilities"]}})
        break
    else:
        responses.append(message)
"#;
        let capabilities =
            json!({"fs":{"readTextFile":true,"writeTextFile":false},"terminal":true});
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec!["-u".into(), "-c".into(), script.into()];
        let max_active = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (client, _events) = AcpClient::spawn_with_services(
            "python3",
            &args,
            &cwd,
            &std::collections::BTreeMap::new(),
            capabilities.clone(),
            Arc::new(Echo {
                active: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                max_active: max_active.clone(),
            }),
        )
        .await
        .unwrap();
        client
            .request("probe", json!({}), Some(Duration::from_secs(2)))
            .await
            .unwrap();
        let collected = client
            .request("collect", json!({}), Some(Duration::from_secs(2)))
            .await
            .unwrap();
        assert_eq!(collected["clientCapabilities"], capabilities);
        let responses = collected["responses"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        let by_id: HashMap<String, &Value> = responses
            .iter()
            .map(|response| (response["id"].to_string(), response))
            .collect();
        assert_eq!(
            by_id[&json!("read-id").to_string()]["result"]["receivedId"],
            json!("read-id")
        );
        assert_eq!(
            by_id[&json!(7).to_string()]["result"]["receivedId"],
            json!(7)
        );
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        client.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn authentication_errors_notify_handler_without_forwarding_error_data() {
        struct AuthNotice(Arc<std::sync::Mutex<Vec<(String, Value)>>>);
        impl AcpRequestHandler for AuthNotice {
            fn handle(
                &self,
                method: String,
                _params: Value,
                _id: Value,
                _history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                Box::pin(async move { Err(AcpWireError::method_not_found(&method)) })
            }

            fn on_notification(&self, method: &str, params: &Value) {
                self.0.lock().unwrap().push((method.into(), params.clone()));
            }
        }
        let script = r#"
import json,sys
def read(): return json.loads(sys.stdin.readline())
def emit(value): print(json.dumps(value),flush=True)
initialization=read()
emit({"jsonrpc":"2.0","id":initialization["id"],"result":{"protocolVersion":1}})
request=read()
emit({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"login required","data":{"token":"must-not-be-notified"}}})
"#;
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec!["-u".into(), "-c".into(), script.into()];
        let notices = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler = Arc::new(AuthNotice(notices.clone()));
        let (client, _events) = AcpClient::spawn_with_services(
            "python3",
            &args,
            &cwd,
            &std::collections::BTreeMap::new(),
            json!({}),
            handler,
        )
        .await
        .unwrap();
        let error = client
            .request("authenticate", json!({}), Some(Duration::from_secs(2)))
            .await
            .unwrap_err();
        let notices = notices.lock().unwrap();
        assert_eq!(
            notices.as_slice(),
            &[("runtime/auth_required".into(), json!({}))]
        );
        assert!(error.contains("auth_required"));
        assert!(!error.contains("must-not-be-notified"));
        assert!(!error.contains("login required"));
        drop(notices);
        client.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn auth_notifications_reach_handler_but_not_durable_event_stream() {
        struct NotificationCapture(Arc<std::sync::Mutex<Vec<(String, Value)>>>);
        impl AcpRequestHandler for NotificationCapture {
            fn handle(
                &self,
                method: String,
                _params: Value,
                _id: Value,
                _history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                Box::pin(async move { Err(AcpWireError::method_not_found(&method)) })
            }

            fn on_notification(&self, method: &str, params: &Value) {
                self.0.lock().unwrap().push((method.into(), params.clone()));
            }
        }
        let script = r#"
import json,sys
def read(): return json.loads(sys.stdin.readline())
def emit(value): print(json.dumps(value),flush=True)
initialize=read()
emit({"jsonrpc":"2.0","id":initialize["id"],"result":{"protocolVersion":1}})
request=read()
emit({"jsonrpc":"2.0","method":"_auth/status_update","params":{"status":"authenticating","accessToken":"auth-notification-secret"}})
emit({"jsonrpc":"2.0","method":"elicitation/complete","params":{"requestId":"login-5","credential":"elicitation-secret"}})
emit({"jsonrpc":"2.0","method":"notifications/message","params":{"message":"ordinary notification"}})
emit({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-5","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"ordinary conversation"}}}})
emit({"jsonrpc":"2.0","id":request["id"],"result":{}})
sys.stdin.readline()
"#;
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec!["-u".into(), "-c".into(), script.into()];
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (client, mut events) = AcpClient::spawn_with_services(
            "python3",
            &args,
            &cwd,
            &std::collections::BTreeMap::new(),
            json!({}),
            Arc::new(NotificationCapture(received.clone())),
        )
        .await
        .unwrap();
        client
            .request("trigger", json!({}), Some(Duration::from_secs(2)))
            .await
            .unwrap();
        let first = events.recv().await.unwrap();
        let second = events.recv().await.unwrap();
        assert_eq!(first["method"], json!("notifications/message"));
        assert_eq!(second["method"], json!("session/update"));
        assert!(events.try_recv().is_err());
        let received = received.lock().unwrap();
        assert_eq!(received.len(), 3);
        assert_eq!(received[0].0, "_auth/status_update");
        assert_eq!(received[1].0, "elicitation/complete");
        assert_eq!(received[2].0, "notifications/message");
        assert!(received[0]
            .1
            .to_string()
            .contains("auth-notification-secret"));
        assert!(received[1].1.to_string().contains("elicitation-secret"));
        assert!(!first.to_string().contains("auth-notification-secret"));
        assert!(!second.to_string().contains("elicitation-secret"));
        drop(received);
        client.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prompt_and_history_error_events_drop_agent_error_data() {
        struct NoCallbacks;
        impl AcpRequestHandler for NoCallbacks {
            fn handle(
                &self,
                method: String,
                _params: Value,
                _id: Value,
                _history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                Box::pin(async move { Err(AcpWireError::method_not_found(&method)) })
            }
        }
        let script = r#"
import json,sys
def read(): return json.loads(sys.stdin.readline())
def emit(value): print(json.dumps(value),flush=True)
def fail(request,code,sentinel):
    emit({"jsonrpc":"2.0","id":request["id"],"error":{"code":code,"message":"private provider diagnostic","data":{"token":sentinel}}})
initialize=read()
emit({"jsonrpc":"2.0","id":initialize["id"],"result":{"protocolVersion":1}})
prompt=read()
fail(prompt,-32000,"prompt-secret-sentinel")
load=read()
emit({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"session-12","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"before failure"}}}})
fail(load,-32013,"history-secret-sentinel")
"#;
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec!["-u".into(), "-c".into(), script.into()];
        let (client, mut events) = AcpClient::spawn_with_services(
            "python3",
            &args,
            &cwd,
            &std::collections::BTreeMap::new(),
            json!({}),
            Arc::new(NoCallbacks),
        )
        .await
        .unwrap();
        let prompt_error = client
            .request(
                "session/prompt",
                json!({"sessionId":"session-12","prompt":[]}),
                Some(Duration::from_secs(2)),
            )
            .await
            .unwrap_err();
        assert_eq!(prompt_error, "auth_required: ACP error code -32000");
        let prompt_event = events.recv().await.unwrap();
        assert_eq!(prompt_event["method"], json!("runtime/prompt_response"));
        assert_eq!(prompt_event["params"]["error"]["code"], json!(-32000));
        assert_eq!(
            prompt_event["params"]["error"]["message"],
            json!("ACP request failed")
        );
        assert!(!prompt_event.to_string().contains("prompt-secret-sentinel"));

        let load_error = client
            .start_history_load("load-12", "session-12", &cwd)
            .await
            .unwrap()
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(load_error, "runtime_error: ACP error code -32013");
        let update = events.recv().await.unwrap();
        let end = events.recv().await.unwrap();
        assert_eq!(update["method"], json!("runtime/history"));
        assert_eq!(end["method"], json!("runtime/history_end"));
        assert_eq!(end["params"]["loadId"], json!("load-12"));
        assert_eq!(end["params"]["response"]["error"]["code"], json!(-32013));
        assert_eq!(
            end["params"]["response"]["error"]["message"],
            json!("ACP request failed")
        );
        assert!(!end.to_string().contains("history-secret-sentinel"));
        client.stop().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sessionless_elicitation_during_history_load_is_tagged_and_rejected() {
        struct ReplayGuard {
            tags: Arc<std::sync::Mutex<Vec<Option<String>>>>,
            actions: Arc<std::sync::atomic::AtomicU64>,
        }
        impl AcpRequestHandler for ReplayGuard {
            fn handle(
                &self,
                _method: String,
                _params: Value,
                _id: Value,
                history_load_id: Option<String>,
            ) -> Pin<Box<dyn Future<Output = Result<Value, AcpWireError>> + Send + 'static>>
            {
                let tags = self.tags.clone();
                let actions = self.actions.clone();
                Box::pin(async move {
                    tags.lock().unwrap().push(history_load_id.clone());
                    if history_load_id.is_some() {
                        Err(AcpWireError {
                            code: -32001,
                            message: "Client callbacks are disabled during history replay".into(),
                            data: None,
                        })
                    } else {
                        actions.fetch_add(1, Ordering::SeqCst);
                        Ok(json!({"opened":true}))
                    }
                })
            }
        }
        let script = r#"
import json,sys
def read(): return json.loads(sys.stdin.readline())
def emit(value): print(json.dumps(value),flush=True)
initialize=read()
emit({"jsonrpc":"2.0","id":initialize["id"],"result":{"protocolVersion":1}})
load=read()
emit({"jsonrpc":"2.0","id":"elicitation-7","method":"elicitation/create","params":{"requestId":"auth-step","mode":"url","url":"https://example.invalid/login"}})
callback=read()
emit({"jsonrpc":"2.0","id":load["id"],"result":{"callbackResponse":callback}})
"#;
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec!["-u".into(), "-c".into(), script.into()];
        let tags = Arc::new(std::sync::Mutex::new(Vec::new()));
        let actions = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let handler = Arc::new(ReplayGuard {
            tags: tags.clone(),
            actions: actions.clone(),
        });
        let (client, _events) = AcpClient::spawn_with_services(
            "python3",
            &args,
            &cwd,
            &std::collections::BTreeMap::new(),
            json!({}),
            handler,
        )
        .await
        .unwrap();
        let result = client
            .start_history_load("load-7", "session-7", &cwd)
            .await
            .unwrap()
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result["callbackResponse"]["error"]["code"], json!(-32001));
        assert_eq!(tags.lock().unwrap().as_slice(), &[Some("load-7".into())]);
        assert_eq!(actions.load(Ordering::SeqCst), 0);
        client.stop().await;
    }
}
