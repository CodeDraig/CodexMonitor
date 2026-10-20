use super::{projection, types::*};
use crate::{
    backend::{acp::AcpClient, app_server::session::WorkspaceSession, events::EventSink},
    shared::{
        runs_core::{
            self,
            store::{with_store, Store},
            types::*,
        },
        workspaces_core,
    },
    types::{AppSettings, WorkspaceEntry},
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, Mutex};

pub(crate) enum RuntimeHandle {
    Codex(Arc<WorkspaceSession>),
    OpenCodeAcp(Arc<AcpClient>),
}
struct Binding {
    ready: AtomicBool,
    generation: String,
    session: RuntimeSession,
    handle: RuntimeHandle,
    events: mpsc::UnboundedSender<Value>,
    permissions: Arc<Mutex<HashMap<String, Value>>>,
}
#[derive(Clone)]
pub(crate) struct RuntimeRegistry {
    bindings: Arc<Mutex<HashMap<String, Arc<Binding>>>>,
    connect_lock: Arc<Mutex<()>>,
    discovery: Arc<Mutex<HashMap<String, Arc<Mutex<Option<Discovery>>>>>>,
    pub owner: String,
    faults: Arc<Mutex<HashMap<String, String>>>,
}
impl Default for RuntimeRegistry {
    fn default() -> Self {
        Self {
            bindings: Default::default(),
            discovery: Default::default(),
            connect_lock: Default::default(),
            faults: Default::default(),
            owner: format!("{}:{}", std::process::id(), runs_core::id()),
        }
    }
}
/// Only contention is retryable. The closure is a database action, never an external submission.
pub(crate) async fn persist<T: Send + 'static>(
    path: PathBuf,
    f: impl Fn(&mut Store) -> Result<T, String> + Send + Sync + 'static,
) -> Result<T, String> {
    let f = Arc::new(f);
    loop {
        let f = f.clone();
        let result = with_store(path.clone(), move |s| f(s)).await;
        if result
            .as_ref()
            .err()
            .is_some_and(|e| runs_core::store::is_busy(e))
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        return result;
    }
}
impl RuntimeRegistry {
    pub async fn profiles(
        &self,
        path: PathBuf,
        settings: &Mutex<AppSettings>,
    ) -> Result<Vec<RuntimeProfile>, String> {
        let settings = settings.lock().await;
        let mut default = RuntimeProfile {
            working_directory: None,
            id: "codex".into(),
            revision: 0,
            name: "Codex".into(),
            kind: RuntimeKind::Codex,
            executable: settings.codex_bin.clone().unwrap_or("codex".into()),
            args: shell_words::split(settings.codex_args.as_deref().unwrap_or(""))
                .map_err(|e| e.to_string())?,
        };
        default.revision =
            configuration_revision(&serde_json::to_string(&default).map_err(|e| e.to_string())?);
        drop(settings);
        let mut profiles = with_store(path, |s| s.runtime_profiles()).await?;
        profiles.insert(0, default);
        Ok(profiles)
    }
    pub async fn create_session<E: EventSink>(
        &self,
        path: PathBuf,
        workspace: WorkspaceEntry,
        profile: RuntimeProfile,
        existing: Option<RuntimeSession>,
        sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
        workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
        settings: &Mutex<AppSettings>,
        sink: E,
    ) -> Result<RuntimeSession, String> {
        let _guard = self.connect_lock.lock().await;
        if let Some(ref previous) = existing {
            if let Some(binding) = self.bindings.lock().await.get(&previous.session_id) {
                if !binding.ready.load(Ordering::SeqCst) {
                    return Err("runtime_busy: conversation has not loaded successfully".into());
                }
                return with_store(path.clone(), {
                    let id = binding.session.session_id.clone();
                    move |s| s.runtime_session(&id)
                })
                .await;
            }
            let sid = previous.session_id.clone();
            let owner = self.owner.clone();
            with_store(path.clone(), move |s| s.claim_runtime_session(&sid, &owner)).await?;
        }
        let (events, rx) = mpsc::unbounded_channel();
        let permissions = Arc::new(Mutex::new(HashMap::new()));
        let mut session = RuntimeSession {
            store_id: with_store(path.clone(), |s| Ok(s.store_id.clone())).await?,
            runtime: profile.reference(),
            session_id: existing
                .as_ref()
                .map(|s| s.session_id.clone())
                .unwrap_or_else(runs_core::id),
            native_session_id: String::new(),
            workspace_id: workspace.id.clone(),
            cwd: workspace.path.clone(),
            profile: profile.clone(),
            capabilities: RuntimeCapabilities::default(),
            configuration: Value::Null,
            agent_info: Value::Null,
        };
        let handle = match profile.kind {
            RuntimeKind::OpenCodeAcp | RuntimeKind::Acp => {
                let args = profile.launch_args();
                let (client, mut wire) = AcpClient::spawn(
                    &profile.executable,
                    &args,
                    profile
                        .working_directory
                        .as_deref()
                        .unwrap_or(&workspace.path),
                )
                .await?;
                session.capabilities = acp_capabilities(&client.initialization);
                session.agent_info = client.initialization.clone();
                let response = if let Some(ref old) = existing {
                    if !session.capabilities.load_session {
                        client.stop().await;
                        return Err("unsupported: this agent cannot load sessions".into());
                    }
                    let sid = old.session_id.clone();
                    let owner = self.owner.clone();
                    let request = runs_core::id();
                    let (load, _) = persist(path.clone(), move |s| {
                        s.begin_history_load(&sid, &request, &owner)
                    })
                    .await?;
                    let response = client
                        .start_history_load(&load.id, &old.native_session_id, &workspace.path)
                        .await?
                        .await
                        .map_err(|_| "transport_lost: replay abandoned")?;
                    match response {
                        Ok(v) => v,
                        Err(e) => {
                            let saved = e.clone();
                            persist(path.clone(), move |s| {
                                s.finish_history_load(&load.id, Some(&saved), 0)
                            })
                            .await?;
                            client.stop().await;
                            return Err(e);
                        }
                    }
                } else {
                    client
                        .request(
                            "session/new",
                            json!({"cwd":workspace.path,"mcpServers":[]}),
                            Some(Duration::from_secs(30)),
                        )
                        .await?
                };
                session.native_session_id = existing
                    .as_ref()
                    .map(|s| s.native_session_id.clone())
                    .or_else(|| response["sessionId"].as_str().map(str::to_string))
                    .ok_or("runtime_error: ACP session ID missing")?;
                session.configuration = response.clone();
                session.capabilities.configuration = configuration_available(&response);
                let forward = events.clone();
                tokio::spawn(async move {
                    while let Some(v) = wire.recv().await {
                        let _ = forward.send(v);
                    }
                });
                RuntimeHandle::OpenCodeAcp(client)
            }
            RuntimeKind::Codex => {
                let spawn_sink = sink.clone();
                workspaces_core::connect_workspace_core(
                    workspace.id.clone(),
                    workspaces,
                    sessions,
                    settings,
                    move |entry, bin, args, home| {
                        crate::backend::app_server::spawn_workspace_session(
                            entry,
                            bin,
                            args,
                            home,
                            env!("CARGO_PKG_VERSION").into(),
                            spawn_sink.clone(),
                        )
                    },
                )
                .await?;
                let client = sessions
                    .lock()
                    .await
                    .get(&workspace.id)
                    .cloned()
                    .ok_or("runtime_error: Codex connection missing")?;
                let response = if let Some(ref old) = existing {
                    client
                        .send_runtime_request_for_workspace(
                            &workspace.id,
                            "thread/resume",
                            json!({"threadId":old.native_session_id,"excludeTurns":true}),
                            events.clone(),
                        )
                        .await?
                } else {
                    client
                        .send_runtime_request_for_workspace(
                            &workspace.id,
                            "thread/start",
                            json!({"cwd":workspace.path,"approvalPolicy":"on-request"}),
                            events.clone(),
                        )
                        .await?
                };
                crate::shared::codex_core::history::rpc_result(&response)?;
                session.native_session_id = response["result"]["thread"]["id"]
                    .as_str()
                    .ok_or("runtime_error: Codex thread ID missing")?
                    .into();
                session.capabilities = RuntimeCapabilities {
                    load_session: true,
                    session_discovery: true,
                    images: true,
                    audio: false,
                    embedded_context: false,
                    cancellation: true,
                    permissions: true,
                    steering: true,
                    configuration: false,
                };
                if let Some(identity) = &client.launch_identity {
                    session.profile.executable =
                        identity["executable"].as_str().unwrap_or("codex").into();
                    session.profile.args =
                        shell_words::split(identity["args"].as_str().unwrap_or(""))
                            .map_err(|e| e.to_string())?;
                    session.profile.revision = configuration_revision(&identity.to_string());
                    session.runtime = session.profile.reference();
                }
                session.agent_info = json!({"name":"Codex","launch":session.profile,"launchIdentity":client.launch_identity,"initialization":client.initialization.lock().await.clone()});
                client
                    .runtime_subscribers
                    .lock()
                    .await
                    .insert(session.native_session_id.clone(), events.clone());
                RuntimeHandle::Codex(client)
            }
        };
        let saved = session.clone();
        let owner = self.owner.clone();
        let old = existing.is_some();
        persist(path.clone(), move |s| {
            if old {
                s.save_runtime_session(&saved)
            } else {
                s.insert_runtime_session(&saved, &owner)
            }
        })
        .await?;
        let binding = Arc::new(Binding {
            ready: AtomicBool::new(true),
            generation: runs_core::id(),
            session: session.clone(),
            handle,
            events,
            permissions,
        });
        self.bindings
            .lock()
            .await
            .insert(session.session_id.clone(), binding.clone());
        let owner = self.owner.clone();
        let worker = binding.clone();
        let faults = self.faults.clone();
        let session_id = session.session_id.clone();
        let path_for_ready = path.clone();
        tokio::spawn(async move {
            if let Err(e) = drive(path, worker, rx, sink, owner).await {
                faults.lock().await.insert(session_id, e.clone());
                eprintln!("Runtime recording stopped: {e}");
            }
        });
        // Preserve the existing load_session contract: submissions after its reply
        // see a committed replay. The additive history API returns its ticket earlier.
        if existing.is_some() && session.profile.is_acp() {
            loop {
                self.check_recording(&session.session_id).await?;
                let sid = session.session_id.clone();
                let loading=with_store(path_for_ready.clone(),move|s|s.conn.query_row("SELECT EXISTS(SELECT 1 FROM history_loads WHERE session_id=? AND state='loading')",[sid],|r|r.get::<_,bool>(0)).map_err(runs_core::store::error)).await?;
                if !loading {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
        Ok(session)
    }
    async fn binding(&self, id: &str) -> Result<Arc<Binding>, String> {
        self.bindings
            .lock()
            .await
            .get(id)
            .filter(|b| b.ready.load(Ordering::SeqCst))
            .cloned()
            .ok_or("transport_lost: load the session on its owning backend before sending".into())
    }
    pub async fn conversation_ready(&self, id: &str) -> bool {
        self.bindings
            .lock()
            .await
            .get(id)
            .is_some_and(|b| b.ready.load(Ordering::SeqCst))
    }
    pub async fn check_recording(&self, session_id: &str) -> Result<(), String> {
        if let Some(error) = self.faults.lock().await.get(session_id) {
            return Err(format!(
                "Runtime persistence failed; execution evidence is incomplete: {error}"
            ));
        }
        Ok(())
    }
    pub async fn submit(
        &self,
        path: PathBuf,
        input: SubmitExecution,
    ) -> Result<ExecutionTicket, String> {
        if let Some(found) = with_store(path.clone(), {
            let input = input.clone();
            move |s| s.find_runtime_request(&input.request_id)
        })
        .await?
        {
            if serde_json::to_value(&found.input).unwrap() != serde_json::to_value(&input).unwrap()
            {
                return Err("invalid_input: submission request identity conflicts".into());
            }
            return Ok(found);
        }
        uuid::Uuid::parse_str(&input.request_id).map_err(|e| format!("invalid_input: {e}"))?;
        let b = self.binding(&input.session_id).await?;
        self.check_recording(&input.session_id).await?;
        validate_prompt(&input, &b.session)?;
        let original = input.clone();
        let owner = self.owner.clone();
        let (ticket, execute) = persist(path.clone(), move |s| {
            s.begin_runtime_execution(original.clone(), &owner)
        })
        .await?;
        if !execute {
            return Ok(ticket);
        }
        if let Some(context) = &input.run_context {
            let c = context.clone();
            let session = b.session.clone();
            let t = ticket.clone();
            persist(path.clone(), move |s| {
                s.validate_context(&c)?;
                s.update_instance(&c.instance_id, |i| {
                    if i.initial_execution_id.is_some()
                        && i.initial_execution_id.as_deref() != Some(&t.execution_id)
                    {
                        return Err("invalid_input: run already has its initial execution".into());
                    }
                    i.runtime = Some(session.runtime.clone());
                    i.session_id = Some(session.session_id.clone());
                    i.initial_execution_id = Some(t.execution_id.clone());
                    i.workspace_id = Some(session.workspace_id.clone());
                    i.workspace_path = Some(session.cwd.clone());
                    i.preparation = "ready".into();
                    i.submission = Some(json!({"prompt":t.input.prompt,"options":t.input.options,"configuration":t.configuration}));
                    i.submission_outcome = SubmissionOutcome::Submitting;
                    i.baseline =
                        crate::shared::runs_core::capture::git_snapshot(&session.cwd, None);
                    Ok(())
                })?;
                Ok(())
            })
            .await?;
        }
        let _ = b
            .events
            .send(json!({"method":"runtime/begin","params":ticket}));
        let prompt_response = if let RuntimeHandle::OpenCodeAcp(client) = &b.handle {
            match client
                .start_request(
                    "session/prompt",
                    json!({"sessionId":b.session.native_session_id,"prompt":input.prompt}),
                )
                .await
            {
                Ok((_, receiver)) => Some(receiver),
                Err(error) => {
                    let _ = b
                        .events
                        .send(json!({"method":"runtime/disconnected","params":{"message":error}}));
                    return Ok(ticket);
                }
            }
        } else {
            None
        };
        let output = ticket.clone();
        tokio::spawn(async move {
            match &b.handle {
                RuntimeHandle::OpenCodeAcp(_) => {
                    if let Some(receiver) = prompt_response {
                        if let Err(error) = receiver.await.unwrap_or_else(|_| {
                            Err("transport_lost: prompt response abandoned".into())
                        }) {
                            if error.starts_with("transport_lost") {
                                let _=b.events.send(json!({"method":"runtime/disconnected","params":{"message":error}}));
                            }
                        }
                    }
                }
                RuntimeHandle::Codex(client) => {
                    let mut params = serde_json::Map::new();
                    params.insert("threadId".into(), json!(b.session.native_session_id));
                    params.insert("cwd".into(), json!(b.session.cwd));
                    params.insert("input".into(), json!(input.prompt));
                    params.insert("approvalPolicy".into(), json!("on-request"));
                    if let Some(options) = input.options.as_object() {
                        for (k, v) in options {
                            params.insert(k.clone(), v.clone());
                        }
                    }
                    match client
                        .send_request_for_workspace(
                            &b.session.workspace_id,
                            "turn/start",
                            Value::Object(params),
                        )
                        .await
                    {
                        Ok(response) => {
                            let _ = b
                                .events
                                .send(json!({"method":"runtime/codex_receipt","params":response}));
                        }
                        Err(error) => {
                            let _ = b.events.send(
                                json!({"method":"runtime/disconnected","params":{"message":error}}),
                            );
                        }
                    }
                }
            }
        });
        Ok(output)
    }
    pub async fn steer(
        &self,
        path: PathBuf,
        session_id: &str,
        execution_id: &str,
        prompt: Vec<Value>,
    ) -> Result<Value, String> {
        let binding = self.binding(session_id).await?;
        let client =
            match &binding.handle {
                RuntimeHandle::Codex(client) => client,
                RuntimeHandle::OpenCodeAcp(_) => return Err(
                    "unsupported: this ACP adapter queues follow-ups and cannot steer an execution"
                        .into(),
                ),
            };
        let id = execution_id.to_string();
        let ticket = with_store(path, move |s| s.runtime_execution(&id)).await?;
        if ticket.session_id != session_id || ticket.execution != ExecutionOutcome::Running {
            return Err("runtime_busy: the expected execution is no longer running".into());
        }
        let turn = ticket
            .native_turn_id
            .ok_or("runtime_busy: execution has no confirmed native turn ID yet")?;
        validate_prompt(
            &SubmitExecution {
                request_id: runs_core::id(),
                session_id: session_id.into(),
                prompt: prompt.clone(),
                options: Value::Null,
                run_context: None,
            },
            &binding.session,
        )?;
        let result=client.send_request_for_workspace(&binding.session.workspace_id,"turn/steer",json!({"threadId":binding.session.native_session_id,"expectedTurnId":turn,"input":prompt})).await?;
        crate::shared::codex_core::history::rpc_result(&result)?;
        let _ = binding
            .events
            .send(json!({"method":"runtime/steered","params":{"input":prompt,"response":result}}));
        Ok(result)
    }
    pub async fn cancel(&self, id: &str) -> Result<(), String> {
        let b = self.binding(id).await?;
        for (key, p) in b.permissions.lock().await.drain() {
            let _ = key;
            answer(&b, &p, None).await?;
        }
        match &b.handle{
            RuntimeHandle::OpenCodeAcp(c)=>c.write(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":b.session.native_session_id}})).await,
            RuntimeHandle::Codex(_)=>{let _=b.events.send(json!({"method":"runtime/cancel"}));Ok(())}
        }
    }
    pub async fn permission(
        &self,
        id: &str,
        request: &str,
        option: Option<String>,
    ) -> Result<(), String> {
        let b = self.binding(id).await?;
        let mut pending = b.permissions.lock().await;
        let raw = pending
            .get(request)
            .cloned()
            .ok_or("invalid_input: stale or settled permission request")?;
        if let Some(ref o) = option {
            let valid = raw["params"]["options"]
                .as_array()
                .is_some_and(|a| a.iter().any(|v| v["optionId"] == *o));
            if !valid && b.session.profile.is_acp() {
                return Err("invalid_input: permission option not advertised".into());
            }
        }
        answer(&b, &raw, option).await?;
        pending.remove(request);
        let _ = b
            .events
            .send(json!({"method":"runtime/permission_settled","params":{"requestId":request}}));
        Ok(())
    }
    pub async fn configure(
        &self,
        path: PathBuf,
        id: &str,
        option: &str,
        value: Value,
    ) -> Result<Value, String> {
        let b = self.binding(id).await?;
        let c = match &b.handle {
            RuntimeHandle::OpenCodeAcp(c) => c,
            _ => return Err("unsupported: use Codex composer settings".into()),
        };
        let current = with_store(path.clone(), {
            let id = id.to_string();
            move |s| s.runtime_session(&id)
        })
        .await?;
        // Older persisted sessions and agents that advertise configuration only in
        // session/update may have options without the cached boolean being set.
        if !current.capabilities.configuration && !configuration_available(&current.configuration) {
            return Err("unsupported: session configuration unavailable".into());
        }
        let response = if let Some(options) = current.configuration["configOptions"].as_array() {
            let selected = options
                .iter()
                .find(|o| o["id"] == option)
                .ok_or("invalid_input: unknown configuration option")?;
            let choices = selected["options"]
                .as_array()
                .ok_or("unsupported: configuration option type")?;
            let valid = choices.iter().any(|o| {
                o["value"] == value
                    || o["options"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| v["value"] == value))
            });
            if !valid {
                return Err("invalid_input: configuration value not advertised".into());
            }
            c.request(
                "session/set_config_option",
                json!({"sessionId":b.session.native_session_id,"configId":option,"value":value}),
                Some(Duration::from_secs(30)),
            )
            .await?
        } else {
            let (method, field) = match option {
                "mode" => ("session/set_mode", "modeId"),
                "model" => ("session/set_model", "modelId"),
                _ => return Err("unsupported: configuration option".into()),
            };
            let advertised = if option == "mode" {
                current.configuration.pointer("/modes/availableModes")
            } else {
                current.configuration.pointer("/models/availableModels")
            };
            let id_field = if option == "mode" { "id" } else { "modelId" };
            if !advertised
                .and_then(Value::as_array)
                .is_some_and(|options| options.iter().any(|o| o[id_field] == value))
            {
                return Err("invalid_input: configuration value not advertised".into());
            }
            let mut params = json!({"sessionId":b.session.native_session_id});
            params[field] = value.clone();
            c.request(method, params, Some(Duration::from_secs(30)))
                .await?;
            let mut updated = current.configuration.clone();
            if option == "mode" {
                updated["modes"]["currentModeId"] = value;
            } else {
                updated["models"]["currentModelId"] = value;
            }
            updated
        };
        let mut saved = current;
        saved.configuration = if response.get("configOptions").is_some() {
            let mut v = saved.configuration.clone();
            v["configOptions"] = response["configOptions"].clone();
            v
        } else {
            response.clone()
        };
        saved.capabilities.configuration = configuration_available(&saved.configuration);
        persist(path, move |s| s.save_runtime_session(&saved)).await?;
        Ok(response)
    }
    pub async fn disconnect(&self, path: PathBuf, id: &str) -> Result<(), String> {
        // A failed initial replay is still a connection that can be released.
        let b = self
            .bindings
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or("not_found: session is not connected on this backend")?;
        let sid = id.to_string();
        if with_store(path.clone(), move |s| {
            Ok(s.runtime_history(&sid)?.iter().any(|t| {
                matches!(
                    t.execution,
                    ExecutionOutcome::Running
                        | ExecutionOutcome::NotStarted
                        | ExecutionOutcome::Unknown
                )
            }))
        })
        .await?
        {
            return Err(
                "runtime_busy: active or uncertain execution cannot be disconnected".into(),
            );
        }
        match &b.handle {
            RuntimeHandle::OpenCodeAcp(c) => c.stop().await,
            RuntimeHandle::Codex(c) => {
                c.runtime_subscribers
                    .lock()
                    .await
                    .remove(&b.session.native_session_id);
            }
        }
        self.bindings.lock().await.remove(id);
        let sid = id.to_string();
        let owner = self.owner.clone();
        persist(path, move |s| s.release_runtime_owner(&sid, &owner)).await
    }
}
fn validate_prompt(input: &SubmitExecution, session: &RuntimeSession) -> Result<(), String> {
    if input.prompt.is_empty() {
        return Err("invalid_input: prompt is empty".into());
    }
    if !input.options.is_null() && !input.options.is_object() {
        return Err("invalid_input: runtime options must be an object".into());
    }
    if session.runtime.kind == RuntimeKind::Codex {
        if let Some(options) = input.options.as_object() {
            for key in ["model", "effort", "serviceTier"] {
                if options
                    .get(key)
                    .is_some_and(|value| !value.is_null() && !value.is_string())
                {
                    return Err(format!("invalid_input: {key} must be a string or null"));
                }
            }
            if options
                .get("collaborationMode")
                .is_some_and(|value| !value.is_null() && !value.is_object())
            {
                return Err("invalid_input: collaborationMode must be an object or null".into());
            }
        }
    }

    for p in &input.prompt {
        match p["type"].as_str() {
            Some("text") if p["text"].is_string() => {}
            Some("image") if session.capabilities.images => {}
            Some("audio") if session.capabilities.audio => {}
            Some("resource") | Some("resource_link") if session.capabilities.embedded_context => {}
            _ => return Err("unsupported: prompt content type".into()),
        }
    }
    if session.profile.is_acp() && input.options.as_object().is_some_and(|o| !o.is_empty()) {
        return Err("unsupported: configure ACP options before submitting".into());
    }
    if session.runtime.kind == RuntimeKind::Codex
        && input.options.as_object().is_some_and(|o| {
            o.keys().any(|k| {
                !["model", "effort", "serviceTier", "collaborationMode"].contains(&k.as_str())
            })
        })
    {
        return Err("unsupported: Codex submission option".into());
    }
    Ok(())
}
async fn answer(b: &Binding, raw: &Value, option: Option<String>) -> Result<(), String> {
    match &b.handle{
    RuntimeHandle::OpenCodeAcp(c)=>c.write(json!({"jsonrpc":"2.0","id":raw["id"],"result":{"outcome":match option{Some(o)=>json!({"outcome":"selected","optionId":o}),None=>json!({"outcome":"cancelled"})}}})).await,
    RuntimeHandle::Codex(c)=>c.send_response(raw["id"].clone(),json!({"decision":option.unwrap_or("decline".into())})).await,
}
}
async fn drive<E: EventSink>(
    path: PathBuf,
    b: Arc<Binding>,
    mut rx: mpsc::UnboundedReceiver<Value>,
    sink: E,
    owner: String,
) -> Result<(), String> {
    let mut active: Option<ExecutionTicket> = None;
    let mut cancel_requested = false;
    while let Some(mut raw) = rx.recv().await {
        let method = raw["method"].as_str().unwrap_or("").to_string();
        if method == "runtime/begin" {
            active =
                Some(serde_json::from_value(raw["params"].clone()).map_err(|e| e.to_string())?);
        }
        if b.session.runtime.kind == RuntimeKind::Codex && !method.starts_with("runtime/") {
            let thread = raw
                .pointer("/params/threadId")
                .or_else(|| raw.pointer("/params/thread/id"))
                .and_then(Value::as_str);
            if thread != Some(b.session.native_session_id.as_str()) {
                continue;
            }
        }
        if method == "turn/started"
            && active.is_none()
            && b.session.runtime.kind == RuntimeKind::Codex
        {
            // A coalesced native Codex composer has one UI path. Observe its live
            // turn boundary without treating a history read as a new execution.
            let input = SubmitExecution {
                request_id: runs_core::id(),
                session_id: b.session.session_id.clone(),
                prompt: vec![],
                options: Value::Null,
                run_context: None,
            };
            let own = owner.clone();
            let (mut ticket, _) = persist(path.clone(), move |s| {
                s.begin_runtime_execution(input.clone(), &own)
            })
            .await?;
            ticket.native_turn_id = raw
                .pointer("/params/turn/id")
                .and_then(Value::as_str)
                .map(str::to_string);
            ticket.execution = ExecutionOutcome::Running;
            ticket.submission = SubmissionOutcome::Accepted;
            active = Some(ticket);
        }
        if method == "runtime/cancel" {
            cancel_requested = true;
        }
        if method == "runtime/history_end" {
            let load = raw["params"]["loadId"]
                .as_str()
                .ok_or("missing replay boundary")?
                .to_string();
            let failure = raw["params"]["response"]
                .get("error")
                .map(ToString::to_string);
            let event = RuntimeEvent {
                schema_version: 1,
                store_id: b.session.store_id.clone(),
                runtime: b.session.runtime.clone(),
                session_id: b.session.session_id.clone(),
                execution_id: None,
                sequence: 0,
                history_load_id: Some(load.clone()),
                source: "history".into(),
                kind: "history_response".into(),
                payload: raw["params"]["response"].clone(),
                raw,
            };
            let committed =
                persist(path.clone(), move |s| s.append_runtime_event(event.clone())).await?;
            persist(path.clone(), move |s| {
                s.finish_history_load(&load, failure.as_deref(), committed.sequence)
            })
            .await?;
            sink.emit_runtime_event(json!({"storeId":b.session.store_id,"sessionId":b.session.session_id,"kind":"history_loaded"}));
            continue;
        }
        let history = method == "runtime/history";
        let history_load_id = if history {
            raw["params"]["loadId"].as_str().map(str::to_string)
        } else {
            None
        };
        if history {
            raw = raw["params"]["event"].clone();
        }

        let foreign = b.session.profile.is_acp()
            && raw["method"]
                .as_str()
                .is_some_and(|method| method.starts_with("session/"))
            && raw
                .pointer("/params/sessionId")
                .and_then(Value::as_str)
                .is_some_and(|id| id != b.session.native_session_id);
        let (mut kind, mut payload) = projection::normalize(&b.session.runtime.kind, &raw);
        if foreign {
            if raw["method"] == "session/request_permission" {
                answer(&b, &raw, None).await?;
            }
            kind = "unattributed_session_update".into();
            payload = raw["params"].clone();
        }
        let mut terminal = false;
        if method == "runtime/prompt_response" {
            terminal = true;
            kind = "execution_completed".into();
            payload = raw["params"].clone();
            if let Some(t) = &mut active {
                t.submission = if payload.get("error").is_some()
                    && !matches!(t.submission, SubmissionOutcome::Accepted)
                {
                    SubmissionOutcome::Rejected
                } else {
                    SubmissionOutcome::Accepted
                };
                t.execution = if payload.get("error").is_some() {
                    ExecutionOutcome::Failed
                } else if payload["result"]["stopReason"] == "cancelled" {
                    ExecutionOutcome::Interrupted
                } else {
                    ExecutionOutcome::Completed
                };
                t.error = payload.get("error").map(ToString::to_string);
            }
        } else if method == "runtime/disconnected" {
            kind = "disconnected".into();
            payload = raw["params"].clone();
            if let Some(t) = &mut active {
                t.submission = if matches!(t.submission, SubmissionOutcome::Accepted) {
                    SubmissionOutcome::Accepted
                } else {
                    SubmissionOutcome::Unknown
                };
                t.execution = ExecutionOutcome::Unknown;
                t.error = Some(payload.to_string());
            }
        } else if method == "runtime/codex_receipt" {
            payload = raw["params"].clone();
            kind = "submission".into();
            if let Some(t) = &mut active {
                if let Some(id) = payload.pointer("/result/turn/id").and_then(Value::as_str) {
                    t.native_turn_id = Some(id.into());
                    t.submission = SubmissionOutcome::Accepted;
                } else {
                    terminal = true;
                    t.submission = SubmissionOutcome::Rejected;
                    t.execution = ExecutionOutcome::Failed;
                    t.error = Some(payload.to_string());
                }
            }
        } else if kind == "execution_completed" && !history {
            terminal = true;
            if let Some(t) = &mut active {
                t.submission = SubmissionOutcome::Accepted;
                t.native_turn_id = payload["id"]
                    .as_str()
                    .map(str::to_string)
                    .or(t.native_turn_id.clone());
                t.execution = runs_core::capture::outcome(payload["status"].as_str().unwrap_or(""));
            }
        } else if !history && !foreign && !method.starts_with("runtime/") && active.is_some() {
            if let Some(t) = &mut active {
                t.submission = SubmissionOutcome::Accepted;
                t.execution = ExecutionOutcome::Running;
                if b.session.runtime.kind == RuntimeKind::Codex {
                    t.native_turn_id = raw
                        .pointer("/params/turnId")
                        .or_else(|| raw.pointer("/params/turn/id"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or(t.native_turn_id.clone());
                }
            }
        }
        if cancel_requested {
            if let (Some(ticket), RuntimeHandle::Codex(client)) = (&active, &b.handle) {
                if let Some(turn) = &ticket.native_turn_id {
                    client
                        .send_request_for_workspace(
                            &b.session.workspace_id,
                            "turn/interrupt",
                            json!({"threadId":b.session.native_session_id,"turnId":turn}),
                        )
                        .await?;
                    cancel_requested = false;
                }
            }
        }
        if kind == "permission" && !history && !foreign {
            let generation = b.generation.clone();
            let key = format!("{generation}:{}", raw["id"]);
            b.permissions.lock().await.insert(key.clone(), raw.clone());
            payload["requestId"] = json!(key);
            if b.session.runtime.kind == RuntimeKind::Codex {
                payload["options"] = json!([{"optionId":"accept","name":"Approve once"},{"optionId":"decline","name":"Decline"}]);
            }
        }
        if method == "runtime/native_permission_settled" {
            let mut permissions = b.permissions.lock().await;
            let key = permissions
                .iter()
                .find(|(_, v)| v["id"] == raw["params"]["nativeRequestId"])
                .map(|(k, _)| k.clone());
            if let Some(key) = key {
                permissions.remove(&key);
                kind = "permission_settled".into();
                payload = json!({"requestId":key});
            }
        }
        if method == "runtime/permission_settled" {
            kind = "permission_settled".into();
            payload = raw["params"].clone();
        }
        if kind == "configuration" && !history {
            let sid = b.session.session_id.clone();
            let update = payload.clone();
            persist(path.clone(), move |s| {
                let mut session = s.runtime_session(&sid)?;
                if let Some(v) = update.get("configOptions") {
                    session.configuration["configOptions"] = v.clone();
                }
                if let Some(v) = update.get("currentModeId") {
                    session.configuration["modes"]["currentModeId"] = v.clone();
                }
                session.capabilities.configuration =
                    configuration_available(&session.configuration);
                s.save_runtime_session(&session)
            })
            .await?;
        }
        let event = RuntimeEvent {
            schema_version: 1,
            store_id: b.session.store_id.clone(),
            runtime: b.session.runtime.clone(),
            session_id: b.session.session_id.clone(),
            execution_id: if history || foreign {
                None
            } else {
                active.as_ref().map(|t| t.execution_id.clone())
            },
            sequence: 0,
            history_load_id,
            source: if history { "history" } else { "live" }.into(),
            kind,
            payload,
            raw,
        };
        if foreign {
            let scope = b.session.profile.connection_scope();
            let native = event.raw["params"]["sessionId"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let saved = event.clone();
            if let Some(committed) = persist(path.clone(), move |s| {
                s.route_foreign_conversation_event(&scope, &native, saved.clone())
            })
            .await?
            {
                sink.emit_runtime_event(
                    serde_json::to_value(committed).map_err(|e| e.to_string())?,
                );
                continue;
            }
        }
        let committed =
            persist(path.clone(), move |s| s.append_runtime_event(event.clone())).await?;
        let mut notification = serde_json::to_value(committed).map_err(|e| e.to_string())?;
        notification["workspaceId"] = json!(b.session.workspace_id);
        sink.emit_runtime_event(notification);
        if let Some(ticket) = active.clone() {
            if terminal {
                if let Some(context) = &ticket.input.run_context {
                    capture_execution_snapshot(
                        path.clone(),
                        context.instance_id.clone(),
                        ticket.execution_id.clone(),
                        format!("initial:{}", context.instance_id),
                        "Initial execution".into(),
                        CaptureKind::InitialTurn,
                        Some(ticket.clone()),
                    )
                    .await?;
                }
            }
            let saved = ticket.clone();
            // Publish terminal state and release the session only after its primary capture commits.
            persist(path.clone(), move |store| {
                store.update_runtime_execution(&saved, terminal)
            })
            .await?;
            if terminal {
                cancel_requested = false;
                active = None;
            }
        }
        if method == "runtime/disconnected" {
            break;
        }
    }
    Ok(())
}
pub(crate) async fn capture_execution(
    path: PathBuf,
    instance_id: String,
    execution_id: String,
    request_id: String,
    label: String,
    kind: CaptureKind,
) -> Result<RunCapture, String> {
    capture_execution_snapshot(
        path,
        instance_id,
        execution_id,
        request_id,
        label,
        kind,
        None,
    )
    .await
}
async fn capture_execution_snapshot(
    path: PathBuf,
    instance_id: String,
    execution_id: String,
    request_id: String,
    label: String,
    kind: CaptureKind,
    terminal_ticket: Option<ExecutionTicket>,
) -> Result<RunCapture, String> {
    persist(path,move|s|{
        if let Some(existing)=s.capture_by_request(&request_id)?{if existing.instance_id!=instance_id||existing.execution_id.as_deref()!=Some(&execution_id){return Err("invalid_input: capture identity conflicts".into());}return Ok(existing);}
        let instance=s.instance(&instance_id)?;let ticket=terminal_ticket.clone().map(Ok).unwrap_or_else(||s.runtime_execution(&execution_id))?;
        if instance.session_id.as_deref()!=Some(&ticket.session_id){return Err("invalid_input: execution does not belong to this run session".into());}
        let session=s.runtime_session(&ticket.session_id)?;let run=s.get_run(&instance.run_id)?;
        let mut cursor=0;let mut events=Vec::new();loop{let page=s.runtime_events(&ticket.session_id,cursor)?;if page.is_empty(){break;}cursor=page.last().unwrap().sequence;events.extend(page.into_iter().filter(|e|e.execution_id.as_deref()==Some(&execution_id)&&e.source=="live"));}
        let items=projection::capture_items(&events);
        let terminal=matches!(ticket.execution,ExecutionOutcome::Completed|ExecutionOutcome::Failed|ExecutionOutcome::Interrupted);
        let git=runs_core::capture::git_snapshot(&session.cwd,run.base_oid.as_deref());
        let mut errors=Vec::new();if !terminal{errors.push("Execution is not known to be terminal".into());}if let Some(e)=ticket.error.clone(){errors.push(e);}if let Some(a)=git["errors"].as_array(){errors.extend(a.iter().map(ToString::to_string));}
        let capture=RunCapture{schema_version:2,id:runs_core::id(),instance_id:instance_id.clone(),request_id:request_id.clone(),kind:kind.clone(),turn_id:ticket.native_turn_id.clone().unwrap_or_default(),execution_id:Some(execution_id.clone()),label:label.clone(),created_at:runs_core::now(),status:ticket.execution.clone(),items,usage:events.iter().rev().find_map(|e| if e.kind=="execution_completed" {e.payload.pointer("/result/usage").map(|u|json!({"scope":"execution","value":u}))}else{None}).or_else(||events.iter().rev().find(|e|e.kind=="usage").map(|e|e.payload.clone())),observed:json!({"runtime":ticket.runtime.as_ref().unwrap_or(&session.runtime),"agentInfo":ticket.agent_info,"configuration":session.configuration,"requested":ticket.input.options,"requestedConfiguration":ticket.configuration,"nativeTurnId":ticket.native_turn_id,"events":events}),git,complete:errors.is_empty(),errors};
        s.append_capture(capture)
    }).await
}

fn configuration_revision(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    }) & ((1u64 << 53) - 1)
}

fn configuration_available(configuration: &Value) -> bool {
    ["configOptions", "modes", "models"].iter().any(|key| {
        configuration
            .get(*key)
            .is_some_and(|value| !value.is_null())
    })
}

struct Discovery {
    client: Arc<AcpClient>,
    cursors: HashMap<String, (String, Option<String>)>,
}
pub(crate) fn acp_capabilities(init: &Value) -> RuntimeCapabilities {
    let c = &init["agentCapabilities"];
    RuntimeCapabilities {
        load_session: c["loadSession"] == true,
        session_discovery: c
            .pointer("/sessionCapabilities/list")
            .is_some_and(|v| !v.is_null() && v != &json!(false)),
        images: c["promptCapabilities"]["image"] == true,
        audio: c["promptCapabilities"]["audio"] == true,
        embedded_context: c["promptCapabilities"]["embeddedContext"] == true,
        cancellation: true,
        permissions: true,
        ..Default::default()
    }
}
impl RuntimeRegistry {
    pub async fn discover(
        &self,
        path: PathBuf,
        profile: RuntimeProfile,
        cwd: Option<String>,
        cursor: Option<String>,
    ) -> Result<Value, String> {
        if !profile.is_acp() {
            return Err("unsupported: use the Codex thread browser".into());
        }
        let scope = profile.connection_scope();
        let slot = self
            .discovery
            .lock()
            .await
            .entry(scope.clone())
            .or_default()
            .clone();
        let mut discovery = slot.lock().await;
        if discovery.as_ref().is_some_and(|c| !c.client.alive()) {
            *discovery = None;
        }
        if discovery.is_none() {
            if cursor.is_some() {
                return Err("invalid_input: discovery cursor expired; restart this source".into());
            }
            let launch_dir = profile
                .working_directory
                .clone()
                .or(cwd.clone())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into()
                });
            let (client, mut events) =
                AcpClient::spawn(&profile.executable, &profile.launch_args(), &launch_dir).await?;
            // Discovery is isolated from prompt/load connections and never restores a session.
            tokio::spawn(async move { while events.recv().await.is_some() {} });
            *discovery = Some(Discovery {
                client,
                cursors: HashMap::new(),
            });
        }
        let connection = discovery.as_mut().unwrap();
        let caps = acp_capabilities(&connection.client.initialization);
        if !caps.session_discovery {
            let rows = with_store(path, move |s| {
                Ok(s.catalog()?
                    .into_iter()
                    .filter(|s| s.connection_scope == scope)
                    .collect::<Vec<_>>())
            })
            .await?;
            return Ok(
                json!({"sessions":rows,"capabilities":caps,"sourceStatus":"unsupported","nextCursor":null}),
            );
        }
        let mut params = json!({});
        if let Some(cwd) = &cwd {
            params["cwd"] = json!(cwd);
        }
        if let Some(cursor) = cursor {
            let (native, filter) = connection
                .cursors
                .get(&cursor)
                .ok_or("invalid_input: discovery cursor expired; restart this source")?;
            if filter != &cwd {
                return Err("invalid_input: cursor belongs to a different directory filter".into());
            }
            params["cursor"] = json!(native);
        }
        let response = connection
            .client
            .request("session/list", params, Some(Duration::from_secs(30)))
            .await?;
        let infos = response["sessions"]
            .as_array()
            .ok_or("runtime_error: invalid session/list response")?
            .clone();
        let next = response["nextCursor"].as_str().map(|native| {
            let token = runs_core::id();
            connection
                .cursors
                .insert(token.clone(), (native.into(), cwd.clone()));
            token
        });
        let rows = persist(path, move |s| {
            infos
                .iter()
                .map(|info| s.catalog_upsert(&profile, info))
                .collect::<Result<Vec<_>, _>>()
        })
        .await?;
        Ok(json!({"sessions":rows,"capabilities":caps,"sourceStatus":"ready","nextCursor":next}))
    }
    pub async fn load_conversation<E: EventSink>(
        &self,
        path: PathBuf,
        session: RuntimeSession,
        request: String,
        sink: E,
    ) -> Result<HistoryLoad, String> {
        if !session.profile.is_acp() {
            return Err("unsupported: Codex history uses its native thread adapter".into());
        }
        let _guard = self.connect_lock.lock().await;
        let permission_pending =
            if let Some(b) = self.bindings.lock().await.get(&session.session_id) {
                !b.permissions.lock().await.is_empty()
            } else {
                false
            };
        let sid = session.session_id.clone();
        let owner = self.owner.clone();
        let (ticket, start) = persist(path.clone(), move |s| {
            if permission_pending && s.history_load(&request)?.is_none() {
                return Err("runtime_busy: permission is pending".into());
            }
            s.begin_history_load(&sid, &request, &owner)
        })
        .await?;
        if !start {
            return Ok(ticket);
        }
        let registry = self.clone();
        let load = ticket.clone();
        tokio::spawn(async move {
            if let Err(e) = registry
                .replay(path.clone(), session, load.clone(), sink)
                .await
            {
                let _ = persist(path, move |s| s.finish_history_load(&load.id, Some(&e), 0)).await;
            }
        });
        Ok(ticket)
    }
    async fn replay<E: EventSink>(
        &self,
        path: PathBuf,
        mut session: RuntimeSession,
        load: HistoryLoad,
        sink: E,
    ) -> Result<(), String> {
        let existing = {
            let mut bindings = self.bindings.lock().await;
            if bindings
                .get(&session.session_id)
                .is_some_and(|b| matches!(&b.handle,RuntimeHandle::OpenCodeAcp(c) if !c.alive()))
            {
                bindings.remove(&session.session_id);
            }
            bindings.get(&session.session_id).cloned()
        };
        let b = if let Some(b) = existing {
            b
        } else {
            let (client, rx) = AcpClient::spawn(
                &session.profile.executable,
                &session.profile.launch_args(),
                session
                    .profile
                    .working_directory
                    .as_deref()
                    .unwrap_or(&session.cwd),
            )
            .await?;
            let cached_configuration = session.configuration.clone();
            let cached_configuration_available = session.capabilities.configuration
                || configuration_available(&cached_configuration);
            session.capabilities = acp_capabilities(&client.initialization);
            // Loading a conversation should not erase configuration negotiated and
            // persisted by an earlier successful load if this replay later fails.
            session.configuration = cached_configuration;
            session.capabilities.configuration = cached_configuration_available;
            session.agent_info = client.initialization.clone();
            let saved = session.clone();
            persist(path.clone(), move |s| s.save_runtime_session(&saved)).await?;
            let (events, drive_rx) = mpsc::unbounded_channel();
            let b = Arc::new(Binding {
                ready: AtomicBool::new(false),
                events: events.clone(),
                generation: runs_core::id(),
                session: session.clone(),
                handle: RuntimeHandle::OpenCodeAcp(client),
                permissions: Default::default(),
            });
            let mut wire = rx;
            tokio::spawn(async move {
                while let Some(v) = wire.recv().await {
                    if events.send(v).is_err() {
                        break;
                    }
                }
            });
            self.bindings
                .lock()
                .await
                .insert(session.session_id.clone(), b.clone());
            let worker = b.clone();
            let work_path = path.clone();
            let owner = self.owner.clone();
            let faults = self.faults.clone();
            let sid = session.session_id.clone();
            tokio::spawn(async move {
                if let Err(e) = drive(work_path, worker, drive_rx, sink, owner).await {
                    faults.lock().await.insert(sid, e);
                }
            });
            b
        };
        let client = match &b.handle {
            RuntimeHandle::OpenCodeAcp(c) => c,
            _ => return Err("unsupported: ACP replay only".into()),
        };
        if !acp_capabilities(&client.initialization).load_session {
            return Err("unsupported: this agent cannot load conversation history".into());
        }
        let response = client
            .start_history_load(&load.id, &session.native_session_id, &session.cwd)
            .await?
            .await
            .map_err(|_| "transport_lost: replay abandoned")??;
        b.ready.store(true, Ordering::SeqCst);
        let sid = session.session_id.clone();
        persist(path, move |s| {
            let mut session = s.runtime_session(&sid)?;
            session.configuration = response.clone();
            session.capabilities.configuration = configuration_available(&response);
            s.save_runtime_session(&session)
        })
        .await?;
        Ok(())
    }
}
