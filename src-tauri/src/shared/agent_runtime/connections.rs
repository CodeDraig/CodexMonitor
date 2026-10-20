//! Shared ACP connection and authentication lifecycle.
//!
//! Connections live on the backend selected by the caller. Prepared connections
//! can be inspected/authenticated before a native session exists, then consumed
//! once by the regular session creation path.
use super::{
    client_callbacks::ClientCallbacks, client_store::AcpTerminalSnapshot,
    login_terminal::LoginTerminals, registry::persist, types::*,
};
use crate::{backend::acp::AcpClient, shared::runs_core::store::with_store};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{mpsc, oneshot, Mutex},
    task::JoinHandle,
};
use uuid::Uuid;

fn capability_advertised(value: &Value) -> bool {
    !value.is_null() && value != false
}

fn terminal_auth_launch(
    profile: &RuntimeProfile,
    method: &Value,
) -> Result<(Vec<String>, std::collections::BTreeMap<String, String>), String> {
    let mut args = profile.launch_args();
    match &method["args"] {
        Value::Null => {}
        Value::Array(values) => {
            for value in values {
                args.push(
                    value
                        .as_str()
                        .ok_or("invalid_input: terminal auth args must be strings")?
                        .to_owned(),
                );
            }
        }
        _ => return Err("invalid_input: terminal auth args must be an array".into()),
    }
    let mut environment = profile.resolve_environment()?;
    match &method["env"] {
        Value::Null => {}
        Value::Object(values) => {
            for (key, value) in values {
                let value = value
                    .as_str()
                    .ok_or("invalid_input: terminal auth environment values must be strings")?;
                if key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
                {
                    return Err(
                        "invalid_input: terminal auth environment contains an invalid entry".into(),
                    );
                }
                environment.insert(key.clone(), value.to_owned());
            }
        }
        _ => return Err("invalid_input: terminal auth environment must be an object".into()),
    }
    Ok((args, environment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (PathBuf, RuntimeProfile) {
        let root = std::env::temp_dir().join(format!("acp-connection-manager-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/shared/agent_runtime/fixtures/client_callbacks_acp.py");
        let profile = RuntimeProfile {
            id: "fixture".into(),
            revision: 1,
            name: "Fixture".into(),
            kind: RuntimeKind::Acp,
            executable: "python3".into(),
            args: vec![fixture.to_string_lossy().into_owned()],
            working_directory: None,
            preset_id: None,
            environment: Default::default(),
            client_policy: Default::default(),
        };
        (root, profile)
    }

    async fn wait_operation(manager: &ConnectionManager, operation_id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let value = manager.operation(operation_id).await.unwrap();
                if value["state"] != "pending" && value["state"] != "awaiting_user" {
                    return value;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn prepared_connection_auth_is_idempotent_and_rejects_stale_generations() {
        let (root, profile) = fixture();
        let manager = ConnectionManager::default();
        let ticket = manager
            .prepare(
                root.join("runs.sqlite3"),
                profile.clone(),
                root.clone(),
                None,
                false,
                "prepare-1".into(),
            )
            .await
            .unwrap();
        let ready = wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await;
        assert_eq!(ready["state"], "succeeded");
        let connection = ticket["connectionId"].as_str().unwrap();
        let generation = ticket["generation"].as_u64().unwrap();
        let first = manager
            .authenticate(
                connection,
                generation,
                "fixture-agent-auth",
                "auth-1".into(),
            )
            .await
            .unwrap();
        let repeated = manager
            .authenticate(
                connection,
                generation,
                "fixture-agent-auth",
                "auth-1".into(),
            )
            .await
            .unwrap();
        assert_eq!(first, repeated);
        let completed = wait_operation(&manager, first["operationId"].as_str().unwrap()).await;
        assert_eq!(completed["state"], "succeeded");
        assert_eq!(completed["result"], json!({"authenticated":true}));
        assert!(manager
            .authenticate(
                connection,
                generation + 1,
                "fixture-agent-auth",
                "auth-stale".into()
            )
            .await
            .unwrap_err()
            .contains("generation is stale"));
        manager
            .close_connection(connection, generation)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn closing_monitor_session_stops_every_matching_acp_connection() {
        let (root, profile) = fixture();
        let manager = ConnectionManager::default();
        let session_id = Uuid::new_v4().to_string();
        let mut tickets = Vec::new();
        for request_id in ["prepare-one", "prepare-two"] {
            let ticket = manager
                .prepare(
                    root.join("runs.sqlite3"),
                    profile.clone(),
                    root.clone(),
                    Some(session_id.clone()),
                    false,
                    request_id.into(),
                )
                .await
                .unwrap();
            assert_eq!(
                wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await["state"],
                "succeeded"
            );
            tickets.push(ticket);
        }

        for ticket in &tickets {
            assert!(manager
                .status(ticket["connectionId"].as_str().unwrap())
                .await
                .is_ok());
        }
        manager.close_session(&session_id).await.unwrap();
        for ticket in &tickets {
            assert!(manager
                .status(ticket["connectionId"].as_str().unwrap())
                .await
                .unwrap_err()
                .contains("connection not found"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn closing_prepared_connection_releases_idempotency_ticket() {
        let (root, profile) = fixture();
        let manager = ConnectionManager::default();
        let request_id = "close-prepared";
        let path = root.join("runs.sqlite3");
        let ticket = manager
            .prepare(
                path.clone(),
                profile.clone(),
                root.clone(),
                None,
                false,
                request_id.into(),
            )
            .await
            .unwrap();
        let completed = wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await;
        assert_eq!(completed["state"], "succeeded");
        assert_eq!(completed["result"]["ready"], true);
        let connection_id = ticket["connectionId"].as_str().unwrap();
        let generation = ticket["generation"].as_u64().unwrap();
        assert_eq!(
            manager
                .close_prepared_connection(connection_id, generation)
                .await
                .unwrap()["state"],
            "closed"
        );
        assert!(manager
            .status(connection_id)
            .await
            .unwrap_err()
            .contains("connection not found"));
        let replacement = manager
            .prepare(path, profile, root.clone(), None, false, request_id.into())
            .await
            .unwrap();
        assert_ne!(replacement["connectionId"], ticket["connectionId"]);
        manager
            .close_prepared_connection(replacement["connectionId"].as_str().unwrap(), generation)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn prepared_handoff_rejects_policy_change_but_allows_display_rename() {
        let (root, mut profile) = fixture();
        profile.client_policy = AcpClientPolicy {
            filesystem: AcpFilesystemPolicy::ReadWrite,
            command_execution: AcpCommandExecutionPolicy::Allow,
            additional_roots: vec![root.to_string_lossy().into_owned()],
        };
        let manager = ConnectionManager::default();
        let ticket = manager
            .prepare(
                root.with_extension("runs.sqlite3"),
                profile.clone(),
                root.clone(),
                None,
                false,
                "policy-handoff".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await["state"],
            "succeeded"
        );
        let connection_id = ticket["connectionId"].as_str().unwrap();

        let mut disabled = profile.clone();
        disabled.client_policy = AcpClientPolicy::default();
        assert_eq!(profile.connection_scope(), disabled.connection_scope());
        assert!(manager
            .take_prepared(connection_id, 1, &disabled, root.to_str().unwrap(), false)
            .await
            .err()
            .unwrap()
            .contains("client policy changed"));

        profile.name = "Renamed presentation".into();
        let (client, _events, _callbacks, consumed_id) = manager
            .take_prepared(connection_id, 1, &profile, root.to_str().unwrap(), false)
            .await
            .unwrap();
        assert!(client.alive());
        manager.close_connection(&consumed_id, 1).await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn close_cancels_a_nonresponsive_initialize_promptly() {
        let (root, mut profile) = fixture();
        profile.args.push("--never-initialize".into());
        let manager = ConnectionManager::default();
        let ticket = manager
            .prepare(
                root.join("runs.sqlite3"),
                profile,
                root.clone(),
                None,
                false,
                "close-during-initialize".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let pid_ready = root.join("fixture.pid").exists();
                let initialize_seen =
                    std::fs::read_to_string(root.join("client-callback-acp.jsonl"))
                        .is_ok_and(|log| log.contains("initialize"));
                if pid_ready && initialize_seen {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture should receive initialize before close");

        let started = std::time::Instant::now();
        manager
            .close_prepared_connection(
                ticket["connectionId"].as_str().unwrap(),
                ticket["generation"].as_u64().unwrap(),
            )
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            manager
                .operation(ticket["operationId"].as_str().unwrap())
                .await
                .unwrap_err()
                .contains("operation not found"),
            true
        );
        assert!(manager
            .status(ticket["connectionId"].as_str().unwrap())
            .await
            .unwrap_err()
            .contains("connection not found"));

        #[cfg(target_os = "linux")]
        {
            let pid = std::fs::read_to_string(root.join("fixture.pid"))
                .unwrap()
                .parse::<u32>()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let proc = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                    let exited = proc
                        .as_deref()
                        .map(|stat| {
                            stat.rsplit_once(')')
                                .map(|(_, tail)| tail.trim_start().starts_with('Z'))
                                .unwrap_or(false)
                        })
                        .unwrap_or(true);
                    if exited {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("cancelled initialize child should exit promptly");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(feature = "acp-host-services")]
    #[tokio::test]
    async fn terminal_auth_cancellation_is_acknowledged_and_does_not_advance_generation() {
        let (root, mut profile) = fixture();
        profile.environment.insert(
            "FIXTURE".into(),
            EnvironmentSource::Literal {
                value: "test".into(),
            },
        );
        let manager = ConnectionManager::default();
        let ticket = manager
            .prepare(
                root.join("runs.sqlite3"),
                profile,
                root.clone(),
                None,
                false,
                "prepare-terminal".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await["state"],
            "succeeded"
        );
        let connection = ticket["connectionId"].as_str().unwrap();
        let generation = ticket["generation"].as_u64().unwrap();
        let auth = manager
            .authenticate(
                connection,
                generation,
                "fixture-terminal-auth",
                "auth-terminal".into(),
            )
            .await
            .unwrap();
        let operation = auth["operationId"].as_str().unwrap();
        assert!(manager
            .login_call(
                "read_login_terminal",
                &json!({"operationId":ticket["operationId"],"generation":generation}),
            )
            .await
            .unwrap_err()
            .contains("no login terminal"));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager.operation(operation).await.unwrap()["state"] == "awaiting_user" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert!(manager
                .login_call(
                    "read_login_terminal",
                    &json!({"operationId":operation,"generation":generation,"connectionId":"another-connection"}),
                )
                .await
                .unwrap_err()
                .contains("another connection"));
            loop {
                let output = manager
                    .login_call(
                        "read_login_terminal",
                        &json!({"operationId":operation,"generation":generation,"cursor":0}),
                    )
                    .await
                    .unwrap();
                if output["output"]
                    .as_str()
                    .unwrap_or("")
                    .contains("CALLBACK_FIXTURE_LOGIN_READY")
                    && output["output"]
                        .as_str()
                        .unwrap_or("")
                        .contains("CALLBACK_FIXTURE_TERMINAL_ENV_OK")
                {
                    break;
                }
                assert_eq!(output["state"], "running", "terminal exited before its readiness marker");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let cancelled = manager
            .cancel_operation(operation, generation)
            .await
            .unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(
            wait_operation(&manager, operation).await["state"],
            "cancelled"
        );
        assert_eq!(
            manager.status(connection).await.unwrap()["generation"],
            generation
        );
        manager
            .close_connection(connection, generation)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
    #[cfg(feature = "acp-host-services")]
    #[tokio::test]
    async fn completed_login_terminal_can_be_read_and_closed_after_reconnect() {
        let (root, profile) = fixture();
        let manager = ConnectionManager::default();
        let ticket = manager
            .prepare(
                root.join("runs.sqlite3"),
                profile,
                root.clone(),
                None,
                false,
                "prepare-successful-terminal".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            wait_operation(&manager, ticket["operationId"].as_str().unwrap()).await["state"],
            "succeeded"
        );
        let connection = ticket["connectionId"].as_str().unwrap();
        let generation = ticket["generation"].as_u64().unwrap();
        let auth = manager
            .authenticate(
                connection,
                generation,
                "fixture-terminal-auth",
                "successful-terminal-auth".into(),
            )
            .await
            .unwrap();
        let operation = auth["operationId"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if manager.operation(operation).await.unwrap()["state"] == "awaiting_user" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        manager.login_call("write_login_terminal", &json!({"operationId":operation,"generation":generation,"inputSequence":1,"data":"QUNDRVBUX0ZJWFRVUkVfTE9HSU4K"})).await.unwrap();
        assert_eq!(
            wait_operation(&manager, operation).await["state"],
            "succeeded"
        );
        assert_eq!(
            manager.status(connection).await.unwrap()["generation"],
            generation + 1
        );
        let read = manager
            .login_call(
                "read_login_terminal",
                &json!({"operationId":operation,"generation":generation,"cursor":0}),
            )
            .await
            .unwrap();
        assert_eq!(read["state"], "succeeded");
        let closed = manager
            .login_call(
                "close_login_terminal",
                &json!({"operationId":operation,"generation":generation}),
            )
            .await
            .unwrap();
        assert_eq!(closed["state"], "succeeded");
        assert!(manager
            .operation(operation)
            .await
            .unwrap_err()
            .contains("operation not found"));
        manager
            .close_connection(connection, generation + 1)
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn session_status_reads_only_the_existing_foreground_connection() {
        let (root, profile) = fixture();
        let manager = ConnectionManager::default();
        let session_id = Uuid::new_v4().to_string();
        let (_client, _events, _callbacks, connection_id) = manager
            .spawn_auto(root.join("runs.sqlite3"), profile, root.clone(), false)
            .await
            .unwrap();
        manager
            .bind(&connection_id, &session_id, "fixture-native-session")
            .await
            .unwrap();

        let status = manager.status_for_session(&session_id).await.unwrap();
        assert_eq!(status["connectionId"], connection_id);
        assert_eq!(status["generation"], 1);
        assert_eq!(status["state"], "ready");
        assert!(status["pendingClientRequests"].is_array());

        let other_backend = ConnectionManager::default();
        assert!(other_backend
            .status_for_session(&session_id)
            .await
            .unwrap_err()
            .contains("live ACP connection for Monitor session not found"));
        assert!(manager
            .status_for_session("unbound-monitor-session")
            .await
            .unwrap_err()
            .contains("live ACP connection for Monitor session not found"));

        manager.close_connection(&connection_id, 1).await.unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}

pub(crate) type SpawnedConnection = (
    Arc<AcpClient>,
    mpsc::UnboundedReceiver<Value>,
    Arc<ClientCallbacks>,
    String,
);

const LOGIN_TERMINAL_RETENTION: Duration = Duration::from_secs(10 * 60);

struct ConnectionEntry {
    id: String,
    generation: u64,
    profile: RuntimeProfile,
    path: PathBuf,
    cwd: PathBuf,
    launch_cwd: PathBuf,
    discovery: bool,
    client: Arc<AcpClient>,
    callbacks: Arc<ClientCallbacks>,
    events: Option<mpsc::UnboundedReceiver<Value>>,
    consumed: bool,
    monitor_session_id: Option<String>,
    bound_native_id: Option<String>,
    auth_methods: Vec<Value>,
    state: String,
}

struct OperationEntry {
    value: Arc<Mutex<Value>>,
    task: Option<JoinHandle<()>>,
    connection_id: String,
    monitor_session_id: Option<String>,
    generation: u64,
    login_terminal: bool,
    preparation: bool,
}

#[derive(Default)]
struct ManagerState {
    connections: HashMap<String, ConnectionEntry>,
    operations: HashMap<String, OperationEntry>,
    prepared_requests: HashMap<(PathBuf, String, String), Value>,
    auth_requests: HashMap<(String, String), Value>,
    closing_connections: std::collections::HashSet<String>,
}

#[derive(Clone)]
pub(crate) struct ConnectionManager {
    state: Arc<Mutex<ManagerState>>,
    login: Arc<LoginTerminals>,
    prepare_lock: Arc<Mutex<()>>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(ManagerState::default())),
            login: Arc::new(LoginTerminals::new()),
            prepare_lock: Arc::new(Mutex::new(())),
        }
    }
}

impl ConnectionManager {
    async fn spawn_inner(
        &self,
        path: PathBuf,
        profile: RuntimeProfile,
        cwd: PathBuf,
        session_id: Option<String>,
        discovery: bool,
        prepared: bool,
        connection_id: Option<String>,
    ) -> Result<(SpawnedConnection, u64, Vec<Value>), String> {
        if !profile.is_acp() {
            return Err("unsupported: ACP connection requires an ACP profile".into());
        }
        if let Some(id) = connection_id.as_deref() {
            if self.state.lock().await.closing_connections.contains(id) {
                return Err("cancelled: ACP connection preparation was closed".into());
            }
        }
        if !cwd.is_absolute() {
            return Err("invalid_input: ACP working directory must be absolute".into());
        }
        let cwd = cwd
            .canonicalize()
            .map_err(|e| format!("invalid_input: ACP working directory: {e}"))?;
        let launch_cwd = profile
            .working_directory
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone())
            .canonicalize()
            .map_err(|e| format!("invalid_input: ACP launch directory: {e}"))?;
        let id = connection_id.unwrap_or_else(|| Uuid::new_v4().to_string());
        let generation = 1;
        let callbacks = Arc::new(ClientCallbacks::new(
            id.clone(),
            generation,
            profile.clone(),
            path.clone(),
            cwd.clone(),
            discovery,
        )?);
        if !prepared {
            if let Some(session_id) = session_id.clone() {
                *callbacks.session_id.lock().await = Some(session_id);
            }
        }
        let environment = profile.resolve_environment()?;
        let args = profile.launch_args();
        let (client, events) = AcpClient::spawn_with_services(
            &profile.executable,
            &args,
            launch_cwd
                .to_str()
                .ok_or("invalid_input: ACP directory is not UTF-8")?,
            &environment,
            callbacks.capabilities(),
            callbacks.clone(),
        )
        .await?;
        let auth_methods = client.initialization["authMethods"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let entry = ConnectionEntry {
            id: id.clone(),
            generation,
            profile,
            path,
            cwd,
            discovery,
            client: client.clone(),
            callbacks: callbacks.clone(),
            events: Some(events),
            consumed: !prepared,
            launch_cwd: launch_cwd.clone(),
            monitor_session_id: session_id,
            bound_native_id: None,
            auth_methods: auth_methods.clone(),
            state: "ready".into(),
        };
        let mut state = self.state.lock().await;
        if state.closing_connections.contains(&id) {
            drop(state);
            callbacks.close().await;
            client.stop().await;
            return Err("cancelled: ACP connection preparation was closed".into());
        }
        state.connections.insert(id.clone(), entry);
        drop(state);
        Ok((
            (client, self.take_events(&id).await?, callbacks, id),
            generation,
            auth_methods,
        ))
    }

    async fn take_events(&self, id: &str) -> Result<mpsc::UnboundedReceiver<Value>, String> {
        self.state
            .lock()
            .await
            .connections
            .get_mut(id)
            .and_then(|entry| entry.events.take())
            .ok_or_else(|| "runtime_error: ACP event stream already consumed".into())
    }

    /// Start a normal ACP conversation connection. `session_id` is supplied by
    /// the caller for a reconnect; otherwise the first returned native session
    /// may claim this provisional connection exactly once in `bind`.
    pub(crate) async fn spawn_auto(
        &self,
        path: PathBuf,
        profile: RuntimeProfile,
        cwd: PathBuf,
        discovery: bool,
    ) -> Result<SpawnedConnection, String> {
        let (connection, _, _) = self
            .spawn_inner(path, profile, cwd, None, discovery, false, None)
            .await?;
        Ok(connection)
    }

    /// Prepare and retain an initialized connection so the caller can show
    /// advertised authentication methods before creating a session.
    pub(crate) async fn prepare(
        &self,
        path: PathBuf,
        profile: RuntimeProfile,
        cwd: PathBuf,
        session_id: Option<String>,
        discovery: bool,
        request_id: String,
    ) -> Result<Value, String> {
        if request_id.trim().is_empty() {
            return Err("invalid_input: requestId is required".into());
        }
        let _prepare_guard = self.prepare_lock.lock().await;
        let key = (path.clone(), profile.id.clone(), request_id.clone());
        if let Some(ticket) = self.state.lock().await.prepared_requests.get(&key).cloned() {
            return Ok(ticket);
        }
        let id = Uuid::new_v4().to_string();
        let generation = 1;
        let operation_id = Uuid::new_v4().to_string();
        let operation_session_id = session_id.clone();
        let ticket = json!({"operationId":operation_id,"connectionId":id,"generation":generation,"state":"pending"});
        let operation = Arc::new(Mutex::new(
            json!({"operationId":operation_id,"requestId":request_id,"connectionId":id,"generation":generation,"state":"pending","error":null}),
        ));
        let mut state = self.state.lock().await;
        state.closing_connections.remove(&id);
        state.operations.insert(
            operation_id.clone(),
            OperationEntry {
                value: operation.clone(),
                task: None,
                connection_id: id.clone(),
                monitor_session_id: operation_session_id,
                generation,
                login_terminal: false,
                preparation: true,
            },
        );
        state.prepared_requests.insert(key, ticket.clone());
        drop(state);
        let manager = self.clone();
        let oid = operation_id.clone();
        let (start_tx, start_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            if start_rx.await.is_err() {
                return;
            }
            let result = manager
                .spawn_inner(
                    path,
                    profile,
                    cwd,
                    session_id,
                    discovery,
                    true,
                    Some(id.clone()),
                )
                .await;
            match result {
                Ok(((client, events, callbacks, _), _, _)) => {
                    if let Some(entry) = manager.state.lock().await.connections.get_mut(&id) {
                        entry.events = Some(events);
                        entry.consumed = false;
                    }
                    manager.finish_operation(&oid,"succeeded",Some(json!({"ready":true,"agentInfo":client.initialization["agentInfo"]})),None).await;
                    let mut auth_state = callbacks.auth_state.lock().await;
                    if auth_state.as_str() == "unknown" {
                        *auth_state = "ready".into();
                    }
                }
                Err(error) => {
                    manager
                        .finish_operation(&oid, "failed", None, Some(error))
                        .await
                }
            }
        });
        if let Some(op) = self.state.lock().await.operations.get_mut(&operation_id) {
            op.task = Some(task);
        }
        let _ = start_tx.send(());
        Ok(ticket)
    }

    /// Take a prepared handle once. The same ACP client and its event stream
    /// continue into session/new; stale or differently configured handles fail.
    pub(crate) async fn take_prepared(
        &self,
        id: &str,
        generation: u64,
        profile: &RuntimeProfile,
        cwd: &str,
        expected_discovery: bool,
    ) -> Result<SpawnedConnection, String> {
        let mut state = self.state.lock().await;
        let entry = state
            .connections
            .get_mut(id)
            .ok_or("transport_lost: prepared connection expired")?;
        if entry.generation != generation {
            return Err("invalid_input: ACP connection generation is stale".into());
        }
        if entry.profile.connection_scope() != profile.connection_scope() {
            return Err("revision_conflict: ACP profile launch configuration changed".into());
        }
        if entry.profile.client_policy != profile.client_policy {
            return Err(
                "revision_conflict: ACP client policy changed; prepare a new connection".into(),
            );
        }
        let requested = Path::new(cwd)
            .canonicalize()
            .map_err(|e| format!("invalid_input: session directory: {e}"))?;
        if requested != entry.cwd {
            return Err("revision_conflict: prepared connection directory changed".into());
        }
        if entry.discovery != expected_discovery {
            return Err(
                "invalid_input: prepared connection role does not match requested operation".into(),
            );
        }
        if entry.consumed {
            return Err("runtime_busy: prepared connection was already consumed".into());
        }
        entry.consumed = true;
        let events = entry
            .events
            .take()
            .ok_or("transport_lost: prepared event stream unavailable")?;
        Ok((
            entry.client.clone(),
            events,
            entry.callbacks.clone(),
            entry.id.clone(),
        ))
    }

    /// Bind the provisional connection to the native id returned by session/new.
    /// The identity is immutable once either side has published it.
    pub(crate) async fn bind(
        &self,
        connection_id: &str,
        monitor_session_id: &str,
        native_id: &str,
    ) -> Result<(), String> {
        if monitor_session_id.is_empty() || native_id.is_empty() {
            return Err("invalid_input: session binding ids are required".into());
        }
        let callbacks = {
            let mut state = self.state.lock().await;
            let entry = state
                .connections
                .get_mut(connection_id)
                .ok_or("transport_lost: ACP connection not found")?;
            if entry.discovery {
                return Err("unsupported: discovery connection cannot be bound".into());
            }
            if let Some(existing) = &entry.monitor_session_id {
                if existing != monitor_session_id {
                    return Err(
                        "unauthorized: ACP connection already belongs to another Monitor session"
                            .into(),
                    );
                }
            }
            if let Some(existing) = &entry.bound_native_id {
                if existing != native_id {
                    return Err(
                        "unauthorized: ACP connection already belongs to another native session"
                            .into(),
                    );
                }
            }
            let provisional_native_id = entry.callbacks.native_id.lock().await.clone();
            if provisional_native_id
                .as_deref()
                .is_some_and(|existing| existing != native_id)
            {
                return Err(
                    "unauthorized: ACP connection reported a different native session id".into(),
                );
            }
            entry.monitor_session_id = Some(monitor_session_id.into());
            entry.bound_native_id = Some(native_id.into());
            entry.callbacks.clone()
        };
        *callbacks.session_id.lock().await = Some(monitor_session_id.into());
        *callbacks.native_id.lock().await = Some(native_id.into());
        Ok(())
    }

    pub(crate) async fn close_connection(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<(), String> {
        let (removed, operations) = {
            let mut state = self.state.lock().await;
            if state
                .connections
                .get(connection_id)
                .is_some_and(|entry| entry.generation != generation)
            {
                return Err("invalid_input: ACP connection generation is stale".into());
            }
            let removed = if state
                .connections
                .get(connection_id)
                .is_some_and(|entry| entry.generation == generation)
            {
                state.connections.remove(connection_id)
            } else {
                None
            };
            if removed.is_some() {
                state.closing_connections.insert(connection_id.into());
            }
            let operations = state
                .operations
                .iter_mut()
                .filter_map(|(id, operation)| {
                    (operation.connection_id == connection_id).then(|| {
                        (
                            id.clone(),
                            operation.generation,
                            operation.login_terminal,
                            operation.task.take(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            state
                .prepared_requests
                .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
            state
                .auth_requests
                .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
            (removed, operations)
        };
        let mut close_error = None;
        for (operation_id, operation_generation, terminal, task) in operations {
            if let Some(task) = task {
                task.abort();
            }
            if terminal {
                if let Err(error) = self.login.close(&operation_id, operation_generation) {
                    if close_error.is_none() && !error.contains("not found") {
                        close_error = Some(error);
                    }
                }
            }
            self.finish_operation(
                &operation_id,
                "cancelled",
                None,
                Some("ACP connection closed".into()),
            )
            .await;
        }
        if let Some(entry) = removed {
            if let Err(error) = entry.callbacks.close().await {
                close_error = Some(error);
            }
            entry.client.stop().await;
        }
        self.remove_connection_operations(connection_id).await;
        close_error.map_or(Ok(()), Err)
    }

    /// Close a connection that has not been consumed by conversation execution.
    /// An active conversation connection must use the registry's disconnect path.
    pub(crate) async fn close_prepared_connection(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<Value, String> {
        let (entry, operations) = {
            let mut state = self.state.lock().await;
            let entry = match state.connections.get(connection_id) {
                Some(entry) if entry.generation != generation => {
                    return Err("invalid_input: ACP connection generation is stale".into());
                }
                Some(entry) if entry.consumed => {
                    return Err(
                        "unsupported: attached conversation must use the disconnect operation"
                            .into(),
                    );
                }
                Some(_) => state.connections.remove(connection_id),
                None => None,
            };
            let operations = state
                .operations
                .iter_mut()
                .filter_map(|(id, operation)| {
                    (operation.connection_id == connection_id).then(|| {
                        (
                            id.clone(),
                            operation.generation,
                            operation.login_terminal,
                            operation.task.take(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            if entry.is_some() || !operations.is_empty() {
                state.closing_connections.insert(connection_id.into());
            } else {
                return Err("transport_lost: ACP connection not found".into());
            }
            state
                .prepared_requests
                .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
            state
                .auth_requests
                .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
            (entry, operations)
        };

        let mut close_error = None;
        for (operation_id, operation_generation, terminal, task) in operations {
            if let Some(task) = task {
                task.abort();
            }
            if terminal {
                if let Err(error) = self.login.close(&operation_id, operation_generation) {
                    if close_error.is_none() && !error.contains("not found") {
                        close_error = Some(error);
                    }
                }
            }
            self.finish_operation(
                &operation_id,
                "cancelled",
                None,
                Some("ACP connection closed".into()),
            )
            .await;
        }
        if let Some(entry) = entry {
            if let Err(error) = entry.callbacks.close().await {
                close_error = Some(error);
            }
            entry.client.stop().await;
        }
        self.remove_connection_operations(connection_id).await;
        match close_error {
            Some(error) => Err(error),
            None => {
                Ok(json!({"connectionId":connection_id,"generation":generation,"state":"closed"}))
            }
        }
    }

    async fn remove_connection_operations(&self, connection_id: &str) {
        let mut state = self.state.lock().await;
        state
            .operations
            .retain(|_, operation| operation.connection_id != connection_id);
        state
            .prepared_requests
            .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
        state
            .auth_requests
            .retain(|_, ticket| ticket["connectionId"].as_str() != Some(connection_id));
    }

    pub(crate) async fn close_session(&self, session_id: &str) -> Result<(), String> {
        let (removed, operations) = {
            let mut state = self.state.lock().await;
            let mut connection_ids = state
                .connections
                .iter()
                .filter_map(|(id, entry)| {
                    (entry.monitor_session_id.as_deref() == Some(session_id)).then(|| id.clone())
                })
                .collect::<std::collections::HashSet<_>>();
            connection_ids.extend(
                state
                    .operations
                    .values()
                    .filter(|operation| {
                        operation.monitor_session_id.as_deref() == Some(session_id)
                            && operation.preparation
                    })
                    .map(|operation| operation.connection_id.clone()),
            );
            state
                .closing_connections
                .extend(connection_ids.iter().cloned());
            let removed = connection_ids
                .iter()
                .filter_map(|id| state.connections.remove(id))
                .collect::<Vec<_>>();
            let operations = state
                .operations
                .iter_mut()
                .filter_map(|(id, operation)| {
                    (operation.monitor_session_id.as_deref() == Some(session_id)
                        || connection_ids.contains(&operation.connection_id))
                    .then(|| {
                        (
                            id.clone(),
                            operation.generation,
                            operation.login_terminal,
                            operation.task.take(),
                        )
                    })
                })
                .collect::<Vec<_>>();
            (removed, operations)
        };
        for (operation_id, generation, terminal, task) in operations {
            if let Some(task) = task {
                task.abort();
            }
            if terminal {
                let _ = self.login.close(&operation_id, generation);
            }
            self.finish_operation(
                &operation_id,
                "cancelled",
                None,
                Some("Monitor session closed".into()),
            )
            .await;
        }
        let mut close_error = None;
        for entry in removed {
            if let Err(error) = entry.callbacks.close().await {
                if close_error.is_none() {
                    close_error = Some(error);
                }
            }
            entry.client.stop().await;
        }
        {
            let mut state = self.state.lock().await;
            let closing = state.closing_connections.clone();
            state.operations.retain(|_, operation| {
                operation.monitor_session_id.as_deref() != Some(session_id)
                    && !closing.contains(&operation.connection_id)
            });
            let operation_ids = state
                .operations
                .keys()
                .cloned()
                .collect::<std::collections::HashSet<_>>();
            state.prepared_requests.retain(|_, ticket| {
                ticket["operationId"]
                    .as_str()
                    .is_some_and(|id| operation_ids.contains(id))
            });
            state.auth_requests.retain(|_, ticket| {
                ticket["operationId"]
                    .as_str()
                    .is_some_and(|id| operation_ids.contains(id))
            });
        }
        close_error.map_or(Ok(()), Err)
    }

    pub(crate) async fn cancel_terminals(&self, session_id: &str) -> Result<(), String> {
        let (targets, operations) = {
            let state = self.state.lock().await;
            let targets = state
                .connections
                .values()
                .filter(|e| e.monitor_session_id.as_deref() == Some(session_id))
                .map(|e| (e.callbacks.clone(), e.id.clone()))
                .collect::<Vec<_>>();
            let connection_ids = targets
                .iter()
                .map(|(_, id)| id.as_str())
                .collect::<std::collections::HashSet<_>>();
            let operations = state
                .operations
                .iter()
                .filter(|(_, operation)| {
                    operation.login_terminal
                        && (operation.monitor_session_id.as_deref() == Some(session_id)
                            || connection_ids.contains(operation.connection_id.as_str()))
                })
                .map(|(id, operation)| (id.clone(), operation.generation))
                .collect::<Vec<_>>();
            (targets, operations)
        };
        let mut close_error = None;
        for (callbacks, _) in targets {
            if let Err(error) = callbacks.cancel_terminals().await {
                if close_error.is_none() {
                    close_error = Some(error);
                }
            }
        }
        for (operation_id, generation) in operations {
            let _ = self.login.cancel(&operation_id, generation);
            let task = {
                let mut state = self.state.lock().await;
                state
                    .operations
                    .get_mut(&operation_id)
                    .and_then(|operation| operation.task.take())
            };
            if let Some(task) = task {
                task.abort();
            }
            self.finish_operation(
                &operation_id,
                "cancelled",
                None,
                Some("Authentication terminal cancelled".into()),
            )
            .await;
        }
        close_error.map_or(Ok(()), Err)
    }

    /// Stop every ACP client, preparation task, login PTY, and client terminal
    /// before the owning backend exits.
    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let (entries, operations) = {
            let mut state = self.state.lock().await;
            let ids = state.connections.keys().cloned().collect::<Vec<_>>();
            state.closing_connections.extend(ids);
            let entries = state
                .connections
                .drain()
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            let operations = state
                .operations
                .iter_mut()
                .map(|(id, operation)| {
                    (
                        id.clone(),
                        operation.generation,
                        operation.login_terminal,
                        operation.task.take(),
                    )
                })
                .collect::<Vec<_>>();
            state.prepared_requests.clear();
            state.auth_requests.clear();
            (entries, operations)
        };
        let mut shutdown_error = None;
        for (operation_id, generation, terminal, task) in operations {
            if terminal {
                let _ = self.login.cancel(&operation_id, generation);
            }
            if let Some(task) = task {
                task.abort();
            }
            self.finish_operation(
                &operation_id,
                "cancelled",
                None,
                Some("ACP backend shutting down".into()),
            )
            .await;
        }
        let terminal_failures = self.login.terminate_all();
        if !terminal_failures.is_empty() {
            shutdown_error = Some(format!(
                "failed to stop {} ACP login terminal(s)",
                terminal_failures.len()
            ));
        }
        for entry in entries {
            if let Err(error) = entry.callbacks.close().await {
                if shutdown_error.is_none() {
                    shutdown_error = Some(error);
                }
            }
            entry.client.stop().await;
        }
        self.state.lock().await.operations.clear();
        shutdown_error.map_or(Ok(()), Err)
    }

    pub(crate) async fn status(&self, connection_id: &str) -> Result<Value, String> {
        let (generation, profile_id, initialization, callbacks, state) = {
            let state = self.state.lock().await;
            let Some(entry) = state.connections.get(connection_id) else {
                if let Some(operation) = state.operations.values().find(|op| {
                    let pending = op
                        .value
                        .try_lock()
                        .is_ok_and(|value| value["state"] == "pending");
                    op.connection_id == connection_id
                        && op.preparation
                        && pending
                        && !state.closing_connections.contains(connection_id)
                }) {
                    return Ok(
                        json!({"connectionId":connection_id,"generation":operation.generation,"profileId":"","agentInfo":null,"capabilities":{},"authMethods":[],"authState":"connecting","pendingClientRequests":[],"state":"connecting"}),
                    );
                }
                return Err("transport_lost: ACP connection not found".into());
            };
            (
                entry.generation,
                entry.profile.id.clone(),
                entry.client.initialization.clone(),
                entry.callbacks.clone(),
                entry.state.clone(),
            )
        };
        let caps = &initialization["agentCapabilities"];
        let session_caps = &caps["sessionCapabilities"];
        let auth_caps = &caps["auth"];
        let capabilities = json!({
            "loadSession":capability_advertised(&caps["loadSession"]),
            "sessionDiscovery":capability_advertised(&session_caps["list"]),
            "images":capability_advertised(&caps["promptCapabilities"]["image"]),
            "audio":capability_advertised(&caps["promptCapabilities"]["audio"]),
            "embeddedContext":capability_advertised(&caps["promptCapabilities"]["embeddedContext"]),
            "configuration":capability_advertised(&caps["configuration"]),
            "cancellation":true,"steering":false,"permissions":true,
            "logout":capability_advertised(&auth_caps["logout"])
        });
        let auth_state = callbacks.auth_state.lock().await.clone();
        let pending = callbacks.pending().await;
        Ok(json!({
            "connectionId":connection_id,"generation":generation,"profileId":profile_id,
            "agentInfo":initialization["agentInfo"],"capabilities":capabilities,
            "authMethods":initialization["authMethods"].as_array().cloned().unwrap_or_default(),
            "authState":auth_state,
            "pendingClientRequests":pending,"state":state
        }))
    }

    /// Read the live foreground ACP connection already bound to this Monitor
    /// conversation. This lookup never prepares, claims, or reconnects a session.
    pub(crate) async fn status_for_session(
        &self,
        monitor_session_id: &str,
    ) -> Result<Value, String> {
        let connection_id = {
            let state = self.state.lock().await;
            let mut matches = state.connections.values().filter(|entry| {
                entry.consumed
                    && !entry.discovery
                    && entry.monitor_session_id.as_deref() == Some(monitor_session_id)
            });
            let Some(entry) = matches.next() else {
                return Err(
                    "transport_lost: live ACP connection for Monitor session not found".into(),
                );
            };
            if matches.next().is_some() {
                return Err("runtime_error: multiple live ACP connections are bound to this Monitor session".into());
            }
            entry.id.clone()
        };
        self.status(&connection_id).await
    }

    pub(crate) async fn operation(&self, operation_id: &str) -> Result<Value, String> {
        let value = {
            let state = self.state.lock().await;
            state
                .operations
                .get(operation_id)
                .map(|op| op.value.clone())
                .ok_or("invalid_input: connection operation not found")?
        };
        let snapshot = value.lock().await.clone();
        Ok(snapshot)
    }

    pub(crate) async fn authenticate(
        &self,
        connection_id: &str,
        generation: u64,
        method_id: &str,
        request_id: String,
    ) -> Result<Value, String> {
        if request_id.trim().is_empty() {
            return Err("invalid_input: requestId is required".into());
        }
        let key = (connection_id.to_owned(), request_id.clone());
        if let Some(ticket) = {
            let state = self.state.lock().await;
            state.auth_requests.get(&key).cloned()
        } {
            return Ok(ticket);
        }
        let (profile, path, cwd, launch_cwd, client, callbacks, method, session_id, discovery) = {
            let state = self.state.lock().await;
            let entry = state
                .connections
                .get(connection_id)
                .ok_or("transport_lost: ACP connection not found")?;
            if entry.generation != generation {
                return Err("invalid_input: ACP connection generation is stale".into());
            }
            let method = entry
                .auth_methods
                .iter()
                .find(|m| m["id"].as_str() == Some(method_id))
                .cloned()
                .ok_or("invalid_input: authentication method is not advertised")?;
            (
                entry.profile.clone(),
                entry.path.clone(),
                entry.cwd.clone(),
                entry.launch_cwd.clone(),
                entry.client.clone(),
                entry.callbacks.clone(),
                method,
                entry.monitor_session_id.clone(),
                entry.discovery,
            )
        };
        let auth_type = method["type"]
            .as_str()
            .or_else(|| method["kind"].as_str())
            .unwrap_or("agent");
        if auth_type != "agent" && auth_type != "terminal" {
            return Err("unsupported: authentication method type is not supported".into());
        }
        let terminal = auth_type == "terminal";
        if terminal && (session_id.is_some() || discovery) {
            return Err("unsupported: terminal login must complete before a conversation session is created".into());
        }
        let operation_id = Uuid::new_v4().to_string();
        let value = Arc::new(Mutex::new(
            json!({"operationId":operation_id,"requestId":request_id,"connectionId":connection_id,"generation":generation,"state":"pending","error":null}),
        ));
        let ticket = json!({"operationId":operation_id,"connectionId":connection_id,"generation":generation,"state":"pending"});
        let mut state = self.state.lock().await;
        if let Some(ticket) = state.auth_requests.get(&key) {
            return Ok(ticket.clone());
        }
        state.operations.insert(
            operation_id.clone(),
            OperationEntry {
                value: value.clone(),
                task: None,
                connection_id: connection_id.into(),
                monitor_session_id: session_id.clone(),
                generation,
                login_terminal: terminal,
                preparation: false,
            },
        );
        state.auth_requests.insert(key, ticket.clone());
        *callbacks.auth_state.lock().await = "authenticating".into();
        drop(state);
        let manager = self.clone();
        let operation = operation_id.clone();
        let cid = connection_id.to_owned();
        let mid = method_id.to_owned();
        let task = tokio::spawn(async move {
            if terminal {
                let args = method["args"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let launch = terminal_auth_launch(&profile, &method).and_then(|(args, env)| {
                    manager.login.spawn(
                        operation.clone(),
                        generation,
                        &profile.executable,
                        &args,
                        &env,
                        &launch_cwd,
                    )
                });
                match launch {
                    Err(error) => {
                        manager
                            .finish_operation(&operation, "failed", None, Some(error))
                            .await
                    }
                    Ok(info) => {
                        if let Ok(terminal) = serde_json::to_value(&info) {
                            manager
                                .set_operation_awaiting_user(&operation, terminal)
                                .await;
                        }
                        loop {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                            let snapshot =
                                manager
                                    .login
                                    .read(&operation, generation, 0, Some(256 * 1024));
                            match snapshot {
                                Err(error) => {
                                    manager
                                        .finish_operation(&operation, "failed", None, Some(error))
                                        .await;
                                    break;
                                }
                                Ok(output) if output.state != "running" => {
                                    if output.state == "succeeded" {
                                        match manager.reconnect_after_login(&cid,generation).await{
                                        Ok(())=>manager.finish_operation(&operation,"succeeded",Some(json!({"exitCode":output.exit_code,"exitSignal":output.exit_signal})),None).await,
                                        Err(error)=>manager.finish_operation(&operation,"failed",None,Some(error)).await,
                                    }
                                    } else {
                                        manager
                                            .finish_operation(
                                                &operation,
                                                if output.state == "cancelled" {
                                                    "cancelled"
                                                } else {
                                                    "failed"
                                                },
                                                None,
                                                Some(format!(
                                                    "Authentication terminal {}",
                                                    output.state
                                                )),
                                            )
                                            .await;
                                    }
                                    break;
                                }
                                Ok(_) => {}
                            }
                        }
                    }
                }
            } else {
                match client
                    .request(
                        "authenticate",
                        json!({"methodId":mid}),
                        Some(Duration::from_secs(120)),
                    )
                    .await
                {
                    Ok(_result) => {
                        *callbacks.auth_state.lock().await = "authenticated".into();
                        manager
                            .finish_operation(
                                &operation,
                                "succeeded",
                                Some(json!({"authenticated":true})),
                                None,
                            )
                            .await;
                    }
                    Err(error) => {
                        *callbacks.auth_state.lock().await = "required".into();
                        manager
                            .finish_operation(&operation, "failed", None, Some(error))
                            .await;
                    }
                }
            }
            if terminal {
                manager.expire_login_terminal(&operation, generation).await;
            }
        });
        if let Some(op) = self.state.lock().await.operations.get_mut(&operation_id) {
            op.task = Some(task);
        }
        Ok(ticket)
    }

    async fn expire_login_terminal(&self, operation_id: &str, generation: u64) {
        tokio::time::sleep(LOGIN_TERMINAL_RETENTION).await;
        let expired = {
            let mut state = self.state.lock().await;
            let completed = if let Some(operation) = state.operations.get(operation_id) {
                if operation.login_terminal && operation.generation == generation {
                    matches!(
                        operation.value.lock().await["state"].as_str(),
                        Some("succeeded" | "failed" | "cancelled")
                    )
                } else {
                    false
                }
            } else {
                false
            };
            if completed {
                state.operations.remove(operation_id);
                state
                    .auth_requests
                    .retain(|_, ticket| ticket["operationId"].as_str() != Some(operation_id));
            }
            completed
        };
        if expired {
            let _ = self.login.close(operation_id, generation);
        }
    }

    async fn finish_operation(
        &self,
        operation_id: &str,
        state_name: &str,
        result: Option<Value>,
        error: Option<String>,
    ) {
        let value = {
            let state = self.state.lock().await;
            state
                .operations
                .get(operation_id)
                .map(|op| op.value.clone())
        };
        if let Some(value) = value {
            let mut v = value.lock().await;
            if matches!(
                v["state"].as_str(),
                Some("cancelled" | "succeeded" | "failed" | "interrupted")
            ) {
                return;
            }
            v["state"] = json!(state_name);
            v["result"] = result.unwrap_or(Value::Null);
            v["error"] = error.map(Value::String).unwrap_or(Value::Null);
        }
    }

    async fn set_operation_awaiting_user(&self, operation_id: &str, login_terminal: Value) {
        let value = {
            let state = self.state.lock().await;
            state
                .operations
                .get(operation_id)
                .map(|operation| operation.value.clone())
        };
        if let Some(value) = value {
            let mut operation = value.lock().await;
            if operation["state"] == "pending" {
                operation["state"] = json!("awaiting_user");
                operation["loginTerminal"] = login_terminal;
            }
        }
    }

    async fn reconnect_after_login(
        &self,
        connection_id: &str,
        old_generation: u64,
    ) -> Result<(), String> {
        let (
            profile,
            path,
            cwd,
            launch_cwd,
            discovery,
            session_id,
            native_id,
            old_client,
            old_callbacks,
        ) = {
            let state = self.state.lock().await;
            let e = state
                .connections
                .get(connection_id)
                .ok_or("transport_lost: connection disappeared during authentication")?;
            if e.generation != old_generation {
                return Err("invalid_input: stale authentication generation".into());
            }
            if e.consumed || e.monitor_session_id.is_some() {
                return Err(
                    "unsupported: cannot replace a connection already attached to a conversation"
                        .into(),
                );
            }
            (
                e.profile.clone(),
                e.path.clone(),
                e.cwd.clone(),
                e.launch_cwd.clone(),
                e.discovery,
                e.monitor_session_id.clone(),
                e.bound_native_id.clone(),
                e.client.clone(),
                e.callbacks.clone(),
            )
        };
        let generation = old_generation.saturating_add(1);
        let callbacks = Arc::new(ClientCallbacks::new(
            connection_id.into(),
            generation,
            profile.clone(),
            path.clone(),
            cwd.clone(),
            discovery,
        )?);
        *callbacks.auth_state.lock().await = "authenticated".into();
        let env = profile.resolve_environment()?;
        let args = profile.launch_args();
        let (client, events) = AcpClient::spawn_with_services(
            &profile.executable,
            &args,
            launch_cwd
                .to_str()
                .ok_or("invalid_input: connection directory is not UTF-8")?,
            &env,
            callbacks.capabilities(),
            callbacks.clone(),
        )
        .await?;
        let methods = client.initialization["authMethods"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let old_close_result = old_callbacks.close().await;
        old_client.stop().await;
        let mut state = self.state.lock().await;
        state.connections.insert(
            connection_id.into(),
            ConnectionEntry {
                id: connection_id.into(),
                generation,
                profile,
                path,
                cwd,
                launch_cwd,
                discovery,
                client,
                callbacks,
                events: Some(events),
                consumed: false,
                monitor_session_id: session_id,
                bound_native_id: native_id,
                auth_methods: methods,
                state: "ready".into(),
            },
        );
        drop(state);
        old_close_result?;
        Ok(())
    }

    pub(crate) async fn cancel_operation(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<Value, String> {
        let (connection_id, op_generation, terminal, task) = {
            let mut state = self.state.lock().await;
            let op = state
                .operations
                .get_mut(operation_id)
                .ok_or("invalid_input: connection operation not found")?;
            (
                op.connection_id.clone(),
                op.generation,
                op.login_terminal,
                op.task.take(),
            )
        };
        if generation != op_generation {
            return Err("invalid_input: connection operation generation is stale".into());
        }
        if terminal {
            let _ = self.login.cancel(operation_id, generation);
        } else if let Some(task) = task {
            task.abort();
        }
        self.finish_operation(
            operation_id,
            "cancelled",
            None,
            Some("Authentication cancelled by user".into()),
        )
        .await;
        if !terminal {
            let active = {
                let state = self.state.lock().await;
                state
                    .connections
                    .get(&connection_id)
                    .map(|entry| (entry.client.clone(), entry.callbacks.clone()))
            };
            if let Some((client, callbacks)) = active {
                client.stop().await;
                *callbacks.auth_state.lock().await = "required".into();
            }
        }
        Ok(
            json!({"operationId":operation_id,"connectionId":connection_id,"generation":generation,"state":"cancelled"}),
        )
    }

    pub(crate) async fn logout(
        &self,
        connection_id: &str,
        generation: u64,
    ) -> Result<Value, String> {
        let (client, callbacks, supported) = {
            let state = self.state.lock().await;
            let e = state
                .connections
                .get(connection_id)
                .ok_or("transport_lost: ACP connection not found")?;
            if e.generation != generation {
                return Err("invalid_input: ACP connection generation is stale".into());
            }
            (
                e.client.clone(),
                e.callbacks.clone(),
                capability_advertised(
                    &e.client.initialization["agentCapabilities"]["auth"]["logout"],
                ),
            )
        };
        if !supported {
            return Err("unsupported: this agent has not advertised logout".into());
        }
        client
            .request("logout", json!({}), Some(Duration::from_secs(30)))
            .await?;
        *callbacks.auth_state.lock().await = "required".into();
        Ok(json!({"connectionId":connection_id,"generation":generation,"state":"required"}))
    }

    pub(crate) async fn respond(
        &self,
        connection_id: &str,
        generation: u64,
        client_request_id: &Value,
        decision: Value,
    ) -> Result<Value, String> {
        let callbacks = {
            let state = self.state.lock().await;
            let e = state
                .connections
                .get(connection_id)
                .ok_or("transport_lost: ACP connection not found")?;
            if e.generation != generation {
                return Err("invalid_input: ACP connection generation is stale".into());
            }
            e.callbacks.clone()
        };
        let id = client_request_id
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| client_request_id.to_string());
        callbacks.respond(&id, decision).await?;
        Ok(
            json!({"connectionId":connection_id,"generation":generation,"clientRequestId":client_request_id,"acknowledged":true}),
        )
    }

    pub(crate) async fn login_call(&self, method: &str, input: &Value) -> Result<Value, String> {
        let operation_id = input["operationId"]
            .as_str()
            .or_else(|| input["operation_id"].as_str())
            .ok_or("invalid_input: operationId is required")?;
        let generation = input["generation"]
            .as_u64()
            .ok_or("invalid_input: generation is required")?;
        let (connection_id, terminal, operation_generation, operation_state, live_generation) = {
            let state = self.state.lock().await;
            let operation = state
                .operations
                .get(operation_id)
                .ok_or("invalid_input: connection operation not found")?;
            let operation_state = operation.value.lock().await["state"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let live_generation = state
                .connections
                .get(&operation.connection_id)
                .map(|entry| entry.generation)
                .ok_or("transport_lost: ACP connection is closed")?;
            (
                operation.connection_id.clone(),
                operation.login_terminal,
                operation.generation,
                operation_state,
                live_generation,
            )
        };
        if !terminal {
            return Err("invalid_input: connection operation has no login terminal".into());
        }
        if generation != operation_generation {
            return Err("invalid_input: login terminal generation is stale".into());
        }
        if input["connectionId"]
            .as_str()
            .is_some_and(|provided| provided != connection_id)
        {
            return Err("invalid_input: login terminal belongs to another connection".into());
        }
        if live_generation != operation_generation
            && !((method == "read_login_terminal" || method == "close_login_terminal")
                && operation_state == "succeeded"
                && live_generation == operation_generation.saturating_add(1))
        {
            return Err("invalid_input: connection generation is stale".into());
        }
        match method {
            "read_login_terminal" => {
                let cursor = input["cursor"].as_u64().unwrap_or(0);
                let limit = input["limit"].as_u64().and_then(|n| u32::try_from(n).ok());
                serde_json::to_value(self.login.read(operation_id, generation, cursor, limit)?)
                    .map_err(|e| e.to_string())
            }
            "write_login_terminal" => {
                let seq = input["inputSequence"]
                    .as_u64()
                    .or_else(|| input["sequence"].as_u64())
                    .ok_or("invalid_input: input sequence required")?;
                let data = input["data"]
                    .as_str()
                    .or_else(|| input["input"].as_str())
                    .ok_or("invalid_input: terminal input required")?;
                serde_json::to_value(self.login.write(operation_id, generation, seq, data)?)
                    .map_err(|e| e.to_string())
            }
            "resize_login_terminal" => {
                let cols = u16::try_from(
                    input["columns"]
                        .as_u64()
                        .or_else(|| input["cols"].as_u64())
                        .ok_or("invalid_input: columns required")?,
                )
                .map_err(|_| "invalid_input: columns out of range")?;
                let rows = u16::try_from(
                    input["rows"]
                        .as_u64()
                        .ok_or("invalid_input: rows required")?,
                )
                .map_err(|_| "invalid_input: rows out of range")?;
                self.login.resize(operation_id, generation, cols, rows)?;
                Ok(json!({"acknowledged":true}))
            }
            "close_login_terminal" => {
                let task = self
                    .state
                    .lock()
                    .await
                    .operations
                    .get_mut(operation_id)
                    .and_then(|op| op.task.take());
                if let Some(task) = task {
                    task.abort();
                }
                let output = self.login.close(operation_id, generation)?;
                let mut state = self.state.lock().await;
                state.operations.remove(operation_id);
                state
                    .auth_requests
                    .retain(|_, ticket| ticket["operationId"].as_str() != Some(operation_id));
                serde_json::to_value(output).map_err(|e| e.to_string())
            }
            _ => Err("unsupported: unknown login terminal operation".into()),
        }
    }

    pub(crate) async fn read_terminal(
        &self,
        path: PathBuf,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<Value, String> {
        let active = {
            let state = self.state.lock().await;
            state
                .connections
                .values()
                .find(|e| e.monitor_session_id.as_deref() == Some(session_id))
                .map(|e| (e.callbacks.clone(), e.generation))
        };
        if let Some((callbacks, generation)) = active {
            let params = json!({"sessionId":session_id,"terminalId":terminal_id,"cursor":0,"outputByteLimit":1024*1024});
            if let Ok(output) = callbacks
                .terminals
                .output(session_id, generation, &params)
                .await
            {
                let exit = output["exitStatus"].clone();
                let snapshot = json!({"output":output["output"],"truncated":output["truncated"],"exitCode":exit["exitCode"],"signal":exit["signal"],"released":false});
                let record = AcpTerminalSnapshot {
                    session_id: session_id.into(),
                    generation,
                    terminal_id: terminal_id.into(),
                    payload: snapshot.clone(),
                };
                persist(path, move |store| store.save_acp_terminal_snapshot(&record)).await?;
                return Ok(snapshot);
            }
        }
        let session_id = session_id.to_owned();
        let terminal_id = terminal_id.to_owned();
        with_store(path, move |store| {
            store
                .read_latest_acp_terminal_snapshot(&session_id, &terminal_id)
                .map(|v| v.map(|s| s.payload).unwrap_or(Value::Null))
        })
        .await
    }
}
