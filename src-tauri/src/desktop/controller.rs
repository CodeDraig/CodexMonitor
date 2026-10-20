//! Native desktop conversation orchestration. This layer turns user actions into
//! one-shot backend requests and applies replies/events to view state; widgets only
//! inspect `state` and call `handle`.
#[path = "controller/actions.rs"]
mod actions;
#[path = "controller/state.rs"]
mod state;

use self::actions::ControllerAction;
use self::state::{
    CodexControls, ControllerState, ConversationState, QueuedPrompt, ThreadSummary, WorkspaceState,
};
use crate::client::contracts::{
    BackendError, BackendMessage, BackendRequest, BackendScope, CodexRequest, ConversationKey,
    ConversationRef, EventKind, RequestEnvelope, RuntimeMethod, RuntimeRequest, WorkspaceRequest,
};
use crate::client::BackendHandle;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::mpsc::TryRecvError,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(crate) use self::actions::ControllerAction as Action;
pub(crate) use self::state::ControllerState as State;

#[derive(Clone, Debug)]
enum Purpose {
    Initialize,
    Workspaces,
    Profiles,
    Catalog,
    Discovery(String),
    OpenedById,
    Threads { workspace_id: String, append: bool },
    ConnectForThreads { workspace_id: String, append: bool },
    CodexModels(String),
    CodexModes(String),
    CodexConfiguredModel(String),
    Resume(ConversationKey),
    ConnectForResume(ConversationKey),
    ConnectForNew(String),
    AcpSession(ConversationKey),
    NegotiatedSession(ConversationKey),
    AcpInitialRead(ConversationKey),
    AcpHistory(ConversationKey),
    AcpRead(ConversationKey),
    CreateCodex(String),
    CreateAcp(String),
    Attach(String),
    Send(ConversationKey, QueuedPrompt),
    Stop(ConversationKey, Vec<Value>),
    Permission(ConversationKey, Value),
    UserInput(ConversationKey, Value),
    Elicitation(String, Value),
    SlashNew { workspace_id: String, rest: String },
    SlashFork { key: ConversationKey, rest: String },
    SlashNotice(ConversationKey, String),
    Configure(ConversationKey),
    Metadata(String),
    Receipt(ConversationKey, Uuid),
    HistoryOlder(ConversationKey),
    ResyncEvents(ConversationKey),
    ResyncRead(ConversationKey),
    ResyncExecution(ConversationKey, String),
}

#[derive(Default)]
struct ActivityCompletions {
    pending: VecDeque<ConversationKey>,
    seen: HashSet<(ConversationKey, String)>,
}

impl ActivityCompletions {
    fn record(&mut self, key: ConversationKey, native_id: &str) -> bool {
        if native_id.is_empty() || !self.seen.insert((key.clone(), native_id.to_owned())) {
            return false;
        }
        self.pending.push_back(key);
        true
    }

    fn record_runtime_event(&mut self, key: ConversationKey, event: &Value) -> bool {
        if event["source"].as_str() != Some("live")
            || event["kind"].as_str() != Some("execution_completed")
        {
            return false;
        }
        let Some(execution_id) = event["executionId"].as_str() else {
            return false;
        };
        self.record(key, execution_id)
    }

    fn record_codex_event(
        &mut self,
        key: ConversationKey,
        method: &str,
        params: &Value,
        active_turn_id: Option<&str>,
    ) -> bool {
        if method != "turn/completed" {
            return false;
        }
        params
            .pointer("/turn/id")
            .or_else(|| params.get("turnId"))
            .and_then(Value::as_str)
            .or(active_turn_id)
            .is_some_and(|turn_id| self.record(key, turn_id))
    }

    fn drain(&mut self) -> Vec<ConversationKey> {
        self.pending.drain(..).collect()
    }

    fn contains(&self, key: &ConversationKey, native_id: &str) -> bool {
        self.seen.contains(&(key.clone(), native_id.to_owned()))
    }
}

#[derive(Default)]
struct HistoryRestartStage {
    items: Vec<Value>,
    previous_generation: Option<String>,
    previous_cursor: Option<Value>,
    previous_load_state: Option<String>,
    previous_coverage: Option<String>,
    previous_stale: bool,
}

pub(crate) struct Controller {
    pub state: ControllerState,
    backend: BackendHandle,
    scope: BackendScope,
    pending: HashMap<Uuid, Purpose>,
    pending_methods: HashMap<Uuid, String>,
    pending_reads: HashSet<ConversationKey>,
    /// Live Codex item patches received after the active resume request began.
    /// They overlay its potentially older response before publishing the view.
    resume_live_overlays: HashMap<ConversationKey, Vec<(String, Value)>>,
    /// A replacement snapshot is accumulated off-screen until all pages arrive,
    /// preserving the prior good view if a later page fails.
    history_restart_staging: HashMap<ConversationKey, HistoryRestartStage>,
    history_generation_retries: HashMap<ConversationKey, u8>,
    history_poll_due_ms: HashMap<ConversationKey, (u64, u32)>,
    last_event_read_ms: HashMap<ConversationKey, u64>,
    last_event_sequences: HashMap<&'static str, u64>,
    runtime_event_sequences: HashMap<ConversationKey, u64>,
    resyncing_runtime: HashSet<ConversationKey>,
    resync_live_buffer: HashMap<ConversationKey, Vec<Value>>,
    resync_errors: HashMap<ConversationKey, String>,
    codex_resync_pending: HashSet<ConversationKey>,
    codex_resync_inflight: HashSet<ConversationKey>,
    misc_results: VecDeque<(Uuid, Result<Value, BackendError>)>,
    activity_completions: ActivityCompletions,
    visible: bool,
}

impl Controller {
    pub(crate) fn new(scope: BackendScope, backend: BackendHandle) -> Self {
        Self {
            state: ControllerState {
                scope: Some(scope.clone()),
                ..ControllerState::default()
            },
            backend,
            scope,
            pending: HashMap::new(),
            pending_methods: HashMap::new(),
            pending_reads: HashSet::new(),
            resume_live_overlays: HashMap::new(),
            history_restart_staging: HashMap::new(),
            history_generation_retries: HashMap::new(),
            history_poll_due_ms: HashMap::new(),
            last_event_read_ms: HashMap::new(),
            last_event_sequences: HashMap::new(),
            runtime_event_sequences: HashMap::new(),
            resyncing_runtime: HashSet::new(),
            resync_live_buffer: HashMap::new(),
            resync_errors: HashMap::new(),
            codex_resync_pending: HashSet::new(),
            codex_resync_inflight: HashSet::new(),
            misc_results: VecDeque::new(),
            activity_completions: ActivityCompletions::default(),
            visible: true,
        }
    }

    pub(crate) fn scope(&self) -> &BackendScope {
        &self.scope
    }

    /// Change endpoint or generation after a backend switch. Old conversation
    /// drafts remain keyed by their original endpoint but are no longer selected.
    pub(crate) fn set_scope(&mut self, scope: BackendScope) {
        if self.scope != scope {
            self.scope = scope.clone();
            self.backend.set_scope(scope.clone());
            self.state.scope = Some(scope);
            self.state.selected = None;
            self.state.workspaces.clear();
            self.state.profiles.clear();
            self.state.catalog.clear();
            self.state.discovery.clear();
            // Conversation identity is scoped to this backend generation. Keeping
            // drafts across an epoch could send them into a replacement session.
            self.state.conversations.clear();
            self.state.codex_controls_by_workspace.clear();
            self.state.codex_elicitations_by_workspace.clear();
            self.state.threads_by_workspace.clear();
            self.state.thread_cursor_by_workspace.clear();
            self.state.thread_loading_by_workspace.clear();
            self.state.initialized = false;
            self.pending.clear();
            self.pending_methods.clear();
            self.pending_reads.clear();
            self.history_poll_due_ms.clear();
            self.last_event_read_ms.clear();
            self.last_event_sequences.clear();
            self.runtime_event_sequences.clear();
            self.resyncing_runtime.clear();
            self.resync_live_buffer.clear();
            self.resync_errors.clear();
            self.codex_resync_pending.clear();
            self.codex_resync_inflight.clear();
            self.misc_results.clear();
            self.activity_completions = ActivityCompletions::default();
            self.resume_live_overlays.clear();
            self.history_restart_staging.clear();
            self.history_generation_retries.clear();
        }
    }

    pub(crate) fn request(
        &mut self,
        request: BackendRequest,
    ) -> Result<Uuid, crate::client::EnqueueError> {
        let envelope = RequestEnvelope::new(self.scope.clone(), request);
        let id = envelope.correlation_id;
        let method = envelope.request.rpc().method;
        self.backend.enqueue(envelope)?;
        self.pending_methods.insert(id, method);
        Ok(id)
    }

    pub(crate) fn take_misc_results(&mut self) -> Vec<(Uuid, Result<Value, BackendError>)> {
        self.misc_results.drain(..).collect()
    }

    /// Native conversation completions waiting for the shell's notification
    /// and sound effects. Duplicate native turn/execution IDs are suppressed.
    pub(crate) fn take_activity_completions(&mut self) -> Vec<ConversationKey> {
        self.activity_completions.drain()
    }

    /// Bounded structural diagnostics; request parameters, prompt bodies,
    /// credentials, catalog records, and conversation bodies are omitted.
    pub(crate) fn diagnostics(&self) -> Value {
        let selected = self.state.selected.as_ref().and_then(|key| {
            let conversation = self.state.conversations.get(key);
            let identity = match &key.conversation {
                ConversationRef::Codex { workspace_id, thread_id } => json!({
                    "kind":"codex", "workspaceId":workspace_id, "threadId":thread_id
                }),
                ConversationRef::Acp { workspace_id, store_id, session_id } => json!({
                    "kind":"acp", "workspaceId":workspace_id, "storeId":store_id, "sessionId":session_id
                }),
            };
            Some(json!({
                "identity":identity,
                "busy":conversation.is_some_and(|c| c.busy),
                "awaitingFirstTurn":conversation.is_some_and(|c| c.awaiting_first_turn),
                "loading":conversation.is_some_and(|c| c.loading),
                "readOnly":conversation.is_some_and(|c| c.read_only),
                "historyRevision":conversation.map(|c| c.history_revision),
                "itemCount":conversation.map(|c| c.items.len()).unwrap_or_default(),
                "hasNextHistoryPage":conversation.is_some_and(|c| c.cursor.is_some()),
                "historyPagePending":self.pending_reads.contains(key),
                "stagedHistoryItems":self.history_restart_staging.get(key).map(|stage|stage.items.len()),
                "capabilityProbeAttempted":conversation.is_some_and(|c| c.capability_probe_attempted),
                "generation":conversation.and_then(|c| c.generation.as_ref()),
                "loadState":conversation.and_then(|c| c.load_state.as_ref()),
                // Error strings can contain provider echoes; surface presence,
                // never potentially echoed prompt or credential material.
                "loadErrorPresent":conversation.is_some_and(|c| c.last_error.is_some()),
                "uncertainRequest":conversation.and_then(|c| c.uncertain_request),
                "queuedPrompts":conversation.map(|c| c.queue.len()).unwrap_or_default(),
                "permissionRequests":conversation.map(|c| c.permissions.len()).unwrap_or_default(),
                "userInputRequests":conversation.map(|c| c.user_input_requests.len()).unwrap_or_default(),
            }))
        });
        let mut pending: Vec<_> = self
            .pending_methods
            .iter()
            .map(|(id, method)| json!({"id":id,"method":method}))
            .collect();
        pending.truncate(100);
        let discoveries: Vec<_> = self
            .state
            .discovery
            .iter()
            .map(|(profile_id, source)| {
                json!({
                    "profileId":profile_id,
                    "loading":source.loading,
                    "sourceStatus":source.status,
                    "errorPresent":source.error.is_some(),
                    "cachedSessions":source.sessions.len(),
                    "hasNextPage":source.cursor.is_some()
                })
            })
            .collect();
        let thread_count: usize = self.state.threads_by_workspace.values().map(Vec::len).sum();
        json!({
            "scope":{"endpointId":self.scope.endpoint_id,"connectionEpoch":self.scope.connection_epoch,"storeId":self.scope.store_id},
            "initialized":self.state.initialized,
            "counts":{"workspaces":self.state.workspaces.len(),"profiles":self.state.profiles.len(),"catalogSessions":self.state.catalog.len(),"threads":thread_count,"conversations":self.state.conversations.len()},
            "selected":selected,
            "pendingRequests":pending,
            "unclaimedReplies":self.misc_results.len(),
            "discovery":discoveries,
            "eventSequences":self.last_event_sequences,
            "controllerErrorPresent":self.state.error.is_some()
        })
    }

    /// Drain backend replies/events and run due idle refreshes. Call from the
    /// native event loop; no request or disk I/O runs during widget rendering.
    pub(crate) fn tick(&mut self, now_ms: u64, visible: bool, config_popup_open: bool) -> usize {
        self.visible = visible;
        if let Some(conversation) = self.state.selected_conversation_mut() {
            conversation.config_popup_open = config_popup_open;
        }
        let mut count = 0;
        loop {
            match self.backend.try_recv() {
                Ok(message) => {
                    count += 1;
                    self.apply_message(message, now_ms);
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        self.poll_loading_histories(now_ms);
        if self.state.selected.is_none() && !self.state.initialized {
            let _ = self.handle(ControllerAction::Initialize);
        }
        if visible {
            let can_refresh = self.state.selected_conversation().is_some_and(|c| {
                !c.busy
                    && !c.awaiting_first_turn
                    && !c.loading
                    && !c.config_popup_open
                    && !c.configuration_pending
                    && c.permissions.is_empty()
                    && c.user_input_requests.is_empty()
                    && !c.pending_submission
                    && !c.completion_ready
                    && !self.selected_workspace_has_elicitation()
                    && c.last_refresh_ms
                        .is_some_and(|last| now_ms.saturating_sub(last) >= 30_000)
            });
            if can_refresh {
                let _ = self.refresh_selected();
            }
        }
        count
    }

    pub(crate) fn handle(&mut self, action: ControllerAction) -> Result<Option<Uuid>, String> {
        use ControllerAction as A;
        match action {
            A::Initialize => {
                let mut emitted = false;
                for (request, purpose) in [
                    (
                        BackendRequest::Workspaces(WorkspaceRequest::read(
                            "list_workspaces",
                            json!({}),
                        )),
                        Purpose::Workspaces,
                    ),
                    (
                        BackendRequest::Runtime(RuntimeRequest::read(
                            RuntimeMethod::Profiles,
                            json!({}),
                        )),
                        Purpose::Profiles,
                    ),
                    (
                        BackendRequest::Runtime(RuntimeRequest::read(
                            RuntimeMethod::Catalog,
                            json!({}),
                        )),
                        Purpose::Catalog,
                    ),
                ] {
                    self.send(request, purpose)?;
                    emitted = true;
                }
                self.state.initialized = true;
                Ok(emitted.then_some(Uuid::nil()))
            }
            A::RefreshWorkspaces => self
                .send(
                    BackendRequest::Workspaces(WorkspaceRequest::read(
                        "list_workspaces",
                        json!({}),
                    )),
                    Purpose::Workspaces,
                )
                .map(Some),
            A::RefreshCatalog => self
                .send(
                    BackendRequest::Runtime(RuntimeRequest::read(
                        RuntimeMethod::Catalog,
                        json!({}),
                    )),
                    Purpose::Catalog,
                )
                .map(Some),
            A::DiscoverAll => {
                let ids: Vec<String> = self
                    .state
                    .profiles
                    .iter()
                    .filter(|p| p["kind"].as_str() != Some("codex"))
                    .filter_map(|p| p["id"].as_str().map(str::to_owned))
                    .collect();
                let mut last = None;
                for id in ids {
                    last = Some(self.discover(id, None)?);
                }
                Ok(last)
            }
            A::DiscoverProfile { profile_id, cursor } => {
                self.discover(profile_id, cursor).map(Some)
            }
            A::AddDiscoveryPage { profile_id } => {
                let cursor = self
                    .state
                    .discovery
                    .get(&profile_id)
                    .and_then(|s| s.cursor.clone())
                    .ok_or_else(|| "No next discovery page".to_owned())?;
                self.discover(profile_id, Some(cursor)).map(Some)
            }
            A::OpenSessionById { profile_id, cwd, native_session_id } => self.send(runtime(RuntimeMethod::OpenSessionById,json!({"profileId":profile_id,"cwd":cwd,"nativeSessionId":native_session_id}),true),Purpose::OpenedById).map(Some),
            A::OpenCatalog {
                catalog_id,
                add_workspace,
            } => {
                let known = self
                    .state
                    .catalog
                    .iter()
                    .find(|row| row["catalogId"].as_str() == Some(&catalog_id))
                    .cloned()
                    .ok_or("Unknown catalog session")?;
                let workspace = self.state.workspaces.iter().find(|w| {
                    w.id == known["workspaceId"].as_str().unwrap_or("")
                        || w.path == known["cwd"].as_str().unwrap_or("")
                });
                let association = if let Some(workspace) = workspace {
                    json!({"kind":"reuse","workspaceId":workspace.id})
                } else if add_workspace {
                    json!({"kind":"add"})
                } else {
                    return Err("Add this session's workspace before opening it".into());
                };
                self.send(
                    runtime(
                        RuntimeMethod::AttachSession,
                        json!({"catalogId":catalog_id,"workspaceAssociation":association}),
                        true,
                    ),
                    Purpose::Attach(catalog_id),
                )
                .map(Some)
            }
            A::CreateConversation {
                workspace_id,
                profile_id,
            } => {
                if let Some(profile_id) = profile_id {
                    let revision = self
                        .state
                        .profiles
                        .iter()
                        .find(|p| p["id"].as_str() == Some(&profile_id))
                        .and_then(|p| p["revision"].as_i64());
                    self.send(runtime(RuntimeMethod::CreateSession, json!({"workspaceId":workspace_id,"profileId":profile_id,"expectedProfileRevision":revision}), true), Purpose::CreateAcp(workspace_id)).map(Some)
                } else {
                    if self.workspace_connected(&workspace_id) {
                        self.start_codex_conversation(workspace_id).map(Some)
                    } else {
                        self.send(
                            BackendRequest::Workspaces(WorkspaceRequest::mutation(
                                "connect_workspace",
                                json!({"id":workspace_id}),
                                None,
                            )),
                            Purpose::ConnectForNew(workspace_id),
                        )
                        .map(Some)
                    }
                }
            }
            A::SelectCodex {
                workspace_id,
                thread_id,
            } => {
                let key = self.key(ConversationRef::Codex {
                    workspace_id: workspace_id.clone(),
                    thread_id: thread_id.clone(),
                });
                self.state.select(key.clone());
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.loading = true;
                }
                if self.workspace_connected(&workspace_id) {
                    self.load_codex_controls(workspace_id.clone());
                    self.resume_codex(key).map(Some)
                } else {
                    match self.send(
                        BackendRequest::Workspaces(WorkspaceRequest::mutation(
                            "connect_workspace",
                            json!({"id":workspace_id}),
                            None,
                        )),
                        Purpose::ConnectForResume(key.clone()),
                    ) {
                        Ok(id) => Ok(Some(id)),
                        Err(error) => {
                            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                                conversation.loading = false;
                            }
                            Err(error)
                        }
                    }
                }
            }
            A::LoadWorkspaceThreads { workspace_id } => self.load_workspace_threads(workspace_id,false).map(Some),
            A::LoadMoreThreads { workspace_id } => self.load_workspace_threads(workspace_id,true).map(Some),
            A::LoadCodexControls { workspace_id } => {
                if !self.workspace_connected(&workspace_id) {
                    return Err("Connect the workspace before loading Codex controls".into());
                }
                self.load_codex_controls(workspace_id);
                Ok(None)
            }
            A::SetCodexOption { workspace_id, name, value } => {
                let controls = self.state.codex_controls_by_workspace.entry(workspace_id).or_default();
                match name.as_str() {
                    "model" => controls.selected_model = value.as_str().map(str::to_owned),
                    "effort" => controls.selected_effort = value.as_str().map(str::to_owned),
                    "accessMode" => if let Some(value) = value.as_str() { controls.selected_access_mode = value.to_owned(); },
                    "serviceTier" => controls.selected_service_tier = value.as_str().map(str::to_owned),
                    "collaborationMode" => controls.selected_collaboration_mode = (!value.is_null()).then_some(value),
                    _ => return Err(format!("Unknown Codex option: {name}")),
                }
                Ok(None)
            }
            A::SelectAcp {
                workspace_id,
                store_id,
                session_id,
            } => {
                let key = self.key(ConversationRef::Acp {
                    workspace_id,
                    store_id,
                    session_id: session_id.clone(),
                });
                self.state.select(key.clone());
                self.send(
                    runtime(
                        RuntimeMethod::Session,
                        json!({"sessionId":session_id}),
                        false,
                    ),
                    Purpose::AcpSession(key),
                )
                .map(Some)
            }
            A::RefreshSelected => {
                if let Some(conversation) = self.state.selected_conversation_mut() {
                    if !conversation.read_only
                        && conversation
                            .session
                            .as_ref()
                            .is_some_and(|session| session["agentInfo"].is_null())
                    {
                        conversation.capability_probe_attempted = false;
                    }
                }
                self.refresh_selected().map(Some)
            }
            A::LoadOlderHistory => self.load_older().map(Some),
            A::SetDraft(text) => {
                if let Some(c) = self.state.selected_conversation_mut() {
                    c.draft = text;
                }
                Ok(None)
            }
            A::Send {
                content,
                options,
                steer,
            } => self.send_selected(content, options, steer).map(Some),
            A::RemoveQueued(id) => {
                if let Some(c) = self.state.selected_conversation_mut() {
                    c.queue.retain(|entry| entry.request_id != id);
                }
                Ok(None)
            }
            A::Stop => self.stop_selected().map(Some),
            A::CheckReceipt => self.check_receipt().map(Some),
            A::Permission {
                request_id,
                option_id,
            } => self.permission(request_id, option_id).map(Some),
            A::CodexUserInput { request_id, answers } => self.codex_user_input(request_id, answers).map(Some),
            A::CodexElicitation { workspace_id, request_id, result } => self.codex_elicitation(workspace_id, request_id, result).map(Some),
            A::Configure { option_id, value } => self.configure(option_id, value).map(Some),
            A::SetConfigurationPopup(open) => {
                if let Some(c) = self.state.selected_conversation_mut() {
                    c.config_popup_open = open;
                }
                Ok(None)
            }
            A::SetReviewPromptOpen(open) => {
                self.state.review_prompt_open = open;
                Ok(None)
            }
            A::UpdateMetadata {
                session_id,
                expected_revision,
                patch,
            } => self
                .send(
                    runtime(
                        RuntimeMethod::UpdateConversationMetadata,
                        metadata_params(session_id.clone(), expected_revision, patch),
                        true,
                    ),
                    Purpose::Metadata(session_id),
                )
                .map(Some),
            A::HideThread {
                workspace_id,
                thread_id,
            } => {
                self.state.hide_thread(&workspace_id, &thread_id);
                Ok(None)
            }
            A::SetVisible(visible) => {
                self.visible = visible;
                Ok(None)
            }
            A::Request(request) => self
                .request(request)
                .map(Some)
                .map_err(|e| format!("Backend request queue: {e:?}")),
        }
    }

    fn discover(&mut self, profile_id: String, cursor: Option<String>) -> Result<Uuid, String> {
        let source = self.state.discovery.entry(profile_id.clone()).or_default();
        source.profile_id = profile_id.clone();
        source.loading = true;
        source.error = None;
        self.send(
            runtime(
                RuntimeMethod::DiscoverSessions,
                json!({"profileId":profile_id,"cursor":cursor}),
                false,
            ),
            Purpose::Discovery(profile_id),
        )
    }

    fn workspace_connected(&self, workspace_id: &str) -> bool {
        self.state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
            .is_some_and(|workspace| workspace.raw["connected"] == true)
    }

    fn start_codex_conversation(&mut self, workspace_id: String) -> Result<Uuid, String> {
        self.send(
            BackendRequest::Codex(CodexRequest::mutation(
                "start_thread",
                json!({"workspaceId":workspace_id}),
                None,
            )),
            Purpose::CreateCodex(workspace_id),
        )
    }

    fn open_started_codex_thread(
        &mut self,
        workspace: String,
        thread: &Value,
        now_ms: u64,
    ) -> Option<ConversationKey> {
        let id = thread["id"].as_str()?.to_owned();
        let key = self.key(ConversationRef::Codex {
            workspace_id: workspace.clone(),
            thread_id: id.clone(),
        });
        let items = project_codex_thread_items(thread);
        let title = thread["name"]
            .as_str()
            .or(thread["preview"].as_str())
            .unwrap_or_default()
            .to_owned();
        let summary = ThreadSummary {
            id,
            workspace_id: workspace.clone(),
            title: title.clone(),
            path: thread["cwd"].as_str().unwrap_or_default().to_owned(),
            parent_id: thread["parentThreadId"].as_str().map(str::to_owned),
            updated_at: thread["updatedAt"].as_i64().unwrap_or_default(),
            processing: is_processing(thread),
            raw: thread.clone(),
            ..ThreadSummary::default()
        };
        let rows = self
            .state
            .threads_by_workspace
            .entry(workspace.clone())
            .or_default();
        if let Some(existing) = rows.iter_mut().find(|row| row.id == summary.id) {
            *existing = summary;
        } else {
            rows.insert(0, summary);
        }
        self.state.select(key.clone());
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            conversation.title = title;
            conversation.items = items;
            conversation.items_revision = conversation.items_revision.wrapping_add(1);
            conversation.loading = false;
            conversation.busy = is_processing(thread);
            conversation.active_turn_id = active_turn_id(thread);
            conversation.awaiting_first_turn = conversation.items.is_empty();
            conversation.last_refresh_ms = Some(now_ms);
        }
        self.load_codex_controls(workspace);
        Some(key)
    }

    fn resume_codex(&mut self, key: ConversationKey) -> Result<Uuid, String> {
        self.resume_codex_inner(key, false)
    }

    fn resume_codex_inner(
        &mut self,
        key: ConversationKey,
        allow_unpersisted_thread: bool,
    ) -> Result<Uuid, String> {
        let ConversationRef::Codex {
            workspace_id,
            thread_id,
        } = &key.conversation
        else {
            return Err("Expected Codex conversation".into());
        };
        if !allow_unpersisted_thread
            && self
                .state
                .conversations
                .get(&key)
                .is_some_and(|conversation| {
                    conversation.awaiting_first_turn && conversation.items.is_empty()
                })
        {
            // thread/start has no persisted rollout yet. The initial start
            // response is already the correct empty view; resume only after
            // first-turn activity arrives.
            return Ok(Uuid::nil());
        }
        if self
            .pending
            .values()
            .any(|purpose| matches!(purpose, Purpose::Resume(pending) if pending == &key))
        {
            return Ok(Uuid::nil());
        }
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            conversation.loading = true;
            conversation.last_error = None;
        }
        self.resume_live_overlays.entry(key.clone()).or_default();
        match self.send(
            BackendRequest::Codex(CodexRequest::read(
                "resume_thread",
                json!({"workspaceId":workspace_id,"threadId":thread_id}),
            )),
            Purpose::Resume(key.clone()),
        ) {
            Ok(id) => Ok(id),
            Err(error) => {
                self.resume_live_overlays.remove(&key);
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.loading = false;
                }
                Err(error)
            }
        }
    }

    fn resume_codex_for_resync(&mut self, key: ConversationKey) -> Result<Uuid, String> {
        self.codex_resync_inflight.insert(key.clone());
        match self.resume_codex_inner(key.clone(), true) {
            Ok(id) => Ok(id),
            Err(error) => {
                self.codex_resync_inflight.remove(&key);
                self.codex_resync_pending.insert(key);
                Err(error)
            }
        }
    }

    fn load_workspace_threads(
        &mut self,
        workspace_id: String,
        append: bool,
    ) -> Result<Uuid, String> {
        if self
            .state
            .thread_loading_by_workspace
            .contains(&workspace_id)
        {
            return Err("Thread list is already loading".into());
        }
        let cursor = if append {
            self.state
                .thread_cursor_by_workspace
                .get(&workspace_id)
                .cloned()
                .flatten()
                .ok_or("There are no older conversations")?
        } else {
            String::new()
        };
        self.state
            .thread_loading_by_workspace
            .insert(workspace_id.clone());
        if self.workspace_connected(&workspace_id) {
            self.list_workspace_threads(
                workspace_id,
                append,
                if append { Some(cursor) } else { None },
            )
        } else {
            let failed_id = workspace_id.clone();
            self.send(
                BackendRequest::Workspaces(WorkspaceRequest::mutation(
                    "connect_workspace",
                    json!({"id":workspace_id}),
                    None,
                )),
                Purpose::ConnectForThreads {
                    workspace_id,
                    append,
                },
            )
            .map_err(|error| {
                self.state.thread_loading_by_workspace.remove(&failed_id);
                error
            })
        }
    }

    fn list_workspace_threads(
        &mut self,
        workspace_id: String,
        append: bool,
        cursor: Option<String>,
    ) -> Result<Uuid, String> {
        self.send(BackendRequest::Codex(CodexRequest::read("list_threads",json!({"workspaceId":workspace_id,"cursor":cursor,"limit":100,"sortKey":"updated_at"}))),Purpose::Threads{workspace_id,append})
    }

    fn load_codex_controls(&mut self, workspace_id: String) {
        self.state
            .codex_controls_by_workspace
            .entry(workspace_id.clone())
            .or_default();
        for (method, purpose) in [
            ("model_list", Purpose::CodexModels(workspace_id.clone())),
            (
                "collaboration_mode_list",
                Purpose::CodexModes(workspace_id.clone()),
            ),
            (
                "get_config_model",
                Purpose::CodexConfiguredModel(workspace_id.clone()),
            ),
        ] {
            if let Err(error) = self.send(
                BackendRequest::Codex(CodexRequest::read(
                    method,
                    json!({"workspaceId":workspace_id}),
                )),
                purpose,
            ) {
                self.state.error = Some(error);
            }
        }
    }

    pub(crate) fn selected_codex_options(&self) -> Option<Value> {
        let ConversationRef::Codex { workspace_id, .. } =
            &self.state.selected.as_ref()?.conversation
        else {
            return None;
        };
        self.state
            .codex_controls_by_workspace
            .get(workspace_id)
            .map(CodexControls::send_options)
    }

    fn refresh_selected(&mut self) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        if self.pending_reads.contains(&key) {
            return Err("Conversation history is already loading".into());
        }
        match &key.conversation {
            ConversationRef::Codex { .. } => self.resume_codex(key),
            ConversationRef::Acp { session_id, .. } => {
                if self.state.conversations.get(&key).is_some_and(|c| {
                    c.busy
                        || c.config_popup_open
                        || !c.permissions.is_empty()
                        || !c.user_input_requests.is_empty()
                }) {
                    return Err("Conversation refresh is deferred while it is busy or a configuration popup is open".into());
                }
                let read_cached_conversation = !self.should_replay_history(&key)
                    || self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|c| c.read_only || c.loading || c.awaiting_first_turn);
                if read_cached_conversation {
                    let (generation, is_loading) = self
                        .state
                        .conversations
                        .get(&key)
                        .map(|c| (c.generation.clone(), c.loading))
                        .ok_or("Conversation is not loaded")?;
                    self.pending_reads.insert(key.clone());
                    return self.send(runtime(RuntimeMethod::ReadConversation,json!({"sessionId":session_id,"generation":if is_loading {Value::Null}else{json!(generation)},"cursor":null,"limit":200}),false),Purpose::AcpRead(key.clone())).map_err(|error| { self.pending_reads.remove(&key); error });
                }
                let _ = session_id;
                self.request_history_load(key)
            }
        }
    }

    fn request_history_load(&mut self, key: ConversationKey) -> Result<Uuid, String> {
        let session_id = match &key.conversation {
            ConversationRef::Acp { session_id, .. } => session_id.clone(),
            _ => return Err("Expected ACP conversation".into()),
        };
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            conversation.loading = true;
        }
        self.send(
            runtime(
                RuntimeMethod::LoadConversationHistory,
                json!({"sessionId":session_id,"requestId":Uuid::new_v4()}),
                true,
            ),
            Purpose::AcpHistory(key),
        )
    }

    fn load_older(&mut self) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        if self.pending_reads.contains(&key) {
            return Err("Conversation history is already loading".into());
        }
        let c = self
            .state
            .conversations
            .get(&key)
            .ok_or("Conversation is not loaded")?;
        if let ConversationRef::Acp { session_id, .. } = &key.conversation {
            let cursor = c.cursor.clone();
            self.pending_reads.insert(key.clone());
            self.send(runtime(RuntimeMethod::ReadConversation,json!({"sessionId":session_id,"generation":c.generation,"cursor":cursor,"limit":200}),false),Purpose::HistoryOlder(key.clone())).map_err(|error| { self.pending_reads.remove(&key); error })
        } else {
            Err("Codex history is loaded by resume; pagination is not exposed".into())
        }
    }

    fn send_selected(
        &mut self,
        content: Vec<Value>,
        mut options: Value,
        steer: bool,
    ) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        let has_workspace_elicitation = self.workspace_has_elicitation_for_key(&key);
        if matches!(key.conversation, ConversationRef::Codex { .. }) {
            if let Some(controls) = match &key.conversation {
                ConversationRef::Codex { workspace_id, .. } => {
                    self.state.codex_controls_by_workspace.get(workspace_id)
                }
                _ => None,
            } {
                if !options.is_object() {
                    options = json!({});
                }
                for (name, value) in controls.send_options().as_object().into_iter().flatten() {
                    if options.get(name).is_none_or(Value::is_null) {
                        options[name] = value.clone();
                    }
                }
            }
        }
        if matches!(&key.conversation, ConversationRef::Codex { .. }) {
            // Validate before taking the draft so unsupported file blocks are
            // reported without discarding either text or attachments.
            let _ = codex_images_from_content(&options["images"], &content)?;
        }
        let conv = self
            .state
            .conversations
            .get_mut(&key)
            .ok_or("Conversation is not loaded")?;
        if conv.read_only {
            return Err("Conversation is read-only because another backend owns it".into());
        }
        if let Some(message) = send_wait_message(conv) {
            return Err(message.into());
        }
        if conv.uncertain_request.is_some() {
            return Err("Check the previous submission receipt before sending again".into());
        }
        if conv.pending_submission {
            return Err(
                "Wait for the current submission acknowledgement before sending again".into(),
            );
        }
        if conv.draft.trim().is_empty() && content.is_empty() {
            return Err("Message is empty".into());
        }
        let entry = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: std::mem::take(&mut conv.draft),
            content,
            options,
        };
        if conv.busy && steer && matches!(key.conversation, ConversationRef::Codex { .. }) {
            if conv.active_turn_id.is_none() {
                return Err("Cannot steer because the active Codex turn is unknown".into());
            }
            return self.dispatch_prompt(key, entry);
        }
        if conv.busy
            || conv.loading
            || !conv.permissions.is_empty()
            || !conv.user_input_requests.is_empty()
            || has_workspace_elicitation
        {
            if entry.text.trim_start().starts_with('/') {
                return Err("Slash commands cannot be queued; wait for the active turn or use the command directly when idle".into());
            }
            conv.queue.push_back(entry);
            return Ok(Uuid::nil());
        }
        if let ConversationRef::Codex {
            workspace_id,
            thread_id,
        } = key.conversation.clone()
        {
            if let Some((command, rest)) = parse_slash(&entry.text) {
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.command_output = None;
                    conversation.last_error = None;
                }
                return self.route_codex_slash(key, workspace_id, thread_id, command, rest);
            }
        }
        self.dispatch_prompt(key, entry)
    }

    fn route_codex_slash(
        &mut self,
        key: ConversationKey,
        workspace_id: String,
        thread_id: String,
        command: SlashCommand,
        rest: String,
    ) -> Result<Uuid, String> {
        match command {
            SlashCommand::New => self.send(
                BackendRequest::Codex(CodexRequest::mutation(
                    "start_thread",
                    json!({"workspaceId":workspace_id}),
                    Some(Uuid::new_v4()),
                )),
                Purpose::SlashNew { workspace_id, rest },
            ),
            SlashCommand::Fork => self.send(
                BackendRequest::Codex(CodexRequest::mutation(
                    "fork_thread",
                    json!({"workspaceId":workspace_id,"threadId":thread_id}),
                    Some(Uuid::new_v4()),
                )),
                Purpose::SlashFork { key, rest },
            ),
            SlashCommand::Resume => self.resume_codex(key),
            SlashCommand::Compact => self.send(
                BackendRequest::Codex(CodexRequest::mutation(
                    "compact_thread",
                    json!({"workspaceId":workspace_id,"threadId":thread_id}),
                    Some(Uuid::new_v4()),
                )),
                Purpose::SlashNotice(key, "Context compaction requested.".into()),
            ),
            SlashCommand::Review => {
                if rest.is_empty() {
                    self.state.review_prompt_open = true;
                    return Ok(Uuid::nil());
                }
                let target = review_target(&rest);
                self.send(
                    BackendRequest::Codex(CodexRequest::mutation(
                        "start_review",
                        json!({"workspaceId":workspace_id,"threadId":thread_id,"target":target,"delivery":"inline"}),
                        Some(Uuid::new_v4()),
                    )),
                    Purpose::SlashNotice(key, "Review started.".into()),
                )
            }
            SlashCommand::Fast => {
                let workspace_controls = self.state.codex_controls_by_workspace.entry(workspace_id).or_default();
                let enabled = workspace_controls.selected_service_tier.as_deref() == Some("fast");
                let action = rest.to_ascii_lowercase();
                let output = match action.as_str() {
                    "" => {
                        workspace_controls.selected_service_tier = (!enabled).then(|| "fast".into());
                        format!("Fast mode {}.", if enabled { "disabled" } else { "enabled" })
                    }
                    "on" => { workspace_controls.selected_service_tier = Some("fast".into()); "Fast mode enabled.".into() }
                    "off" => { workspace_controls.selected_service_tier = None; "Fast mode disabled.".into() }
                    "status" => format!("Fast mode is {}.", if enabled { "on" } else { "off" }),
                    _ => "Usage: /fast, /fast on, /fast off, or /fast status.".into(),
                };
                self.set_command_output(&key, output);
                Ok(Uuid::nil())
            }
            SlashCommand::Status => {
                let controls = self.state.codex_controls_by_workspace.get(&workspace_id).cloned().unwrap_or_default();
                let mode = controls.selected_collaboration_mode.as_ref().and_then(|m| m["mode"].as_str().or(m["name"].as_str())).unwrap_or("default");
                let output = format!("Model: {}\nEffort: {}\nAccess: {}\nService tier: {}\nCollaboration: {}",
                    controls.selected_model.as_deref().or(controls.configured_model.as_deref()).unwrap_or("default"),
                    controls.selected_effort.as_deref().unwrap_or("default"),
                    controls.selected_access_mode,
                    controls.selected_service_tier.as_deref().unwrap_or("standard"),
                    mode);
                self.set_command_output(&key, output);
                Ok(Uuid::nil())
            }
            SlashCommand::Mcp => self.send(
                BackendRequest::Codex(CodexRequest::read("list_mcp_server_status", json!({"workspaceId":workspace_id,"cursor":null,"limit":100}))),
                Purpose::SlashNotice(key, "MCP status loaded.".into()),
            ),
            SlashCommand::Apps => self.send(
                BackendRequest::Codex(CodexRequest::read("apps_list", json!({"workspaceId":workspace_id,"cursor":null,"limit":100,"threadId":thread_id}))),
                Purpose::SlashNotice(key, "Apps loaded.".into()),
            ),
        }
    }

    fn set_command_output(&mut self, key: &ConversationKey, text: String) {
        if let Some(conversation) = self.state.conversations.get_mut(key) {
            conversation.command_output = Some(text);
        }
    }

    fn dispatch_prompt(
        &mut self,
        key: ConversationKey,
        entry: QueuedPrompt,
    ) -> Result<Uuid, String> {
        let active_turn = self
            .state
            .conversations
            .get(&key)
            .and_then(|c| c.active_turn_id.clone());
        let request = prompt_request(&key, &entry, active_turn)?;
        let id = self.send(request, Purpose::Send(key.clone(), entry.clone()))?;
        if let Some(c) = self.state.conversations.get_mut(&key) {
            c.busy = true;
            c.pending_submission = true;
        }
        Ok(id)
    }

    fn stop_selected(&mut self) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        if self
            .state
            .conversations
            .get(&key)
            .is_some_and(|conversation| conversation.read_only)
        {
            return Err("This conversation is read-only while another backend owns it".into());
        }
        let request = match &key.conversation {
            ConversationRef::Codex {
                workspace_id,
                thread_id,
            } => {
                let turn_id = self
                    .state
                    .conversations
                    .get(&key)
                    .and_then(|c| c.active_turn_id.as_deref())
                    .ok_or("No active Codex turn")?;
                BackendRequest::Codex(CodexRequest::mutation(
                    "turn_interrupt",
                    json!({"workspaceId":workspace_id,"threadId":thread_id,"turnId":turn_id}),
                    None,
                ))
            }
            ConversationRef::Acp { session_id, .. } => {
                runtime(RuntimeMethod::Cancel, json!({"sessionId":session_id}), true)
            }
        };
        let permissions_at_cancel = self
            .state
            .conversations
            .get(&key)
            .map(|conversation| {
                conversation
                    .permissions
                    .iter()
                    .filter_map(|permission| permission.get("requestId").cloned())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(c) = self.state.conversations.get_mut(&key) {
            c.queue.clear();
        }
        self.send(request, Purpose::Stop(key, permissions_at_cancel))
    }

    fn permission(&mut self, request_id: Value, option_id: String) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        if self
            .state
            .conversations
            .get(&key)
            .is_some_and(|conversation| conversation.read_only)
        {
            return Err("This conversation is read-only while another backend owns it".into());
        }
        let request = match &key.conversation {
            ConversationRef::Acp { session_id, .. } => runtime(
                RuntimeMethod::Permission,
                json!({"sessionId":session_id,"requestId":request_id,"optionId":option_id}),
                true,
            ),
            ConversationRef::Codex { workspace_id, .. } => {
                BackendRequest::Codex(CodexRequest::mutation(
                    "respond_to_server_request",
                    json!({"workspaceId":workspace_id,"requestId":request_id,"result":{"decision":option_id}}),
                    None,
                ))
            }
        };
        self.send(request, Purpose::Permission(key, request_id))
    }

    fn configure(&mut self, option_id: String, value: String) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        let ConversationRef::Acp { session_id, .. } = &key.conversation else {
            return Err(
                "Configuration is available only when the selected agent advertises it".into(),
            );
        };
        let conversation = self
            .state
            .conversations
            .get(&key)
            .ok_or("Conversation is not loaded")?;
        if conversation.read_only {
            return Err("This conversation is read-only while another backend owns it".into());
        }
        if conversation.configuration_pending {
            return Err("A configuration change is already waiting for the agent".into());
        }
        let session = conversation
            .session
            .as_ref()
            .ok_or("Session configuration is not loaded")?;
        if !configuration_available(session) {
            return Err("This agent does not support configuration".into());
        }
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            conversation.configuration_pending = true;
        }
        let result = self.send(
            runtime(
                RuntimeMethod::Configure,
                json!({"sessionId":session_id,"optionId":option_id,"value":value}),
                true,
            ),
            Purpose::Configure(key.clone()),
        );
        if result.is_err() {
            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                conversation.configuration_pending = false;
            }
        }
        result
    }

    fn codex_user_input(&mut self, request_id: Value, answers: Value) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        let ConversationRef::Codex { workspace_id, .. } = &key.conversation else {
            return Err("Codex user input cannot be sent to an ACP session".into());
        };
        self.send(
            BackendRequest::Codex(CodexRequest::mutation(
                "respond_to_server_request",
                json!({"workspaceId":workspace_id,"requestId":request_id,"result":{"answers":answers}}),
                None,
            )),
            Purpose::UserInput(key, request_id),
        )
    }

    fn codex_elicitation(
        &mut self,
        workspace_id: String,
        request_id: Value,
        result: Value,
    ) -> Result<Uuid, String> {
        self.send(
            BackendRequest::Codex(CodexRequest::mutation(
                "respond_to_server_request",
                json!({"workspaceId":workspace_id,"requestId":request_id,"result":result}),
                None,
            )),
            Purpose::Elicitation(workspace_id, request_id),
        )
    }

    fn check_receipt(&mut self) -> Result<Uuid, String> {
        let key = self
            .state
            .selected
            .clone()
            .ok_or("No selected conversation")?;
        let request_id = self
            .state
            .conversations
            .get(&key)
            .and_then(|c| c.uncertain_request)
            .ok_or("There is no uncertain submission")?;
        match key.conversation.clone() {
            ConversationRef::Acp { .. } => self.send(
                runtime(
                    RuntimeMethod::Execution,
                    json!({"requestId":request_id}),
                    false,
                ),
                Purpose::Receipt(key, request_id),
            ),
            _ => Err("Codex submissions are reconciled by the live thread".into()),
        }
    }

    fn send(&mut self, request: BackendRequest, purpose: Purpose) -> Result<Uuid, String> {
        let envelope = RequestEnvelope::new(self.scope.clone(), request);
        let id = envelope.correlation_id;
        let method = envelope.request.rpc().method;
        self.backend
            .enqueue(envelope)
            .map_err(|e| format!("Could not queue backend request: {e:?}"))?;
        self.pending.insert(id, purpose);
        self.pending_methods.insert(id, method);
        Ok(id)
    }

    fn key(&self, conversation: ConversationRef) -> ConversationKey {
        ConversationKey {
            endpoint_id: self.scope.endpoint_id.clone(),
            conversation,
        }
    }

    fn apply_message(&mut self, message: BackendMessage, now_ms: u64) {
        match message {
            BackendMessage::Reply {
                correlation_id,
                scope,
                result,
            } => {
                if scope != self.scope {
                    return;
                }
                self.pending_methods.remove(&correlation_id);
                let Some(purpose) = self.pending.remove(&correlation_id) else {
                    self.misc_results.push_back((correlation_id, result));
                    return;
                };
                match result {
                    Ok(value) => self.apply_reply(purpose, value, now_ms),
                    Err(error) => self.apply_error(purpose, error, now_ms),
                }
            }
            BackendMessage::Event { scope, event } => {
                if scope == self.scope {
                    if let Some(sequence) = event.sequence {
                        let stream = match event.kind {
                            EventKind::Runtime => "runtime",
                            EventKind::AppServer => "appServer",
                            EventKind::TerminalOutput | EventKind::TerminalExit => "terminal",
                        };
                        self.last_event_sequences.insert(stream, sequence);
                    }
                    match event.kind {
                        EventKind::Runtime => self.apply_event(event.payload, now_ms),
                        EventKind::AppServer => self.apply_codex_event(event.payload),
                        _ => {}
                    }
                }
            }
            BackendMessage::ResyncRequired { scope, stream } => {
                if scope == self.scope {
                    self.recover_event_stream(stream, now_ms);
                }
            }
        }
    }

    fn recover_event_stream(
        &mut self,
        stream: crate::client::contracts::EventStream,
        _now_ms: u64,
    ) {
        use crate::client::contracts::EventStream;
        match stream {
            EventStream::Runtime => {
                let mut keys = HashSet::new();
                for key in self.state.conversations.keys() {
                    if matches!(&key.conversation, ConversationRef::Acp { store_id, .. }
                        if self.scope.store_id.as_deref().is_none_or(|scope_store| scope_store == store_id))
                    {
                        keys.insert(key.clone());
                    }
                }
                for row in &self.state.catalog {
                    let (Some(store), Some(session), Some(workspace)) = (
                        row["storeId"].as_str(),
                        row["sessionId"].as_str(),
                        row["workspaceId"].as_str(),
                    ) else {
                        continue;
                    };
                    if self
                        .scope
                        .store_id
                        .as_deref()
                        .is_some_and(|scope_store| scope_store != store)
                    {
                        continue;
                    }
                    keys.insert(self.key(ConversationRef::Acp {
                        workspace_id: workspace.into(),
                        store_id: store.into(),
                        session_id: session.into(),
                    }));
                }
                for key in keys {
                    let Some(session_id) = (match &key.conversation {
                        ConversationRef::Acp { session_id, .. } => Some(session_id.clone()),
                        _ => None,
                    }) else {
                        continue;
                    };
                    if !self.resyncing_runtime.insert(key.clone()) {
                        continue;
                    }
                    self.resync_live_buffer.insert(key.clone(), Vec::new());
                    if let Some(c) = self.state.conversations.get_mut(&key) {
                        c.stale = true;
                        c.last_error = Some("Runtime event stream needs resynchronization".into());
                    }
                    let after = self.runtime_event_sequences.get(&key).copied().unwrap_or(0);
                    if self
                        .send(
                            runtime(
                                RuntimeMethod::Events,
                                json!({"sessionId":session_id,"after":after}),
                                false,
                            ),
                            Purpose::ResyncEvents(key.clone()),
                        )
                        .is_err()
                    {
                        self.resyncing_runtime.remove(&key);
                        self.resync_live_buffer.remove(&key);
                    }
                }
            }
            EventStream::AppServer => {
                let keys: Vec<_> = self
                    .state
                    .conversations
                    .keys()
                    .filter(|key| matches!(key.conversation, ConversationRef::Codex { .. }))
                    .cloned()
                    .collect();
                for key in keys {
                    if let Some(conversation) = self.state.conversations.get_mut(&key) {
                        conversation.stale = true;
                        conversation.last_error =
                            Some("Codex event stream needs resynchronization".into());
                    }
                    if self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|c| c.pending_submission)
                    {
                        self.codex_resync_pending.insert(key);
                        continue;
                    }
                    if self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|c| c.loading)
                    {
                        self.codex_resync_pending.insert(key);
                        continue;
                    }
                    let _ = self.resume_codex_for_resync(key);
                }
            }
            EventStream::Terminal => {}
        }
    }

    fn finish_runtime_resync(&mut self, key: ConversationKey, _now_ms: u64) {
        let mut buffered = self.resync_live_buffer.remove(&key).unwrap_or_default();
        buffered.sort_by_key(|event| event["sequence"].as_u64().unwrap_or(u64::MAX));
        for mut event in buffered {
            event["workspaceId"] = match &key.conversation {
                ConversationRef::Acp { workspace_id, .. } => json!(workspace_id),
                _ => Value::Null,
            };
            self.apply_event_inner(event, _now_ms, true);
        }
        self.resyncing_runtime.remove(&key);
        self.drain_queue(key.clone());
        let ConversationRef::Acp { session_id, .. } = &key.conversation else {
            return;
        };
        if self
            .send(
                runtime(
                    RuntimeMethod::ReadConversation,
                    json!({"sessionId":session_id,"limit":200}),
                    false,
                ),
                Purpose::ResyncRead(key.clone()),
            )
            .is_err()
        {
            if let Some(c) = self.state.conversations.get_mut(&key) {
                c.stale = true;
                c.last_error =
                    Some("Could not read cached conversation after event recovery".into());
            }
        }
    }

    fn apply_reply(&mut self, purpose: Purpose, value: Value, now_ms: u64) {
        match purpose {
            Purpose::Initialize => {}
            Purpose::Workspaces => {
                self.state.workspaces = as_array(&value)
                    .iter()
                    .filter_map(|w| {
                        Some(WorkspaceState {
                            id: w["id"].as_str()?.into(),
                            path: w["path"].as_str().unwrap_or_default().into(),
                            name: w["name"].as_str().unwrap_or_default().into(),
                            raw: w.clone(),
                        })
                    })
                    .collect();
            }
            Purpose::Profiles => {
                self.state.profiles = as_array(&value["profiles"]).to_vec();
                self.state.runtime_store_id = value["storeId"].as_str().map(str::to_owned);
                let _ = self.handle(ControllerAction::DiscoverAll);
            }
            Purpose::Catalog => self.state.catalog = as_array(&value).to_vec(),
            Purpose::OpenedById => {
                let _ = self.handle(ControllerAction::RefreshCatalog);
                let cwd = value["cwd"].as_str().unwrap_or_default();
                let catalog_id = value["catalogId"].as_str().map(str::to_owned);
                let workspace_id = self
                    .state
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.path == cwd)
                    .map(|workspace| workspace.id.clone());
                if let (Some(workspace_id), Some(catalog_id)) = (workspace_id, catalog_id) {
                    let _ = self.send(
                        runtime(
                            RuntimeMethod::AttachSession,
                            json!({"catalogId":catalog_id,"workspaceAssociation":{"kind":"reuse","workspaceId":workspace_id}}),
                            true,
                        ),
                        Purpose::Attach(catalog_id),
                    );
                } else {
                    self.state.error = Some("Session added to the catalog. Add its reported directory as a workspace, then open it from the sidebar.".into());
                }
            }
            Purpose::Discovery(profile) => {
                let src = self.state.discovery.entry(profile).or_default();
                src.loading = false;
                src.status = value["sourceStatus"].as_str().map(str::to_owned);
                src.cursor = value["nextCursor"].as_str().map(str::to_owned);
                for session in as_array(&value["sessions"]) {
                    let catalog_id = session["catalogId"].as_str();
                    if let Some(existing) = catalog_id.and_then(|id| {
                        src.sessions
                            .iter_mut()
                            .find(|row| row["catalogId"].as_str() == Some(id))
                    }) {
                        *existing = session.clone();
                    } else {
                        src.sessions.push(session.clone());
                    }
                    if let Some(catalog_id) = catalog_id {
                        if let Some(existing) = self
                            .state
                            .catalog
                            .iter_mut()
                            .find(|row| row["catalogId"].as_str() == Some(catalog_id))
                        {
                            *existing = session.clone();
                        } else {
                            self.state.catalog.push(session.clone());
                        }
                    }
                }
            }
            Purpose::Threads {
                workspace_id: workspace,
                append,
            } => {
                self.state.thread_loading_by_workspace.remove(&workspace);
                let mut rows = parse_threads(&value, &workspace);
                if append {
                    let mut combined = self
                        .state
                        .threads_by_workspace
                        .get(&workspace)
                        .cloned()
                        .unwrap_or_default();
                    combined.append(&mut rows);
                    rows = combined;
                }
                for row in &rows {
                    if let Some(parent) = &row.parent_id {
                        self.state
                            .parents_by_thread
                            .insert((workspace.clone(), row.id.clone()), parent.clone());
                    }
                }
                let processing = rows
                    .iter()
                    .filter(|r| r.processing)
                    .map(|r| r.id.clone())
                    .collect();
                self.state.reconcile_threads(&workspace, rows, &processing);
                let result = value.get("result").unwrap_or(&value);
                self.state
                    .thread_cursor_by_workspace
                    .insert(workspace, result["nextCursor"].as_str().map(str::to_owned));
            }
            Purpose::CodexModels(workspace) => {
                let controls = self
                    .state
                    .codex_controls_by_workspace
                    .entry(workspace)
                    .or_default();
                controls.models = value["result"]["data"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| as_array(&value).to_vec());
                if controls.selected_model.is_none() {
                    controls.selected_model = controls.configured_model.clone().or_else(|| {
                        controls.models.first().and_then(|m| {
                            m["model"]
                                .as_str()
                                .or_else(|| m["id"].as_str())
                                .map(str::to_owned)
                        })
                    });
                }
            }
            Purpose::CodexModes(workspace) => {
                let controls = self
                    .state
                    .codex_controls_by_workspace
                    .entry(workspace)
                    .or_default();
                controls.collaboration_modes = value["result"]["data"]
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| as_array(&value).to_vec());
                if controls.selected_collaboration_mode.is_none() {
                    controls.selected_collaboration_mode = controls
                        .collaboration_modes
                        .iter()
                        .find(|m| {
                            m["mode"]
                                .as_str()
                                .or_else(|| m["name"].as_str())
                                .is_some_and(|v| {
                                    v.eq_ignore_ascii_case("default")
                                        || v.eq_ignore_ascii_case("code")
                                })
                        })
                        .cloned()
                        .or_else(|| controls.collaboration_modes.first().cloned());
                }
            }
            Purpose::CodexConfiguredModel(workspace) => {
                let controls = self
                    .state
                    .codex_controls_by_workspace
                    .entry(workspace)
                    .or_default();
                controls.configured_model = value["model"].as_str().map(str::to_owned);
                let first_model = controls
                    .models
                    .first()
                    .and_then(|m| m["model"].as_str().or_else(|| m["id"].as_str()));
                if controls.selected_model.is_none()
                    || controls.selected_model.as_deref() == first_model
                {
                    controls.selected_model = controls
                        .configured_model
                        .clone()
                        .or_else(|| controls.selected_model.clone());
                }
            }
            Purpose::ConnectForThreads {
                workspace_id,
                append,
            } => {
                if let Some(workspace) = self
                    .state
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace_id)
                {
                    workspace.raw["connected"] = json!(true);
                }
                let cursor = if append {
                    self.state
                        .thread_cursor_by_workspace
                        .get(&workspace_id)
                        .cloned()
                        .flatten()
                } else {
                    None
                };
                let _ = self.list_workspace_threads(workspace_id, append, cursor);
            }
            Purpose::Resume(key) => {
                if let Some(thread) = extract_thread(&value) {
                    let is_resync = self.codex_resync_inflight.remove(&key);
                    let mut items = project_codex_thread_items(&thread);
                    if let Some(overlay) = self.resume_live_overlays.remove(&key) {
                        for (method, params) in overlay {
                            apply_codex_item_event(&mut items, &method, &params);
                        }
                    }
                    let should_drain = {
                        let conv = self.state.conversations.entry(key.clone()).or_default();
                        let completed_after_request =
                            conv.completion_ready && !conv.pending_submission;
                        conv.items = items;
                        conv.history_revision = conv.history_revision.wrapping_add(1);
                        conv.items_revision = conv.items_revision.wrapping_add(1);
                        conv.title = thread["name"].as_str().unwrap_or_default().to_owned();
                        conv.busy = is_processing(&thread) && !completed_after_request;
                        if is_resync && !is_processing(&thread) && !conv.pending_submission {
                            conv.pending_execution_id = None;
                        }
                        conv.active_turn_id = if completed_after_request {
                            None
                        } else {
                            active_turn_id(&thread)
                        };
                        conv.loading = false;
                        conv.stale = false;
                        conv.completion_ready = false;
                        conv.last_refresh_ms = Some(now_ms);
                        conv.last_error = None;
                        !conv.busy && !conv.pending_submission
                    };
                    let repeat_resync = self.codex_resync_pending.contains(&key)
                        && self
                            .state
                            .conversations
                            .get(&key)
                            .is_some_and(|c| !c.pending_submission);
                    if repeat_resync {
                        self.codex_resync_pending.remove(&key);
                        let _ = self.resume_codex_for_resync(key);
                    } else if should_drain {
                        self.drain_queue(key);
                    }
                }
            }
            Purpose::ConnectForResume(key) => {
                let workspace_id = match &key.conversation {
                    ConversationRef::Codex { workspace_id, .. } => workspace_id.clone(),
                    _ => return,
                };
                if let Some(workspace) = self
                    .state
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace_id)
                {
                    workspace.raw["connected"] = json!(true);
                }
                self.load_codex_controls(workspace_id);
                let _ = self.resume_codex(key);
            }
            Purpose::ConnectForNew(workspace_id) => {
                if let Some(workspace) = self
                    .state
                    .workspaces
                    .iter_mut()
                    .find(|w| w.id == workspace_id)
                {
                    workspace.raw["connected"] = json!(true);
                }
                let _ = self.start_codex_conversation(workspace_id);
            }
            Purpose::AcpSession(key) => {
                if self.state.selected.as_ref() == Some(&key) {
                    if let Some(c) = self.state.conversations.get_mut(&key) {
                        c.session = Some(value.clone());
                        c.read_only = value["readOnly"] == true;
                        c.title = value["title"]
                            .as_str()
                            .or_else(|| value["nativeSessionId"].as_str())
                            .unwrap_or_default()
                            .to_owned();
                        c.loading = true;
                    }
                    let session_id = value["sessionId"].as_str().unwrap_or_default();
                    let _ = self.send(
                        runtime(
                            RuntimeMethod::ReadConversation,
                            json!({"sessionId":session_id,"limit":200}),
                            false,
                        ),
                        Purpose::AcpInitialRead(key),
                    );
                }
            }
            Purpose::NegotiatedSession(key) => {
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.session = Some(value.clone());
                    conversation.title = value["title"]
                        .as_str()
                        .or_else(|| value["nativeSessionId"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                }
            }
            Purpose::AcpInitialRead(key) => {
                self.apply_page(key.clone(), value, false, now_ms);
                if self.probe_capabilities_and_load(&key) {
                    // Cached content was read first. The shared history-load
                    // operation now initializes ACP and negotiates capabilities.
                } else if self.should_replay_history(&key) {
                    let _ = self.request_history_load(key);
                }
            }
            Purpose::AcpHistory(key) => {
                if self.state.selected.as_ref() == Some(&key) {
                    if let Some(c) = self.state.conversations.get_mut(&key) {
                        c.loading = true;
                        c.last_error = None;
                    }
                    let session_id = match &key.conversation {
                        ConversationRef::Acp { session_id, .. } => session_id.clone(),
                        _ => return,
                    };
                    if self.pending_reads.insert(key.clone()) {
                        let _ = self.send(
                            runtime(
                                RuntimeMethod::ReadConversation,
                                json!({"sessionId":session_id,"limit":200}),
                                false,
                            ),
                            Purpose::AcpRead(key),
                        );
                    }
                }
            }
            Purpose::AcpRead(key) => {
                self.pending_reads.remove(&key);
                let was_awaiting_first_turn = self
                    .state
                    .conversations
                    .get(&key)
                    .is_some_and(|c| c.awaiting_first_turn);
                self.apply_page(key.clone(), value, false, now_ms);
                if self.probe_capabilities_and_load(&key) {
                    // One automatic negotiation attempt is made per selection.
                } else if self.capability_probe_finished_without_session(&key) {
                    self.refresh_negotiated_session(key.clone());
                } else if was_awaiting_first_turn
                    && self.should_replay_history(&key)
                    && self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|c| !c.loading && !c.busy && !c.items.is_empty())
                {
                    let _ = self.request_history_load(key);
                } else {
                    self.request_next_history_page(key);
                }
            }
            Purpose::HistoryOlder(key) => {
                self.pending_reads.remove(&key);
                self.apply_page(key.clone(), value, true, now_ms);
                self.request_next_history_page(key);
            }
            Purpose::ResyncEvents(key) => {
                let events = as_array(&value).to_vec();
                for event in &events {
                    let mut projected = event.clone();
                    projected["workspaceId"] = match &key.conversation {
                        ConversationRef::Acp { workspace_id, .. } => json!(workspace_id),
                        _ => Value::Null,
                    };
                    self.apply_event_inner(projected, now_ms, true);
                }
                if events.len() >= 1000 {
                    let after = self.runtime_event_sequences.get(&key).copied().unwrap_or(0);
                    let session_id = match &key.conversation {
                        ConversationRef::Acp { session_id, .. } => session_id.clone(),
                        _ => String::new(),
                    };
                    if self
                        .send(
                            runtime(
                                RuntimeMethod::Events,
                                json!({"sessionId":session_id,"after":after}),
                                false,
                            ),
                            Purpose::ResyncEvents(key.clone()),
                        )
                        .is_ok()
                    {
                        return;
                    }
                    if let Some(c) = self.state.conversations.get_mut(&key) {
                        c.last_error = Some("Could not continue runtime event recovery".into());
                    }
                    self.resync_errors.insert(
                        key.clone(),
                        "Could not continue runtime event recovery".into(),
                    );
                }
                let execution_id = self
                    .state
                    .conversations
                    .get(&key)
                    .and_then(|c| c.pending_execution_id.clone());
                if let Some(execution_id) = execution_id {
                    let session_id = match &key.conversation {
                        ConversationRef::Acp { session_id, .. } => session_id.clone(),
                        _ => String::new(),
                    };
                    if self
                        .send(
                            runtime(
                                RuntimeMethod::Execution,
                                json!({"sessionId":session_id,"executionId":execution_id}),
                                false,
                            ),
                            Purpose::ResyncExecution(key.clone(), execution_id),
                        )
                        .is_ok()
                    {
                        return;
                    }
                }
                self.finish_runtime_resync(key, now_ms);
            }
            Purpose::ResyncExecution(key, execution_id) => {
                let mut terminal = false;
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    let outcome = value["execution"].as_str().unwrap_or("");
                    if matches!(outcome, "completed" | "failed" | "interrupted") {
                        terminal = true;
                        c.pending_execution_id = None;
                        c.busy = false;
                        c.completion_ready = false;
                        c.last_error = None;
                    } else if value.is_null() {
                        let error = format!(
                            "Execution receipt {execution_id} was not found during event recovery"
                        );
                        self.resync_errors.insert(key.clone(), error.clone());
                        c.last_error = Some(error);
                    }
                }
                if terminal {
                    self.activity_completions.record(key.clone(), &execution_id);
                }
                self.finish_runtime_resync(key, now_ms);
            }
            Purpose::ResyncRead(key) => {
                self.apply_page(key.clone(), value, false, now_ms);
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    if let Some(error) = self.resync_errors.remove(&key) {
                        c.last_error = Some(error);
                        c.stale = true;
                    } else {
                        c.stale = c.last_error.is_some();
                    }
                }
            }
            Purpose::CreateCodex(workspace) => {
                if let Some(thread) = extract_thread(&value) {
                    let _ = self.open_started_codex_thread(workspace, &thread, now_ms);
                }
            }
            Purpose::SlashNew { workspace_id, rest } => {
                if let Some(thread) = extract_thread(&value) {
                    if let Some(key) =
                        self.open_started_codex_thread(workspace_id.clone(), &thread, now_ms)
                    {
                        if !rest.is_empty() {
                            let options = self
                                .state
                                .codex_controls_by_workspace
                                .get(&workspace_id)
                                .map(CodexControls::send_options)
                                .unwrap_or_else(|| json!({}));
                            let prompt = QueuedPrompt {
                                request_id: Uuid::new_v4(),
                                text: rest,
                                content: Vec::new(),
                                options,
                            };
                            if let Err(error) = self.dispatch_prompt(key.clone(), prompt.clone()) {
                                restore_failed_prompt_as_draft(
                                    &mut self.state,
                                    &key,
                                    &prompt,
                                    error,
                                );
                            }
                        }
                    }
                }
            }
            Purpose::SlashFork { key, rest } => {
                if let Some(thread) = extract_thread(&value) {
                    if let Some(thread_id) = thread["id"].as_str().map(str::to_owned) {
                        if let ConversationRef::Codex {
                            workspace_id,
                            thread_id: parent_id,
                        } = &key.conversation
                        {
                            self.state.parents_by_thread.insert(
                                (workspace_id.clone(), thread_id.clone()),
                                parent_id.clone(),
                            );
                            let _ = self.handle(ControllerAction::LoadWorkspaceThreads {
                                workspace_id: workspace_id.clone(),
                            });
                            let _ = self.handle(ControllerAction::SelectCodex {
                                workspace_id: workspace_id.clone(),
                                thread_id: thread_id.clone(),
                            });
                            if !rest.is_empty() {
                                let options = self
                                    .state
                                    .codex_controls_by_workspace
                                    .get(workspace_id)
                                    .map(CodexControls::send_options)
                                    .unwrap_or_else(|| json!({}));
                                let prompt = QueuedPrompt {
                                    request_id: Uuid::new_v4(),
                                    text: rest,
                                    content: Vec::new(),
                                    options,
                                };
                                let prompt_key = self.key(ConversationRef::Codex {
                                    workspace_id: workspace_id.clone(),
                                    thread_id,
                                });
                                if let Err(error) =
                                    self.dispatch_prompt(prompt_key.clone(), prompt.clone())
                                {
                                    restore_failed_prompt_as_draft(
                                        &mut self.state,
                                        &prompt_key,
                                        &prompt,
                                        error,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Purpose::SlashNotice(key, label) => {
                let result = value.get("result").unwrap_or(&value);
                let detail = if label.starts_with("MCP") {
                    format_mcp_summary(result)
                } else if label.starts_with("Apps") {
                    format_apps_summary(result)
                } else {
                    String::new()
                };
                self.set_command_output(&key, if detail.is_empty() { label } else { detail });
            }
            Purpose::CreateAcp(workspace) => {
                if let Some(session_id) = value["sessionId"].as_str() {
                    let store = value["storeId"].as_str().unwrap_or_default();
                    let key = self.key(ConversationRef::Acp {
                        workspace_id: workspace.clone(),
                        store_id: store.into(),
                        session_id: session_id.into(),
                    });
                    mark_fresh_acp_session(&mut self.state, key);
                    let _ = self.handle(ControllerAction::SelectAcp {
                        workspace_id: workspace,
                        store_id: store.into(),
                        session_id: session_id.into(),
                    });
                }
            }
            Purpose::Attach(_catalog) => {
                let _ = self.handle(ControllerAction::RefreshWorkspaces);
                let _ = self.handle(ControllerAction::RefreshCatalog);
                if let Some(workspace) = value["workspaceId"].as_str() {
                    if let Some(session) = value["sessionId"].as_str() {
                        let store = value["storeId"].as_str().unwrap_or_default();
                        let _ = self.handle(ControllerAction::SelectAcp {
                            workspace_id: workspace.into(),
                            store_id: store.into(),
                            session_id: session.into(),
                        });
                    }
                }
            }
            Purpose::Send(key, entry) => {
                let execution_id = value["executionId"].as_str().map(str::to_owned);
                let completed_before_ack = execution_id
                    .as_deref()
                    .is_some_and(|id| self.activity_completions.contains(&key, id))
                    || self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|conversation| conversation.completion_ready);
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.pending_submission = false;
                    c.last_error = None;
                    if completed_before_ack {
                        c.completion_ready = true;
                        c.pending_execution_id = None;
                        c.active_turn_id = None;
                        c.busy = false;
                    }
                    if let Some(execution_id) = execution_id {
                        if completed_before_ack {
                            c.pending_execution_id = None;
                            c.busy = false;
                        } else {
                            c.pending_execution_id = Some(execution_id);
                            c.busy = true;
                        }
                    }
                }
                let resync_pending = self.codex_resync_pending.remove(&key);
                let codex = matches!(key.conversation, ConversationRef::Codex { .. });
                if resync_pending {
                    let _ = self.resume_codex_for_resync(key.clone());
                } else if codex {
                    if completed_before_ack {
                        self.refresh_selected_after_send(key.clone());
                    }
                    // An acknowledged Codex submission is active until its
                    // live completion event triggers the canonical resume.
                } else if self.state.selected.as_ref() == Some(&key) {
                    self.refresh_selected_after_send(key.clone());
                } else {
                    self.drain_queue(key.clone());
                }
                let _ = entry;
            }
            Purpose::Stop(key, permissions_at_cancel) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.busy = false;
                    c.permissions.retain(|permission| {
                        !permissions_at_cancel
                            .iter()
                            .any(|request_id| permission.get("requestId") == Some(request_id))
                    });
                }
            }
            Purpose::Permission(key, request_id) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.last_error = None;
                    remove_permission_request(c, &request_id);
                }
                self.drain_queue(key);
            }
            Purpose::UserInput(key, request_id) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.user_input_requests
                        .retain(|request| request["requestId"] != request_id);
                    c.last_error = None;
                }
                self.drain_queue(key);
            }
            Purpose::Elicitation(workspace, request_id) => {
                if let Some(requests) = self
                    .state
                    .codex_elicitations_by_workspace
                    .get_mut(&workspace)
                {
                    requests.retain(|request| request["requestId"] != request_id);
                }
            }
            Purpose::Configure(key) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.configuration_pending = false;
                    c.last_error = None;
                }
                // `read_conversation` may replay the entire transcript, but the
                // UI needs the agent's authoritative updated configuration here.
                // Refresh only the session snapshot and leave transcript freshness
                // to the normal idle path.
                self.refresh_negotiated_session(key);
            }
            Purpose::Metadata(session) => {
                if let Some(old) = self
                    .state
                    .catalog
                    .iter_mut()
                    .find(|r| r["sessionId"].as_str() == Some(&session))
                {
                    *old = value;
                }
            }
            Purpose::Receipt(key, request_id) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    if value.is_null() {
                        c.last_error = Some(format!("No durable receipt found for {request_id}"));
                    } else {
                        c.uncertain_request = None;
                        c.uncertain_prompt = None;
                        c.pending_execution_id = value["executionId"].as_str().map(str::to_owned);
                        c.last_error = None;
                    }
                }
            }
        }
    }

    fn refresh_selected_after_send(&mut self, key: ConversationKey) {
        match key.conversation.clone() {
            ConversationRef::Acp { session_id, .. } => {
                let _ = self.send(
                    runtime(
                        RuntimeMethod::ReadConversation,
                        json!({"sessionId":session_id,"limit":200}),
                        false,
                    ),
                    Purpose::AcpRead(key),
                );
            }
            ConversationRef::Codex { .. } => {
                let _ = self.resume_codex(key);
            }
        }
    }

    fn apply_page(&mut self, key: ConversationKey, value: Value, append: bool, now_ms: u64) {
        let next_generation = value["generation"].as_str().map(str::to_owned);
        let items = as_array(&value["items"]);
        // While an agent is building a replacement replay, ReadConversation
        // continues to serve the last published generation. If that generation
        // is already fully paged in memory, polling its first page must not
        // truncate the view or start another page walk.
        let preserve_complete_published_generation = !append
            && value["load"]["state"].as_str() == Some("loading")
            && next_generation
                .as_deref()
                .is_some_and(|generation| generation != "live")
            && self
                .state
                .conversations
                .get(&key)
                .is_some_and(|conversation| {
                    !conversation.items.is_empty()
                        && conversation.generation == next_generation
                        && conversation.cursor.is_none()
                        && value
                            .get("nextCursor")
                            .is_some_and(|cursor| !cursor.is_null())
                });
        let begins_staged_refresh = !append
            && !preserve_complete_published_generation
            && !self.history_restart_staging.contains_key(&key)
            && value
                .get("nextCursor")
                .is_some_and(|cursor| !cursor.is_null())
            && self
                .state
                .conversations
                .get(&key)
                .is_some_and(|conversation| !conversation.items.is_empty());
        if begins_staged_refresh {
            if let Some(previous) = self.state.conversations.get(&key) {
                self.history_restart_staging.insert(
                    key.clone(),
                    HistoryRestartStage {
                        items: Vec::new(),
                        previous_generation: previous.generation.clone(),
                        previous_cursor: previous.cursor.clone(),
                        previous_load_state: previous.load_state.clone(),
                        previous_coverage: previous.coverage.clone(),
                        previous_stale: previous.stale,
                    },
                );
            }
        }
        let staged = self.history_restart_staging.contains_key(&key);
        let completed_stage = if staged {
            let complete = {
                let staging = self.history_restart_staging.entry(key.clone()).or_default();
                if append {
                    staging.items.extend(items.iter().cloned());
                } else {
                    staging.items = items.to_vec();
                }
                value.get("nextCursor").is_none_or(Value::is_null)
                    && (value["load"]["state"].as_str() != Some("loading")
                        || staging.previous_generation == next_generation)
            };
            if complete {
                self.history_restart_staging.remove(&key)
            } else {
                None
            }
        } else {
            None
        };
        let Some(c) = self.state.conversations.get_mut(&key) else {
            return;
        };
        let authoritative_changed = !append && c.generation != next_generation;
        if let Some(complete_stage) = completed_stage {
            let authoritative_changed = complete_stage.previous_generation != next_generation;
            c.items = complete_stage.items;
            c.items_revision = c.items_revision.wrapping_add(1);
            if authoritative_changed {
                c.history_revision = c.history_revision.wrapping_add(1);
            }
            self.history_generation_retries.remove(&key);
        } else if staged {
            // Keep the last successful transcript visible until the replacement
            // generation's final page commits.
        } else if append {
            c.items.extend(items.iter().cloned());
            c.items_revision = c.items_revision.wrapping_add(1);
        } else if !preserve_complete_published_generation {
            c.items = items.to_vec();
            c.items_revision = c.items_revision.wrapping_add(1);
            if authoritative_changed {
                c.history_revision = c.history_revision.wrapping_add(1);
            }
        }
        c.generation = next_generation;
        if !preserve_complete_published_generation {
            c.cursor = value.get("nextCursor").filter(|v| !v.is_null()).cloned();
        }
        let keep_loading_notice = c.last_error.as_deref().is_some_and(|message| {
            message.starts_with("Agent history replay is still loading after 60 seconds")
        });
        c.load_state = value["load"]["state"].as_str().map(str::to_owned);
        c.coverage = value["coverage"].as_str().map(str::to_owned);
        c.stale = value["freshness"]["stale"] == true;
        c.read_only = value["readOnly"] == true;
        c.busy = active_after_history_read(c, value["activeExecutionId"].as_str());
        c.loading = c.load_state.as_deref() == Some("loading");
        if c.awaiting_first_turn && !c.busy && !c.items.is_empty() {
            // This also recovers if the completion event was missed but a later
            // read sees the first persisted turn.
            c.awaiting_first_turn = false;
        }
        c.last_refresh_ms = Some(now_ms);
        if let Some(error) = value["load"]["error"].as_str() {
            c.last_error = Some(error.to_owned());
        } else if !c.loading || !keep_loading_notice {
            c.last_error = None;
        }
        let loading = c.loading;
        if loading {
            let attempts = self
                .history_poll_due_ms
                .get(&key)
                .map(|(_, attempts)| *attempts)
                .unwrap_or(0);
            self.history_poll_due_ms
                .insert(key, (now_ms.saturating_add(2_000), attempts));
        } else {
            self.history_poll_due_ms.remove(&key);
        }
    }

    fn should_replay_history(&self, key: &ConversationKey) -> bool {
        self.state
            .conversations
            .get(key)
            .is_some_and(can_replay_history)
    }

    fn probe_capabilities_and_load(&mut self, key: &ConversationKey) -> bool {
        let should_probe = self
            .state
            .conversations
            .get(key)
            .is_some_and(should_probe_capabilities);
        if !should_probe {
            return false;
        }
        if let Some(conversation) = self.state.conversations.get_mut(key) {
            conversation.capability_probe_attempted = true;
        }
        if let Err(error) = self.request_history_load(key.clone()) {
            if let Some(conversation) = self.state.conversations.get_mut(key) {
                conversation.loading = false;
                conversation.last_error = Some(error);
            }
        }
        true
    }

    fn capability_probe_finished_without_session(&self, key: &ConversationKey) -> bool {
        self.state
            .conversations
            .get(key)
            .is_some_and(|conversation| {
                conversation.capability_probe_attempted
                    && !conversation.loading
                    && matches!(
                        conversation.load_state.as_deref(),
                        Some("complete" | "failed")
                    )
                    && conversation
                        .session
                        .as_ref()
                        .is_some_and(|session| session["agentInfo"].is_null())
            })
    }

    fn refresh_negotiated_session(&mut self, key: ConversationKey) {
        let ConversationRef::Acp { session_id, .. } = &key.conversation else {
            return;
        };
        let _ = self.send(
            runtime(
                RuntimeMethod::Session,
                json!({"sessionId":session_id}),
                false,
            ),
            Purpose::NegotiatedSession(key),
        );
    }

    fn request_next_history_page(&mut self, key: ConversationKey) {
        let Some(c) = self.state.conversations.get(&key) else {
            self.pending_reads.remove(&key);
            return;
        };
        let Some(cursor) = c.cursor.clone() else {
            self.pending_reads.remove(&key);
            if c.loading {
                return;
            }
            self.drain_queue(key);
            return;
        };
        let (session_id, generation) = match &key.conversation {
            ConversationRef::Acp { session_id, .. } => (session_id.clone(), c.generation.clone()),
            _ => return,
        };
        if !self.pending_reads.insert(key.clone()) {
            return;
        }
        if let Err(error) = self.send(
            runtime(
                RuntimeMethod::ReadConversation,
                json!({"sessionId":session_id,"generation":generation,"cursor":cursor,"limit":200}),
                false,
            ),
            Purpose::HistoryOlder(key.clone()),
        ) {
            self.pending_reads.remove(&key);
            self.fail_staged_history(&key);
            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                conversation.last_error = Some(error);
            }
        }
    }

    fn restart_acp_history_after_generation_conflict(&mut self, key: ConversationKey) -> bool {
        let retries = self
            .history_generation_retries
            .entry(key.clone())
            .or_default();
        if *retries >= 1 {
            return false;
        }
        *retries += 1;
        let ConversationRef::Acp { session_id, .. } = &key.conversation else {
            return false;
        };
        let session_id = session_id.clone();
        let (
            previous_generation,
            previous_cursor,
            previous_load_state,
            previous_coverage,
            previous_stale,
        ) = self
            .state
            .conversations
            .get(&key)
            .map(|conversation| {
                (
                    conversation.generation.clone(),
                    conversation.cursor.clone(),
                    conversation.load_state.clone(),
                    conversation.coverage.clone(),
                    conversation.stale,
                )
            })
            .unwrap_or_default();
        self.history_restart_staging.insert(
            key.clone(),
            HistoryRestartStage {
                items: Vec::new(),
                previous_generation,
                previous_cursor,
                previous_load_state,
                previous_coverage,
                previous_stale,
            },
        );
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            conversation.loading = true;
            conversation.last_error = None;
        }
        self.pending_reads.insert(key.clone());
        if self
            .send(
                runtime(
                    RuntimeMethod::ReadConversation,
                    json!({"sessionId":session_id,"generation":null,"cursor":null,"limit":200}),
                    false,
                ),
                Purpose::AcpRead(key.clone()),
            )
            .is_ok()
        {
            true
        } else {
            self.pending_reads.remove(&key);
            self.fail_staged_history(&key);
            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                conversation.loading = false;
                conversation.last_error =
                    Some("Could not restart history after its generation changed".into());
            }
            false
        }
    }

    fn fail_staged_history(&mut self, key: &ConversationKey) {
        let Some(staging) = self.history_restart_staging.remove(key) else {
            return;
        };
        if let Some(conversation) = self.state.conversations.get_mut(key) {
            conversation.generation = staging.previous_generation;
            conversation.cursor = staging.previous_cursor;
            conversation.load_state = staging.previous_load_state;
            conversation.coverage = staging.previous_coverage;
            conversation.stale = staging.previous_stale;
        }
    }

    fn poll_loading_histories(&mut self, now_ms: u64) {
        if !self.visible {
            return;
        }
        let due: Vec<_> = self
            .history_poll_due_ms
            .iter()
            .filter(|(key, (at, _))| *at <= now_ms && self.state.selected.as_ref() == Some(key))
            .map(|(key, (at, attempts))| (key.clone(), *at, *attempts))
            .collect();
        for (key, _, attempts) in due {
            if attempts >= 30 {
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.last_error = Some("Agent history replay is still loading after 60 seconds; automatic checks continue.".into());
                }
            }
            if !self.pending_reads.insert(key.clone()) {
                self.history_poll_due_ms
                    .insert(key, (now_ms.saturating_add(2_000), attempts));
                continue;
            }
            let session_id = match &key.conversation {
                ConversationRef::Acp { session_id, .. } => session_id.clone(),
                _ => continue,
            };
            if self
                .send(
                    runtime(
                        RuntimeMethod::ReadConversation,
                        json!({"sessionId":session_id,"limit":200}),
                        false,
                    ),
                    Purpose::AcpRead(key.clone()),
                )
                .is_ok()
            {
                self.history_poll_due_ms.insert(
                    key,
                    (now_ms.saturating_add(2_000), attempts.saturating_add(1)),
                );
            } else {
                self.pending_reads.remove(&key);
                self.history_poll_due_ms
                    .insert(key, (now_ms.saturating_add(2_000), attempts));
            }
        }
    }

    fn drain_queue(&mut self, key: ConversationKey) {
        let workspace_has_elicitation = self.workspace_has_elicitation_for_key(&key);
        let next = self.state.conversations.get_mut(&key).and_then(|c| {
            if c.busy
                || c.loading
                || c.read_only
                || !c.permissions.is_empty()
                || !c.user_input_requests.is_empty()
                || workspace_has_elicitation
                || c.pending_submission
                || c.completion_ready
                || c.uncertain_request.is_some()
            {
                None
            } else {
                c.queue.pop_front()
            }
        });
        if let Some(entry) = next {
            if let Err(error) = self.dispatch_prompt(key.clone(), entry.clone()) {
                restore_failed_queued_prompt(&mut self.state, &key, entry, error);
            }
        }
    }

    fn selected_workspace_has_elicitation(&self) -> bool {
        self.state
            .selected
            .as_ref()
            .is_some_and(|key| self.workspace_has_elicitation_for_key(key))
    }

    fn workspace_has_elicitation_for_key(&self, key: &ConversationKey) -> bool {
        let ConversationRef::Codex { workspace_id, .. } = &key.conversation else {
            return false;
        };
        self.state
            .codex_elicitations_by_workspace
            .get(workspace_id)
            .is_some_and(|requests| !requests.is_empty())
    }

    fn apply_history_read_error(
        &mut self,
        key: ConversationKey,
        message: String,
        now_ms: u64,
        owns_loading_state: bool,
    ) {
        self.pending_reads.remove(&key);
        if message.to_ascii_lowercase().contains("generation changed")
            && self.restart_acp_history_after_generation_conflict(key.clone())
        {
            return;
        }
        self.fail_staged_history(&key);
        self.history_generation_retries.remove(&key);
        if let Some(conversation) = self.state.conversations.get_mut(&key) {
            if owns_loading_state {
                conversation.loading = false;
            }
            conversation.busy = active_after_history_read(conversation, None);
            conversation.last_refresh_ms = Some(now_ms);
            conversation.last_error = Some(message);
        }
    }

    fn apply_error(&mut self, purpose: Purpose, error: BackendError, now_ms: u64) {
        let message = error.message.clone();
        match purpose {
            Purpose::Discovery(profile) => {
                let src = self.state.discovery.entry(profile).or_default();
                src.loading = false;
                src.error = Some(message.clone());
            }
            Purpose::Send(key, entry) => {
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.pending_submission = false;
                    c.busy = active_after_history_read(c, None);
                    c.uncertain_prompt = Some(entry.clone());
                    if error.code == "transport_lost" {
                        c.uncertain_request = Some(entry.request_id);
                        c.last_error = Some(format!(
                            "Submission outcome is uncertain. Check receipt {} before sending again. {message}",
                            entry.request_id
                        ));
                    } else {
                        c.uncertain_request = None;
                        if c.draft.is_empty() {
                            c.draft = entry.text;
                        }
                        c.last_error = Some(message);
                    }
                }
                if self.codex_resync_pending.contains(&key)
                    && self
                        .state
                        .conversations
                        .get(&key)
                        .is_some_and(|c| !c.pending_submission)
                {
                    self.codex_resync_pending.remove(&key);
                    let _ = self.resume_codex_for_resync(key);
                }
            }
            Purpose::Configure(key) => {
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.configuration_pending = false;
                    conversation.last_error = Some(message);
                }
            }
            Purpose::Resume(key) => {
                let resync_failed = self.codex_resync_inflight.remove(&key);
                let first_turn_not_persisted =
                    message.to_ascii_lowercase().contains("no rollout found")
                        && self
                            .state
                            .conversations
                            .get(&key)
                            .is_some_and(|conversation| {
                                conversation.awaiting_first_turn
                                    && !conversation.pending_submission
                                    && !conversation.busy
                            });
                let can_continue_queue =
                    if let Some(conversation) = self.state.conversations.get_mut(&key) {
                        conversation.loading = false;
                        conversation.last_refresh_ms = Some(now_ms);
                        if resync_failed {
                            conversation.stale = true;
                        }
                        if first_turn_not_persisted {
                            conversation.last_error = None;
                        } else {
                            conversation.last_error = Some(message);
                        }
                        if conversation.completion_ready && !conversation.pending_submission {
                            conversation.completion_ready = false;
                            conversation.pending_execution_id = None;
                            conversation.active_turn_id = None;
                            conversation.busy = false;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                self.resume_live_overlays.remove(&key);
                if can_continue_queue {
                    self.drain_queue(key);
                }
            }
            Purpose::ConnectForResume(key) => {
                if let Some(conversation) = self.state.conversations.get_mut(&key) {
                    conversation.loading = false;
                    conversation.last_refresh_ms = Some(now_ms);
                    conversation.last_error = Some(message);
                }
            }
            Purpose::AcpRead(key) => {
                self.apply_history_read_error(key, message, now_ms, true);
            }
            Purpose::HistoryOlder(key) => {
                // A failed page request must not hide a separate history replay
                // that is still loading. The older-page request does not own
                // the replay's loading state.
                self.apply_history_read_error(key, message, now_ms, false);
            }
            Purpose::ResyncEvents(key) | Purpose::ResyncExecution(key, _) => {
                self.resync_errors.insert(key.clone(), message.clone());
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.stale = true;
                    c.last_error = Some(message);
                }
                self.finish_runtime_resync(key, now_ms);
            }
            Purpose::ResyncRead(key) => {
                self.resync_errors.remove(&key);
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.stale = true;
                    c.last_error = Some(message);
                }
            }
            Purpose::AcpHistory(key)
            | Purpose::AcpInitialRead(key)
            | Purpose::AcpSession(key)
            | Purpose::NegotiatedSession(key)
            | Purpose::Stop(key, _)
            | Purpose::Permission(key, _)
            | Purpose::UserInput(key, _)
            | Purpose::SlashNotice(key, _)
            | Purpose::Receipt(key, _) => {
                self.pending_reads.remove(&key);
                self.history_poll_due_ms.remove(&key);
                if let Some(c) = self.state.conversations.get_mut(&key) {
                    c.loading = false;
                    c.busy = active_after_history_read(c, None);
                    c.last_refresh_ms = Some(now_ms);
                    c.last_error = Some(message);
                }
            }
            Purpose::Elicitation(workspace, request_id) => {
                self.state.error = Some(message);
                // Keep the request visible so the user can choose another response.
                let _ = (workspace, request_id);
            }
            Purpose::Threads { workspace_id, .. }
            | Purpose::ConnectForThreads { workspace_id, .. } => {
                self.state.thread_loading_by_workspace.remove(&workspace_id);
                self.state.error = Some(message);
            }
            Purpose::CodexModels(_) | Purpose::CodexModes(_) | Purpose::CodexConfiguredModel(_) => {
                self.state.error = Some(message)
            }
            Purpose::OpenedById => self.state.error = Some(message),
            Purpose::Workspaces
            | Purpose::Profiles
            | Purpose::Catalog
            | Purpose::ConnectForNew(_)
            | Purpose::CreateCodex(_)
            | Purpose::SlashNew { .. }
            | Purpose::SlashFork { .. }
            | Purpose::CreateAcp(_)
            | Purpose::Attach(_)
            | Purpose::Metadata(_)
            | Purpose::Initialize => self.state.error = Some(message),
        }
    }

    fn apply_event(&mut self, payload: Value, now_ms: u64) {
        self.apply_event_inner(payload, now_ms, false);
    }

    fn apply_event_inner(&mut self, payload: Value, now_ms: u64, from_resync: bool) {
        let kind = payload["kind"].as_str().unwrap_or("");
        if !runtime_event_projects_to_acp(&payload) {
            return;
        }
        let Some(store) = payload["storeId"].as_str() else {
            return;
        };
        let Some(session) = payload["sessionId"].as_str() else {
            return;
        };
        let workspace =
            runtime_event_workspace(&self.state, store, session, payload["workspaceId"].as_str());
        let Some(workspace) = workspace else {
            return;
        };
        let key = self.key(ConversationRef::Acp {
            workspace_id: workspace,
            store_id: store.into(),
            session_id: session.into(),
        });
        let sequence = payload["sequence"].as_u64();
        if let Some(sequence) = sequence {
            if sequence <= self.runtime_event_sequences.get(&key).copied().unwrap_or(0) {
                return;
            }
        }
        if !from_resync && payload["source"] == "live" && self.resyncing_runtime.contains(&key) {
            let buffer = self.resync_live_buffer.entry(key).or_default();
            if sequence
                .is_none_or(|sequence| !buffer.iter().any(|event| event["sequence"] == sequence))
            {
                buffer.push(payload);
            }
            return;
        }
        if let Some(sequence) = sequence {
            self.runtime_event_sequences
                .entry(key.clone())
                .and_modify(|cursor| *cursor = (*cursor).max(sequence))
                .or_insert(sequence);
        }
        if payload["source"] == "history"
            && !matches!(kind, "history_loaded" | "history_load_failed")
        {
            return;
        }
        if from_resync && matches!(kind, "history_loaded" | "history_load_failed") {
            return;
        }
        self.activity_completions
            .record_runtime_event(key.clone(), &payload);
        if matches!(kind, "history_loaded" | "history_load_failed") {
            self.history_poll_due_ms.remove(&key);
            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                conversation.loading = false;
                if kind == "history_load_failed" {
                    conversation.last_error = payload["error"]
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| Some("Agent history replay failed".into()));
                }
            }
            if self.pending_reads.insert(key.clone()) {
                let _ = self.send(
                    runtime(
                        RuntimeMethod::ReadConversation,
                        json!({"sessionId":session,"limit":200}),
                        false,
                    ),
                    Purpose::AcpRead(key),
                );
            }
            return;
        }
        let (can_read, can_drain, replay_after_completion) = {
            let c = self.state.conversations.entry(key.clone()).or_default();
            if self.state.selected.as_ref() != Some(&key) {
                c.unread = true;
            }
            let replay_after_completion = kind == "execution_completed"
                && !self.resyncing_runtime.contains(&key)
                && !c.read_only
                && !c.config_popup_open
                && c.permissions.is_empty()
                && c.user_input_requests.is_empty()
                && c.session
                    .as_ref()
                    .is_some_and(|session| is_advertised(&session["capabilities"]["loadSession"]));
            match kind {
                "execution_submitted" | "message_delta" | "tool_patch" => {
                    c.busy = true;
                }
                "execution_completed" | "disconnected" => {
                    c.busy = false;
                    c.pending_submission = false;
                    c.pending_execution_id = None;
                }
                "permission" => {
                    if let Some(p) = payload.get("payload") {
                        c.permissions.push(p.clone());
                    }
                }
                "permission_settled" => {
                    let request_id = payload.get("payload").and_then(|p| p.get("requestId"));
                    if let Some(request_id) = request_id {
                        c.permissions
                            .retain(|permission| permission.get("requestId") != Some(request_id));
                    }
                }
                "session_info_update" => {
                    let _ = now_ms;
                }
                _ => (),
            }
            if kind == "execution_completed" {
                c.awaiting_first_turn = false;
            }
            let can_read = self.state.selected.as_ref() == Some(&key)
                && matches!(
                    payload["kind"].as_str(),
                    Some(
                        "message_delta"
                            | "tool_patch"
                            | "execution_completed"
                            | "session_info_update"
                    )
                )
                && !self.resyncing_runtime.contains(&key)
                && !c.config_popup_open
                && !replay_after_completion;
            (
                can_read,
                !self.resyncing_runtime.contains(&key) && !c.busy && c.permissions.is_empty(),
                replay_after_completion,
            )
        };
        if replay_after_completion && self.state.selected.as_ref() == Some(&key) {
            let _ = self.request_history_load(key.clone());
        }
        if can_read {
            let recently_read = self
                .last_event_read_ms
                .get(&key)
                .is_some_and(|last| now_ms.saturating_sub(*last) < 300);
            if !recently_read && self.pending_reads.insert(key.clone()) {
                self.last_event_read_ms.insert(key.clone(), now_ms);
                let session_id = session.to_owned();
                let _ = self.send(
                    runtime(
                        RuntimeMethod::ReadConversation,
                        json!({"sessionId":session_id,"limit":200}),
                        false,
                    ),
                    Purpose::AcpRead(key.clone()),
                );
            }
        }
        if can_drain {
            self.drain_queue(key);
        }
    }

    fn apply_codex_event(&mut self, payload: Value) {
        let workspace = payload["workspaceId"].as_str().unwrap_or_default();
        let message = payload.get("message").unwrap_or(&payload);
        let params = message.get("params").unwrap_or(message);
        let method = message["method"].as_str().unwrap_or_default();
        if capture_codex_elicitation(&mut self.state, workspace, message) {
            return;
        }
        if method == "serverRequest/resolved" {
            if let Some(request_id) = params.get("requestId") {
                if let Some(requests) = self
                    .state
                    .codex_elicitations_by_workspace
                    .get_mut(workspace)
                {
                    requests.retain(|request| request["requestId"] != *request_id);
                }
                if !self
                    .state
                    .codex_elicitations_by_workspace
                    .get(workspace)
                    .is_some_and(|requests| !requests.is_empty())
                {
                    let keys: Vec<_> = self.state.conversations.keys().filter(|key| {
                        matches!(&key.conversation, ConversationRef::Codex { workspace_id, .. } if workspace_id.as_str() == workspace)
                    }).cloned().collect();
                    for key in keys {
                        self.drain_queue(key);
                    }
                }
            }
        }
        let thread_id = params["threadId"]
            .as_str()
            .or(params["thread_id"].as_str())
            .or(params.pointer("/thread/id").and_then(Value::as_str))
            .or(params.pointer("/turn/threadId").and_then(Value::as_str))
            .or(params.pointer("/turn/thread_id").and_then(Value::as_str))
            .unwrap_or_default();
        if thread_id.is_empty() {
            return;
        }
        let key = self.key(ConversationRef::Codex {
            workspace_id: workspace.into(),
            thread_id: thread_id.into(),
        });
        self.state
            .conversations
            .entry(key.clone())
            .or_insert_with(|| ConversationState {
                key: Some(key.clone()),
                ..ConversationState::default()
            });
        let is_selected = self.state.selected.as_ref() == Some(&key);
        if !is_selected {
            if let Some(conversation) = self.state.conversations.get_mut(&key) {
                conversation.unread = true;
            }
        }
        if let ConversationRef::Codex {
            workspace_id,
            thread_id,
        } = &key.conversation
        {
            if let Some(summary) = self
                .state
                .threads_by_workspace
                .get_mut(workspace_id)
                .and_then(|rows| rows.iter_mut().find(|row| row.id == *thread_id))
            {
                match method {
                    "turn/started" => summary.processing = true,
                    "turn/completed" => summary.processing = false,
                    _ => (),
                }
                if !is_selected {
                    summary.unread = true;
                }
            }
        }
        let active_turn_id = self
            .state
            .conversations
            .get(&key)
            .and_then(|conversation| conversation.active_turn_id.as_deref());
        self.activity_completions
            .record_codex_event(key.clone(), method, params, active_turn_id);
        if method == "item/tool/requestUserInput" {
            let request_id = message.get("id").cloned().unwrap_or(Value::Null);
            let conversation = self.state.conversations.entry(key.clone()).or_default();
            if !conversation
                .user_input_requests
                .iter()
                .any(|request| request["requestId"] == request_id)
            {
                conversation
                    .user_input_requests
                    .push(json!({"requestId":request_id,"params":params}));
            }
            conversation.busy = true;
        }
        let mut item_changed = false;
        if let Some(c) = self.state.conversations.get_mut(&key) {
            match method {
                "turn/started" => {
                    c.busy = true;
                    c.completion_ready = false;
                    c.active_turn_id = params["turn"]["id"].as_str().map(str::to_owned);
                }
                "turn/completed" => {
                    c.busy = false;
                    c.active_turn_id = None;
                    c.pending_execution_id = None;
                    c.completion_ready = true;
                    c.awaiting_first_turn = false;
                }
                "item/agentMessage/delta"
                | "item/reasoning/summaryTextDelta"
                | "item/reasoning/textDelta"
                | "item/plan/delta"
                | "item/commandExecution/outputDelta"
                | "item/fileChange/outputDelta"
                | "item/mcpToolCall/progress"
                | "item/commandExecution/terminalInteraction"
                | "item/reasoning/summaryPartAdded"
                | "item/started"
                | "item/completed"
                | "item/fileChange/patchUpdated"
                | "turn/plan/updated"
                | "turn/diff/updated"
                | "enteredReviewMode"
                | "exitedReviewMode" => {
                    item_changed = apply_codex_item_event(&mut c.items, method, params);
                    if matches!(
                        method,
                        "item/agentMessage/delta" | "item/started" | "item/completed"
                    ) {
                        c.busy = true;
                    }
                }
                "thread/name/updated" => {
                    c.title = params["threadName"]
                        .as_str()
                        .or_else(|| params["thread_name"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                }
                _ => (),
            }
            if item_changed {
                c.items_revision = c.items_revision.wrapping_add(1);
            }
        }
        if item_changed
            && (self.pending.values().any(
                |purpose| matches!(purpose, Purpose::Resume(pending_key) if pending_key == &key),
            ) || self.codex_resync_pending.contains(&key)
                || self.codex_resync_inflight.contains(&key))
        {
            if let Some(id) = codex_live_event_item_id(method, params) {
                if self
                    .state
                    .conversations
                    .get(&key)
                    .is_some_and(|conversation| {
                        conversation.items.iter().any(|item| item["id"] == id)
                    })
                {
                    self.resume_live_overlays
                        .entry(key.clone())
                        .or_default()
                        .push((method.to_owned(), params.clone()));
                }
            }
        }
        if method == "turn/completed" {
            let should_resume = self
                .state
                .conversations
                .get(&key)
                .is_some_and(|conversation| {
                    !conversation.pending_submission
                        && (is_selected || !conversation.queue.is_empty())
                });
            let refresh_pending = self.pending.values().any(
                |purpose| matches!(purpose, Purpose::Resume(pending_key) if pending_key == &key),
            );
            if should_resume && !refresh_pending {
                // A completed turn has its final item payloads in the event stream,
                // but a resume gives the main view the server's canonical complete
                // history snapshot and metadata. It replaces/upserts by native IDs.
                let _ = self.resume_codex(key);
            }
        }
    }
}

fn runtime(method: RuntimeMethod, params: Value, mutation: bool) -> BackendRequest {
    BackendRequest::Runtime(if mutation {
        RuntimeRequest::mutation(method, params, None)
    } else {
        RuntimeRequest::read(method, params)
    })
}
fn metadata_params(session_id: String, expected_revision: i64, patch: Value) -> Value {
    let mut p = patch.as_object().cloned().unwrap_or_default();
    p.insert("sessionId".into(), json!(session_id));
    p.insert("expectedRevision".into(), json!(expected_revision));
    Value::Object(p)
}
fn as_array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn runtime_event_workspace(
    state: &ControllerState,
    store_id: &str,
    session_id: &str,
    workspace_hint: Option<&str>,
) -> Option<String> {
    workspace_hint
        .filter(|workspace| !workspace.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            state
                .conversations
                .keys()
                .find_map(|key| match &key.conversation {
                    ConversationRef::Acp {
                        workspace_id,
                        store_id: known_store,
                        session_id: known_session,
                    } if known_store == store_id && known_session == session_id => {
                        Some(workspace_id.clone())
                    }
                    _ => None,
                })
        })
        .or_else(|| {
            state
                .catalog
                .iter()
                .find(|row| {
                    row["storeId"].as_str() == Some(store_id)
                        && row["sessionId"].as_str() == Some(session_id)
                })
                .and_then(|row| row["workspaceId"].as_str().map(str::to_owned))
        })
}
fn is_advertised(value: &Value) -> bool {
    value.as_bool() == Some(true)
        || value.is_object()
        || value.as_array().is_some_and(|values| !values.is_empty())
}

fn runtime_event_projects_to_acp(event: &Value) -> bool {
    event["runtime"]["kind"].as_str() != Some("codex")
}

fn can_replay_history(conversation: &ConversationState) -> bool {
    !conversation.awaiting_first_turn
        && !conversation.read_only
        && conversation
            .session
            .as_ref()
            .is_some_and(|session| is_advertised(&session["capabilities"]["loadSession"]))
}

fn should_probe_capabilities(conversation: &ConversationState) -> bool {
    !conversation.capability_probe_attempted
        && !conversation.read_only
        && !conversation.awaiting_first_turn
        && !conversation.loading
        && !conversation.busy
        && conversation
            .session
            .as_ref()
            .is_some_and(|session| session["agentInfo"].is_null())
}

fn send_wait_message(conversation: &ConversationState) -> Option<&'static str> {
    conversation
        .loading
        .then_some("Conversation history is still loading; wait until it finishes before sending.")
}

fn configuration_available(session: &Value) -> bool {
    is_advertised(&session["capabilities"]["configuration"])
        || ["configOptions", "modes", "models"].iter().any(|key| {
            session["configuration"]
                .get(*key)
                .is_some_and(|value| !value.is_null())
        })
}

fn remove_permission_request(conversation: &mut ConversationState, request_id: &Value) {
    conversation
        .permissions
        .retain(|request| request.get("requestId") != Some(request_id));
}

fn codex_images_from_content(
    existing_images: &Value,
    content: &[Value],
) -> Result<Vec<String>, String> {
    let mut images: Vec<String> = existing_images
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    for item in content {
        if item["type"].as_str() != Some("image") {
            return Err("Codex accepts image attachments only".into());
        }
        let mime = item["mimeType"]
            .as_str()
            .filter(|mime| mime.starts_with("image/"))
            .ok_or("Codex image attachment is missing an image MIME type")?;
        let data = item["data"]
            .as_str()
            .ok_or("Codex image attachment is missing its data")?;
        if data.starts_with("data:") {
            images.push(data.to_owned());
        } else {
            images.push(format!("data:{mime};base64,{data}"));
        }
    }
    Ok(images)
}

fn prompt_request(
    key: &ConversationKey,
    entry: &QueuedPrompt,
    active_turn: Option<String>,
) -> Result<BackendRequest, String> {
    match &key.conversation {
        ConversationRef::Codex {
            workspace_id,
            thread_id,
        } => {
            let images = codex_images_from_content(&entry.options["images"], &entry.content)?;
            if let Some(turn_id) = active_turn {
                Ok(BackendRequest::Codex(CodexRequest::mutation(
                    "turn_steer",
                    json!({"workspaceId":workspace_id,"threadId":thread_id,"turnId":turn_id,"text":entry.text,"images":images,"appMentions":entry.options["appMentions"]}),
                    Some(entry.request_id),
                )))
            } else {
                Ok(BackendRequest::Codex(CodexRequest::mutation(
                    "send_user_message",
                    json!({"workspaceId":workspace_id,"threadId":thread_id,"text":entry.text,"model":entry.options["model"],"effort":entry.options["effort"],"accessMode":entry.options["accessMode"],"serviceTier":entry.options["serviceTier"],"images":images,"appMentions":entry.options["appMentions"],"collaborationMode":entry.options["collaborationMode"]}),
                    Some(entry.request_id),
                )))
            }
        }
        ConversationRef::Acp { session_id, .. } => {
            let mut prompt = vec![json!({"type":"text","text":entry.text})];
            prompt.extend(entry.content.iter().cloned());
            Ok(BackendRequest::Runtime(RuntimeRequest::mutation(
                RuntimeMethod::Submit,
                json!({"requestId":entry.request_id,"sessionId":session_id,"prompt":prompt,"options":entry.options}),
                Some(entry.request_id),
            )))
        }
    }
}

fn mark_fresh_acp_session(state: &mut ControllerState, key: ConversationKey) {
    state.select(key.clone());
    if let Some(conversation) = state.conversations.get_mut(&key) {
        conversation.awaiting_first_turn = true;
    }
}
fn extract_thread(value: &Value) -> Option<Value> {
    value
        .get("thread")
        .cloned()
        .or_else(|| value.get("result").and_then(|r| r.get("thread")).cloned())
        .or_else(|| value.get("data").and_then(|r| r.get("thread")).cloned())
}
fn project_codex_thread_items(thread: &Value) -> Vec<Value> {
    let mut items = Vec::new();
    for (turn_index, turn) in as_array(&thread["turns"]).iter().enumerate() {
        let turn_id = turn["id"].as_str().unwrap_or_default();
        for (item_index, item) in as_array(&turn["items"]).iter().enumerate() {
            let fallback_id = format!("codex:{turn_index}:{turn_id}:{item_index}");
            upsert_codex_item(&mut items, project_codex_item(item, &fallback_id));
        }
    }
    items
}

fn project_codex_item(item: &Value, fallback_id: &str) -> Value {
    let id = item["id"].as_str().unwrap_or(fallback_id);
    let item_type = item["type"].as_str().unwrap_or("unknown");
    let mut projected = match item_type {
        "agentMessage" | "userMessage" => {
            let content = as_array(&item["content"]);
            let text = item["text"].as_str().map(str::to_owned).unwrap_or_else(|| {
                content
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            });
            let images: Vec<_> = content
                .iter()
                .filter(|block| matches!(block["type"].as_str(), Some("image" | "localImage")))
                .filter_map(|block| {
                    ["url", "path", "value", "data", "source"]
                        .iter()
                        .find_map(|field| block[field].as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                })
                .collect();
            let mut dto = json!({
                "id":id,
                "kind":"message",
                "role":if item_type == "userMessage" {"user"} else {"assistant"},
                "text":text
            });
            if item.get("content").is_some() {
                dto["content"] = item["content"].clone();
            }
            if !images.is_empty() {
                dto["images"] = json!(images);
            }
            dto
        }
        "reasoning" => json!({
            "id":id,
            "kind":"reasoning",
            "summary":codex_text(&item["summary"]),
            "content":codex_text(&item["content"])
        }),
        "plan" => json!({
            "id":id,
            "kind":"tool",
            "toolType":"plan",
            "title":"Plan",
            "detail":item["status"].as_str().unwrap_or_default(),
            "status":item["status"].as_str().unwrap_or_default(),
            "output":item["text"].as_str().unwrap_or_default()
        }),
        "commandExecution" => {
            let command = if let Some(parts) = item["command"].as_array() {
                parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                item["command"].as_str().unwrap_or_default().to_owned()
            };
            json!({
                "id":id,"kind":"tool","toolType":item_type,
                "title":if command.is_empty() {String::from("Command")} else {format!("Command: {command}")},
                "detail":item["cwd"].as_str().unwrap_or_default(),
                "status":item["status"].as_str().unwrap_or_default(),
                "output":item["aggregatedOutput"].as_str().unwrap_or_default(),
                "durationMs":item.get("durationMs").or_else(||item.get("duration_ms")).cloned().unwrap_or(Value::Null)
            })
        }
        "fileChange" => {
            let changes: Vec<_> = as_array(&item["changes"])
                .iter()
                .filter_map(|change| {
                    let path = change["path"].as_str()?;
                    let kind = change["kind"]
                        .as_str()
                        .or_else(|| change["kind"]["type"].as_str())
                        .map(str::to_lowercase);
                    Some(json!({"path":path,"kind":kind,"diff":change["diff"]}))
                })
                .collect();
            let output = changes
                .iter()
                .filter_map(|change| change["diff"].as_str())
                .filter(|diff| !diff.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let paths = changes
                .iter()
                .filter_map(|change| change["path"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            json!({
                "id":id,"kind":"tool","toolType":item_type,"title":"File changes",
                "detail":if paths.is_empty() {String::from("Pending changes")} else {paths},
                "status":item["status"].as_str().unwrap_or_default(),"output":output,"changes":changes
            })
        }
        "mcpToolCall" => json!({
            "id":id,"kind":"tool","toolType":item_type,
            "title":format!("Tool: {}{}",item["server"].as_str().unwrap_or_default(),
                item["tool"].as_str().map(|tool|format!(" / {tool}")).unwrap_or_default()),
            "detail":codex_json(&item["arguments"]),
            "status":item["status"].as_str().unwrap_or_default(),
            "output":codex_display_value(item.get("result").or_else(||item.get("error")))
        }),
        "webSearch" => json!({
            "id":id,"kind":"tool","toolType":item_type,"title":"Web search",
            "detail":item["query"].as_str().unwrap_or_default(),
            "status":item["status"].as_str().unwrap_or("completed"),"output":""
        }),
        "imageView" => json!({
            "id":id,"kind":"tool","toolType":item_type,"title":"Image view",
            "detail":item["path"].as_str().unwrap_or_default(),"status":"","output":""
        }),
        "contextCompaction" => json!({
            "id":id,"kind":"tool","toolType":item_type,"title":"Context compaction",
            "detail":"Compacting conversation context to fit token limits.",
            "status":item["status"].as_str().unwrap_or("completed"),"output":""
        }),
        "enteredReviewMode" | "exitedReviewMode" => json!({
            "id":id,"kind":"review",
            "state":if item_type == "enteredReviewMode" {"started"} else {"completed"},
            "text":item["review"].as_str().unwrap_or_default()
        }),
        _ => json!({
            "id":id,"kind":"tool","toolType":item_type,"title":item_type,
            "detail":"","status":item["status"].as_str().unwrap_or_default(),
            "output":codex_json(item)
        }),
    };
    // Keep original protocol fields available for future item types and debugging.
    projected["native"] = item.clone();
    projected
}

fn apply_codex_item_event(items: &mut Vec<Value>, method: &str, params: &Value) -> bool {
    match method {
        "item/started" | "item/completed" => {
            let Some(item) = params.get("item") else {
                return false;
            };
            let fallback = format!("codex-live:{}", items.len());
            upsert_codex_item(items, project_codex_item(item, &fallback));
            true
        }
        "item/agentMessage/delta" => append_codex_text(
            items,
            params["itemId"].as_str().unwrap_or_default(),
            "message",
            "text",
            &params["delta"],
            "assistant",
            "Agent message",
        ),
        "item/reasoning/summaryTextDelta" => append_codex_text(
            items,
            params["itemId"].as_str().unwrap_or_default(),
            "reasoning",
            "summary",
            &params["delta"],
            "",
            "Reasoning",
        ),
        "item/reasoning/textDelta" => append_codex_text(
            items,
            params["itemId"].as_str().unwrap_or_default(),
            "reasoning",
            "content",
            &params["delta"],
            "",
            "Reasoning",
        ),
        "item/reasoning/summaryPartAdded" => {
            let id = params["itemId"].as_str().unwrap_or_default();
            let Some(item) = items
                .iter_mut()
                .find(|item| item["id"] == id && item["kind"] == "reasoning")
            else {
                upsert_codex_item(
                    items,
                    json!({"id":id,"kind":"reasoning","summary":"","content":""}),
                );
                return true;
            };
            let summary = item["summary"].as_str().unwrap_or_default();
            if !summary.is_empty() && !summary.ends_with('\n') {
                item["summary"] = json!(format!("{summary}\n"));
            }
            true
        }
        "item/plan/delta" => append_codex_text(
            items,
            params["itemId"].as_str().unwrap_or_default(),
            "tool",
            "output",
            &params["delta"],
            "plan",
            "Plan",
        ),
        "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/mcpToolCall/progress" => {
            let id = params["itemId"].as_str().unwrap_or_default();
            let delta = if method == "item/mcpToolCall/progress" {
                params["message"]
                    .as_str()
                    .map(|text| format!("{text}\n"))
                    .unwrap_or_default()
            } else {
                params["delta"].as_str().unwrap_or_default().to_owned()
            };
            append_codex_text(
                items,
                id,
                "tool",
                "output",
                &json!(delta),
                "tool",
                "Tool activity",
            )
        }
        "item/commandExecution/terminalInteraction" => {
            let stdin = params["stdin"].as_str().unwrap_or_default();
            if stdin.is_empty() {
                return false;
            }
            let normalized = stdin.replace("\r\n", "\n");
            let suffix = if normalized.ends_with('\n') { "" } else { "\n" };
            append_codex_text(
                items,
                params["itemId"].as_str().unwrap_or_default(),
                "tool",
                "output",
                &json!(format!("\n[stdin]\n{normalized}{suffix}")),
                "tool",
                "Tool activity",
            )
        }
        "item/fileChange/patchUpdated" => {
            let id = params["itemId"].as_str().unwrap_or_default();
            let item = json!({"id":id,"type":"fileChange","status":"inProgress","changes":params["changes"]});
            upsert_codex_item(items, project_codex_item(&item, id));
            true
        }
        "turn/plan/updated" => {
            let turn_id = params["turnId"].as_str().unwrap_or("current");
            let id = format!("codex-plan:{turn_id}");
            let mut lines = Vec::new();
            if let Some(explanation) = params["explanation"]
                .as_str()
                .filter(|text| !text.is_empty())
            {
                lines.push(explanation.to_owned());
            }
            for step in as_array(&params["plan"]) {
                let text = step["step"]
                    .as_str()
                    .or_else(|| step["text"].as_str())
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let status = step["status"].as_str().unwrap_or("pending");
                let mark = if status == "completed" { "x" } else { " " };
                lines.push(format!("- [{mark}] {text}"));
            }
            upsert_codex_item(
                items,
                json!({
                    "id":id,"kind":"tool","toolType":"plan","title":"Plan",
                    "detail":"","status":"inProgress","output":lines.join("\n"),"native":params
                }),
            );
            true
        }
        "turn/diff/updated" => {
            let turn_id = params["turnId"].as_str().unwrap_or("current");
            let id = format!("codex-diff:{turn_id}");
            upsert_codex_item(
                items,
                json!({
                    "id":id,"kind":"diff","title":"Turn changes",
                    "diff":params["diff"].as_str().unwrap_or_default(),"native":params
                }),
            );
            true
        }
        "enteredReviewMode" | "exitedReviewMode" => {
            let fallback = format!("codex-review:{}", items.len());
            upsert_codex_item(items, project_codex_item(params, &fallback));
            true
        }
        _ => false,
    }
}

fn codex_live_event_item_id(method: &str, params: &Value) -> Option<String> {
    match method {
        "item/started" | "item/completed" => params["item"]["id"].as_str().map(str::to_owned),
        "item/agentMessage/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/textDelta"
        | "item/plan/delta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/mcpToolCall/progress"
        | "item/commandExecution/terminalInteraction"
        | "item/reasoning/summaryPartAdded"
        | "item/fileChange/patchUpdated" => params["itemId"].as_str().map(str::to_owned),
        "turn/plan/updated" => Some(format!(
            "codex-plan:{}",
            params["turnId"].as_str().unwrap_or("current")
        )),
        "turn/diff/updated" => Some(format!(
            "codex-diff:{}",
            params["turnId"].as_str().unwrap_or("current")
        )),
        "enteredReviewMode" | "exitedReviewMode" => params["id"].as_str().map(str::to_owned),
        _ => None,
    }
}

fn append_codex_text(
    items: &mut Vec<Value>,
    id: &str,
    kind: &str,
    field: &str,
    delta: &Value,
    tool_type: &str,
    title: &str,
) -> bool {
    if id.is_empty() {
        return false;
    }
    let addition = delta.as_str().unwrap_or_default();
    if let Some(item) = items
        .iter_mut()
        .find(|item| item["id"] == id && item["kind"] == kind)
    {
        let previous = item[field].as_str().unwrap_or_default();
        item[field] = json!(format!("{previous}{addition}"));
    } else if kind == "message" {
        items.push(json!({"id":id,"kind":"message","role":"assistant","text":addition}));
    } else if kind == "reasoning" {
        let mut item = json!({"id":id,"kind":"reasoning","summary":"","content":""});
        item[field] = json!(addition);
        items.push(item);
    } else {
        let mut item = json!({"id":id,"kind":"tool","toolType":tool_type,"title":title,"detail":"","status":"inProgress","output":""});
        item[field] = json!(addition);
        items.push(item);
    }
    true
}

fn upsert_codex_item(items: &mut Vec<Value>, item: Value) {
    if let Some(existing) = items
        .iter_mut()
        .find(|existing| existing["id"] == item["id"])
    {
        *existing = item;
    } else {
        items.push(item);
    }
}

fn codex_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                part.as_str()
                    .or_else(|| part["text"].as_str())
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn codex_json(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }
}

fn codex_display_value(value: Option<&Value>) -> String {
    value
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| codex_json(value))
        })
        .unwrap_or_default()
}
fn is_processing(thread: &Value) -> bool {
    thread["status"]["type"]
        .as_str()
        .is_some_and(|s| s == "inProgress" || s == "running")
}
fn active_turn_id(thread: &Value) -> Option<String> {
    thread["turns"]
        .as_array()?
        .iter()
        .rev()
        .find(|turn| {
            turn["status"].as_str() == Some("inProgress")
                || turn["status"].as_str() == Some("running")
        })
        .and_then(|turn| turn["id"].as_str())
        .map(str::to_owned)
}
fn parse_threads(value: &Value, workspace_id: &str) -> Vec<ThreadSummary> {
    let result = value.get("result").unwrap_or(value);
    as_array(&result["data"])
        .iter()
        .filter_map(|row| {
            let id = row["id"].as_str()?.to_owned();
            Some(ThreadSummary {
                id,
                workspace_id: workspace_id.into(),
                title: row["name"]
                    .as_str()
                    .or(row["preview"].as_str())
                    .unwrap_or_default()
                    .into(),
                path: row["cwd"].as_str().unwrap_or_default().into(),
                parent_id: row["parentThreadId"].as_str().map(str::to_owned),
                updated_at: row["updatedAt"].as_i64().unwrap_or_default(),
                processing: is_processing(row),
                raw: row.clone(),
                ..ThreadSummary::default()
            })
        })
        .collect()
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlashCommand {
    New,
    Fork,
    Resume,
    Compact,
    Review,
    Fast,
    Status,
    Mcp,
    Apps,
}

fn parse_slash(text: &str) -> Option<(SlashCommand, String)> {
    let trimmed = text.trim();
    let (head, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    let command = match head.to_ascii_lowercase().as_str() {
        "/new" => SlashCommand::New,
        "/fork" => SlashCommand::Fork,
        "/resume" => SlashCommand::Resume,
        "/compact" => SlashCommand::Compact,
        "/review" => SlashCommand::Review,
        "/fast" => SlashCommand::Fast,
        "/status" => SlashCommand::Status,
        "/mcp" => SlashCommand::Mcp,
        "/apps" => SlashCommand::Apps,
        _ => return None,
    };
    Some((command, rest.trim().to_owned()))
}

fn review_target(rest: &str) -> Value {
    let lower = rest.to_ascii_lowercase();
    if lower.starts_with("base ") {
        return json!({"type":"baseBranch","branch":rest[5..].trim()});
    }
    if lower.starts_with("commit ") {
        let mut parts = rest[7..].splitn(2, char::is_whitespace);
        let sha = parts.next().unwrap_or_default();
        let title = parts.next().unwrap_or_default().trim();
        return if title.is_empty() {
            json!({"type":"commit","sha":sha})
        } else {
            json!({"type":"commit","sha":sha,"title":title})
        };
    }
    if lower.starts_with("custom ") {
        return json!({"type":"custom","instructions":rest[7..].trim()});
    }
    if rest.trim().is_empty() {
        json!({"type":"uncommittedChanges"})
    } else {
        json!({"type":"custom","instructions":rest.trim()})
    }
}

fn format_mcp_summary(result: &Value) -> String {
    let Some(data) = result.get("data").and_then(Value::as_array) else {
        return format!(
            "MCP tools:\n{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
    };
    if data.is_empty() {
        return "MCP tools: no servers reported.".into();
    }
    let lines = data
        .iter()
        .map(|server| {
            let name = server["name"]
                .as_str()
                .or(server["serverName"].as_str())
                .unwrap_or("MCP server");
            let status = server["status"]
                .as_str()
                .or(server["startupStatus"].as_str())
                .unwrap_or("status unknown");
            format!("- {name}: {status}")
        })
        .collect::<Vec<_>>();
    format!("MCP tools:\n{}", lines.join("\n"))
}

fn format_apps_summary(result: &Value) -> String {
    let Some(data) = result.get("data").and_then(Value::as_array) else {
        return format!(
            "Apps:\n{}",
            serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
        );
    };
    if data.is_empty() {
        return "Apps: none available.".into();
    }
    format!(
        "Apps:\n{}",
        data.iter()
            .map(|app| {
                let name = app["name"].as_str().or(app["id"].as_str()).unwrap_or("App");
                let desc = app["description"].as_str().filter(|s| !s.is_empty());
                desc.map(|d| format!("- {name}: {d}"))
                    .unwrap_or_else(|| format!("- {name}"))
            })
            .collect::<Vec<_>>()
            .join("\n")
    )
}

fn active_after_history_read(
    conversation: &ConversationState,
    active_execution_id: Option<&str>,
) -> bool {
    active_execution_id.is_some()
        || conversation.pending_submission
        || conversation.pending_execution_id.is_some()
        || conversation.active_turn_id.is_some()
}

fn restore_failed_queued_prompt(
    state: &mut ControllerState,
    key: &ConversationKey,
    entry: QueuedPrompt,
    error: String,
) {
    if let Some(conversation) = state.conversations.get_mut(key) {
        conversation.queue.push_front(entry);
        conversation.last_error = Some(format!("Queued prompt was not sent: {error}"));
    }
}

fn restore_failed_prompt_as_draft(
    state: &mut ControllerState,
    key: &ConversationKey,
    entry: &QueuedPrompt,
    error: String,
) {
    if let Some(conversation) = state.conversations.get_mut(key) {
        if conversation.draft.is_empty() {
            conversation.draft = entry.text.clone();
        }
        conversation.last_error = Some(error);
    }
}

fn capture_codex_elicitation(
    state: &mut ControllerState,
    workspace: &str,
    message: &Value,
) -> bool {
    if message["method"].as_str() != Some("mcpServer/elicitation/request") {
        return false;
    }
    let request_id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let requests = state
        .codex_elicitations_by_workspace
        .entry(workspace.into())
        .or_default();
    if !requests.iter().any(|r| r["requestId"] == request_id) {
        requests.push(json!({"requestId":request_id,"params":params}));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::state::{ControllerState, ConversationState, QueuedPrompt, WorkspaceState};
    use super::Action;
    use super::{
        active_after_history_read, apply_codex_item_event, can_replay_history,
        capture_codex_elicitation, codex_images_from_content, configuration_available,
        is_advertised, mark_fresh_acp_session, parse_slash, project_codex_thread_items,
        prompt_request, remove_permission_request, restore_failed_queued_prompt, review_target,
        runtime_event_projects_to_acp, runtime_event_workspace, send_wait_message,
        should_probe_capabilities, ActivityCompletions, Controller, Purpose, SlashCommand,
    };
    use crate::client::{
        BackendDriver, BackendHandle, BackendMessage, BackendRequest, BackendScope, ClientFuture,
        ConversationKey, ConversationRef, EventStream, RuntimeMethod,
    };
    use serde_json::json;
    use serde_json::Value;
    use std::{collections::VecDeque, sync::mpsc, time::Duration};
    use tokio::sync::oneshot;
    use uuid::Uuid;

    struct HeldReplyDriver {
        calls: mpsc::Sender<(BackendRequest, oneshot::Sender<Result<Value, String>>)>,
    }

    impl BackendDriver for HeldReplyDriver {
        fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a> {
            let calls = self.calls.clone();
            Box::pin(async move {
                let (reply_tx, reply_rx) = oneshot::channel();
                calls
                    .send((request, reply_tx))
                    .map_err(|error| error.to_string())?;
                reply_rx.await.map_err(|error| error.to_string())?
            })
        }
    }

    #[test]
    fn empty_object_capability_is_advertised_but_false_is_not() {
        assert!(is_advertised(&json!({})));
        assert!(is_advertised(&json!(true)));
        assert!(!is_advertised(&json!(false)));
        assert!(!is_advertised(&json!(null)));
        assert!(!is_advertised(&json!([])));
    }

    fn acp_key(endpoint: &str, store: &str, session: &str) -> ConversationKey {
        ConversationKey {
            endpoint_id: endpoint.into(),
            conversation: ConversationRef::Acp {
                workspace_id: "workspace".into(),
                store_id: store.into(),
                session_id: session.into(),
            },
        }
    }

    fn live_runtime_event(sequence: u64, kind: &str, payload: Value) -> Value {
        json!({
            "schemaVersion":1,"storeId":"store","runtime":{"profileId":"fixture","profileRevision":1,"kind":"acp"},
            "sessionId":"session","executionId":if kind == "execution_completed" {payload.get("executionId").cloned()} else {None},
            "sequence":sequence,"source":"live","kind":kind,"payload":payload,"raw":{}
        })
    }

    fn next_backend_call(
        calls: &mpsc::Receiver<(BackendRequest, oneshot::Sender<Result<Value, String>>)>,
    ) -> (BackendRequest, oneshot::Sender<Result<Value, String>>) {
        calls
            .recv_timeout(Duration::from_secs(2))
            .expect("backend request should be enqueued")
    }

    fn tick_until_backend_call(
        controller: &mut Controller,
        calls: &mpsc::Receiver<(BackendRequest, oneshot::Sender<Result<Value, String>>)>,
        now_ms: u64,
    ) -> (BackendRequest, oneshot::Sender<Result<Value, String>>) {
        for _ in 0..100 {
            if let Ok(call) = calls.try_recv() {
                return call;
            }
            controller.tick(now_ms, false, false);
            if let Ok(call) = calls.try_recv() {
                return call;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("backend request should be enqueued after processing the reply");
    }

    #[test]
    fn configure_is_single_flight_until_reply_then_can_be_retried() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 1,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .session = Some(json!({
            "capabilities":{"configuration":{}},
            "configuration":{"configOptions":[{"id":"model"}]}
        }));
        controller.tick(1_000, false, false);

        controller
            .handle(Action::Configure {
                option_id: "model".into(),
                value: "provider/model-a".into(),
            })
            .unwrap();
        let (request, first_reply) = next_backend_call(&calls_rx);
        assert!(
            matches!(request, BackendRequest::Runtime(ref runtime) if runtime.method == RuntimeMethod::Configure)
        );
        assert!(controller.state.conversations[&key].configuration_pending);
        controller.tick(31_001, true, false);
        assert!(
            calls_rx.try_recv().is_err(),
            "idle refresh must defer while provider configuration is pending"
        );

        let duplicate = controller.handle(Action::Configure {
            option_id: "model".into(),
            value: "provider/model-b".into(),
        });
        assert!(duplicate.is_err());
        assert!(
            calls_rx.try_recv().is_err(),
            "duplicate configure must not enqueue"
        );

        first_reply.send(Ok(json!({"ok":true}))).unwrap();
        for _ in 0..100 {
            controller.tick(1_000, false, false);
            if !controller.state.conversations[&key].configuration_pending {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!controller.state.conversations[&key].configuration_pending);

        let (request, session_reply) = next_backend_call(&calls_rx);
        assert!(
            matches!(request, BackendRequest::Runtime(ref runtime) if runtime.method == RuntimeMethod::Session)
        );
        session_reply.send(Ok(json!({
            "sessionId":"session",
            "title":"Fixture",
            "configuration":{"configOptions":[{"id":"model","currentValue":"provider/model-a"}]},
            "capabilities":{"configuration":{}}
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(1_010, false, false);
            if controller.state.conversations[&key]
                .session
                .as_ref()
                .and_then(|session| session.pointer("/configuration/configOptions/0/currentValue"))
                .and_then(Value::as_str)
                == Some("provider/model-a")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            controller.state.conversations[&key]
                .session
                .as_ref()
                .and_then(|session| session.pointer("/configuration/configOptions/0/currentValue"))
                .and_then(Value::as_str),
            Some("provider/model-a")
        );

        controller
            .handle(Action::Configure {
                option_id: "model".into(),
                value: "provider/model-b".into(),
            })
            .unwrap();
        let (request, second_reply) = next_backend_call(&calls_rx);
        assert!(
            matches!(request, BackendRequest::Runtime(ref runtime) if runtime.method == RuntimeMethod::Configure)
        );
        assert!(controller.state.conversations[&key].configuration_pending);
        second_reply.send(Ok(json!({"ok":true}))).unwrap();
        for _ in 0..100 {
            controller.tick(2_000, false, false);
            if !controller.state.conversations[&key].configuration_pending {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!controller.state.conversations[&key].configuration_pending);
        let (request, session_reply) = next_backend_call(&calls_rx);
        assert!(
            matches!(request, BackendRequest::Runtime(ref runtime) if runtime.method == RuntimeMethod::Session)
        );
        session_reply.send(Ok(json!({
            "sessionId":"session",
            "title":"Fixture",
            "configuration":{"configOptions":[{"id":"model","currentValue":"provider/model-b"}]},
            "capabilities":{"configuration":{}}
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(2_010, false, false);
            if controller.state.conversations[&key]
                .session
                .as_ref()
                .and_then(|session| session.pointer("/configuration/configOptions/0/currentValue"))
                .and_then(Value::as_str)
                == Some("provider/model-b")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            controller.state.conversations[&key]
                .session
                .as_ref()
                .and_then(|session| session.pointer("/configuration/configOptions/0/currentValue"))
                .and_then(Value::as_str),
            Some("provider/model-b")
        );
    }

    #[test]
    fn codex_full_thread_projection_preserves_order_repeated_text_and_protocol_items() {
        let thread = json!({"turns":[
            {"id":"turn-1","items":[
                {"id":"user-1","type":"userMessage","content":[
                    {"type":"text","text":"inspect these"},
                    {"type":"localImage","path":"/tmp/input.png"}
                ]},
                {"id":"answer-1","type":"agentMessage","text":"same answer"},
                {"id":"reason-1","type":"reasoning","summary":[{"text":"Check the file"}],"content":"Read metadata"},
                {"id":"command-1","type":"commandExecution","command":["file","/tmp/input.png"],"cwd":"/tmp","status":"completed","aggregatedOutput":"PNG image data"},
                {"id":"files-1","type":"fileChange","status":"completed","changes":[{"path":"out.txt","kind":{"type":"add"},"diff":"+done"}]},
                {"id":"mcp-1","type":"mcpToolCall","server":"docs","tool":"search","arguments":{"q":"one"},"result":{"matches":2},"status":"completed"},
                {"id":"search-1","type":"webSearch","query":"example","status":"completed"},
                {"id":"future-1","type":"futureCodexItem","payload":{"retained":true}}
            ]},
            {"id":"turn-2","items":[
                {"id":"answer-2","type":"agentMessage","text":"same answer"}
            ]}
        ]});

        let items = project_codex_thread_items(&thread);

        assert_eq!(items.len(), 9);
        assert_eq!(items[0]["kind"], "message");
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["text"], "inspect these");
        assert_eq!(items[0]["images"][0], "/tmp/input.png");
        assert_eq!(items[0]["native"]["content"][1]["type"], "localImage");
        assert_eq!(items[1]["kind"], "message");
        assert_eq!(items[1]["text"], "same answer");
        assert_eq!(items[2]["kind"], "reasoning");
        assert_eq!(items[2]["summary"], "Check the file");
        assert_eq!(items[2]["content"], "Read metadata");
        assert_eq!(items[3]["kind"], "tool");
        assert_eq!(items[3]["output"], "PNG image data");
        assert_eq!(items[4]["changes"][0]["path"], "out.txt");
        assert_eq!(items[5]["detail"], "{\n  \"q\": \"one\"\n}");
        assert_eq!(items[6]["title"], "Web search");
        assert_eq!(items[7]["native"]["payload"]["retained"], true);
        assert_eq!(items[8]["id"], "answer-2");
        assert_eq!(items[8]["text"], items[1]["text"]);
    }

    #[test]
    fn codex_live_message_stream_projects_to_one_replaceable_main_view_item() {
        let mut items = Vec::new();
        assert!(apply_codex_item_event(
            &mut items,
            "item/started",
            &json!({"item":{"id":"answer","type":"agentMessage","text":""}})
        ));
        assert!(apply_codex_item_event(
            &mut items,
            "item/agentMessage/delta",
            &json!({"itemId":"answer","delta":"native "})
        ));
        assert!(apply_codex_item_event(
            &mut items,
            "item/agentMessage/delta",
            &json!({"itemId":"answer","delta":"answer"})
        ));
        assert!(apply_codex_item_event(
            &mut items,
            "item/completed",
            &json!({"item":{"id":"answer","type":"agentMessage","text":"native answer"}})
        ));
        assert!(apply_codex_item_event(
            &mut items,
            "item/completed",
            &json!({"item":{"id":"answer-copy","type":"agentMessage","text":"native answer"}})
        ));

        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["kind"], "message");
        assert_eq!(items[0]["id"], "answer");
        assert_eq!(items[0]["text"], "native answer");
        assert_eq!(items[1]["id"], "answer-copy");
        assert_eq!(items[1]["text"], items[0]["text"]);

        assert!(apply_codex_item_event(
            &mut items,
            "item/commandExecution/outputDelta",
            &json!({"itemId":"command","delta":"first"})
        ));
        assert!(apply_codex_item_event(
            &mut items,
            "item/commandExecution/outputDelta",
            &json!({"itemId":"command","delta":" second"})
        ));
        assert_eq!(items[2]["kind"], "tool");
        assert_eq!(items[2]["output"], "first second");
    }

    #[test]
    fn codex_turn_completion_requests_one_authoritative_resume_and_projects_snapshot() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 4,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        let completed = json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn-1","status":"completed"}}}
        });

        controller.apply_codex_event(completed.clone());
        let (request, reply) = next_backend_call(&calls_rx);
        assert_eq!(request.rpc().method, "resume_thread");
        controller.apply_codex_event(completed);
        assert!(
            calls_rx.try_recv().is_err(),
            "duplicate completion must share the pending resume"
        );

        reply.send(Ok(json!({"thread":{"id":"thread","name":"A conversation","status":{"type":"idle"},"turns":[
            {"id":"turn-1","status":"completed","items":[
                {"id":"user-1","type":"userMessage","content":[{"type":"text","text":"hello"}]},
                {"id":"answer-1","type":"agentMessage","text":"world"}
            ]}
        ]}}))).unwrap();
        for _ in 0..100 {
            controller.tick(500, false, false);
            if controller.state.conversations[&key].history_revision > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items.len(), 2);
        assert_eq!(conversation.items[0]["kind"], "message");
        assert_eq!(conversation.items[0]["role"], "user");
        assert_eq!(conversation.items[1]["kind"], "message");
        assert_eq!(conversation.items[1]["text"], "world");
        assert_eq!(conversation.history_revision, 1);
    }

    #[test]
    fn codex_completed_turn_resumes_then_dispatches_one_queued_followup() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 7,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        let queued = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "follow up".into(),
            content: Vec::new(),
            options: json!({}),
        };
        let conversation = controller.state.conversations.get_mut(&key).unwrap();
        conversation.busy = true;
        conversation.active_turn_id = Some("turn-1".into());
        conversation.queue.push_back(queued);

        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn-1","status":"completed"}}}
        }));
        assert!(controller.state.conversations[&key].loading);
        assert_eq!(controller.state.conversations[&key].queue.len(), 1);
        let (resume, resume_reply) = next_backend_call(&calls_rx);
        assert_eq!(resume.rpc().method, "resume_thread");

        resume_reply
            .send(Ok(
                json!({"thread":{"id":"thread","status":{"type":"idle"},"turns":[]}}),
            ))
            .unwrap();
        let (send, send_reply) = tick_until_backend_call(&mut controller, &calls_rx, 100);
        assert_eq!(send.rpc().method, "send_user_message");
        assert_eq!(send.rpc().params["text"], "follow up");
        assert!(controller.state.conversations[&key].queue.is_empty());
        assert!(controller.state.conversations[&key].pending_submission);
        send_reply
            .send(Ok(json!({"executionId":"execution-2"})))
            .unwrap();
        for _ in 0..100 {
            controller.tick(100, false, false);
            if controller.state.conversations[&key]
                .pending_execution_id
                .as_deref()
                == Some("execution-2")
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            controller.state.conversations[&key]
                .pending_execution_id
                .as_deref(),
            Some("execution-2")
        );
    }

    #[test]
    fn codex_completion_before_send_ack_holds_queue_and_unselected_events_mark_unread() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 8,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        controller.state.initialized = true;
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        let current = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "current".into(),
            content: Vec::new(),
            options: json!({}),
        };
        controller.dispatch_prompt(key.clone(), current).unwrap();
        let (send, send_reply) = next_backend_call(&calls_rx);
        assert_eq!(send.rpc().method, "send_user_message");
        controller.state.selected = None;
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .queue
            .push_back(QueuedPrompt {
                request_id: Uuid::new_v4(),
                text: "queued".into(),
                content: Vec::new(),
                options: json!({}),
            });
        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn-1","status":"completed"}}}
        }));
        let conversation = &controller.state.conversations[&key];
        assert!(conversation.unread);
        assert!(conversation.completion_ready);
        assert!(conversation.pending_submission);
        assert_eq!(conversation.queue.len(), 1);
        assert!(
            !conversation.loading,
            "resume must wait for send acknowledgement"
        );
        assert!(
            calls_rx.try_recv().is_err(),
            "completion before submission ack must not overlap resume"
        );
        send_reply
            .send(Ok(json!({"executionId":"execution-1"})))
            .unwrap();
        for _ in 0..100 {
            controller.tick(100, false, false);
            if controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let (resume, resume_reply) = next_backend_call(&calls_rx);
        assert_eq!(resume.rpc().method, "resume_thread");
        resume_reply
            .send(Ok(
                json!({"thread":{"id":"thread","status":{"type":"idle"},"turns":[]}}),
            ))
            .unwrap();
        let (queued, _queued_reply) = tick_until_backend_call(&mut controller, &calls_rx, 100);
        assert_eq!(queued.rpc().method, "send_user_message");
        assert_eq!(queued.rpc().params["text"], "queued");
        assert!(controller.state.conversations[&key].unread);
        controller.state.select(key.clone());
        assert!(!controller.state.conversations[&key].unread);
    }

    #[test]
    fn codex_live_item_arriving_during_resume_overlays_the_older_snapshot() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 9,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .queue
            .push_back(QueuedPrompt {
                request_id: Uuid::new_v4(),
                text: "after completion".into(),
                content: Vec::new(),
                options: json!({}),
            });
        let _ = controller.resume_codex(key.clone()).unwrap();
        let (_resume, resume_reply) = next_backend_call(&calls_rx);
        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"item/agentMessage/delta","params":{"threadId":"thread","itemId":"answer","delta":" newer"}}
        }));
        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/completed","params":{"threadId":"thread","turn":{"id":"turn-1","status":"completed"}}}
        }));
        resume_reply
            .send(Ok(
                json!({"thread":{"id":"thread","status":{"type":"inProgress"},"turns":[
                    {"id":"turn-1","items":[{"id":"answer","type":"agentMessage","text":"older"}]}
                ]}}),
            ))
            .unwrap();
        for _ in 0..100 {
            controller.tick(10, false, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            controller.state.conversations[&key].items[0]["text"],
            "older newer"
        );
        assert!(controller.state.conversations[&key].busy);
        assert!(controller.state.conversations[&key].pending_submission);
        let (queued, _queued_reply) = next_backend_call(&calls_rx);
        assert_eq!(queued.rpc().params["text"], "after completion");
    }

    #[test]
    fn generation_conflict_restarts_from_page_one_and_keeps_old_snapshot_until_complete() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 10,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        let prior = json!({"id":"prior","kind":"message","role":"assistant","text":"complete prior transcript"});
        let conversation = controller.state.conversations.get_mut(&key).unwrap();
        conversation.items = vec![prior.clone()];
        conversation.generation = Some("generation-1".into());
        conversation.cursor = Some(json!("cursor-old"));
        conversation.capability_probe_attempted = true;

        controller.request_next_history_page(key.clone());
        let (_old_page, old_page_reply) = next_backend_call(&calls_rx);
        old_page_reply
            .send(Err(
                "revision_conflict: conversation generation changed; restart pagination".into(),
            ))
            .unwrap();
        let (first_page, first_page_reply) =
            tick_until_backend_call(&mut controller, &calls_rx, 100);
        assert_eq!(first_page.rpc().params["generation"], Value::Null);
        assert_eq!(first_page.rpc().params["cursor"], Value::Null);
        assert_eq!(controller.state.conversations[&key].items, [prior.clone()]);
        first_page_reply.send(Ok(json!({
            "generation":"generation-2","items":[{"id":"new-1","kind":"message","role":"user","text":"new first"}],
            "nextCursor":"cursor-new","load":{"state":"complete"},"freshness":{"stale":false}
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(200, false, false);
            if controller.state.conversations[&key].generation.as_deref() == Some("generation-2") {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(controller.state.conversations[&key].items, [prior.clone()]);

        let (second_page, second_page_reply) =
            tick_until_backend_call(&mut controller, &calls_rx, 200);
        assert_eq!(second_page.rpc().params["generation"], "generation-2");
        assert_eq!(second_page.rpc().params["cursor"], "cursor-new");
        second_page_reply.send(Ok(json!({
            "generation":"generation-2","items":[{"id":"new-2","kind":"message","role":"assistant","text":"new last"}],
            "nextCursor":null,"load":{"state":"complete"},"freshness":{"stale":false}
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(300, false, false);
            if controller.state.conversations[&key].items.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let items = &controller.state.conversations[&key].items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "new-1");
        assert_eq!(items[1]["id"], "new-2");
        assert_eq!(controller.state.conversations[&key].history_revision, 1);
    }

    #[test]
    fn ordinary_paginated_refresh_keeps_last_complete_transcript_after_later_page_failure() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 10,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        let previous_items = vec![json!({
            "id":"old-1","kind":"message","role":"assistant","text":"last successful transcript"
        })];
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.items = previous_items.clone();
            conversation.generation = Some("generation-old".into());
            conversation.cursor = Some(json!("old-cursor"));
            conversation.load_state = Some("complete".into());
        }

        controller.apply_page(
            key.clone(),
            json!({
                "generation":"generation-new",
                "items":[{"id":"new-1","kind":"message","role":"user","text":"replacement first page"}],
                "nextCursor":"new-cursor",
                "load":{"state":"complete"},"freshness":{"stale":false}
            }),
            false,
            100,
        );
        assert_eq!(controller.state.conversations[&key].items, previous_items);

        controller.request_next_history_page(key.clone());
        let (second_page, second_page_reply) = next_backend_call(&calls_rx);
        assert_eq!(second_page.rpc().params["generation"], "generation-new");
        assert_eq!(second_page.rpc().params["cursor"], "new-cursor");
        second_page_reply
            .send(Err("temporary read failure".into()))
            .unwrap();
        for _ in 0..100 {
            controller.tick(200, false, false);
            if controller.state.conversations[&key].last_error.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items, previous_items);
        assert_eq!(conversation.generation.as_deref(), Some("generation-old"));
        assert_eq!(conversation.cursor, Some(json!("old-cursor")));
        assert_eq!(conversation.load_state.as_deref(), Some("complete"));
        assert_eq!(
            conversation.last_error.as_deref(),
            Some("temporary read failure")
        );
    }

    #[test]
    fn published_snapshot_pages_finish_while_a_new_history_load_is_running() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 18,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.initialized = true;
        controller.state.select(key.clone());

        // The session already has a published 10k-item generation. Selection
        // cached its first page; then the asynchronous replay ticket began.
        let first_items: Vec<_> = (0..200)
            .map(|ordinal| json!({"id":format!("item-{ordinal}"),"kind":"message","text":ordinal}))
            .collect();
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.items = first_items.clone();
            conversation.generation = Some("cached-generation".into());
            conversation.cursor = Some(json!(199));
            conversation.load_state = Some("complete".into());
            conversation.loading = true;
        }

        controller.apply_page(
            key.clone(),
            json!({
                "generation":"cached-generation",
                "items":first_items,
                "nextCursor":199,
                "load":{"state":"loading"},
                "coverage":"agent_replay",
                "freshness":{"stale":false}
            }),
            false,
            1_000,
        );
        controller.request_next_history_page(key.clone());
        // Page traversal owns the pending-read slot, so history polling cannot
        // issue a second overlapping page request.
        controller.request_next_history_page(key.clone());

        let mut fetched = 200usize;
        let mut expected_page_cursor = 199u64;
        let mut pages = 0usize;
        let mut next_call = Some(next_backend_call(&calls_rx));
        while fetched < 10_000 {
            let (request, reply) = next_call.take().expect("page request queued");
            assert_eq!(request.rpc().method, "read_conversation");
            assert_eq!(request.rpc().params["generation"], "cached-generation");
            assert_eq!(request.rpc().params["cursor"], expected_page_cursor);
            assert!(controller.pending_reads.contains(&key));

            let end = (fetched + 200).min(10_000);
            let page: Vec<_> = (fetched..end)
                .map(|ordinal| json!({"id":format!("item-{ordinal}"),"kind":"message","text":ordinal}))
                .collect();
            let next_cursor = (end < 10_000).then_some((end - 1) as u64);
            reply
                .send(Ok(json!({
                    "generation":"cached-generation",
                    "items":page,
                    "nextCursor":next_cursor,
                    "load":{"state":"loading"},
                    "coverage":"agent_replay",
                    "freshness":{"stale":false}
                })))
                .unwrap();
            pages += 1;
            fetched = end;
            if let Some(cursor) = next_cursor {
                expected_page_cursor = cursor;
                next_call = Some(tick_until_backend_call(
                    &mut controller,
                    &calls_rx,
                    1_001 + fetched as u64,
                ));
            } else {
                for _ in 0..100 {
                    controller.tick(1_001 + fetched as u64, false, false);
                    if controller.history_restart_staging.get(&key).is_none() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items.len(), 10_000);
        assert!(conversation
            .items
            .iter()
            .enumerate()
            .all(|(ordinal, item)| item["id"] == format!("item-{ordinal}")));
        assert_eq!(
            conversation.generation.as_deref(),
            Some("cached-generation")
        );
        assert_eq!(conversation.load_state.as_deref(), Some("loading"));
        assert!(
            conversation.loading,
            "the new replay ticket remains independent of cached pagination"
        );
        assert!(conversation.cursor.is_none());
        assert_eq!(conversation.history_revision, 0);
        assert!(controller.history_restart_staging.get(&key).is_none());
        assert_eq!(pages, 49);
        assert!(
            calls_rx.try_recv().is_err(),
            "each cached page is requested once"
        );
    }

    #[test]
    fn loading_poll_does_not_truncate_a_fully_paged_published_snapshot() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 20,
            store_id: Some("store".into()),
        };
        let (calls_tx, _calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        let published: Vec<_> = (0..10_000)
            .map(|ordinal| json!({"id":format!("item-{ordinal}"),"kind":"message","text":ordinal}))
            .collect();
        let first_page = published[..200].to_vec();
        {
            let conversation = controller
                .state
                .conversations
                .entry(key.clone())
                .or_default();
            conversation.items = published.clone();
            conversation.items_revision = 7;
            conversation.history_revision = 3;
            conversation.generation = Some("published-generation".into());
            conversation.cursor = None;
            conversation.load_state = Some("complete".into());
            conversation.loading = true;
        }

        controller.apply_page(
            key.clone(),
            json!({
                "generation":"published-generation",
                "items":first_page,
                "nextCursor":199,
                "load":{"state":"loading"},
                "coverage":"agent_replay",
                "freshness":{"stale":false}
            }),
            false,
            9_000,
        );

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items, published);
        assert_eq!(conversation.items_revision, 7);
        assert_eq!(conversation.history_revision, 3);
        assert_eq!(conversation.cursor, None);
        assert_eq!(
            conversation.generation.as_deref(),
            Some("published-generation")
        );
        assert_eq!(conversation.load_state.as_deref(), Some("loading"));
        assert!(conversation.loading);
        assert_eq!(conversation.last_refresh_ms, Some(9_000));
    }

    #[test]
    fn loading_replay_keeps_polling_past_sixty_seconds_and_preserves_cache_and_draft() {
        let scope = BackendScope {
            endpoint_id: "remote:test".into(),
            connection_epoch: 23,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("remote:test", "store", "session");
        controller.state.initialized = true;
        controller.state.select(key.clone());
        let cached: Vec<_> = (0..10_000)
            .map(
                |ordinal| json!({"id":format!("cached-{ordinal}"),"kind":"message","text":ordinal}),
            )
            .collect();
        let refreshed: Vec<_> = (0..10_000)
            .map(|ordinal| json!({"id":format!("fresh-{ordinal}"),"kind":"message","text":ordinal}))
            .collect();
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.items = cached.clone();
            conversation.generation = Some("published-generation".into());
            conversation.cursor = None;
            conversation.load_state = Some("loading".into());
            conversation.loading = true;
            conversation.read_only = true;
            conversation.draft = "preserve this draft".into();
            conversation.session = Some(json!({
                "agentInfo":{"agentInfo":{"name":"fixture"}},
                "capabilities":{"loadSession":{}}
            }));
            conversation.capability_probe_attempted = true;
        }
        controller
            .history_poll_due_ms
            .insert(key.clone(), (60_000, 30));

        // The 60-second notice is informational: the owner is still replaying,
        // so the read-only viewer continues bounded polling.
        controller.tick(60_000, true, false);
        let (first_poll, first_reply) = next_backend_call(&calls_rx);
        assert_eq!(first_poll.rpc().method, "read_conversation");
        first_reply
            .send(Ok(json!({
                "generation":"published-generation",
                "items":cached[..200].to_vec(),
                "nextCursor":199,
                "load":{"state":"loading"},
                "coverage":"agent_replay",
                "freshness":{"stale":false},
                "readOnly":true
            })))
            .unwrap();
        for _ in 0..100 {
            controller.tick(60_001, false, false);
            if !controller.pending_reads.contains(&key) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        {
            let conversation = &controller.state.conversations[&key];
            assert_eq!(conversation.items, cached);
            assert!(conversation.loading);
            assert_eq!(conversation.draft, "preserve this draft");
            assert!(conversation
                .last_error
                .as_deref()
                .is_some_and(|message| message.contains("automatic checks continue")));
        }

        controller.tick(62_001, true, false);
        let (completion_poll, completion_reply) = next_backend_call(&calls_rx);
        assert_eq!(completion_poll.rpc().method, "read_conversation");
        completion_reply
            .send(Ok(json!({
                "generation":"completed-generation",
                "items":refreshed[..200].to_vec(),
                "nextCursor":199,
                "load":{"state":"complete"},
                "coverage":"agent_replay",
                "freshness":{"stale":false},
                "readOnly":true
            })))
            .unwrap();
        let mut fetched = 200usize;
        let mut cursor = 199u64;
        let mut next_call = Some(tick_until_backend_call(&mut controller, &calls_rx, 62_002));
        while fetched < refreshed.len() {
            let (request, reply) = next_call.take().expect("history page request queued");
            assert_eq!(request.rpc().method, "read_conversation");
            assert_eq!(request.rpc().params["generation"], "completed-generation");
            assert_eq!(request.rpc().params["cursor"], cursor);
            let end = (fetched + 200).min(refreshed.len());
            let next_cursor = (end < refreshed.len()).then_some((end - 1) as u64);
            reply
                .send(Ok(json!({
                    "generation":"completed-generation",
                    "items":refreshed[fetched..end].to_vec(),
                    "nextCursor":next_cursor,
                    "load":{"state":"complete"},
                    "coverage":"agent_replay",
                    "freshness":{"stale":false},
                    "readOnly":true
                })))
                .unwrap();
            fetched = end;
            if let Some(next_cursor) = next_cursor {
                cursor = next_cursor;
                next_call = Some(tick_until_backend_call(
                    &mut controller,
                    &calls_rx,
                    62_003 + fetched as u64,
                ));
            } else {
                for _ in 0..100 {
                    controller.tick(80_000, false, false);
                    if controller.history_restart_staging.get(&key).is_none() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items, refreshed);
        assert_eq!(conversation.draft, "preserve this draft");
        assert!(!conversation.loading);
        assert!(conversation.last_error.is_none());
        assert!(!controller.history_poll_due_ms.contains_key(&key));
        assert!(controller.history_restart_staging.get(&key).is_none());
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn completed_same_generation_read_paginates_and_keeps_live_tail_items() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 21,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.initialized = true;
        controller.state.select(key.clone());
        let previous: Vec<_> = (0..10_000)
            .map(|ordinal| json!({"id":format!("item-{ordinal}"),"kind":"message","text":ordinal}))
            .collect();
        let updated: Vec<_> = (0..10_002)
            .map(|ordinal| json!({"id":format!("item-{ordinal}"),"kind":"message","text":ordinal}))
            .collect();
        {
            let conversation = controller
                .state
                .conversations
                .entry(key.clone())
                .or_default();
            conversation.items = previous;
            conversation.items_revision = 7;
            conversation.history_revision = 3;
            conversation.generation = Some("published-generation".into());
            conversation.cursor = None;
            conversation.load_state = Some("complete".into());
        }

        controller.apply_page(
            key.clone(),
            json!({
                "generation":"published-generation",
                "items":updated[..200].to_vec(),
                "nextCursor":199,
                "load":{"state":"complete"},
                "coverage":"agent_replay",
                "freshness":{"stale":false}
            }),
            false,
            10_000,
        );
        controller.request_next_history_page(key.clone());

        let mut fetched = 200usize;
        let mut cursor = 199u64;
        let mut next_call = Some(next_backend_call(&calls_rx));
        while fetched < updated.len() {
            let (request, reply) = next_call.take().expect("history page request queued");
            assert_eq!(request.rpc().method, "read_conversation");
            assert_eq!(request.rpc().params["generation"], "published-generation");
            assert_eq!(request.rpc().params["cursor"], cursor);
            let end = (fetched + 200).min(updated.len());
            let next_cursor = (end < updated.len()).then_some((end - 1) as u64);
            reply
                .send(Ok(json!({
                    "generation":"published-generation",
                    "items":updated[fetched..end].to_vec(),
                    "nextCursor":next_cursor,
                    "load":{"state":"complete"},
                    "coverage":"agent_replay",
                    "freshness":{"stale":false}
                })))
                .unwrap();
            fetched = end;
            if let Some(next_cursor) = next_cursor {
                cursor = next_cursor;
                next_call = Some(tick_until_backend_call(
                    &mut controller,
                    &calls_rx,
                    10_001 + fetched as u64,
                ));
            } else {
                for _ in 0..100 {
                    controller.tick(20_000, false, false);
                    if controller.history_restart_staging.get(&key).is_none() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items, updated);
        assert_eq!(conversation.items_revision, 8);
        assert_eq!(conversation.history_revision, 3);
        assert_eq!(conversation.cursor, None);
        assert_eq!(conversation.load_state.as_deref(), Some("complete"));
        assert!(!conversation.loading);
        assert!(controller.history_restart_staging.get(&key).is_none());
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn cached_page_failure_preserves_inflight_replay_and_pauses_composer() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 19,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.initialized = true;
        controller.state.select(key.clone());
        let visible_items = vec![
            json!({"id":"cached-1","kind":"message","text":"previous answer"}),
            json!({"id":"cached-2","kind":"message","text":"previous follow-up"}),
        ];
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.items = visible_items.clone();
            conversation.generation = Some("published-generation".into());
            conversation.cursor = Some(json!(199));
            conversation.load_state = Some("loading".into());
            conversation.loading = true;
            conversation.draft = "keep this for after replay".into();
        }

        controller.request_next_history_page(key.clone());
        let (page_request, page_reply) = next_backend_call(&calls_rx);
        assert_eq!(page_request.rpc().method, "read_conversation");
        page_reply
            .send(Err("temporary cached-page failure".into()))
            .unwrap();
        for _ in 0..100 {
            controller.tick(2_000, false, false);
            if controller.state.conversations[&key].last_error.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let conversation = &controller.state.conversations[&key];
        assert_eq!(conversation.items, visible_items);
        assert_eq!(
            conversation.generation.as_deref(),
            Some("published-generation")
        );
        assert_eq!(conversation.load_state.as_deref(), Some("loading"));
        assert!(conversation.loading);
        assert!(!conversation.pending_submission);
        assert_eq!(conversation.draft, "keep this for after replay");
        assert_eq!(
            conversation.last_error.as_deref(),
            Some("temporary cached-page failure")
        );

        let send_error = controller
            .send_selected(Vec::new(), Value::Null, false)
            .expect_err("send during replay must be paused");
        assert!(send_error.contains("still loading"));
        let conversation = &controller.state.conversations[&key];
        assert!(conversation.queue.is_empty());
        assert_eq!(conversation.draft, "keep this for after replay");
        assert!(
            calls_rx.try_recv().is_err(),
            "failed cache paging cannot dispatch a prompt or replay"
        );
    }

    #[test]
    fn fresh_codex_thread_is_sendable_without_resume_until_first_turn_completes() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 10,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        controller.state.initialized = true;
        controller.state.workspaces.push(WorkspaceState {
            id: "workspace".into(),
            raw: json!({"connected":true}),
            ..WorkspaceState::default()
        });

        controller
            .start_codex_conversation("workspace".into())
            .unwrap();
        let (start, start_reply) = next_backend_call(&calls_rx);
        assert_eq!(start.rpc().method, "start_thread");
        start_reply
            .send(Ok(json!({"thread":{"id":"fresh-thread","name":"New thread","cwd":"/workspace","turns":[]}})))
            .unwrap();
        let (first_control, first_control_reply) =
            tick_until_backend_call(&mut controller, &calls_rx, 100);
        assert_eq!(first_control.rpc().method, "model_list");
        first_control_reply.send(Ok(json!({"data":[]}))).unwrap();
        for expected in ["collaboration_mode_list", "get_config_model"] {
            let (request, reply) = next_backend_call(&calls_rx);
            assert_eq!(request.rpc().method, expected);
            reply.send(Ok(json!({}))).unwrap();
        }
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "fresh-thread".into(),
            },
        };
        assert!(controller.state.conversations[&key].awaiting_first_turn);
        let conversation = &controller.state.conversations[&key];
        assert!(!conversation.loading);
        assert!(conversation.awaiting_first_turn);
        assert!(conversation.last_error.is_none());
        assert!(conversation.items.is_empty());
        assert_eq!(conversation.title, "New thread");
        assert_eq!(
            controller.state.threads_by_workspace["workspace"][0].id, "fresh-thread",
            "start_thread result must make the new conversation visible in the sidebar"
        );
        assert!(controller.resume_codex(key.clone()).unwrap().is_nil());
        assert!(
            calls_rx.try_recv().is_err(),
            "fresh threads must not be resumed"
        );

        controller.state.conversations.get_mut(&key).unwrap().draft = "first prompt".into();
        controller
            .handle(Action::Send {
                content: Vec::new(),
                options: json!({}),
                steer: false,
            })
            .unwrap();
        let (send, send_reply) = next_backend_call(&calls_rx);
        assert_eq!(send.rpc().method, "send_user_message");
        send_reply
            .send(Ok(json!({"executionId":"first-execution"})))
            .unwrap();
        for _ in 0..100 {
            controller.tick(200, false, false);
            if !controller.state.conversations[&key].pending_submission {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(controller.state.conversations[&key].busy);
        assert!(
            calls_rx.try_recv().is_err(),
            "send acknowledgement must not resume active turn"
        );

        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/started","params":{"threadId":"fresh-thread","turn":{"id":"first-turn"}}}
        }));
        assert!(controller.state.conversations[&key].awaiting_first_turn);
        controller.apply_codex_event(json!({
            "workspaceId":"workspace",
            "message":{"method":"turn/completed","params":{"threadId":"fresh-thread","turn":{"id":"first-turn","status":"completed"}}}
        }));
        assert!(!controller.state.conversations[&key].awaiting_first_turn);
        let (resume, _resume_reply) = tick_until_backend_call(&mut controller, &calls_rx, 300);
        assert_eq!(resume.rpc().method, "resume_thread");
    }

    #[test]
    fn slow_codex_resume_starts_next_idle_freshness_interval_after_completion() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 11,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        controller.resume_codex(key.clone()).unwrap();
        let (_initial, initial_reply) = next_backend_call(&calls_rx);
        initial_reply
            .send(Ok(
                json!({"thread":{"id":"thread","status":{"type":"idle"},"turns":[]}}),
            ))
            .unwrap();
        for _ in 0..100 {
            controller.tick(120_000, true, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            calls_rx.try_recv().is_err(),
            "a multi-minute load must not cause an immediate reload"
        );
        controller.tick(149_999, true, false);
        assert!(
            calls_rx.try_recv().is_err(),
            "idle freshness is measured from the successful response"
        );
        controller.tick(150_000, true, false);
        let (refresh, _refresh_reply) = next_backend_call(&calls_rx);
        assert_eq!(refresh.rpc().method, "resume_thread");
    }

    #[test]
    fn activity_completion_queue_deduplicates_native_ids_per_scoped_conversation() {
        let mut completions = ActivityCompletions::default();
        let local = acp_key("local", "store-a", "session");
        let remote = acp_key("daemon", "store-b", "session");

        assert!(completions.record(local.clone(), "execution-7"));
        assert!(!completions.record(local.clone(), "execution-7"));
        assert!(completions.record(remote.clone(), "execution-7"));
        assert_eq!(completions.drain(), [local, remote]);
        assert!(completions.drain().is_empty());
    }

    #[test]
    fn stale_history_read_during_submission_preserves_busy_queue_and_uncertainty() {
        let mut conversation = ConversationState {
            busy: true,
            pending_submission: true,
            queue: VecDeque::from([QueuedPrompt {
                request_id: Uuid::new_v4(),
                text: "queued follow-up".into(),
                content: Vec::new(),
                options: Value::Null,
            }]),
            uncertain_request: Some(Uuid::new_v4()),
            ..ConversationState::default()
        };
        let queued_before = conversation.queue.clone();
        let uncertain_before = conversation.uncertain_request;

        // This page was requested before the user submitted, so its inactive
        // snapshot cannot clear the in-flight submission anchor.
        conversation.busy = active_after_history_read(&conversation, None);

        assert!(conversation.busy);
        assert_eq!(conversation.queue, queued_before);
        assert_eq!(conversation.uncertain_request, uncertain_before);
        assert!(active_after_history_read(&conversation, None));
    }

    #[test]
    fn history_read_preserves_acknowledged_but_uncompleted_execution_and_codex_turn() {
        let mut conversation = ConversationState {
            pending_execution_id: Some("execution-1".into()),
            ..ConversationState::default()
        };
        assert!(active_after_history_read(&conversation, None));
        conversation.pending_execution_id = None;
        conversation.active_turn_id = Some("turn-1".into());
        assert!(active_after_history_read(&conversation, None));
        conversation.active_turn_id = None;
        assert!(!active_after_history_read(&conversation, None));
        assert!(active_after_history_read(
            &conversation,
            Some("execution-2")
        ));
    }

    #[test]
    fn failed_queued_dispatch_keeps_the_original_payload_and_reports_the_error() {
        let key = acp_key("local", "store", "session");
        let mut state = ControllerState::default();
        let entry = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "send with attachment".into(),
            content: vec![json!({"type":"image","data":"image-bytes"})],
            options: json!({"mode":"fast"}),
        };
        let expected = entry.clone();
        state.conversations.entry(key.clone()).or_default();

        restore_failed_queued_prompt(&mut state, &key, entry, "backend is disconnected".into());

        let conversation = state.conversations.get(&key).unwrap();
        assert_eq!(conversation.queue.front(), Some(&expected));
        assert_eq!(
            conversation.last_error.as_deref(),
            Some("Queued prompt was not sent: backend is disconnected")
        );
    }

    #[test]
    fn activity_completions_ignore_replayed_history_and_codex_runtime_projection() {
        let mut completions = ActivityCompletions::default();
        let key = acp_key("local", "store", "session");
        assert!(!completions.record_runtime_event(
            key.clone(),
            &json!({"source":"history","kind":"execution_completed","executionId":"old-turn"})
        ));
        assert!(!completions.record_runtime_event(
            key.clone(),
            &json!({"source":"live","kind":"message_delta","executionId":"current-turn"})
        ));
        assert!(!completions
            .record_runtime_event(key, &json!({"source":"live","kind":"execution_completed"})));
        let key = acp_key("local", "store", "session");
        assert!(completions.record_runtime_event(
            key.clone(),
            &json!({"source":"live","kind":"execution_completed","executionId":"live-turn"})
        ));
        assert!(!completions.record_runtime_event(
            key,
            &json!({"source":"live","kind":"execution_completed","executionId":"live-turn"})
        ));
        assert!(!runtime_event_projects_to_acp(
            &json!({"runtime":{"kind":"codex"}})
        ));
        assert!(runtime_event_projects_to_acp(
            &json!({"runtime":{"kind":"opencode"}})
        ));
        assert_eq!(completions.drain().len(), 1);
    }

    #[test]
    fn historical_acp_events_are_not_live_activity_or_actionable_permissions() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 12,
            store_id: Some("store".into()),
        };
        let (calls_tx, _calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.catalog.push(json!({
            "storeId":"store","sessionId":"session","workspaceId":"workspace"
        }));
        controller.state.select(key.clone());
        controller.state.selected = None;
        for kind in ["permission", "message_delta", "execution_submitted"] {
            controller.apply_event(
                json!({
                    "source":"history","runtime":{"kind":"opencode"},
                    "storeId":"store","sessionId":"session","workspaceId":"workspace",
                    "kind":kind,"payload":{"requestId":"old-request","text":"old transcript"},
                    "sequence":1001
                }),
                100,
            );
        }
        let conversation = &controller.state.conversations[&key];
        assert!(!conversation.busy);
        assert!(!conversation.unread);
        assert!(conversation.permissions.is_empty());

        controller.apply_event(json!({
            "source":"live","runtime":{"kind":"opencode"},
            "storeId":"store","sessionId":"session","workspaceId":"workspace",
            "kind":"permission","payload":{"requestId":"permission-1","options":[{"optionId":"accept"}]},
            "sequence":1002
        }), 200);
        assert_eq!(controller.state.conversations[&key].permissions.len(), 1);
        assert!(controller.state.conversations[&key].unread);
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .permissions
            .push(json!({"requestId":"permission-2","options":[{"optionId":"accept"}]}));
        controller.apply_event(
            json!({
                "source":"live","runtime":{"kind":"opencode"},
                "storeId":"store","sessionId":"session","workspaceId":"workspace",
                "kind":"permission_settled","payload":{"requestId":"permission-1"},
                "sequence":1003
            }),
            300,
        );
        let remaining = &controller.state.conversations[&key].permissions;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0]["requestId"], "permission-2");
    }

    #[test]
    fn codex_completion_notifications_are_deduplicated_by_native_turn_id() {
        let mut completions = ActivityCompletions::default();
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        let params = json!({"turn":{"id":"turn-12"}});
        assert!(!completions.record_codex_event(key.clone(), "turn/started", &params, None));
        assert!(completions.record_codex_event(key.clone(), "turn/completed", &params, None));
        assert!(!completions.record_codex_event(key.clone(), "turn/completed", &params, None));
        assert_eq!(completions.drain(), [key]);
    }

    #[test]
    fn fresh_acp_session_skips_replay_until_its_first_turn_is_persisted() {
        let mut state = super::ControllerState::default();
        let key = super::ConversationKey {
            endpoint_id: "local".into(),
            conversation: super::ConversationRef::Acp {
                workspace_id: "workspace".into(),
                store_id: "store".into(),
                session_id: "fresh-session".into(),
            },
        };
        mark_fresh_acp_session(&mut state, key.clone());
        let conversation = state.conversations.get_mut(&key).unwrap();
        conversation.session = Some(json!({"capabilities":{"loadSession":{}}}));

        // `CreateSession` marks the returned key before it enters SelectAcp ->
        // Session -> ReadConversation.
        assert!(!can_replay_history(conversation));

        let conversation = state.conversations.get_mut(&key).unwrap();
        conversation.awaiting_first_turn = false;
        // A selected/discovered existing session still gets the normal replay.
        assert!(can_replay_history(conversation));
    }

    #[test]
    fn selected_existing_acp_session_can_replay_when_load_is_advertised() {
        let conversation = super::state::ConversationState {
            session: Some(json!({"capabilities":{"loadSession":{}}})),
            ..Default::default()
        };
        assert!(can_replay_history(&conversation));
    }

    #[test]
    fn attached_acp_binding_negotiates_unknown_caps_after_cache_read_and_preserves_owner() {
        let mut conversation = ConversationState {
            session: Some(json!({
                "capabilities":{"loadSession":false},
                "agentInfo":null
            })),
            ..ConversationState::default()
        };

        // attach_session persists default capability values. The cached read is
        // complete before this decision; unknown is identified by the absent
        // initialization record rather than treated as unsupported.
        assert!(should_probe_capabilities(&conversation));
        conversation.capability_probe_attempted = true;
        conversation.session = Some(json!({
            "capabilities":{"loadSession":{}},
            "agentInfo":{"agentInfo":{"name":"fixture"}}
        }));
        assert!(!should_probe_capabilities(&conversation));
        assert!(can_replay_history(&conversation));

        // Negotiated lack of load support is terminal, while a foreign owner
        // stays read-only and is never probed.
        conversation.capability_probe_attempted = false;
        conversation.session = Some(json!({
            "capabilities":{"loadSession":false},
            "agentInfo":{"agentInfo":{"name":"limited fixture"}}
        }));
        assert!(!should_probe_capabilities(&conversation));
        assert!(!can_replay_history(&conversation));
        conversation.session = Some(json!({
            "capabilities":{"loadSession":false},
            "agentInfo":null
        }));
        conversation.read_only = true;
        assert!(!should_probe_capabilities(&conversation));
    }

    #[test]
    fn send_during_history_negotiation_keeps_draft_and_attachment_input() {
        let conversation = ConversationState {
            loading: true,
            draft: "keep this prompt".into(),
            ..ConversationState::default()
        };
        let content = vec![json!({"type":"image","data":"attachment-bytes"})];

        let error = send_wait_message(&conversation).unwrap();

        assert_eq!(
            error,
            "Conversation history is still loading; wait until it finishes before sending."
        );
        assert_eq!(conversation.draft, "keep this prompt");
        assert_eq!(content[0]["data"], "attachment-bytes");
    }

    #[test]
    fn stale_configuration_flag_does_not_hide_advertised_configuration_data() {
        assert!(configuration_available(&json!({
            "capabilities":{"configuration":false},
            "configuration":{"configOptions":[]}
        })));
        assert!(configuration_available(&json!({
            "capabilities":{"configuration":false},
            "configuration":{"models":{"availableModels":[]}}
        })));
        assert!(!configuration_available(&json!({
            "capabilities":{"configuration":false},
            "configuration":{}
        })));
    }

    #[test]
    fn answering_one_permission_keeps_other_concurrent_requests() {
        let mut conversation = super::state::ConversationState {
            permissions: vec![
                json!({"requestId":"permission-1"}),
                json!({"requestId":"permission-2"}),
            ],
            ..Default::default()
        };

        remove_permission_request(&mut conversation, &json!("permission-1"));

        assert_eq!(conversation.permissions.len(), 1);
        assert_eq!(conversation.permissions[0]["requestId"], "permission-2");
    }

    #[test]
    fn read_only_conversation_cannot_send_a_permission_response() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 13,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .read_only = true;

        let error = controller
            .permission(json!("permission-1"), "accept".into())
            .unwrap_err();
        assert!(error.contains("read-only"));
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn read_only_conversation_cannot_cancel_owner_run_or_clear_queued_prompt() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 15,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        let queued = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "keep this queued".into(),
            content: Vec::new(),
            options: Value::Null,
        };
        let conversation = controller.state.conversations.get_mut(&key).unwrap();
        conversation.read_only = true;
        conversation.busy = true;
        conversation.queue.push_back(queued.clone());

        let error = controller.stop_selected().unwrap_err();
        assert!(error.contains("read-only"));
        assert_eq!(
            controller.state.conversations[&key].queue.front(),
            Some(&queued)
        );
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn successful_cancel_settles_only_permissions_pending_when_stop_started() {
        let scope = BackendScope {
            endpoint_id: "remote:test".into(),
            connection_epoch: 22,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("remote:test", "store", "session");
        controller.state.select(key.clone());
        let queued = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "queued follow-up".into(),
            content: Vec::new(),
            options: Value::Null,
        };
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.busy = true;
            conversation.permissions = vec![json!({"requestId":"permission-at-stop"})];
            conversation.queue.push_back(queued);
        }

        controller.stop_selected().unwrap();
        let (cancel, reply) = next_backend_call(&calls_rx);
        assert_eq!(cancel.rpc().method, "cancel");
        let conversation = &controller.state.conversations[&key];
        assert!(
            conversation.queue.is_empty(),
            "Stop keeps its existing queue-clear behavior"
        );
        assert_eq!(
            conversation.permissions.len(),
            1,
            "permission remains pending until cancel is acknowledged"
        );

        // A permission that arrives after cancellation began is not included
        // in the request's cleanup set and must remain actionable.
        controller
            .state
            .conversations
            .get_mut(&key)
            .unwrap()
            .permissions
            .push(json!({"requestId":"permission-after-stop"}));
        reply.send(Ok(json!({"cancelled":true}))).unwrap();
        for _ in 0..100 {
            controller.tick(1_000, false, false);
            if !controller.state.conversations[&key].busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let conversation = &controller.state.conversations[&key];
        assert!(!conversation.busy);
        assert_eq!(
            conversation.permissions,
            vec![json!({"requestId":"permission-after-stop"})]
        );
        assert!(conversation.queue.is_empty());
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn read_only_conversation_cannot_change_agent_configuration() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 14,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        let conversation = controller.state.conversations.get_mut(&key).unwrap();
        conversation.read_only = true;
        conversation.session = Some(json!({
            "capabilities":{"configuration":{}},
            "configuration":{"configOptions":[{"id":"model"}]}
        }));

        let error = controller
            .configure("model".into(), "provider/model-a".into())
            .unwrap_err();
        assert!(error.contains("read-only"));
        assert!(!controller.state.conversations[&key].configuration_pending);
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn runtime_resync_replays_events_then_buffered_live_settlement_once_and_reads_cache() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 16,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope.clone(), backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        controller.state.conversations.get_mut(&key).unwrap().busy = true;
        controller
            .state
            .catalog
            .push(json!({"storeId":"store","sessionId":"session","workspaceId":"workspace"}));

        controller.apply_message(
            BackendMessage::ResyncRequired {
                scope: scope.clone(),
                stream: EventStream::Runtime,
            },
            100,
        );
        let (events_req, events_reply) = next_backend_call(&calls_rx);
        assert_eq!(events_req.rpc().method, "events");
        assert_eq!(events_req.rpc().params["after"], 0);

        let settled = live_runtime_event(
            12,
            "permission_settled",
            json!({"requestId":"permission-1"}),
        );
        let mut live = settled.clone();
        live["workspaceId"] = json!("workspace");
        controller.apply_event(live, 101);
        events_reply
            .send(Ok(json!([
                live_runtime_event(
                    10,
                    "permission",
                    json!({"requestId":"permission-1","options":[{"optionId":"accept"}]})
                ),
                live_runtime_event(
                    11,
                    "execution_completed",
                    json!({"executionId":"execution-1"})
                )
            ])))
            .unwrap();
        let (read, read_reply) = tick_until_backend_call(&mut controller, &calls_rx, 102);
        assert_eq!(read.rpc().method, "read_conversation");
        assert!(controller.state.conversations[&key].permissions.is_empty());
        assert!(!controller.state.conversations[&key].busy);
        assert_eq!(controller.activity_completions.drain(), [key.clone()]);
        read_reply.send(Ok(json!({
            "generation":"generation-1","items":[],"nextCursor":null,
            "load":{"state":"complete"},"freshness":{"stale":false},"readOnly":false,"activeExecutionId":null
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(103, false, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        controller.apply_message(
            BackendMessage::ResyncRequired {
                scope,
                stream: EventStream::Runtime,
            },
            104,
        );
        let (second_events, second_events_reply) = next_backend_call(&calls_rx);
        assert_eq!(second_events.rpc().params["after"], 12);
        second_events_reply.send(Ok(json!([]))).unwrap();
        let (second_read, second_read_reply) =
            tick_until_backend_call(&mut controller, &calls_rx, 105);
        assert_eq!(second_read.rpc().method, "read_conversation");
        second_read_reply.send(Ok(json!({
            "generation":"generation-1","items":[],"nextCursor":null,
            "load":{"state":"complete"},"freshness":{"stale":false},"readOnly":false,"activeExecutionId":null
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(106, false, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(controller.activity_completions.drain().is_empty());
        assert!(controller.state.conversations[&key].permissions.is_empty());
    }

    #[test]
    fn runtime_resync_reconciles_missing_completion_by_execution_id() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 17,
            store_id: Some("store".into()),
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope.clone(), backend);
        let key = acp_key("local", "store", "session");
        controller.state.select(key.clone());
        let conversation = controller.state.conversations.get_mut(&key).unwrap();
        conversation.busy = true;
        conversation.pending_execution_id = Some("execution-9".into());
        controller
            .state
            .catalog
            .push(json!({"storeId":"store","sessionId":"session","workspaceId":"workspace"}));

        controller.apply_message(
            BackendMessage::ResyncRequired {
                scope,
                stream: EventStream::Runtime,
            },
            200,
        );
        let (events, events_reply) = next_backend_call(&calls_rx);
        assert_eq!(events.rpc().method, "events");
        events_reply.send(Ok(json!([]))).unwrap();
        let (execution, execution_reply) = tick_until_backend_call(&mut controller, &calls_rx, 201);
        assert_eq!(execution.rpc().method, "execution");
        assert_eq!(execution.rpc().params["executionId"], "execution-9");
        execution_reply
            .send(Ok(json!({"execution":"completed"})))
            .unwrap();
        let (read, read_reply) = tick_until_backend_call(&mut controller, &calls_rx, 202);
        assert_eq!(read.rpc().method, "read_conversation");
        assert!(!controller.state.conversations[&key].busy);
        assert!(controller.state.conversations[&key]
            .pending_execution_id
            .is_none());
        read_reply.send(Ok(json!({
            "generation":"generation-1","items":[],"nextCursor":null,
            "load":{"state":"complete"},"freshness":{"stale":false},"readOnly":false,"activeExecutionId":null
        }))).unwrap();
        for _ in 0..100 {
            controller.tick(203, false, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn codex_resync_resume_waits_for_submission_ack_and_never_resends_prompt() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 18,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope.clone(), backend);
        controller.state.initialized = true;
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        let prompt = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "one prompt".into(),
            content: Vec::new(),
            options: Value::Null,
        };
        controller.dispatch_prompt(key.clone(), prompt).unwrap();
        let (send, send_reply) = next_backend_call(&calls_rx);
        assert_eq!(send.rpc().method, "send_user_message");
        controller.apply_message(
            BackendMessage::ResyncRequired {
                scope: scope.clone(),
                stream: EventStream::AppServer,
            },
            300,
        );
        assert!(
            calls_rx.try_recv().is_err(),
            "resync must wait for the original submission acknowledgement"
        );
        send_reply
            .send(Ok(json!({"executionId":"execution-1"})))
            .unwrap();
        let (resume, resume_reply) = tick_until_backend_call(&mut controller, &calls_rx, 301);
        assert_eq!(resume.rpc().method, "resume_thread");
        resume_reply
            .send(Ok(
                json!({"thread":{"id":"thread","status":{"type":"idle"},"turns":[]}}),
            ))
            .unwrap();
        for _ in 0..100 {
            controller.tick(302, false, false);
            if !controller.state.conversations[&key].loading {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(controller.state.conversations[&key]
            .pending_execution_id
            .is_none());
        assert!(
            calls_rx.try_recv().is_err(),
            "authoritative resync must not submit the prompt again"
        );
    }

    #[test]
    fn codex_prompt_conversion_preserves_existing_images_and_rejects_other_content() {
        let converted = codex_images_from_content(
            &json!(["data:image/jpeg;base64,existing"]),
            &[json!({"type":"image","mimeType":"image/png","data":"new-png"})],
        )
        .unwrap();
        assert_eq!(
            converted,
            [
                "data:image/jpeg;base64,existing",
                "data:image/png;base64,new-png"
            ]
        );
        assert!(codex_images_from_content(&Value::Null, &[json!({"type":"resource"})]).is_err());
    }

    #[test]
    fn failed_codex_steer_keeps_original_turn_busy_and_queued_followup() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 12,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        controller.state.initialized = true;
        let queued = QueuedPrompt {
            request_id: Uuid::new_v4(),
            text: "queued follow-up".into(),
            content: Vec::new(),
            options: json!({}),
        };
        {
            let conversation = controller.state.conversations.get_mut(&key).unwrap();
            conversation.busy = true;
            conversation.active_turn_id = Some("original-turn".into());
            conversation.pending_execution_id = Some("original-execution".into());
            conversation.queue.push_back(queued.clone());
            conversation.draft = "steer the current turn".into();
        }

        controller
            .handle(Action::Send {
                content: Vec::new(),
                options: json!({}),
                steer: true,
            })
            .unwrap();
        let (request, reply) = next_backend_call(&calls_rx);
        assert_eq!(request.rpc().method, "turn_steer");
        reply
            .send(Err("steer rejected while original turn continues".into()))
            .unwrap();
        for _ in 0..100 {
            controller.tick(100, false, false);
            if !controller.state.conversations[&key].pending_submission {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let conversation = &controller.state.conversations[&key];
        assert!(conversation.busy);
        assert!(!conversation.pending_submission);
        assert_eq!(
            conversation.active_turn_id.as_deref(),
            Some("original-turn")
        );
        assert_eq!(
            conversation.pending_execution_id.as_deref(),
            Some("original-execution")
        );
        assert_eq!(conversation.queue.front(), Some(&queued));
        assert_eq!(conversation.draft, "steer the current turn");
        assert!(
            calls_rx.try_recv().is_err(),
            "no follow-up is submitted early"
        );
    }

    #[test]
    fn failed_new_codex_send_restores_draft_and_returns_to_idle_without_activity_anchors() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 13,
            store_id: None,
        };
        let (calls_tx, calls_rx) = mpsc::channel();
        let backend =
            BackendHandle::spawn(HeldReplyDriver { calls: calls_tx }, scope.clone(), || {});
        let mut controller = Controller::new(scope, backend);
        let key = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };
        controller.state.select(key.clone());
        controller.state.initialized = true;
        controller.state.conversations.get_mut(&key).unwrap().draft = "new prompt".into();
        controller
            .handle(Action::Send {
                content: Vec::new(),
                options: json!({}),
                steer: false,
            })
            .unwrap();
        let (request, reply) = next_backend_call(&calls_rx);
        assert_eq!(request.rpc().method, "send_user_message");
        reply.send(Err("submission rejected".into())).unwrap();
        for _ in 0..100 {
            controller.tick(100, false, false);
            if !controller.state.conversations[&key].pending_submission {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        let conversation = &controller.state.conversations[&key];
        assert!(!conversation.busy);
        assert!(!conversation.pending_submission);
        assert!(conversation.active_turn_id.is_none());
        assert!(conversation.pending_execution_id.is_none());
        assert_eq!(conversation.draft, "new prompt");
        assert!(calls_rx.try_recv().is_err());
    }

    #[test]
    fn codex_send_and_steer_wire_attached_images_while_acp_keeps_native_content() {
        let prompt = super::QueuedPrompt {
            request_id: uuid::Uuid::new_v4(),
            text: "inspect this".into(),
            content: vec![json!({"type":"image","mimeType":"image/png","data":"cG5n"})],
            options: json!({}),
        };
        let codex = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        };

        let send = prompt_request(&codex, &prompt, None).unwrap().rpc();
        assert_eq!(send.method, "send_user_message");
        assert_eq!(send.params["images"][0], "data:image/png;base64,cG5n");

        let steer = prompt_request(&codex, &prompt, Some("turn".into()))
            .unwrap()
            .rpc();
        assert_eq!(steer.method, "turn_steer");
        assert_eq!(steer.params["images"][0], "data:image/png;base64,cG5n");

        let acp = ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Acp {
                workspace_id: "workspace".into(),
                store_id: "store".into(),
                session_id: "session".into(),
            },
        };
        let request = prompt_request(&acp, &prompt, None).unwrap().rpc();
        assert_eq!(request.method, "submit");
        assert_eq!(request.params["prompt"][1]["type"], "image");
        assert_eq!(request.params["prompt"][1]["data"], "cG5n");
    }

    #[test]
    fn workspace_less_history_event_resolves_to_known_scoped_session() {
        let mut state = super::ControllerState::default();
        state.scope = Some(BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 3,
            store_id: Some("store".into()),
        });
        state.selected = Some(ConversationKey {
            endpoint_id: "local".into(),
            conversation: ConversationRef::Acp {
                workspace_id: "workspace".into(),
                store_id: "store".into(),
                session_id: "session".into(),
            },
        });
        let selected = state.selected.clone().unwrap();
        state
            .conversations
            .insert(selected.clone(), super::state::ConversationState::default());
        assert_eq!(
            runtime_event_workspace(&state, "store", "session", None).as_deref(),
            Some("workspace")
        );
        assert_eq!(
            runtime_event_workspace(&state, "store", "unknown", None),
            None
        );
    }

    #[test]
    fn idle_slash_parser_matches_only_supported_command_tokens() {
        assert_eq!(
            parse_slash(" /NeW inspect this "),
            Some((SlashCommand::New, "inspect this".into()))
        );
        assert_eq!(
            parse_slash("/fork"),
            Some((SlashCommand::Fork, String::new()))
        );
        assert_eq!(
            parse_slash("/resume now"),
            Some((SlashCommand::Resume, "now".into()))
        );
        assert_eq!(parse_slash("/newer issue"), None);
        assert_eq!(parse_slash("ordinary /new text"), None);
    }

    #[test]
    fn review_command_targets_follow_codex_textual_syntax() {
        assert_eq!(review_target(""), json!({"type":"uncommittedChanges"}));
        assert_eq!(
            review_target("base main"),
            json!({"type":"baseBranch","branch":"main"})
        );
        assert_eq!(
            review_target("commit deadbeef A fix"),
            json!({"type":"commit","sha":"deadbeef","title":"A fix"})
        );
        assert_eq!(
            review_target("look at this"),
            json!({"type":"custom","instructions":"look at this"})
        );
    }

    #[test]
    fn mcp_elicitation_is_retained_without_thread_id_and_deduplicated() {
        let mut state = super::ControllerState::default();
        let event = json!({"id":"request-1","method":"mcpServer/elicitation/request","params":{"mode":"form","requestedSchema":{"type":"object"}}});
        assert!(capture_codex_elicitation(&mut state, "workspace", &event));
        assert!(capture_codex_elicitation(&mut state, "workspace", &event));
        let requests = state
            .codex_elicitations_by_workspace
            .get("workspace")
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["requestId"], "request-1");
        assert_eq!(requests[0]["params"]["mode"], "form");
    }
}
