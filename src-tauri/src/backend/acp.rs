//! ACP v1 bidirectional newline JSON-RPC transport. Reading never waits on storage or a prompt.
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, oneshot, Mutex},
};

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
        let mut child = Command::new(executable)
            .args(args)
            .current_dir(cwd)
            .env("PWD", cwd)
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
            let mut lines = BufReader::new(stdout).lines();
            let failure = loop {
                match lines.next_line().await {
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
                                if method == "session/prompt" {
                                    let _ = tx.send(json!({"method":"runtime/prompt_response","params":v.clone()}));
                                }
                                if let Some(load) = method.strip_prefix("history:") {
                                    read_loads.lock().await.retain(|_, v| v != load);
                                    // This barrier is on the same queue as every preceding update.
                                    let _ = tx.send(json!({"method":"runtime/history_end","params":{"loadId":load,"response":v}}));
                                }
                                let result = if let Some(e) = v.get("error") {
                                    Err(format!(
                                        "{}: {}",
                                        if e["code"] == -32000 {
                                            "auth_required"
                                        } else {
                                            "runtime_error"
                                        },
                                        e
                                    ))
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
                        } else if v.get("id").is_some() {
                            let response = json!({"jsonrpc":"2.0","id":v["id"],"error":{"code":-32601,"message":"Client capability unavailable"}});
                            let mut input = read_stdin.lock().await;
                            let _ = input.write_all(format!("{response}\n").as_bytes()).await;
                        }
                    }
                    Ok(None) => break "ACP process closed its output".into(),
                    Err(e) => break e.to_string(),
                }
            };
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
        let initialized = client.request("initialize",json!({"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false,"session":{"compaction":{}}},"clientInfo":{"name":"codex-monitor","version":env!("CARGO_PKG_VERSION")}}),Some(Duration::from_secs(30))).await?;
        if initialized["protocolVersion"] != 1 {
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
