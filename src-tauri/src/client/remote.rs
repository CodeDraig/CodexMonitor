use super::{
    contracts::{BackendRequest, EventKind},
    events::EventInbox,
    BackendDriver, BackendHandle, ClientFuture,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::{mpsc, oneshot, Mutex},
    time::timeout,
};

#[derive(Clone)]
pub(crate) struct RemoteConfig {
    pub host: String,
    /// Kept private from Debug output and never included in endpoint identity.
    pub token: Option<String>,
}

struct RemoteWire {
    tx: mpsc::Sender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub(crate) struct RemoteDriver {
    wire: Arc<RemoteWire>,
}

impl RemoteDriver {
    pub(crate) async fn connect(config: RemoteConfig, inbox: EventInbox) -> Result<Self, String> {
        let origin_scope = inbox.scope();
        let stream = TcpStream::connect(config.host.as_str())
            .await
            .map_err(|e| format!("transport_lost: {e}"))?;
        let (read, mut write) = stream.into_split();
        let (tx, mut outbound) = mpsc::channel::<String>(256);
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let inbox_reader = inbox.clone();

        tokio::spawn(async move {
            while let Some(line) = outbound.recv().await {
                if write.write_all(line.as_bytes()).await.is_err()
                    || write.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(value) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if let Some(id) = value.get("id").and_then(Value::as_u64) {
                    let result = if let Some(error) = value.get("error") {
                        Err(error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("remote error")
                            .to_owned())
                    } else {
                        Ok(value.get("result").cloned().unwrap_or(Value::Null))
                    };
                    if let Some(sender) = pending_reader.lock().await.remove(&id) {
                        let _ = sender.send(result);
                    }
                    continue;
                }
                let method = value
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let params = value.get("params").cloned().unwrap_or(Value::Null);
                match method {
                    "runtime-event" => inbox_reader.push_event_for(
                        origin_scope.clone(),
                        EventKind::Runtime,
                        params,
                    ),
                    "app-server-event" => inbox_reader.push_event_for(
                        origin_scope.clone(),
                        EventKind::AppServer,
                        normalize_app_server_event(params),
                    ),
                    "terminal-output" => inbox_reader.push_event_for(
                        origin_scope.clone(),
                        EventKind::TerminalOutput,
                        params,
                    ),
                    "terminal-exit" => inbox_reader.push_event_for(
                        origin_scope.clone(),
                        EventKind::TerminalExit,
                        params,
                    ),
                    "resync-required" => inbox_reader.push_resync_for(
                        origin_scope.clone(),
                        super::contracts::EventStream::Runtime,
                    ),
                    _ => {}
                }
            }
            for (_, sender) in pending_reader.lock().await.drain() {
                let _ = sender.send(Err("transport_lost: remote connection closed".into()));
            }
        });

        let driver = Self {
            wire: Arc::new(RemoteWire {
                tx,
                pending,
                next_id: AtomicU64::new(1),
            }),
        };
        if let Some(token) = config.token {
            let _ = timeout(
                Duration::from_secs(15),
                driver.call("auth", json!({"token":token})),
            )
            .await
            .map_err(|_| "auth_required: daemon authentication timed out".to_string())??;
        }
        Ok(driver)
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.wire.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.wire.pending.lock().await.insert(id, tx);
        let line = serde_json::to_string(&json!({"id":id,"method":method,"params":params}))
            .map_err(|e| e.to_string())?;
        timeout(Duration::from_secs(15), self.wire.tx.send(line))
            .await
            .map_err(|_| "transport_lost: request queue timeout".to_string())?
            .map_err(|_| "transport_lost: connection closed".to_string())?;
        timeout(Duration::from_secs(300), rx)
            .await
            .map_err(|_| "transport_lost: remote request timed out".to_string())?
            .map_err(|_| "transport_lost: connection closed".to_string())?
    }
}

/// The daemon's established JSON-RPC wire shape serializes AppServerEvent with
/// the Rust field name `workspace_id`, while the host-independent native
/// controller contract uses the Tauri-style `workspaceId` spelling. Normalize
/// at this adapter boundary and retain the daemon's public wire contract.
fn normalize_app_server_event(mut params: Value) -> Value {
    if let Some(object) = params.as_object_mut() {
        if !object.contains_key("workspaceId") {
            if let Some(workspace_id) = object.remove("workspace_id") {
                object.insert("workspaceId".into(), workspace_id);
            }
        }
    }
    params
}

impl BackendDriver for RemoteDriver {
    fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a> {
        Box::pin(async move {
            let (method, params) = remote_rpc_call(request);
            self.call(&method, params).await
        })
    }
}

fn remote_rpc_call(request: BackendRequest) -> (String, Value) {
    let rpc = request.rpc();
    match request {
        BackendRequest::Runtime(_) => (
            "runtime_request".to_string(),
            json!({"method":rpc.method,"input":rpc.params}),
        ),
        BackendRequest::Runs(_) => (rpc.method, json!({"input":rpc.params})),
        BackendRequest::Workspaces(_) if rpc.method == "add_workspace_from_git_url" => {
            let mut params = rpc.params;
            if let Some(object) = params.as_object_mut() {
                if let Some(value) = object.remove("destinationPath") {
                    object.insert("destination_path".into(), value);
                }
                if let Some(value) = object.remove("targetFolderName") {
                    object.insert("target_folder_name".into(), value);
                }
            }
            (rpc.method, params)
        }
        _ => (rpc.method, rpc.params),
    }
}

struct LazyRemoteDriver {
    config: RemoteConfig,
    inbox: EventInbox,
    connected: Mutex<Option<RemoteDriver>>,
}

impl BackendDriver for LazyRemoteDriver {
    fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a> {
        Box::pin(async move {
            let driver = {
                let mut guard = self.connected.lock().await;
                if guard.is_none() {
                    *guard =
                        Some(RemoteDriver::connect(self.config.clone(), self.inbox.clone()).await?);
                }
                guard.as_ref().expect("remote connected").clone()
            };
            let result = driver.execute(request).await;
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error.starts_with("transport_lost:"))
            {
                let mut guard = self.connected.lock().await;
                if guard
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(&current.wire, &driver.wire))
                {
                    *guard = None;
                }
            }
            result
        })
    }
}

pub(crate) fn spawn_remote(
    config: RemoteConfig,
    scope: super::contracts::BackendScope,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<BackendHandle, String> {
    let (inbox, rx) = EventInbox::new(scope.clone(), Arc::new(wake));
    Ok(BackendHandle::spawn_parts(
        LazyRemoteDriver {
            config,
            inbox: inbox.clone(),
            connected: Mutex::new(None),
        },
        scope,
        inbox,
        rx,
    ))
}

/// Remote operations use the daemon, while desktop-only app settings remain in
/// the local settings file and are never sent to the remote store.
pub(crate) fn spawn_remote_with_host(
    config: RemoteConfig,
    core: Arc<crate::shared::host_core::CoreState>,
    scope: super::contracts::BackendScope,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<BackendHandle, String> {
    let (inbox, rx) = EventInbox::new(scope.clone(), Arc::new(wake));
    let host = super::local::LocalDriver::new(core, inbox.clone());
    Ok(BackendHandle::spawn_parts(
        RemoteWithHostDriver {
            remote: LazyRemoteDriver {
                config,
                inbox: inbox.clone(),
                connected: Mutex::new(None),
            },
            host,
        },
        scope,
        inbox,
        rx,
    ))
}

struct RemoteWithHostDriver {
    remote: LazyRemoteDriver,
    host: super::local::LocalDriver,
}

impl BackendDriver for RemoteWithHostDriver {
    fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a> {
        match request {
            request @ BackendRequest::Settings(_) => self.host.execute(request),
            request => self.remote.execute(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::contracts::{
        CodexRequest, RequestSafety, RunsRequest, RuntimeMethod, RuntimeRequest, WorkspaceRequest,
    };
    use uuid::Uuid;

    #[test]
    fn runtime_and_runs_requests_match_daemon_wire_envelopes() {
        let (method, params) = remote_rpc_call(BackendRequest::Runtime(RuntimeRequest::read(
            RuntimeMethod::ReadConversation,
            json!({"sessionId":"s","cursor":12}),
        )));
        assert_eq!(method, "runtime_request");
        assert_eq!(
            params,
            json!({"method":"read_conversation","input":{"sessionId":"s","cursor":12}})
        );

        let request = RunsRequest::mutation(
            "save_run_comparison",
            json!({"requestId":"idempotency-key","notes":"draft"}),
            Some(Uuid::new_v4()),
        );
        let (method, params) = remote_rpc_call(BackendRequest::Runs(request));
        assert_eq!(method, "save_run_comparison");
        assert_eq!(params["input"]["requestId"], "idempotency-key");
        assert!(matches!(
            BackendRequest::Runs(RunsRequest::mutation(
                "save_run_comparison",
                json!({}),
                None
            ))
            .rpc()
            .safety,
            RequestSafety::Mutation { .. }
        ));
    }

    #[test]
    fn workspace_url_payload_matches_the_existing_daemon_contract() {
        let (method, params) = remote_rpc_call(BackendRequest::Workspaces(
            WorkspaceRequest::mutation(
                "add_workspace_from_git_url",
                json!({"url":"https://example.invalid/repo.git","destinationPath":"/tmp/copy","targetFolderName":"copy"}),
                None,
            ),
        ));
        assert_eq!(method, "add_workspace_from_git_url");
        assert_eq!(
            params,
            json!({"url":"https://example.invalid/repo.git","destination_path":"/tmp/copy","target_folder_name":"copy"})
        );
    }

    #[test]
    fn codex_feature_flag_uses_the_existing_daemon_wire_contract() {
        let request = CodexRequest::set_feature_flag("unified_exec", true);
        assert!(matches!(
            request.0.safety,
            RequestSafety::Mutation { request_id: None }
        ));
        let (method, params) = remote_rpc_call(BackendRequest::Codex(request));
        assert_eq!(method, "set_codex_feature_flag");
        assert_eq!(params, json!({"featureKey":"unified_exec","enabled":true}));
    }

    #[test]
    fn files_and_git_categories_keep_the_native_daemon_method_and_payload() {
        use crate::client::contracts::{FilesRequest, GitRequest};

        let files_params = json!({"workspaceId":"workspace-1"});
        let (files_method, sent_files) = remote_rpc_call(BackendRequest::Files(
            FilesRequest::read("list_workspace_files", files_params.clone()),
        ));
        assert_eq!(files_method, "list_workspace_files");
        assert_eq!(sent_files, files_params);

        let git_params = json!({"workspaceId":"workspace-1","prNumber":27});
        let (git_method, sent_git) = remote_rpc_call(BackendRequest::Git(GitRequest::read(
            "get_github_pull_request_diff",
            git_params.clone(),
        )));
        assert_eq!(git_method, "get_github_pull_request_diff");
        assert_eq!(sent_git, git_params);
    }

    #[test]
    fn remote_configuration_has_no_debug_surface_for_token() {
        let config = RemoteConfig {
            host: "example.invalid:4732".into(),
            token: Some("secret".into()),
        };
        assert_eq!(config.host, "example.invalid:4732");
    }

    #[test]
    fn remote_with_host_keeps_app_settings_on_the_local_store() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-remote-host-settings-{}",
                Uuid::new_v4()
            ));
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(crate::shared::host_core::CoreState::load(paths).unwrap());
            let scope = super::super::contracts::BackendScope {
                endpoint_id: "remote-fixture".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = RemoteWithHostDriver {
                remote: LazyRemoteDriver {
                    config: RemoteConfig {
                        // This endpoint is intentionally unusable: if settings are
                        // accidentally sent remotely, this request must fail.
                        host: "127.0.0.1:0".into(),
                        token: Some("must-not-be-sent".into()),
                    },
                    inbox: inbox.clone(),
                    connected: Mutex::new(None),
                },
                host: super::super::local::LocalDriver::new(core, inbox),
            };
            let result = driver
                .execute(BackendRequest::Settings(
                    super::super::contracts::SettingsRequest::read("get_app_settings", json!({})),
                ))
                .await
                .expect("settings should be read from the local host without connecting remotely");
            assert!(result.is_object());
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn authenticated_daemon_fixture_exercises_runtime_request_and_event_delivery() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                let auth: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(auth["method"], "auth");
                assert_eq!(auth["params"]["token"], "fixture-token");
                write.write_all(format!("{{\"id\":{},\"result\":{{\"ok\":true}}}}\n", auth["id"].as_u64().unwrap()).as_bytes()).await.unwrap();

                let request: Value = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], "runtime_request");
                assert_eq!(request["params"]["method"], "read_conversation");
                assert_eq!(request["params"]["input"]["sessionId"], "session-1");
                write.write_all(b"{\"method\":\"runtime-event\",\"params\":{\"storeId\":\"store-1\",\"sessionId\":\"session-1\",\"sequence\":1,\"kind\":\"message\"}}\n").await.unwrap();
                write.write_all(format!("{{\"id\":{},\"result\":{{\"items\":[1]}}}}\n", request["id"].as_u64().unwrap()).as_bytes()).await.unwrap();
            });

            let scope = super::super::contracts::BackendScope { endpoint_id: "fixture-daemon".into(), connection_epoch: 4, store_id: Some("store-1".into()) };
            let (inbox, rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = RemoteDriver::connect(RemoteConfig { host: address.to_string(), token: Some("fixture-token".into()) }, inbox).await.unwrap();
            let result = driver.execute(BackendRequest::Runtime(RuntimeRequest::read(RuntimeMethod::ReadConversation, json!({"sessionId":"session-1"})))).await.unwrap();
            assert_eq!(result, json!({"items":[1]}));
            let message = rx.recv().unwrap();
            assert!(matches!(message, super::super::contracts::BackendMessage::Event { event: super::super::contracts::BackendEvent { sequence: Some(1), .. }, .. }));
            server.await.unwrap();
        });
    }

    #[test]
    fn daemon_codex_notifications_normalize_legacy_workspace_field_and_keep_scope() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let event_payloads = [
                serde_json::to_value(crate::backend::events::AppServerEvent {
                    workspace_id: "workspace-remote".into(),
                    message: json!({
                        "method":"item/agentMessage/delta",
                        "params":{"threadId":"thread-remote","turnId":"turn-1","itemId":"item-1","delta":"remote answer"}
                    }),
                })
                .unwrap(),
                serde_json::to_value(crate::backend::events::AppServerEvent {
                    workspace_id: "workspace-remote".into(),
                    message: json!({
                        "method":"turn/completed",
                        "params":{"threadId":"thread-remote","turn":{"id":"turn-1","status":"completed"}}
                    }),
                })
                .unwrap(),
            ];
            assert!(event_payloads[0].get("workspace_id").is_some());
            assert!(event_payloads[0].get("workspaceId").is_none());

            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                let auth: Value = serde_json::from_str(
                    &lines.next_line().await.unwrap().expect("auth line"),
                )
                .unwrap();
                assert_eq!(auth["method"], "auth");
                write
                    .write_all(
                        format!(
                            "{{\"id\":{},\"result\":{{\"ok\":true}}}}\n",
                            auth["id"].as_u64().unwrap()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                for params in event_payloads {
                    let line = serde_json::to_string(
                        &json!({"method":"app-server-event","params":params}),
                    )
                    .unwrap();
                    write.write_all(line.as_bytes()).await.unwrap();
                    write.write_all(b"\n").await.unwrap();
                }
            });

            let scope = super::super::contracts::BackendScope {
                endpoint_id: "remote-fixture".into(),
                connection_epoch: 9,
                store_id: Some("remote-store".into()),
            };
            let (inbox, rx) = EventInbox::new(scope.clone(), Arc::new(|| {}));
            let _driver = RemoteDriver::connect(
                RemoteConfig {
                    host: address.to_string(),
                    token: Some("fixture-token".into()),
                },
                inbox,
            )
            .await
            .unwrap();

            let mut received = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), async {
                while received.len() < 2 {
                    if let Ok(message) = rx.try_recv() {
                        received.push(message);
                    } else {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            })
            .await
            .expect("daemon app-server events should reach the native inbox");
            server.await.unwrap();

            for event in &received {
                assert!(matches!(event, super::super::contracts::BackendMessage::Event { scope: actual, event } if actual == &scope && event.kind == EventKind::AppServer));
            }
            let payloads: Vec<_> = received
                .into_iter()
                .map(|event| match event {
                    super::super::contracts::BackendMessage::Event { event, .. } => event.payload,
                    other => panic!("expected Codex event, got {other:?}"),
                })
                .collect();
            assert_eq!(payloads[0]["workspaceId"], "workspace-remote");
            assert!(payloads[0].get("workspace_id").is_none());
            assert_eq!(
                payloads[0]["message"]["params"]["delta"],
                "remote answer"
            );
            assert_eq!(payloads[1]["workspaceId"], "workspace-remote");
            assert_eq!(payloads[1]["message"]["method"], "turn/completed");
            assert_eq!(payloads[1]["message"]["params"]["threadId"], "thread-remote");
        });
    }

    #[test]
    fn remote_with_host_routes_codex_feature_flag_to_effective_remote_daemon() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
                    .await
                    .expect("Codex mutation was not routed to the effective remote backend")
                    .unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = tokio::io::BufReader::new(read).lines();
                let auth: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().expect("auth line"))
                        .unwrap();
                assert_eq!(auth["method"], "auth");
                assert_eq!(auth["params"]["token"], "feature-flag-fixture-token");
                write
                    .write_all(
                        format!(
                            "{{\"id\":{},\"result\":{{\"ok\":true}}}}\n",
                            auth["id"].as_u64().unwrap()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().expect("request line"))
                        .unwrap();
                assert_eq!(request["method"], "set_codex_feature_flag");
                assert_eq!(
                    request["params"],
                    json!({"featureKey":"unified_exec","enabled":true})
                );
                write
                    .write_all(
                        format!(
                            "{{\"id\":{},\"result\":{{\"ok\":true}}}}\n",
                            request["id"].as_u64().unwrap()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            });

            let data = std::env::temp_dir().join(format!(
                "codex-monitor-remote-feature-flag-{}",
                Uuid::new_v4()
            ));
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(crate::shared::host_core::CoreState::load(paths).unwrap());
            let scope = super::super::contracts::BackendScope {
                endpoint_id: "remote-feature-flag-fixture".into(),
                connection_epoch: 2,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = RemoteWithHostDriver {
                remote: LazyRemoteDriver {
                    config: RemoteConfig {
                        host: address.to_string(),
                        token: Some("feature-flag-fixture-token".into()),
                    },
                    inbox: inbox.clone(),
                    connected: Mutex::new(None),
                },
                host: super::super::local::LocalDriver::new(core, inbox),
            };
            let response = driver
                .execute(BackendRequest::Codex(CodexRequest::set_feature_flag(
                    "unified_exec",
                    true,
                )))
                .await
                .unwrap();
            assert_eq!(response, json!({"ok":true}));
            server.await.unwrap();
            std::fs::remove_dir_all(data).unwrap();
        });
    }
}
