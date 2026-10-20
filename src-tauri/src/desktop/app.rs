//! Native composition root. Controllers own transitions; panels emit effects.

mod attachments;
mod acp_connections;
mod browser;
mod codex_controls;
mod composer;
mod daemon_panel;
mod diagnostics;
mod home;
mod host_controls;
pub(crate) mod media;
mod open_in;
mod presentation;
mod recovery;
mod shortcuts;
mod sidebar;
mod suggestions;
mod terminal_dock;
mod theme;
mod update_panel;
mod workspace_dialogs;

use super::{
    content::{ContentAction, ConversationView},
    controller::{Action, Controller, WorkspaceFilesChanged},
    platform::{HostAction, HostResultValue, PlatformService, WindowState},
    workbench::{workspace_relative_event_matches, PanelEffect, Workbench, WorkbenchPanel},
};
use crate::{
    client::{self, contracts::*},
    shared::{
        host_core::{CoreState, HostPaths},
        remote_settings::active_remote_backend,
        ui_state_core::{read_latest_legacy_export, UiStateStore, UiStateV1},
    },
    types::{AppSettings, BackendMode},
};
use egui::{Color32, RichText};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub(crate) fn run(data_dir: Option<PathBuf>) -> Result<(), String> {
    let paths = HostPaths::resolve(data_dir)?;
    let core = Arc::new(CoreState::load(paths.clone())?);
    let settings = core
        .app_settings
        .try_lock()
        .map_err(|e| e.to_string())?
        .clone();
    let ui_store = UiStateStore::new(paths.data_dir.clone());
    let mut presentation = ui_store.load().map_err(|e| e.to_string())?.state;
    if let Some(snapshot) = read_latest_legacy_export(&paths.data_dir).map_err(|e| e.to_string())? {
        let (state, report) = ui_store
            .import_legacy(&snapshot)
            .map_err(|e| e.to_string())?;
        presentation = state;
        if !report.conflicts.is_empty() {
            eprintln!(
                "UI import retained existing fields: {}",
                report.conflicts.join(", ")
            );
        }
    }
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([700.0, 500.0])
            .with_app_id("com.dimillian.codexmonitor.native"),
        ..Default::default()
    };
    eframe::run_native(
        "Codex Monitor",
        options,
        Box::new(move |cc| {
            NativeApp::new(cc, paths, core, settings, presentation)
                .map(|app| Box::new(app) as Box<dyn eframe::App>)
                .map_err(|error| {
                    Box::<dyn std::error::Error + Send + Sync>::from(std::io::Error::other(error))
                })
        }),
    )
    .map_err(|e| e.to_string())
}

struct RuntimeHost {
    handle: tokio::runtime::Handle,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}
impl RuntimeHost {
    fn new() -> Result<Self, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        let (stop, done) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("native-host-services".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let _ = tx.send(Ok(runtime.handle().clone()));
                        runtime.block_on(async {
                            let _ = done.await;
                        });
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error.to_string()));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: rx.recv().map_err(|e| e.to_string())??,
            stop: Some(stop),
        })
    }
}
impl Drop for RuntimeHost {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

enum HostPurpose {
    AddWorkspace,
    Attach(ConversationKey),
    AttachClipboard(ConversationKey),
    ImportUi,
}
enum RootRefresh {
    Workspaces,
    Profiles,
    Catalog,
    Settings,
    CreatedWorkspace(workspace_dialogs::WorkspaceCreationContext),
}
enum BackgroundResult {
    Attachments(ConversationKey, u64, Result<Vec<Value>, String>),
    Media(
        String,
        String,
        u64,
        Result<media::DecodedAttachment, String>,
    ),
    UiSaved(Result<(), String>),
}

#[derive(Clone, Debug)]
struct PendingFileRead {
    workspace_id: String,
    path: String,
    generation: u64,
}

fn preview_refresh_for_change(
    change: &WorkspaceFilesChanged,
    target: Option<&(String, String)>,
    workspace_path: &str,
) -> Option<(String, String)> {
    let (workspace_id, preview_path) = target?;
    if workspace_id != &change.workspace_id
        || !change.paths.iter().any(|path| {
            workspace_relative_event_matches(path, preview_path, workspace_path)
        })
    {
        return None;
    }
    Some((workspace_id.clone(), preview_path.clone()))
}

fn file_read_is_current(
    pending: &PendingFileRead,
    generation: u64,
    target: Option<&(String, String)>,
) -> bool {
    pending.generation == generation
        && target.is_some_and(|target| {
            target.0 == pending.workspace_id && target.1 == pending.path
        })
}

pub(super) struct NativeApp {
    controller: Controller,
    core: Arc<CoreState>,
    paths: HostPaths,
    settings: AppSettings,
    runtime: RuntimeHost,
    platform: PlatformService,
    host_controls: host_controls::HostControls,
    content: ConversationView,
    workbench: Workbench,
    presentation: UiStateV1,
    selected_workspace: Option<String>,
    agent_chooser: Option<String>,
    browser_open: bool,
    browser_search: String,
    browser_profile: Option<String>,
    profile_editor: Option<browser::ProfileEditor>,
    acp_connections: acp_connections::AcpConnections,
    open_id_profile: String,
    open_id_native: String,
    open_id_cwd: String,
    workbench_open: bool,
    terminal_open: bool,
    terminals: HashMap<String, terminal_dock::WorkspaceTerminals>,
    attachments: HashMap<ConversationKey, Vec<Value>>,
    host_pending: HashMap<Uuid, HostPurpose>,
    attachment_pending: HashMap<Uuid, (String, String)>,
    acp_terminal_open: Option<(String, String)>,
    acp_terminal_pending: HashMap<Uuid, String>,
    acp_terminal_output: String,
    acp_terminal_truncated: bool,
    acp_terminal_finished: bool,
    acp_terminal_exit: Option<String>,
    acp_terminal_next_poll_ms: u64,
    background_tx: mpsc::Sender<BackgroundResult>,
    background_rx: mpsc::Receiver<BackgroundResult>,
    connection_epoch: u64,
    popup_open: bool,
    last_ui_save: u64,
    presentation_dirty: bool,
    notices: Vec<String>,
    file_preview: Option<(String, String, String)>,
    file_preview_target: Option<(String, String)>,
    file_preview_generation: u64,
    file_pending: HashMap<Uuid, PendingFileRead>,
    open_in_path: Option<PathBuf>,
    user_input_answers: HashMap<String, String>,
    draft_revisions: HashMap<ConversationKey, u64>,
    recovered_drafts: recovery::CompositionRecovery,
    after_request: HashMap<Uuid, RootRefresh>,
    workspace_dialogs: workspace_dialogs::WorkspaceDialogs,
    host_preferences_open: bool,
    last_geometry: Option<super::platform::WindowState>,
    scroll_anchors: HashMap<ConversationKey, super::content::ScrollAnchor>,
    home: home::HomePanel,
    home_pending: HashMap<Uuid, home::HomeUsageQuery>,
    update_panel: update_panel::UpdatePanel,
    update_open: bool,
    review_target: String,
    suggestions: suggestions::SuggestionState,
    last_presented_conversation: Option<ConversationKey>,
    prompt_history_cursor: presentation::PromptHistoryCursor,
    debug_open: bool,
    quit_requested: bool,
    ime_composing: bool,
    last_tray_update: u64,
    focus_composer: bool,
    frame_metrics: diagnostics::FrameMetrics,
    daemon_panel: daemon_panel::DaemonPanel,
    exit_request: Option<Uuid>,
    graphics_info: String,
}

impl NativeApp {
    fn new(
        cc: &eframe::CreationContext<'_>,
        paths: HostPaths,
        core: Arc<CoreState>,
        settings: AppSettings,
        presentation: UiStateV1,
    ) -> Result<Self, String> {
        theme::apply(&cc.egui_ctx, &settings);
        let graphics_info = cc
            .wgpu_render_state
            .as_ref()
            .map(|state| {
                let info = state.adapter.get_info();
                format!(
                    "{} · {:?} · {:?}",
                    info.name, info.backend, info.device_type
                )
            })
            .unwrap_or_else(|| "Wgpu render state unavailable".into());
        eprintln!("Native renderer: {graphics_info}");
        let runtime = RuntimeHost::new()?;
        let ctx = cc.egui_ctx.clone();
        let platform = PlatformService::new(paths.clone(), runtime.handle.clone(), move || {
            ctx.request_repaint()
        });
        platform.enqueue(host_controls::HostControls::model_status_action(&settings))?;
        if let Ok(window) = platform.load_window_state() {
            restore_window_geometry(&cc.egui_ctx, window);
        }
        let (background_tx, background_rx) = mpsc::channel();
        let scope = scope_for(&settings, &paths, 1);
        let ctx = cc.egui_ctx.clone();
        let backend = make_backend(&core, &settings, scope.clone(), move || {
            ctx.request_repaint()
        })?;
        let controller = Controller::new(scope, backend);
        Ok(Self {
            controller,
            core,
            paths,
            settings,
            runtime,
            platform,
            host_controls: host_controls::HostControls::default(),
            content: ConversationView::default(),
            workbench: Workbench::default(),
            presentation,
            selected_workspace: None,
            agent_chooser: None,
            browser_open: false,
            browser_search: String::new(),
            browser_profile: None,
            profile_editor: None,
            acp_connections: acp_connections::AcpConnections::default(),
            open_id_profile: String::new(),
            open_id_native: String::new(),
            open_id_cwd: String::new(),
            workbench_open: false,
            terminal_open: false,
            terminals: HashMap::new(),
            attachments: HashMap::new(),
            host_pending: HashMap::new(),
            attachment_pending: HashMap::new(),
            acp_terminal_open: None,
            acp_terminal_pending: HashMap::new(),
            acp_terminal_output: String::new(),
            acp_terminal_truncated: false,
            acp_terminal_finished: false,
            acp_terminal_exit: None,
            acp_terminal_next_poll_ms: 0,
            background_tx,
            background_rx,
            connection_epoch: 1,
            popup_open: false,
            last_ui_save: 0,
            presentation_dirty: false,
            notices: Vec::new(),
            file_preview: None,
            file_preview_target: None,
            file_preview_generation: 0,
            file_pending: HashMap::new(),
            open_in_path: None,
            user_input_answers: HashMap::new(),
            draft_revisions: HashMap::new(),
            recovered_drafts: recovery::CompositionRecovery::default(),
            after_request: HashMap::new(),
            workspace_dialogs: workspace_dialogs::WorkspaceDialogs::default(),
            host_preferences_open: false,
            last_geometry: None,
            scroll_anchors: HashMap::new(),
            home: home::HomePanel::new(),
            home_pending: HashMap::new(),
            update_panel: update_panel::UpdatePanel::default(),
            update_open: false,
            review_target: String::new(),
            suggestions: suggestions::SuggestionState::default(),
            last_presented_conversation: None,
            prompt_history_cursor: presentation::PromptHistoryCursor::default(),
            debug_open: false,
            quit_requested: false,
            ime_composing: false,
            last_tray_update: 0,
            focus_composer: false,
            frame_metrics: diagnostics::FrameMetrics::default(),
            daemon_panel: daemon_panel::DaemonPanel::new(),
            exit_request: None,
            graphics_info,
        })
    }

    fn act(&mut self, action: Action) {
        let remember = matches!(&action, Action::SetCodexOption { .. });
        let show_conversation = conversation_action_opens_main_view(&action);
        if matches!(&action, Action::SetDraft(_)) {
            if let Some(key) = &self.controller.state.selected {
                *self.draft_revisions.entry(key.clone()).or_default() += 1;
            }
        }
        if let Err(error) = self.controller.handle(action) {
            self.notices.push(error);
        } else {
            if show_conversation {
                self.workbench_open = false;
            }
            if remember {
                self.remember_codex_controls();
            }
        }
    }
    fn remember_codex_controls(&mut self) {
        let Some(key) = self.controller.state.selected.as_ref() else {
            return;
        };
        let ConversationRef::Codex { workspace_id, .. } = &key.conversation else {
            return;
        };
        if let Some(controls) = self
            .controller
            .state
            .codex_controls_by_workspace
            .get(workspace_id)
        {
            let params = presentation::CodexThreadParams::from_selection(
                controls.selected_model.clone(),
                controls.selected_effort.clone(),
                Some(controls.selected_access_mode.clone()),
                controls.selected_service_tier.clone(),
                controls.selected_collaboration_mode.as_ref(),
            );
            self.presentation_dirty |=
                presentation::save_codex_params(&mut self.presentation, key, params);
        }
    }
    fn restore_codex_controls(&mut self) {
        let selected = self.controller.state.selected.clone();
        if selected == self.last_presented_conversation {
            return;
        }
        self.last_presented_conversation = selected.clone();
        self.prompt_history_cursor.reset();
        self.ime_composing = false;
        let Some(key) = selected else { return };
        let ConversationRef::Codex { workspace_id, .. } = &key.conversation else {
            return;
        };
        let params = presentation::load_codex_params(&self.presentation, &key).unwrap_or_default();
        let controls = self
            .controller
            .state
            .codex_controls_by_workspace
            .entry(workspace_id.clone())
            .or_default();
        controls.selected_model = params.model_id;
        controls.selected_effort = params.effort;
        controls.selected_access_mode = params.access_mode.unwrap_or_else(|| "current".into());
        controls.selected_service_tier = params.service_tier;
        controls.selected_collaboration_mode = controls
            .collaboration_modes
            .iter()
            .find(|mode| {
                mode["id"]
                    .as_str()
                    .or(mode["mode"].as_str())
                    .or(mode["name"].as_str())
                    == params.collaboration_mode_id.as_deref()
            })
            .cloned();
    }
    fn host(&mut self, action: HostAction, purpose: Option<HostPurpose>) {
        match self.platform.enqueue(action) {
            Ok(id) => {
                if let Some(purpose) = purpose {
                    self.host_pending.insert(id, purpose);
                }
            }
            Err(error) => self.notices.push(error),
        }
    }
    fn switch_backend(
        &mut self,
        ctx: &egui::Context,
        mode: BackendMode,
        remote_id: Option<String>,
    ) {
        let previous_settings = self.settings.clone();
        let previous_epoch = self.connection_epoch;
        self.settings.backend_mode = mode;
        if remote_id.is_some() {
            self.settings.active_remote_backend_id = remote_id;
        }
        self.connection_epoch += 1;
        let scope = scope_for(&self.settings, &self.paths, self.connection_epoch);
        let wake = ctx.clone();
        match make_backend(&self.core, &self.settings, scope, move || {
            wake.request_repaint()
        }) {
            Ok(backend) => {
                let next = Controller::new(
                    scope_for(&self.settings, &self.paths, self.connection_epoch),
                    backend,
                );
                let previous_scope = self.controller.scope().clone();
                for (key, conversation) in &self.controller.state.conversations {
                    self.recovered_drafts.preserve(
                        key.clone(),
                        previous_scope.clone(),
                        conversation.draft.clone(),
                        self.attachments.remove(key).unwrap_or_default(),
                    );
                }
                for (key, attachments) in self.attachments.drain() {
                    self.recovered_drafts.preserve(
                        key,
                        previous_scope.clone(),
                        String::new(),
                        attachments,
                    );
                }
                self.controller = next;
                self.content = ConversationView::default();
                self.scroll_anchors.clear();
                self.draft_revisions.clear();
                self.ime_composing = false;
                self.selected_workspace = None;
                self.agent_chooser = None;
                self.profile_editor = None;
                self.browser_profile = None;
                self.open_id_profile.clear();
                self.open_id_native.clear();
                self.open_id_cwd.clear();
                self.workspace_dialogs = workspace_dialogs::WorkspaceDialogs::default();
                self.file_preview = None;
                self.file_preview_target = None;
                self.file_preview_generation = self.file_preview_generation.wrapping_add(1);
                self.open_in_path = None;
                self.user_input_answers.clear();
                self.workbench = Workbench::default();
                self.attachment_pending.clear();
                self.file_pending.clear();
                self.after_request.clear();
                self.host_pending.clear();
                self.home_pending.clear();
                self.suggestions = Default::default();
                self.last_presented_conversation = None;
                self.persist_settings();
            }
            Err(error) => {
                self.settings = previous_settings;
                self.connection_epoch = previous_epoch;
                self.notices.push(error);
            }
        }
    }
    fn persist_settings(&mut self) {
        let request = BackendRequest::Settings(SettingsRequest::mutation(
            "update_app_settings",
            json!({"settings": self.settings}),
            None,
        ));
        match self.controller.request(request) {
            Ok(id) => {
                self.after_request.insert(id, RootRefresh::Settings);
            }
            Err(error) => self
                .notices
                .push(format!("Settings were not saved: {error:?}")),
        }
    }
    fn request_exit(&mut self, ctx: &egui::Context) {
        if self.quit_requested || self.exit_request.is_some() {
            return;
        }
        if self.settings.keep_daemon_running_after_app_close {
            self.quit_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        match self
            .platform
            .enqueue(HostAction::ManagedDaemonShutdownForExit {
                settings: self.settings.clone(),
            }) {
            Ok(id) => self.exit_request = Some(id),
            Err(error) => self
                .notices
                .push(format!("Application shutdown was not queued: {error}")),
        }
    }
    fn panel_effect(&mut self, effect: PanelEffect, scope: &BackendScope) {
        match effect {
            PanelEffect::Request { key, request } => match self.controller.request(request) {
                Ok(id) => self.workbench.track_request(id, key, scope.clone()),
                Err(error) => self
                    .notices
                    .push(format!("Could not queue request: {error:?}")),
            },
            PanelEffect::OpenConversation(key) => self.open_conversation(key),
            PanelEffect::OpenIn { workspace_id, path } => {
                self.prepare_open_in(&workspace_id, &path)
            }
            PanelEffect::InsertPrompt { prompt } => {
                if let Some(key) = self.controller.state.selected.clone() {
                    let inserted = if matches!(&key.conversation, ConversationRef::Codex { .. }) {
                        suggestions::prompt_insert_text(&prompt)
                    } else {
                        prompt["content"].as_str().unwrap_or_default().to_owned()
                    };
                    let previous = self
                        .controller
                        .state
                        .selected_conversation()
                        .map(|c| c.draft.as_str())
                        .unwrap_or_default();
                    self.act(Action::SetDraft(format!(
                        "{previous}{}{inserted}",
                        if previous.is_empty() { "" } else { "\n" }
                    )));
                    self.workbench_open = false;
                    self.focus_composer = true;
                } else {
                    self.notices
                        .push("Select a conversation before inserting a prompt.".into());
                }
            }
        }
    }
    fn add_workspace(&mut self) {
        if matches!(self.settings.backend_mode, BackendMode::Remote) {
            self.workspace_dialogs.begin_remote();
        } else {
            self.host(HostAction::PickDirectory, Some(HostPurpose::AddWorkspace));
        }
    }
    fn open_conversation(&mut self, key: ConversationKey) {
        if key.endpoint_id != self.controller.scope().endpoint_id {
            self.notices
                .push("This conversation belongs to another backend".into());
            return;
        }
        self.workbench_open = false;
        match key.conversation {
            ConversationRef::Codex {
                workspace_id,
                thread_id,
            } => {
                self.selected_workspace = Some(workspace_id.clone());
                self.act(Action::SelectCodex {
                    workspace_id,
                    thread_id,
                });
            }
            ConversationRef::Acp {
                workspace_id,
                store_id,
                session_id,
            } => {
                self.selected_workspace = Some(workspace_id.clone());
                self.act(Action::SelectAcp {
                    workspace_id,
                    store_id,
                    session_id,
                });
            }
        }
    }
    fn process_replies(&mut self, ctx: &egui::Context) {
        let scope = self.controller.scope().clone();
        let mut updated_settings = None;
        for (id, result) in self.controller.take_misc_results() {
            if let Some(_terminal_id) = self.acp_terminal_pending.remove(&id) {
                match result {
                    Ok(value) => {
                        self.acp_terminal_output = value["output"].as_str().unwrap_or_default().to_owned();
                        self.acp_terminal_truncated = value["truncated"].as_bool().unwrap_or(false);
                        self.acp_terminal_finished = value["released"].as_bool().unwrap_or(false)
                            || !value["exitCode"].is_null() || !value["signal"].is_null();
                        self.acp_terminal_exit = if !value["exitCode"].is_null() || !value["signal"].is_null() {
                            Some(format!("exitCode={} signal={}", value["exitCode"], value["signal"]))
                        } else if value["released"].as_bool() == Some(true) { Some("released".into()) } else { None };
                        self.acp_terminal_next_poll_ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64 + 1000;
                    }
                    Err(error) => {
                        self.acp_terminal_output.push_str(&format!("\n[terminal read failed: {}]", error.message));
                        self.acp_terminal_next_poll_ms = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64 + 1000;
                    }
                }
                continue;
            }
            if self.acp_connections.apply_result(id, result.clone(), &mut self.controller) {
                continue;
            }
            if self.suggestions.apply_result(id, result.clone()) {
                continue;
            }
            if let Some(query) = self.home_pending.remove(&id) {
                self.home.complete_usage(
                    &query,
                    result
                        .map_err(|e| e.message)
                        .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string())),
                );
                continue;
            }
            if let Ok(value) = &result {
                if value.get("backendMode").is_some() {
                    if let Ok(settings) = serde_json::from_value::<AppSettings>(value.clone()) {
                        updated_settings = Some(settings);
                    }
                }
            }
            if let Some(refresh) = self.after_request.remove(&id) {
                match result {
                    Ok(value) => match refresh {
                        RootRefresh::Workspaces => self.act(Action::RefreshWorkspaces),
                        RootRefresh::Profiles => self.act(Action::Initialize),
                        RootRefresh::Catalog => self.act(Action::RefreshCatalog),
                        RootRefresh::Settings => {}
                        RootRefresh::CreatedWorkspace(completion) => {
                            self.act(Action::RefreshWorkspaces);
                            if completion.choose_agent_after_create {
                                self.agent_chooser = value["id"].as_str().map(str::to_owned);
                            }
                        }
                    },
                    Err(error) => self.notices.push(error.message),
                }
                continue;
            }
            if let Some((identity, attachment)) = self.attachment_pending.remove(&id) {
                match result {
                    Ok(value) => {
                        let mime = value["mimeType"]
                            .as_str()
                            .unwrap_or("application/octet-stream")
                            .to_owned();
                        if let Some(data) = value["data"]
                            .as_str()
                            .or_else(|| value["contentBase64"].as_str())
                        {
                            self.decode_media(identity, attachment, mime, data.to_owned(), ctx);
                        } else {
                            self.notices
                                .push("Attachment response has no content".into());
                        }
                    }
                    Err(error) => self.notices.push(error.message),
                }
            } else if let Some(pending) = self.file_pending.remove(&id) {
                if !file_read_is_current(
                    &pending,
                    self.file_preview_generation,
                    self.file_preview_target.as_ref(),
                ) {
                    continue;
                }
                match result {
                    Ok(value) => {
                        self.file_preview = Some((
                            pending.workspace_id,
                            pending.path,
                            value["content"]
                                .as_str()
                                .or_else(|| value["text"].as_str())
                                .unwrap_or("")
                                .to_owned(),
                        ))
                    }
                    Err(error) => self.notices.push(error.message),
                }
            } else {
                self.workbench.apply_reply(BackendMessage::Reply {
                    correlation_id: id,
                    scope: scope.clone(),
                    result,
                });
            }
        }
        if let Some(settings) = updated_settings {
            let old = scope_for(&self.settings, &self.paths, self.connection_epoch);
            let old_target = active_remote_backend(&self.settings);
            let new = scope_for(&settings, &self.paths, self.connection_epoch);
            let new_target = active_remote_backend(&settings);
            let reconnect = old.endpoint_id != new.endpoint_id
                || (matches!(settings.backend_mode, BackendMode::Remote)
                    && old_target.token != new_target.token);
            self.settings = settings;
            theme::apply(ctx, &self.settings);
            if reconnect {
                self.switch_backend(
                    ctx,
                    self.settings.backend_mode.clone(),
                    self.settings.active_remote_backend_id.clone(),
                );
            }
        }
        let scope = self.controller.scope().clone();
        for effect in self.workbench.take_effects(&scope) {
            self.panel_effect(effect, &scope);
        }
        for result in self.platform.drain() {
            if self.exit_request == Some(result.request_id) {
                self.exit_request = None;
                match result.result {
                    Ok(_) => {
                        self.quit_requested = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    Err(error) => self.notices.push(format!("The app-managed daemon could not be stopped: {error}. Retry Quit or enable keeping the daemon running in Desktop preferences.")),
                }
                continue;
            }
            self.daemon_panel.handle_result(&result);
            self.update_panel.handle_result(&result);
            let dictation_scope = self.dictation_scope();
            let permission_pending = self.permission_pending();
            for effect in self.host_controls.handle_host_result(
                &result,
                dictation_scope.as_ref(),
                permission_pending,
            ) {
                match effect {
                    host_controls::HostControlsEffect::InsertTranscript { scope, text } => {
                        if self.dictation_scope().as_ref() == Some(&scope) {
                            let previous = self
                                .controller
                                .state
                                .selected_conversation()
                                .map(|c| c.draft.clone())
                                .unwrap_or_default();
                            self.act(Action::SetDraft(format!(
                                "{previous}{}{text}",
                                if previous.is_empty() { "" } else { " " }
                            )));
                        }
                    }
                    host_controls::HostControlsEffect::Notice(notice) => self.notices.push(notice),
                }
            }
            let purpose = self.host_pending.remove(&result.request_id);
            match result.result {
                Ok(HostResultValue::Directory(Some(path))) => {
                    if matches!(purpose, Some(HostPurpose::AddWorkspace)) {
                        let request = BackendRequest::Workspaces(WorkspaceRequest::mutation(
                            "add_workspace",
                            json!({"path":path}),
                            None,
                        ));
                        match self.controller.request(request) {
                            Ok(id) => {
                                self.after_request.insert(id, RootRefresh::Workspaces);
                            }
                            Err(error) => self
                                .notices
                                .push(format!("Workspace request failed: {error:?}")),
                        }
                    }
                }
                Ok(HostResultValue::Files(Some(paths))) => {
                    if let Some(HostPurpose::Attach(key)) = purpose {
                        self.read_attachments(paths, key, ctx);
                    }
                }
                Ok(HostResultValue::ClipboardImage {
                    width,
                    height,
                    rgba,
                }) => {
                    if let Some(HostPurpose::AttachClipboard(key)) = purpose {
                        self.attach_clipboard_image(key, width, height, rgba, ctx);
                    }
                }
                Ok(HostResultValue::WindowAction(action)) => match action {
                    super::platform::WindowAction::HideToTray => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false))
                    }
                    super::platform::WindowAction::Quit | super::platform::WindowAction::Close => {
                        self.request_exit(ctx)
                    }
                },
                Ok(_) => {}
                Err(error) => self.notices.push(error),
            }
        }
        while let Ok(result) = self.background_rx.try_recv() {
            match result {
                BackgroundResult::Attachments(key, epoch, result) => {
                    if epoch != self.connection_epoch {
                        continue;
                    }
                    match result {
                        Ok(blocks) => self.attachments.entry(key).or_default().extend(blocks),
                        Err(error) => self.notices.push(error),
                    }
                }
                BackgroundResult::Media(identity, id, epoch, result) => {
                    if epoch != self.connection_epoch {
                        continue;
                    }
                    match result {
                        Ok(media::DecodedAttachment::Image {
                            width,
                            height,
                            rgba,
                        }) => self
                            .content
                            .cache_image(ctx, &identity, id, width, height, &rgba),
                        Ok(media::DecodedAttachment::Audio { mime, bytes }) => {
                            self.content.cache_audio(&identity, id, mime, bytes)
                        }
                        Ok(media::DecodedAttachment::Text {
                            mime,
                            text,
                            truncated,
                            original_base64,
                        }) => self.content.cache_resource_text_original(
                            &identity,
                            id,
                            mime,
                            text,
                            truncated,
                            original_base64,
                        ),
                        Ok(media::DecodedAttachment::Binary {
                            mime,
                            byte_len,
                            hex_preview,
                            decode_note,
                            original_base64,
                        }) => self.content.cache_resource_binary_original(
                            &identity,
                            id,
                            mime,
                            byte_len,
                            hex_preview,
                            decode_note,
                            original_base64,
                        ),
                        Err(error) => self.notices.push(error),
                    }
                }
                BackgroundResult::UiSaved(result) => {
                    if let Err(error) = result {
                        self.notices.push(error)
                    }
                }
            }
        }
    }

    fn request_file_preview(&mut self, workspace_id: String, path: String) {
        self.file_preview_generation = self.file_preview_generation.wrapping_add(1);
        let generation = self.file_preview_generation;
        self.file_preview_target = Some((workspace_id.clone(), path.clone()));
        let request = BackendRequest::Files(FilesRequest::read(
            "read_workspace_file",
            json!({"workspaceId":workspace_id,"path":path,"maxBytes":1048576}),
        ));
        match self.controller.request(request) {
            Ok(id) => {
                self.file_pending.insert(
                    id,
                    PendingFileRead {
                        workspace_id,
                        path,
                        generation,
                    },
                );
            }
            Err(error) => self
                .notices
                .push(format!("Could not refresh workspace file preview: {error:?}")),
        }
    }

    fn consume_workspace_file_changes(&mut self) {
        for change in self.controller.take_workspace_files_changed() {
            if change.scope != *self.controller.scope() {
                continue;
            }
            let workspace_path = self
                .controller
                .state
                .workspaces
                .iter()
                .find(|workspace| workspace.id == change.workspace_id)
                .map(|workspace| workspace.path.as_str())
                .unwrap_or_default()
                .to_owned();
            self.workbench.mark_workspace_files_changed(
                &change.workspace_id,
                &workspace_path,
                &change.paths,
            );
            if let Some((workspace_id, preview_path)) =
                preview_refresh_for_change(
                    &change,
                    self.file_preview_target.as_ref(),
                    &workspace_path,
                )
            {
                self.request_file_preview(workspace_id, preview_path);
            }
        }
    }
    fn decode_media(
        &self,
        identity: String,
        id: String,
        mime: String,
        data: String,
        ctx: &egui::Context,
    ) {
        let tx = self.background_tx.clone();
        let wake = ctx.clone();
        let epoch = self.connection_epoch;
        self.runtime.handle.spawn_blocking(move || {
            let result = media::decode_inline(&mime, &data)
                .map(|(mime, bytes)| media::decode_attachment(&mime, bytes));
            let _ = tx.send(BackgroundResult::Media(identity, id, epoch, result));
            wake.request_repaint();
        });
    }
    fn dictation_scope(&self) -> Option<host_controls::DictationScope> {
        let key = self.controller.state.selected.clone()?;
        Some(host_controls::DictationScope {
            draft_revision: self.draft_revisions.get(&key).copied().unwrap_or(0),
            conversation: key,
            connection_epoch: self.connection_epoch,
        })
    }
    fn permission_pending(&self) -> bool {
        self.controller
            .state
            .selected_conversation()
            .is_some_and(|c| !c.permissions.is_empty() || !c.user_input_requests.is_empty())
    }
    fn read_attachments(&self, paths: Vec<PathBuf>, key: ConversationKey, ctx: &egui::Context) {
        let capabilities = if matches!(&key.conversation, ConversationRef::Acp { .. }) {
            Some(
                self.controller
                    .state
                    .conversations
                    .get(&key)
                    .and_then(|c| c.session.as_ref())
                    .map(|s| s["capabilities"].clone())
                    .unwrap_or(Value::Null),
            )
        } else {
            None
        };
        let tx = self.background_tx.clone();
        let wake = ctx.clone();
        let epoch = self.connection_epoch;
        self.runtime.handle.spawn_blocking(move || {
            let result = attachments::encode_files(paths, capabilities);
            let _ = tx.send(BackgroundResult::Attachments(key, epoch, result));
            wake.request_repaint();
        });
    }
    fn show_header(&mut self, ui: &mut egui::Ui) {
        // The bar owns the whole row. Nesting another MenuBar for File/View/Help
        // reserves the entire width before the backend and host controls draw.
        show_header_menu_bar(ui, |ui| {
            self.show_menus(ui);
            ui.label(RichText::new("Codex Monitor").strong());
            ui.label(RichText::new("RUST").small().color(theme::ACCENT));
            ui.separator();
            let current = if matches!(self.settings.backend_mode, BackendMode::Remote) {
                active_remote_backend(&self.settings).name
            } else {
                "Local".into()
            };
            egui::ComboBox::new("backend-choice", "Backend")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            matches!(self.settings.backend_mode, BackendMode::Local),
                            "Local",
                        )
                        .clicked()
                    {
                        self.switch_backend(ui.ctx(), BackendMode::Local, None);
                    }
                    for target in self.settings.remote_backends.clone() {
                        if ui
                            .selectable_label(
                                self.settings.active_remote_backend_id.as_deref()
                                    == Some(&target.id)
                                    && matches!(self.settings.backend_mode, BackendMode::Remote),
                                &target.name,
                            )
                            .clicked()
                        {
                            self.switch_backend(ui.ctx(), BackendMode::Remote, Some(target.id));
                        }
                    }
                });
            let compact = ui.available_width() < 600.0;
            show_header_action_layout(ui, compact, |ui| self.show_header_actions(ui));
        });
    }
    fn show_header_actions(&mut self, ui: &mut egui::Ui) {
        if let Some(workspace) = self.selected_workspace.clone() {
            if ui.button("Open in…").clicked() {
                self.prepare_open_in(&workspace, "");
            }
        }
        if ui.button("Settings").clicked() {
            self.workbench_open = true;
            self.workbench.active = WorkbenchPanel::Settings;
        }
        if ui.button("Desktop preferences").clicked() {
            self.host_preferences_open = true;
        }
        if ui.button("About / Updates").clicked() {
            self.update_open = true;
        }
        if ui
            .selectable_label(self.workbench_open, "Workbench")
            .clicked()
        {
            self.workbench_open = !self.workbench_open;
        }
        if ui
            .selectable_label(self.terminal_open, "Terminal")
            .clicked()
        {
            self.terminal_open = !self.terminal_open;
        }
        if ui.button("Home").clicked() {
            self.controller.state.selected = None;
            self.workbench_open = false;
        }
    }
    fn show_main(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.show_terminal(ui);
        if !self.workbench_open && self.controller.state.selected.is_some() {
            let height = self
                .presentation
                .panels
                .get("composerHeight")
                .and_then(Value::as_f64)
                .unwrap_or(200.0) as f32;
            let response = egui::Panel::bottom("composer-dock")
                .resizable(true)
                .default_size(height.clamp(120.0, 600.0))
                .show(ui, |ui| self.show_composer(ui));
            let height = response.response.rect.height();
            if self
                .presentation
                .panels
                .get("composerHeight")
                .and_then(Value::as_f64)
                .is_none_or(|saved| (height - saved as f32).abs() > 1.0)
            {
                self.presentation
                    .panels
                    .insert("composerHeight".into(), json!(height));
                self.presentation_dirty = true;
            }
        }
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(18.0)).show(ui,|ui|{
            if self.workbench_open{
                self.workbench.set_workspace(self.selected_workspace.clone());let scope=self.controller.scope().clone();for effect in self.workbench.show(ui,&scope){self.panel_effect(effect,&scope);}return;
            }
            if let Some(key)=self.controller.state.selected.clone(){
                let conversation=self.controller.state.selected_conversation().map(|c|c.presentation_snapshot()).unwrap_or_default();
                let agent_label=match &key.conversation {ConversationRef::Codex{..}=>"Codex",ConversationRef::Acp{..}=>conversation.session.as_ref().and_then(|session|session["profile"]["name"].as_str()).unwrap_or("ACP agent")};
                ui.horizontal(|ui|{ui.heading(if conversation.title.is_empty(){"Conversation"}else{&conversation.title});ui.weak(agent_label);if ui.button("Refresh conversation").clicked(){self.act(Action::RefreshSelected);}if conversation.loading{ui.spinner();}if conversation.read_only{ui.label("Read-only: another backend owns this session");}});
                if let Some(error)=conversation.last_error{ui.colored_label(theme::ERROR,error);}
                if let Some(coverage)=conversation.coverage{ui.label(RichText::new(if coverage=="agent_replay"{"Agent-supplied conversation"}else{"Observed history only"}).small().color(theme::MUTED));}
                if conversation.stale{ui.colored_label(theme::MUTED,"Cached history is stale");}
                if let Some(output)=&conversation.command_output {ui.group(|ui| {ui.label(output);});}
                let mut render_scope=self.controller.scope().clone();render_scope.store_id=self.controller.state.runtime_store_id.clone().or(render_scope.store_id);let identity=recovery::renderer_identity(&render_scope,&key);let items=self.controller.state.selected_conversation().map(|c|c.items.as_slice()).unwrap_or_default();let revision=self.controller.state.selected_conversation().map(|c|c.items_revision).unwrap_or_default();let response=self.content.show_with_revision(ui,items,&identity,revision);
                if let Some(anchor)=response.scroll_anchor {self.scroll_anchors.insert(key.clone(),anchor);}
                for action in response.actions{match action{
                    ContentAction::Quote{text,..}=>{let draft=self.controller.state.selected_conversation().map(|c|c.draft.clone()).unwrap_or_default();self.act(Action::SetDraft(format!("{draft}{}{}",if draft.is_empty(){""}else{"\n\n"},text)));},
                    ContentAction::OpenLink{url}=>self.host(HostAction::OpenUrl{url},None),
                    ContentAction::OpenFile{path}=>{let workspace_id=match &key.conversation{ConversationRef::Codex{workspace_id,..}|ConversationRef::Acp{workspace_id,..}=>workspace_id.clone()};self.request_file_preview(workspace_id,path);},
                    ContentAction::LoadAttachment{id}=>if let ConversationRef::Acp{session_id,..}=&key.conversation{if !self.attachment_pending.values().any(|pending|pending==&(identity.clone(),id.clone())){if let Ok(request)=self.controller.request(BackendRequest::Runtime(RuntimeRequest::read(RuntimeMethod::ReadConversationAttachment,json!({"sessionId":session_id,"attachmentId":id})))){self.attachment_pending.insert(request,(identity.clone(),id));}}},
                    ContentAction::ReadAcpTerminal{terminal_id}=>if let ConversationRef::Acp{session_id,..}=&key.conversation{self.acp_terminal_open=Some((session_id.clone(),terminal_id.clone()));self.acp_terminal_output.clear();self.acp_terminal_truncated=false;self.acp_terminal_finished=false;self.acp_terminal_exit=None;self.request_acp_terminal(session_id, &terminal_id);},
                    ContentAction::LoadInlineAttachment{id,mime,data}=>self.decode_media(identity.clone(),id,mime,data,&ctx),
                    ContentAction::LoadWorkspaceAttachment{id,path}=>{
                        let workspace_id=match &key.conversation{ConversationRef::Codex{workspace_id,..}|ConversationRef::Acp{workspace_id,..}=>workspace_id};
                        let request=BackendRequest::Files(FilesRequest::read("read_workspace_file_binary",json!({"workspaceId":workspace_id,"path":path})));
                        if let Ok(request_id)=self.controller.request(request){self.attachment_pending.insert(request_id,(identity.clone(),id));}
                    },
                    ContentAction::PlayAudio{mime,bytes,..}=>{let action=self.host_controls.begin_audio(bytes,mime);self.host(action,None);},
                    _=>{},
                }}
            }else{
                let scope=self.controller.scope().clone();
                for effect in self.home.show(ui,&scope,&self.controller.state,self.selected_workspace.as_deref()) {
                    match effect {
                        home::HomeEffect::LoadUsage(query)=>{
                            let request=BackendRequest::Codex(CodexRequest::read("local_usage_snapshot",json!({"days":query.days,"workspacePath":query.workspace_path})));
                            match self.controller.request(request) {Ok(id)=>{self.home_pending.insert(id,query);},Err(error)=>self.notices.push(format!("Usage request was not queued: {error:?}"))}
                        },
                        home::HomeEffect::SelectConversation{workspace_id,thread_id}=>self.act(Action::SelectCodex{workspace_id,thread_id}),
                    }
                }
                ui.add_space(16.0);ui.heading("Workspaces");
                if ui.button("Browse agent sessions").clicked(){self.browser_open=true;self.act(Action::DiscoverAll);}
                if ui.button("Add workspace").clicked(){self.add_workspace();}
                if ui.button("Clone repository URL").clicked(){self.workspace_dialogs.begin_git_url();}
                ui.separator();for workspace in self.controller.state.workspaces.clone(){ui.horizontal(|ui|{ui.strong(&workspace.name);ui.label(&workspace.path);if ui.button("New agent").clicked(){self.agent_chooser=Some(workspace.id.clone());}if ui.button("Recorded runs").clicked(){self.selected_workspace=Some(workspace.id);self.workbench_open=true;self.workbench.active=WorkbenchPanel::Runs;}});}
            }
        });
    }
}

impl eframe::App for NativeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(notice) = theme::take_font_fallback_notice(ctx) {
            self.notices.push(notice);
        }
        let now = now_ms();
        if !self.quit_requested && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.platform.window_close_behavior()
                == super::platform::WindowCloseBehavior::HideToTray
            {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.request_exit(ctx);
            }
        }
        for event in self.platform.drain_tray_events() {
            match event {
                super::platform::TrayEvent::ShowWindow => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                super::platform::TrayEvent::QuitApplication => {
                    self.request_exit(ctx);
                }
                super::platform::TrayEvent::OpenConversation { key } => {
                    match serde_json::from_str::<ConversationKey>(&key) {
                        Ok(key) => {
                            self.open_conversation(key);
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                        Err(error) => self
                            .notices
                            .push(format!("Invalid tray conversation: {error}")),
                    }
                }
            }
        }
        self.handle_shortcuts(ctx);
        let visible = ctx.input(|input| !input.viewport().minimized.unwrap_or(false));
        let previous_history = self
            .controller
            .state
            .selected_conversation()
            .map(|c| c.history_revision);
        self.controller.tick(now, visible, self.popup_open);
        self.consume_workspace_file_changes();
        let next_history = self
            .controller
            .state
            .selected_conversation()
            .map(|c| c.history_revision);
        if previous_history != next_history {
            if let Some(anchor) = self
                .controller
                .state
                .selected
                .as_ref()
                .and_then(|key| self.scroll_anchors.get(key))
            {
                self.content
                    .set_scroll_anchor(anchor.item_id.clone(), anchor.offset);
            }
        }
        self.process_replies(ctx);
        for key in self.controller.take_activity_completions() {
            let is_child = match &key.conversation {
                ConversationRef::Codex {
                    workspace_id,
                    thread_id,
                } => self
                    .controller
                    .state
                    .parents_by_thread
                    .contains_key(&(workspace_id.clone(), thread_id.clone())),
                ConversationRef::Acp { .. } => false,
            };
            if is_child && !self.settings.subagent_system_notifications_enabled {
                continue;
            }
            let title = self
                .controller
                .state
                .conversations
                .get(&key)
                .map(|c| c.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| "Agent conversation".into());
            if self.settings.system_notifications_enabled && !ctx.input(|input| input.focused) {
                self.host(
                    HostAction::Notify {
                        title: "Agent activity ended".into(),
                        body: title,
                    },
                    None,
                );
            }
            if self.settings.notification_sounds_enabled {
                self.host(
                    HostAction::PlayNotificationSound {
                        sound: super::platform::NotificationSound::Success,
                    },
                    None,
                );
            }
        }
        if let Some(effect) = self
            .update_panel
            .automatic_check(self.settings.automatic_app_update_checks_enabled)
        {
            match self.platform.enqueue(effect.action) {
                Ok(id) => self.update_panel.track_request(id, effect.purpose),
                Err(error) => self.notices.push(error),
            }
        }
        if now.saturating_sub(self.last_tray_update) > 10000
            && self.platform.tray_availability().is_available()
        {
            self.last_tray_update = now;
            let mut recent = self
                .controller
                .state
                .conversations
                .iter()
                .map(|(key, c)| {
                    (
                        c.last_refresh_ms.unwrap_or_default(),
                        super::platform::TrayConversation {
                            key: serde_json::to_string(key).unwrap_or_default(),
                            title: if c.title.is_empty() {
                                "Conversation".into()
                            } else {
                                c.title.clone()
                            },
                            workspace: match &key.conversation {
                                ConversationRef::Codex { workspace_id, .. }
                                | ConversationRef::Acp { workspace_id, .. } => self
                                    .controller
                                    .state
                                    .workspaces
                                    .iter()
                                    .find(|w| w.id == *workspace_id)
                                    .map(|w| w.name.clone())
                                    .unwrap_or_default(),
                            },
                        },
                    )
                })
                .collect::<Vec<_>>();
            recent.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
            let recent = recent
                .into_iter()
                .take(6)
                .map(|(_, entry)| entry)
                .collect::<Vec<_>>();
            let usage = super::platform::TrayUsageSummary {
                short_window: self.home.tray_usage_summary(),
                long_window: None,
            };
            if let Err(error) = self.platform.update_tray_menu(&recent, &usage) {
                self.notices.push(error);
            }
        }
        self.restore_codex_controls();
        let dropped = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        if !dropped.is_empty() {
            if let Some(key) = self.controller.state.selected.clone() {
                if self
                    .controller
                    .state
                    .selected_conversation()
                    .is_some_and(|c| !c.read_only)
                {
                    self.read_attachments(dropped, key, ctx);
                }
            }
        }
        let dictation_scope = self.dictation_scope();
        let permission_pending = self.permission_pending();
        for action in self.host_controls.hold_to_talk(
            ctx,
            &self.settings,
            dictation_scope,
            permission_pending,
        ) {
            self.host(action, None);
        }
        if let Some(key) = &self.controller.state.selected {
            self.selected_workspace = Some(match &key.conversation {
                ConversationRef::Codex { workspace_id, .. }
                | ConversationRef::Acp { workspace_id, .. } => workspace_id.clone(),
            });
        }
        if self.presentation_dirty && now.saturating_sub(self.last_ui_save) > 1500 {
            self.last_ui_save = now;
            self.presentation_dirty = false;
            let mut state = self.presentation.clone();
            let store = UiStateStore::new(self.paths.data_dir.clone());
            let tx = self.background_tx.clone();
            let wake = ctx.clone();
            self.runtime.handle.spawn_blocking(move || {
                let result = store.save(&mut state).map_err(|e| e.to_string());
                let _ = tx.send(BackgroundResult::UiSaved(result));
                wake.request_repaint();
            });
        }
        let geometry = ctx.input(|input| {
            let viewport = input.viewport();
            let rect = viewport.outer_rect;
            super::platform::WindowState {
                x: rect.map(|r| r.min.x),
                y: rect.map(|r| r.min.y),
                width: viewport.inner_rect.map(|r| r.width()),
                height: viewport.inner_rect.map(|r| r.height()),
                maximized: viewport.maximized.unwrap_or(false),
            }
        });
        if self.last_geometry != Some(geometry) {
            self.last_geometry = Some(geometry);
            if let Err(error) = self.platform.save_window_state(geometry) {
                self.notices.push(error);
            }
        }
        ctx.request_repaint_after(Duration::from_millis(
            if self.host_controls.needs_frequent_repaint() {
                33
            } else {
                1500
            },
        ));
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.popup_open = false;
        self.frame_metrics.record(frame.info().cpu_usage);
        let ctx = ui.ctx().clone();
        egui::Panel::top("app-header")
            .exact_size(42.0)
            .show(ui, |ui| self.show_header(ui));
        self.show_sidebar(ui);
        self.show_main(ui);
        self.show_browser(&ctx);
        self.show_agent_chooser(&ctx);
        self.show_profile_editor(&ctx);
        self.show_acp_terminal(&ctx);
        let mut acp_connections = std::mem::take(&mut self.acp_connections);
        acp_connections.show(self, &ctx);
        self.acp_connections = acp_connections;
        self.show_diagnostics(ui);
        if let Some(workspace_id) = self.selected_workspace.clone() {
            let forms = self
                .controller
                .state
                .codex_elicitations_by_workspace
                .get(&workspace_id)
                .cloned()
                .unwrap_or_default();
            if !forms.is_empty() {
                let key = ConversationKey {
                    endpoint_id: self.controller.scope().endpoint_id.clone(),
                    conversation: ConversationRef::Codex {
                        workspace_id,
                        thread_id: "workspace-forms".into(),
                    },
                };
                egui::Window::new("MCP server information request")
                    .show(&ctx, |ui| self.show_elicitation(ui, &key, &forms));
            }
        }
        if self.controller.state.review_prompt_open {
            let mut open = true;
            egui::Window::new("Choose review target").open(&mut open).show(&ctx,|ui|{
                ui.label("Review uncommitted changes, a base branch, or a commit.");
                ui.add(egui::TextEdit::singleline(&mut self.review_target).hint_text("base main / commit <SHA> / custom instructions"));
                if ui.button("Review working tree").clicked() { self.review_target="working tree".into(); }
                if ui.button("Start review").clicked() {
                    let target=if self.review_target=="working tree" {json!({"type":"uncommittedChanges"})} else if let Some(branch)=self.review_target.strip_prefix("base ") {json!({"type":"baseBranch","branch":branch})} else if let Some(commit)=self.review_target.strip_prefix("commit ") {json!({"type":"commit","sha":commit})} else {json!({"type":"custom","instructions":self.review_target})};
                    if let Some(ConversationKey{conversation:ConversationRef::Codex{workspace_id,thread_id},..})=&self.controller.state.selected {
                        let request=BackendRequest::Codex(CodexRequest::mutation("start_review",json!({"workspaceId":workspace_id,"threadId":thread_id,"target":target,"delivery":"inline"}),None));
                        if let Err(error)=self.controller.request(request) {self.notices.push(format!("Review was not queued: {error:?}"));}
                    }
                    self.act(Action::SetReviewPromptOpen(false));
                }
            });
            if !open {
                self.act(Action::SetReviewPromptOpen(false));
            }
        }
        if self.update_open {
            let mut open = true;
            egui::Window::new("About and native updates")
                .open(&mut open)
                .default_size([640.0, 600.0])
                .show(&ctx, |ui| {
                    for effect in self.update_panel.show(ui) {
                        match self.platform.enqueue(effect.action) {
                            Ok(id) => self.update_panel.track_request(id, effect.purpose),
                            Err(error) => self.notices.push(error),
                        }
                    }
                });
            self.update_open = open;
        }
        for effect in self.workspace_dialogs.show(&ctx) {
            let workspace_dialogs::WorkspaceDialogEffect::Request {
                request,
                completion,
            } = effect;
            match self.controller.request(request) {
                Ok(id) => {
                    self.after_request
                        .insert(id, RootRefresh::CreatedWorkspace(completion));
                }
                Err(error) => self
                    .notices
                    .push(format!("Workspace creation was not queued: {error:?}")),
            }
        }
        if self.host_preferences_open {
            let mut open = true;
            egui::Window::new("Desktop preferences")
                .open(&mut open)
                .default_size([660.0, 650.0])
                .vscroll(true)
                .show(&ctx, |ui| {
                    if host_controls::HostControls::show_preferences(ui, &mut self.settings) {
                        self.persist_settings();
                    }
                    ui.separator();
                    for effect in self.daemon_panel.show(ui, &self.settings) {
                        match self.platform.enqueue(effect.action) {
                            Ok(id) => self.daemon_panel.track(id, effect.purpose),
                            Err(error) => self.notices.push(error),
                        }
                    }
                });
            self.host_preferences_open = open;
        }
        self.show_open_in(&ctx);
        if let Some((workspace, path, text)) = self.file_preview.clone() {
            let mut open = true;
            egui::Window::new(&path)
                .open(&mut open)
                .default_size([760.0, 540.0])
                .show(&ctx, |ui| {
                    if ui.button("Open in…").clicked() {
                        self.prepare_open_in(&workspace, &path);
                    }
                    let mut text = text.as_str();
                    egui::ScrollArea::both().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut text)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY),
                        );
                    });
                });
            if !open {
                self.file_preview = None;
                self.file_preview_target = None;
                self.file_preview_generation = self.file_preview_generation.wrapping_add(1);
            }
        }
        if let Some(error) = self.controller.state.error.clone() {
            if !self.notices.contains(&error) {
                self.notices.push(error);
            }
        }
        if !self.notices.is_empty() {
            egui::Window::new("Application messages")
                .anchor(egui::Align2::RIGHT_BOTTOM, [-18.0, -18.0])
                .resizable(true)
                .show(&ctx, |ui| {
                    for notice in &self.notices {
                        ui.colored_label(theme::ERROR, notice);
                    }
                    if ui.button("Dismiss").clicked() {
                        self.notices.clear();
                        self.controller.state.error = None;
                    }
                });
        }
        if self.exit_request.is_some() {
            egui::Window::new("Closing application")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(&ctx, |ui| {
                    ui.spinner();
                    ui.label("Stopping the app-managed daemon…");
                });
        }
        self.popup_open |= egui::Popup::is_any_open(&ctx) || self.agent_chooser.is_some();
    }
    fn on_exit(&mut self) {
        if self.presentation_dirty {
            if let Err(error) =
                UiStateStore::new(self.paths.data_dir.clone()).save(&mut self.presentation)
            {
                eprintln!("Native UI state was not saved on exit: {error}");
            }
        }
        for terminal in self.terminals.values_mut() {
            terminal.close_all();
        }
        // The local core may still own ACP clients even when the selected UI
        // backend is remote. Shut down only this process-local registry; the
        // managed daemon has its own exit path.
        let runtime_shutdown = self
            .runtime
            .handle
            .block_on(self.core.runtime_registry.shutdown(self.paths.run_store_path.clone()));
        if let Err(error) = runtime_shutdown {
            eprintln!("Local ACP runtime shutdown did not complete cleanly: {error}");
        }
    }
}

fn conversation_action_opens_main_view(action: &Action) -> bool {
    matches!(
        action,
        Action::SelectCodex { .. }
            | Action::SelectAcp { .. }
            | Action::OpenCatalog { .. }
            | Action::OpenSessionById { .. }
            | Action::CreateConversation { .. }
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RestoredWindowGeometry {
    position: Option<[f32; 2]>,
    size: Option<[f32; 2]>,
    maximized: bool,
}

fn restored_window_geometry(state: WindowState) -> RestoredWindowGeometry {
    // Desktop APIs use signed virtual-desktop coordinates; negative positions
    // are valid when a secondary monitor sits left or above the primary one.
    // The generous limit rejects corrupt values without excluding normal
    // multi-monitor arrangements.
    const MAX_POSITION: f32 = 65_536.0;
    const MIN_WIDTH: f32 = 700.0;
    const MIN_HEIGHT: f32 = 500.0;
    const MAX_DIMENSION: f32 = 16_384.0;

    let position = match (state.x, state.y) {
        (Some(x), Some(y))
            if x.is_finite()
                && y.is_finite()
                && x.abs() <= MAX_POSITION
                && y.abs() <= MAX_POSITION =>
        {
            Some([x, y])
        }
        _ => None,
    };
    let size = match (state.width, state.height) {
        (Some(width), Some(height))
            if width.is_finite()
                && height.is_finite()
                && width > 0.0
                && height > 0.0
                && width <= MAX_DIMENSION
                && height <= MAX_DIMENSION =>
        {
            Some([width.max(MIN_WIDTH), height.max(MIN_HEIGHT)])
        }
        _ => None,
    };

    RestoredWindowGeometry {
        position,
        size,
        maximized: state.maximized,
    }
}

fn restore_window_geometry(ctx: &egui::Context, state: WindowState) {
    let geometry = restored_window_geometry(state);
    if let Some([x, y]) = geometry.position {
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
    }
    if let Some([width, height]) = geometry.size {
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, height)));
    }
    if geometry.maximized {
        ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
    }
}

fn show_header_menu_bar(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::MenuBar::new().ui(ui, content);
}

fn show_header_action_layout(
    ui: &mut egui::Ui,
    compact: bool,
    actions: impl FnOnce(&mut egui::Ui),
) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if compact {
            ui.menu_button("Panels", actions);
        } else {
            actions(ui);
        }
    });
}

#[cfg(test)]
mod window_restore_tests {
    use super::{restored_window_geometry, RestoredWindowGeometry};
    use crate::desktop::platform::WindowState;

    #[test]
    fn restores_negative_multimonitor_position_size_and_maximized_state() {
        assert_eq!(
            restored_window_geometry(WindowState {
                x: Some(-1920.0),
                y: Some(-120.0),
                width: Some(640.0),
                height: Some(480.0),
                maximized: true,
            }),
            RestoredWindowGeometry {
                position: Some([-1920.0, -120.0]),
                size: Some([700.0, 500.0]),
                maximized: true,
            }
        );
    }

    #[test]
    fn ignores_nonfinite_or_implausible_geometry_without_losing_maximized_state() {
        assert_eq!(
            restored_window_geometry(WindowState {
                x: Some(f32::INFINITY),
                y: Some(20.0),
                width: Some(20_000.0),
                height: Some(800.0),
                maximized: true,
            }),
            RestoredWindowGeometry {
                position: None,
                size: None,
                maximized: true,
            }
        );
    }

    #[test]
    fn accepts_positionless_geometry_and_rejects_partial_position() {
        assert_eq!(
            restored_window_geometry(WindowState {
                x: Some(10.0),
                width: Some(1200.0),
                height: Some(800.0),
                ..WindowState::default()
            }),
            RestoredWindowGeometry {
                position: None,
                size: Some([1200.0, 800.0]),
                maximized: false,
            }
        );
    }
}

#[cfg(test)]
mod header_layout_tests {
    use super::{show_header_action_layout, show_header_menu_bar};
    use egui::{accesskit::Role, Vec2};
    use egui_kittest::{kittest::Queryable as _, Harness};
    use std::{cell::Cell, rc::Rc};

    fn verify_header_at_width(width: f32, compact: bool) {
        let preferences_clicked = Rc::new(Cell::new(false));
        let backend_changed = Rc::new(Cell::new(false));
        let preferences_state = preferences_clicked.clone();
        let backend_state = backend_changed.clone();
        let mut harness = Harness::new_ui(move |ui| {
            show_header_menu_bar(ui, |ui| {
                let _ = ui.menu_button("File", |ui| {
                    let _ = ui.button("New workspace…");
                });
                let _ = ui.menu_button("View", |ui| {
                    let _ = ui.button("Settings");
                });
                let _ = ui.menu_button("Help", |ui| {
                    let _ = ui.button("About / Updates…");
                });
                ui.label("Codex Monitor");
                ui.separator();
                egui::ComboBox::new("backend-choice-test", "Backend")
                    .selected_text("Local")
                    .show_ui(ui, |ui| {
                        ui.selectable_label(false, "Local");
                        if ui.selectable_label(false, "Remote test").clicked() {
                            backend_state.set(true);
                        }
                    });
                let is_compact = ui.available_width() < 600.0;
                show_header_action_layout(ui, is_compact, |ui| {
                    if ui.button("Desktop preferences").clicked() {
                        preferences_state.set(true);
                    }
                });
            });
        });
        harness.set_size(Vec2::new(width, 42.0));
        harness.run();

        assert_eq!(
            harness.query_by_label("Panels").is_some(),
            compact,
            "compact action placement should match width {width}"
        );
        let backend = harness.get_by_role_and_label(Role::ComboBox, "Backend");
        backend.click();
        harness.run();
        harness.get_by_label("Remote test").click();
        harness.run();
        assert!(
            backend_changed.get(),
            "backend chooser remains clickable at width {width}"
        );

        if compact {
            harness.get_by_label("Panels").click();
            harness.run();
            assert!(harness.query_by_label("Desktop preferences").is_some());
        }
        harness.get_by_label("Desktop preferences").click();
        harness.run();
        assert!(
            preferences_clicked.get(),
            "Desktop preferences remains clickable at width {width}"
        );
    }

    #[test]
    fn backend_and_host_controls_remain_visible_and_clickable_in_header_layout() {
        verify_header_at_width(700.0, true);
        verify_header_at_width(1200.0, false);
    }
}

#[cfg(test)]
mod workspace_file_change_tests {
    use super::{file_read_is_current, preview_refresh_for_change, PendingFileRead};
    use crate::{client::BackendScope, desktop::controller::WorkspaceFilesChanged};

    fn change(workspace: &str, paths: &[&str]) -> WorkspaceFilesChanged {
        WorkspaceFilesChanged {
            scope: BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 3,
                store_id: Some("store".into()),
            },
            store_id: "store".into(),
            session_id: "session".into(),
            workspace_id: workspace.into(),
            paths: paths.iter().map(|path| (*path).into()).collect(),
        }
    }

    #[test]
    fn only_matching_open_workspace_path_requests_preview_refresh() {
        let target = Some(("workspace-a".into(), "src/main.rs".into()));
        assert_eq!(
            preview_refresh_for_change(&change("workspace-a", &["src/main.rs"]), target.as_ref(), "/work/repo"),
            target
        );
        assert_eq!(
            preview_refresh_for_change(&change("workspace-a", &["src/other.rs"]), target.as_ref(), "/work/repo"),
            None
        );
        assert_eq!(
            preview_refresh_for_change(&change("workspace-b", &["src/main.rs"]), target.as_ref(), "/work/repo"),
            None
        );
        assert_eq!(
            preview_refresh_for_change(&change("workspace-a", &[]), target.as_ref(), "/work/repo"),
            None,
            "additional-root writes invalidate panels but cannot target a workspace preview"
        );
    }

    #[test]
    fn absolute_file_uri_refresh_matches_reported_workspace_but_not_another_root() {
        let target = Some(("workspace-a".into(), "file:///work/repo/src/main%20file.rs".into()));
        assert_eq!(
            preview_refresh_for_change(
                &change("workspace-a", &["src/main file.rs"]),
                target.as_ref(),
                "/work/repo",
            ),
            target
        );
        assert_eq!(
            preview_refresh_for_change(
                &change("workspace-a", &["src/main file.rs"]),
                Some(&("workspace-a".into(), "/work/other/src/main file.rs".into())),
                "/work/repo",
            ),
            None
        );
    }

    #[test]
    fn stale_preview_read_cannot_replace_a_newer_target_or_generation() {
        let target = Some(("workspace-a".into(), "src/main.rs".into()));
        let pending = PendingFileRead {
            workspace_id: "workspace-a".into(),
            path: "src/main.rs".into(),
            generation: 5,
        };
        assert!(file_read_is_current(&pending, 5, target.as_ref()));
        assert!(!file_read_is_current(&pending, 6, target.as_ref()));
        assert!(!file_read_is_current(&pending, 5, Some(&("workspace-a".into(), "README.md".into()))));
        assert!(!file_read_is_current(&pending, 5, None));
    }
}

fn scope_for(settings: &AppSettings, paths: &HostPaths, epoch: u64) -> BackendScope {
    let endpoint_id = if matches!(settings.backend_mode, BackendMode::Remote) {
        use sha2::{Digest, Sha256};
        let target = active_remote_backend(settings);
        let host_hash = format!("{:x}", Sha256::digest(target.host.trim().as_bytes()));
        format!("remote:{}:{}", target.id, &host_hash[..16])
    } else {
        format!("local:{}", paths.data_dir.display())
    };
    BackendScope {
        endpoint_id,
        connection_epoch: epoch,
        store_id: None,
    }
}
fn make_backend(
    core: &Arc<CoreState>,
    settings: &AppSettings,
    scope: BackendScope,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<client::BackendHandle, String> {
    if matches!(settings.backend_mode, BackendMode::Remote) {
        let target = active_remote_backend(settings);
        client::remote::spawn_remote_with_host(
            client::remote::RemoteConfig {
                host: target.host,
                token: target.token,
            },
            core.clone(),
            scope,
            wake,
        )
    } else {
        client::local::spawn_local(core.clone(), scope, wake)
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
