use super::{
    mutate_codex, mutate_runs, mutate_runtime, mutate_workspace, read_codex, read_runs,
    read_runtime, read_workspace, PanelEffect, WorkbenchRequestKey,
};
use crate::client::contracts::{
    BackendError, BackendScope, ConversationKey, ConversationRef, RuntimeMethod,
};
use crate::desktop::content::{DiffMode, DiffView};
use base64::Engine as _;
use egui::{self, Ui};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct CreateRunIntent {
    request_id: String,
    workspace_id: String,
    scope: BackendScope,
    title: String,
    prompt: String,
    mode: String,
    instances: Vec<Value>,
}

#[derive(Debug)]
struct ImageJobCompletion {
    intent: CreateRunIntent,
    images: Result<Vec<String>, String>,
}

#[derive(Clone, Debug)]
struct ActiveLaunch {
    run: Value,
    images: Vec<String>,
    index: usize,
    workspace_id: String,
    phase: LaunchPhase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaunchPhase {
    Begin,
    AwaitWorktree,
    AwaitSetup,
    AwaitConnect,
    AwaitThread,
    AwaitRuntimeSession,
    AwaitSubmit,
    AwaitFailure,
}

#[derive(Debug, Default)]
pub(crate) struct RunsPanel {
    workspace: Option<String>,
    pub(crate) runs: Vec<Value>,
    cursor: Option<String>,
    loading: bool,
    selected_left: String,
    selected_right: String,
    left: Option<Value>,
    right: Option<Value>,
    comparisons: Vec<Value>,
    choice: String,
    notes: String,
    revision: u64,
    dirty: bool,
    status: Option<String>,
    error: Option<String>,
    tab: usize,
    changed: bool,
    turn_id: String,
    diff_view: DiffView,
    diff_revision: u64,
    comparison_pair: Option<(String, String)>,
    turns_by_instance: std::collections::HashMap<String, Vec<Value>>,
    profiles: Vec<Value>,
    profiles_stale: bool,
    codex_models: Vec<Value>,
    models_loading: bool,
    workspaces: HashMap<String, Value>,
    workspaces_stale: bool,
    prompt_draft: String,
    title_draft: String,
    image_paths_draft: String,
    pending_images: Vec<String>,
    image_job: Option<Receiver<ImageJobCompletion>>,
    run_mode: String,
    model_draft: String,
    effort_draft: String,
    selected_agents: BTreeSet<String>,
    queued: VecDeque<PanelEffect>,
    active_launch: Option<ActiveLaunch>,
    creating: bool,
    creation_request_id: Option<String>,
}

impl RunsPanel {
    pub(crate) fn reset_workspace(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn change_workspace(&mut self) {
        self.runs.clear();
        self.cursor = None;
        self.loading = false;
        self.selected_left.clear();
        self.selected_right.clear();
        self.left = None;
        self.right = None;
        self.comparisons.clear();
        self.comparison_pair = None;
        self.turns_by_instance.clear();
        self.changed = true;
        self.workspaces_stale = true;
        self.profiles_stale = true;
        self.codex_models.clear();
        self.models_loading = false;
    }
    pub(crate) fn take_effects(&mut self) -> Vec<PanelEffect> {
        self.queued.drain(..).collect()
    }
    pub(crate) fn poll_image_job(&mut self, scope: &BackendScope) {
        let Some(polled) = self.image_job.as_ref().map(Receiver::try_recv) else {
            return;
        };
        let completion = match polled {
            Ok(completion) => completion,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.image_job = None;
                self.creating = false;
                self.status = None;
                self.error = Some("Image preparation stopped unexpectedly. No run was submitted; check the paths and try again.".into());
                return;
            }
        };
        self.image_job = None;
        if &completion.intent.scope != scope {
            self.creating = false;
            self.status =
                Some("Image preparation finished for an earlier backend connection.".into());
            self.error = Some("The backend changed while images were being read. No run was submitted; review the paths and try again.".into());
            return;
        }
        match completion.images {
            Ok(images) => {
                self.pending_images = images.clone();
                self.queued
                    .push_back(create_run_effect(completion.intent, images));
                self.status = Some("Saving run record before starting agents…".into());
            }
            Err(error) => {
                self.creating = false;
                self.error = Some(error);
                self.status = None;
            }
        }
    }
    pub(crate) fn has_pending_launch(&self) -> bool {
        self.active_launch.is_some() || self.creating
    }
    pub(crate) fn mark_scope_interrupted(&mut self) {
        if self.has_pending_launch() {
            self.active_launch = None;
            self.creating = false;
            self.image_job = None;
            self.queued.clear();
            self.status = Some("Backend connection changed during run creation or launch. The saved record may need reconciliation on its original backend.".into());
            self.error = Some("Run launch stopped because the backend scope changed; no new requests were sent to the new backend.".into());
        }
    }
    pub(crate) fn mark_stale(&mut self) {
        self.changed = true;
        self.profiles_stale = true;
        self.workspaces_stale = true;
        self.codex_models.clear();
        self.models_loading = false;
    }

    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace: Option<&str>,
        scope: &BackendScope,
    ) {
        if self.selected_agents.is_empty() {
            self.selected_agents.insert("codex".into());
        }
        if self.run_mode.is_empty() {
            self.run_mode = "local".into();
        }
        if self.workspace.as_deref() != workspace {
            self.workspace = workspace.map(str::to_owned);
            self.runs.clear();
            self.cursor = None;
            self.changed = true;
            self.profiles_stale = true;
            self.workspaces_stale = true;
            self.codex_models.clear();
            self.models_loading = false;
            self.selected_left.clear();
            self.selected_right.clear();
            self.left = None;
            self.right = None;
        }
        self.poll_image_job(scope);
        ui.horizontal(|ui| {
            ui.heading("Recorded runs and comparisons");
            if ui.button("Refresh").clicked() {
                self.changed = true;
            }
            if self.loading {
                ui.spinner();
            }
        });
        while let Some(effect) = self.queued.pop_front() {
            effects.push(effect);
        }
        if self.profiles_stale {
            effects.push(PanelEffect::Request {
                key: WorkbenchRequestKey::RunProfiles,
                request: read_runtime(RuntimeMethod::Profiles, json!({})),
            });
            self.profiles_stale = false;
        }
        if self.workspaces_stale {
            effects.push(PanelEffect::Request {
                key: WorkbenchRequestKey::RunWorkspaces,
                request: read_workspace("list_workspaces", json!({})),
            });
            self.workspaces_stale = false;
        }
        self.show_create_run(ui, effects, workspace, scope);
        if self.changed && !self.loading {
            self.runs.clear();
            self.cursor = None;
            effects.push(PanelEffect::Request {
                key: WorkbenchRequestKey::RunsList,
                request: read_runs(
                    "list_runs",
                    json!({"workspaceId":workspace,"cursor":null,"limit":50}),
                ),
            });
            self.loading = true;
            self.changed = false;
        }
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if self.runs.is_empty() && !self.loading {
            ui.label("No recorded runs for this workspace.");
        }
        let visible_runs = self.runs.clone();
        egui::ScrollArea::vertical().id_salt("native-runs-list").max_height(270.0).show(ui, |ui| {
            for run in &visible_runs {
                let run_id = s(run, "id");
                egui::CollapsingHeader::new(format!("{} · {} instance(s)", s(run, "title"), run["instances"].as_array().map_or(0, Vec::len)))
                    .id_salt(("run", run_id)).show(ui, |ui| {
                        ui.label(format!("Workspace: {} · Base: {}", s(run,"workspaceName"), opt(&run["baseOid"])));
                        ui.label(format!("Original prompt: {}", s(run,"prompt")));
                        if ui.button("Reconcile saved run").clicked() {
                            effects.push(PanelEffect::Request { key: WorkbenchRequestKey::RunReconcile(run_id.to_owned()), request: mutate_runs("reconcile_run", json!({"runId":run_id})) });
                        }
                        if let Some(instances) = run["instances"].as_array() {
                            for instance in instances {
                                let instance_id = s(instance,"id");
                                ui.group(|ui| {
                                    ui.label(format!("{} #{} · {} · {}", instance["requested"]["runtimeName"].as_str().unwrap_or("Codex"), instance["ordinal"].as_u64().unwrap_or(0)+1, s(instance,"submissionOutcome"), s(instance,"executionOutcome")));
                                    ui.label(format!("{} · setup {}", s(instance,"preparation"), s(&instance["setup"],"status")));
                                    if !instance["error"].is_null() { ui.colored_label(egui::Color32::LIGHT_RED, s(instance,"error")); }
                                    if let Some(thread) = instance["threadId"].as_str() {
                                        if ui.small_button("Open conversation").clicked() {
                                            if let Some(workspace_id) = instance["workspaceId"].as_str() {
                                                effects.push(PanelEffect::OpenConversation(ConversationKey { endpoint_id: scope.endpoint_id.clone(), conversation: ConversationRef::Codex { workspace_id: workspace_id.to_owned(), thread_id: thread.to_owned() }}));
                                            }
                                        }
                                    } else if let Some(session) = instance["sessionId"].as_str() {
                                        if ui.small_button("Open runtime conversation").clicked() {
                                            if let (Some(workspace_id),Some(store_id))=(instance["workspaceId"].as_str(),run["storeId"].as_str()) {
                                                effects.push(PanelEffect::OpenConversation(ConversationKey { endpoint_id: scope.endpoint_id.clone(), conversation: ConversationRef::Acp { workspace_id: workspace_id.to_owned(), store_id: store_id.to_owned(), session_id: session.to_owned() }}));
                                            }
                                        }
                                    }
                                    if let Some(captures) = instance["captures"].as_array() {
                                        for cap in captures {
                                            ui.horizontal(|ui| {
                                                ui.label(format!("{} · {}{}", s(cap,"label"), s(cap,"kind"), if cap["complete"] == false {" · incomplete"} else {""}));
                                                let id=s(cap,"id");
                                                if ui.small_button("Compare left").clicked() { self.select_capture(true,id,effects); }
                                                if ui.small_button("Compare right").clicked() { self.select_capture(false,id,effects); }
                                            });
                                        }
                                    }
                                    ui.horizontal(|ui| {
                                        if ui.button("Load turns").clicked() {
                                            let (request,key)=if let Some(session_id)=instance["sessionId"].as_str(){(read_runtime(RuntimeMethod::History,json!({"sessionId":session_id})),WorkbenchRequestKey::RunTurns(instance_id.to_owned()))}else{(read_runs("list_run_turns",json!({"instanceId":instance_id})),WorkbenchRequestKey::RunTurns(instance_id.to_owned()))};
                                            effects.push(PanelEffect::Request{key,request});
                                        }
                                        ui.label("Capture a later turn ID:");
                                        let turns=self.turns_by_instance.get(instance_id).cloned().unwrap_or_default();
                                        if !turns.is_empty() { egui::ComboBox::from_id_salt(("later-turn",instance_id)).selected_text(if self.turn_id.is_empty(){"Select turn"}else{self.turn_id.as_str()}).show_ui(ui,|ui|for turn in &turns{let id=turn["executionId"].as_str().or(turn["id"].as_str()).unwrap_or_default();ui.selectable_value(&mut self.turn_id,id.to_owned(),format!("{} · {}",id,turn["execution"].as_str().or(turn["status"].as_str()).unwrap_or("turn")));}); }
                                        if turns.is_empty(){ui.text_edit_singleline(&mut self.turn_id);}
                                        if ui.button("Capture").clicked() && !self.turn_id.trim().is_empty() {
                                            let req=if let Some(session_id)=instance["sessionId"].as_str(){mutate_runtime(RuntimeMethod::Capture,json!({"sessionId":session_id,"instanceId":instance_id,"executionId":self.turn_id.trim(),"requestId":Uuid::new_v4().to_string(),"label":"Later execution"}))}else{mutate_runs("capture_run_instance", json!({"instanceId":instance_id,"turnId":self.turn_id.trim(),"requestId":Uuid::new_v4().to_string(),"label":"Later turn"}))};
                                            effects.push(PanelEffect::Request { key: WorkbenchRequestKey::RunCaptureLater(instance_id.to_owned()), request: req });
                                        }
                                    });
                                });
                            }
                        }
                    });
            }
        });
        if self.cursor.is_some()
            && ui
                .add_enabled(!self.loading, egui::Button::new("Load more runs"))
                .clicked()
        {
            effects.push(PanelEffect::Request {
                key: WorkbenchRequestKey::RunsList,
                request: read_runs(
                    "list_runs",
                    json!({"workspaceId":workspace,"cursor":self.cursor,"limit":50}),
                ),
            });
            self.loading = true;
        }
        ui.separator();
        ui.horizontal(|ui| {
            if capture_picker(ui, "Left capture", &self.runs, &mut self.selected_left) {
                self.pair_changed(effects);
                self.load_capture(effects, true);
            }
            if capture_picker(ui, "Right capture", &self.runs, &mut self.selected_right) {
                self.pair_changed(effects);
                self.load_capture(effects, false);
            }
            if ui.button("Load comparison").clicked() {
                self.load_capture(effects, true);
                self.load_capture(effects, false);
            }
        });
        if let Some(left) = self.left.clone() {
            self.show_comparison(ui, effects, left);
        }
    }

    fn needs_codex_model(&self) -> bool {
        self.selected_agents.contains("codex")
            || self.profiles.iter().any(|profile| {
                profile["kind"] == "codex"
                    && profile["id"]
                        .as_str()
                        .is_some_and(|id| self.selected_agents.contains(id))
            })
    }

    fn show_create_run(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace: Option<&str>,
        scope: &BackendScope,
    ) {
        ui.collapsing("Create recorded run", |ui| {
            ui.label("The initial prompt and selected agent requests are recorded before launch.");
            ui.label("Prompt");
            ui.add(egui::TextEdit::multiline(&mut self.prompt_draft).desired_rows(4));
            ui.label("Image file paths (one per line; images are embedded before the backend request)");
            ui.add(egui::TextEdit::multiline(&mut self.image_paths_draft).desired_rows(2).code_editor());
            ui.horizontal(|ui| {
                ui.label("Title"); ui.text_edit_singleline(&mut self.title_draft);
                egui::ComboBox::from_id_salt("run-mode-native").selected_text(&self.run_mode).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.run_mode, "local".into(), "Local (one agent)");
                    ui.selectable_value(&mut self.run_mode, "worktree".into(), "Worktree (parallel agents)");
                });
            });
            if self.needs_codex_model() {
            ui.horizontal(|ui| {
                ui.label("Codex model");
                egui::ComboBox::from_id_salt("run-codex-model").selected_text(if self.model_draft.is_empty() {"Codex default"} else {&self.model_draft}).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.model_draft, String::new(), "Codex default");
                    for model in &self.codex_models {
                        let id = model["model"].as_str().or(model["id"].as_str()).unwrap_or_default();
                        if !id.is_empty() { ui.selectable_value(&mut self.model_draft, id.to_owned(), model["displayName"].as_str().unwrap_or(id)); }
                    }
                });
                let model_button = if self.codex_models.is_empty() { "Load Codex models" } else { "Refresh Codex models" };
                if ui.add_enabled(workspace.is_some() && !self.models_loading, egui::Button::new(model_button)).clicked() {
                    if let Some(workspace_id) = workspace {
                        effects.push(PanelEffect::Request {
                            key: WorkbenchRequestKey::RunCodexModels,
                            request: read_codex("model_list", json!({"workspaceId":workspace_id})),
                        });
                        self.models_loading = true;
                    }
                }
                if self.models_loading { ui.spinner(); }
                if self.codex_models.is_empty() && !self.models_loading {
                    ui.weak("Codex default remains available; load the catalog only if you want to choose a specific model.");
                }
                ui.label("Effort");
                egui::ComboBox::from_id_salt("run-codex-effort").selected_text(if self.effort_draft.is_empty() {"default"} else {&self.effort_draft}).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.effort_draft, String::new(), "default");
                    for level in ["low","medium","high","xhigh"] { ui.selectable_value(&mut self.effort_draft, level.into(), level); }
                });
            });
            }
            ui.label("Agents");
            let mut candidates = vec![("codex".to_owned(), "Codex (native)".to_owned())];
            candidates.extend(self.profiles.iter().filter_map(|profile| {
                let id = profile["id"].as_str()?;
                Some((id.to_owned(), format!("{} · {}", profile["name"].as_str().unwrap_or(id), profile["kind"].as_str().unwrap_or("ACP"))))
            }));
            for (id, label) in candidates {
                let mut selected = self.selected_agents.contains(&id);
                if ui.checkbox(&mut selected, label).changed() {
                    if self.run_mode == "local" && selected { self.selected_agents.clear(); }
                    if selected { self.selected_agents.insert(id.clone()); } else { self.selected_agents.remove(&id); }
                }
            }
            if self.profiles.is_empty() { ui.weak("No saved runtime profiles are available yet; use Settings to add one, or select native Codex."); }
            if self.run_mode == "local" && self.selected_agents.len() > 1 { ui.colored_label(egui::Color32::YELLOW, "Local runs allow one agent; selecting an agent clears the previous selection."); }
            if self.creating { ui.spinner(); ui.label("Creating the durable run record…"); }
            if self.image_job.is_some() { ui.spinner(); ui.label("Reading and preparing image attachments…"); }
            if let Some(status) = &self.status { ui.label(status); }
            let enabled = !self.creating && self.image_job.is_none() && (!self.prompt_draft.trim().is_empty() || !self.image_paths_draft.trim().is_empty()) && !self.selected_agents.is_empty() && (self.run_mode != "local" || self.selected_agents.len() == 1);
            if ui.add_enabled(enabled, egui::Button::new("Record and launch run")).clicked() {
                let Some(workspace_id) = workspace else { self.error = Some("Select a workspace before creating a run.".into()); return; };
                let request_id = self.creation_request_id.get_or_insert_with(|| Uuid::new_v4().to_string()).clone();
                let paths = self.image_paths_draft.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect::<Vec<_>>();
                if self.prompt_draft.trim().is_empty() && paths.is_empty() {
                    self.error = Some("Enter a prompt or attach at least one image.".into());
                    return;
                }
                let instances: Vec<Value> = self.selected_agents.iter().map(|agent| {
                    if agent == "codex" {
                        json!({"model": null_if_empty(&self.model_draft), "modelId": null_if_empty(&self.model_draft), "modelLabel": if self.model_draft.trim().is_empty() {"Codex default"} else {self.model_draft.trim()}, "effort": null_if_empty(&self.effort_draft)})
                    } else {
                        let profile = self.profiles.iter().find(|p| p["id"].as_str() == Some(agent.as_str())).cloned().unwrap_or(Value::Null);
                        if profile["kind"] == "codex" {
                            json!({"model":null_if_empty(&self.model_draft),"modelId":null_if_empty(&self.model_draft),"modelLabel":if self.model_draft.trim().is_empty(){"Codex default"}else{self.model_draft.trim()},"effort":null_if_empty(&self.effort_draft),"runtimeProfileId":agent,"runtimeName":profile["name"],"runtime":{"profileId":agent,"profileRevision":profile["revision"],"kind":profile["kind"]}})
                        } else {
                            json!({"modelLabel":"Agent default","runtimeProfileId":agent,"runtimeName":profile["name"],"runtime":{"profileId":agent,"profileRevision":profile["revision"],"kind":profile["kind"]}})
                        }
                    }
                }).collect();
                let intent = CreateRunIntent {
                    request_id,
                    workspace_id: workspace_id.to_owned(),
                    scope: scope.clone(),
                    title: if self.title_draft.trim().is_empty() { build_run_title(&self.prompt_draft) } else { self.title_draft.trim().to_owned() },
                    prompt: self.prompt_draft.trim().to_owned(),
                    mode: self.run_mode.clone(),
                    instances,
                };
                self.creating = true;
                self.error = None;
                if paths.is_empty() {
                    self.queued.push_back(create_run_effect(intent, Vec::new()));
                    self.status = Some("Saving run record before starting agents…".into());
                } else {
                    let (sender, receiver) = mpsc::channel();
                    self.image_job = Some(receiver);
                    self.status = Some("Preparing image attachments before saving the run…".into());
                    std::thread::spawn(move || {
                        let paths = paths.join("\n");
                        let images = encode_image_paths(&paths);
                        let _ = sender.send(ImageJobCompletion { intent, images });
                    });
                }
            }
        });
    }

    fn load_capture(&mut self, effects: &mut Vec<PanelEffect>, left: bool) {
        let id = if left {
            self.selected_left.clone()
        } else {
            self.selected_right.clone()
        };
        if id.is_empty() {
            return;
        }
        effects.push(PanelEffect::Request {
            key: WorkbenchRequestKey::RunCapture(id.clone()),
            request: read_runs("get_run_capture", json!({"captureId":id})),
        });
        effects.push(PanelEffect::Request {
            key: WorkbenchRequestKey::RunComparisons(id.clone()),
            request: read_runs("list_run_comparisons", json!({"captureId":id})),
        });
    }

    fn select_capture(&mut self, left: bool, id: &str, effects: &mut Vec<PanelEffect>) {
        let selected = if left {
            &mut self.selected_left
        } else {
            &mut self.selected_right
        };
        if selected != id {
            *selected = id.to_owned();
            self.pair_changed(effects);
        }
        self.load_capture(effects, left);
    }

    fn pair_changed(&mut self, effects: &mut Vec<PanelEffect>) {
        let pair = (self.selected_left.clone(), self.selected_right.clone());
        if self.comparison_pair.as_ref() != Some(&pair) {
            self.comparison_pair = Some(pair);
            self.notes.clear();
            self.choice = "undecided".into();
            self.revision = 0;
            self.dirty = false;
            self.error = None;
            self.status = None;
            if !self.selected_left.is_empty() {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::RunComparisons(self.selected_left.clone()),
                    request: read_runs(
                        "list_run_comparisons",
                        json!({"captureId":self.selected_left}),
                    ),
                });
            }
        }
    }

    fn show_comparison(&mut self, ui: &mut Ui, effects: &mut Vec<PanelEffect>, left: Value) {
        ui.heading("Capture evidence");
        ui.label(format!(
            "{} · {} · {}",
            s(&left, "label"),
            s(&left, "status"),
            if left["complete"] == false {
                "Incomplete evidence"
            } else {
                "Complete evidence"
            }
        ));
        ui.horizontal(|ui| {
            for (idx, name) in ["Output", "Settings", "Commands", "Tokens", "Files"]
                .iter()
                .enumerate()
            {
                if ui.selectable_label(self.tab == idx, *name).clicked() {
                    self.tab = idx;
                }
            }
        });
        let evidence = match self.tab {
            0 => json!(left["items"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter(|x| x["kind"] == "message" || x["type"] == "agentMessage")
                .cloned()
                .collect::<Vec<_>>()),
            1 => json!({"observed":left["observed"],"usage":left["usage"]}),
            2 => json!(left["items"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .filter(|x| x["kind"] == "tool" || x["type"] == "commandExecution")
                .cloned()
                .collect::<Vec<_>>()),
            3 => left["usage"].clone(),
            _ => {
                json!({"files":left["git"]["files"],"baseOid":left["git"]["baseOid"],"headOid":left["git"]["headOid"],"errors":left["git"]["errors"]})
            }
        };
        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                ui.monospace(serde_json::to_string_pretty(&evidence).unwrap_or_default())
            });
        if self.tab == 4 {
            for file in left["git"]["files"].as_array().into_iter().flatten() {
                let path = file["path"].as_str().unwrap_or("changed file");
                ui.collapsing(path, |ui| {
                    let identity = format!("run-capture-{}-{path}", s(&left, "id"));
                    if let Some(patch) = file["diff"].as_str() {
                        self.diff_view.show_text_versioned(
                            ui,
                            &identity,
                            patch,
                            self.diff_revision,
                            DiffMode::Unified,
                        );
                    } else {
                        self.diff_view.show_versioned(
                            ui,
                            &identity,
                            &file["diff"],
                            self.diff_revision,
                            DiffMode::Unified,
                        );
                    }
                });
            }
        }
        let right = self.right.clone();
        if let Some(right) = right.filter(|r| s(r, "id") != s(&left, "id")) {
            ui.separator();
            ui.label(format!(
                "Right capture: {} · {}",
                s(&right, "label"),
                s(&right, "status")
            ));
            ui.horizontal(|ui| {
                for (key, label) in [
                    ("undecided", "Undecided"),
                    ("left", "Left"),
                    ("right", "Right"),
                    ("tie", "Tie"),
                ] {
                    if ui.selectable_label(self.choice == key, label).clicked() {
                        self.choice = key.to_owned();
                        self.dirty = true;
                    }
                }
            });
            ui.label("Comparison notes (your draft is retained if the stored revision changed):");
            if ui
                .add(egui::TextEdit::multiline(&mut self.notes).desired_rows(4))
                .changed()
            {
                self.dirty = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Save comparison").clicked() {
                    effects.push(PanelEffect::Request { key: WorkbenchRequestKey::RunSaveComparison,
                        request: mutate_runs("save_run_comparison",json!({"leftCaptureId":s(&left,"id"),"rightCaptureId":s(&right,"id"),"choice":self.choice,"notes":self.notes,"expectedRevision":self.revision})) });
                }
                if ui.button("Reload saved notes").clicked() { self.load_capture(effects,true); self.load_capture(effects,false); self.dirty=false; }
                if !self.dirty { if let Some(status)=&self.status {ui.label(status);} }
                if let Some(error)=&self.error { ui.colored_label(egui::Color32::LIGHT_RED,error); }
            });
        }
    }

    pub(crate) fn apply(&mut self, key: &WorkbenchRequestKey, value: Value) {
        match key {
            WorkbenchRequestKey::RunProfiles => {
                self.profiles = value["profiles"].as_array().cloned().unwrap_or_default();
            }
            WorkbenchRequestKey::RunCodexModels => {
                self.models_loading = false;
                self.codex_models = value["result"]["data"]
                    .as_array()
                    .or(value["data"].as_array())
                    .cloned()
                    .unwrap_or_default();
            }
            WorkbenchRequestKey::RunWorkspaces => {
                let rows = value
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| value["workspaces"].as_array().cloned().unwrap_or_default());
                self.workspaces = rows
                    .into_iter()
                    .filter_map(|w| Some((w["id"].as_str()?.to_owned(), w)))
                    .collect();
            }
            WorkbenchRequestKey::RunCreate => {
                self.creating = false;
                self.creation_request_id = None;
                self.prompt_draft.clear();
                self.title_draft.clear();
                self.image_paths_draft.clear();
                self.status = Some(format!(
                    "Run {} recorded; preparing its instances…",
                    s(&value, "id")
                ));
                self.changed = true;
                let workspace_id = s(&value, "workspaceId").to_owned();
                self.active_launch = Some(ActiveLaunch {
                    run: value,
                    images: std::mem::take(&mut self.pending_images),
                    index: 0,
                    workspace_id,
                    phase: LaunchPhase::Begin,
                });
                self.advance_launch();
            }
            WorkbenchRequestKey::RunCreateWorktree(instance) => {
                if let Some(active) = self
                    .active_launch
                    .as_mut()
                    .filter(|a| instance_id(&a.run, a.index) == instance.as_str())
                {
                    active.workspace_id = s(&value, "id").to_owned();
                    self.workspaces
                        .insert(active.workspace_id.clone(), value.clone());
                    active.phase = LaunchPhase::AwaitSetup;
                    let has_setup = value["settings"]["worktreeSetupScript"]
                        .as_str()
                        .is_some_and(|script| !script.trim().is_empty());
                    let observation = if has_setup {
                        json!({"status":"dispatch_failed","message":"The native workbench did not dispatch the configured worktree setup script."})
                    } else {
                        json!({"status":"not_configured"})
                    };
                    self.queued.push_back(PanelEffect::Request {
                        key: WorkbenchRequestKey::RunCreateSetup(instance.clone()),
                        request: mutate_runs(
                            "record_run_setup",
                            json!({"instanceId":instance,"observation":observation}),
                        ),
                    });
                }
            }
            WorkbenchRequestKey::RunCreateSetup(instance) => {
                if let Some(active) = self
                    .active_launch
                    .as_mut()
                    .filter(|a| instance_id(&a.run, a.index) == instance.as_str())
                {
                    active.phase = LaunchPhase::Begin;
                    self.launch_agent();
                }
            }
            WorkbenchRequestKey::RunCreateConnect(instance) => {
                if let Some(active) = self
                    .active_launch
                    .as_mut()
                    .filter(|a| instance_id(&a.run, a.index) == instance.as_str())
                {
                    active.phase = LaunchPhase::AwaitThread;
                    self.queue_start_thread(instance);
                }
            }
            WorkbenchRequestKey::RunCreateCodexThread(instance) => {
                if let Some(active) = self
                    .active_launch
                    .as_mut()
                    .filter(|a| instance_id(&a.run, a.index) == instance.as_str())
                {
                    let thread = value
                        .as_str()
                        .or(value["threadId"].as_str())
                        .or(value["thread"]["id"].as_str())
                        .or(value["result"]["thread"]["id"].as_str())
                        .or(value["data"]["thread"]["id"].as_str())
                        .unwrap_or_default()
                        .to_owned();
                    if thread.is_empty() {
                        self.fail_instance(
                            instance,
                            "Thread creation returned no thread ID".into(),
                        );
                    } else {
                        active.phase = LaunchPhase::AwaitSubmit;
                        self.queue_codex_submit(instance, thread);
                    }
                }
            }
            WorkbenchRequestKey::RunCreateRuntimeSession(instance) => {
                if let Some(active) = self
                    .active_launch
                    .as_mut()
                    .filter(|a| instance_id(&a.run, a.index) == instance.as_str())
                {
                    let session = value["sessionId"].as_str().unwrap_or_default().to_owned();
                    if session.is_empty() {
                        self.fail_instance(instance, "Runtime created no session ID".into());
                    } else {
                        active.phase = LaunchPhase::AwaitSubmit;
                        self.queue_runtime_submit(instance, session);
                    }
                }
            }
            WorkbenchRequestKey::RunCreateCodexSubmit(_)
            | WorkbenchRequestKey::RunCreateRuntimeSubmit(_) => {
                self.finish_instance();
            }
            WorkbenchRequestKey::RunCreateFailure(_) => self.finish_instance(),
            WorkbenchRequestKey::RunsList => {
                let runs = value["runs"].as_array().cloned().unwrap_or_default();
                self.runs.extend(runs);
                self.cursor = value["nextCursor"].as_str().map(str::to_owned);
                self.loading = false;
            }
            WorkbenchRequestKey::RunCapture(id) => {
                if id == &self.selected_left || id == &self.selected_right {
                    if s(&value, "id") == self.selected_left {
                        self.left = Some(value.clone());
                        self.diff_revision = self.diff_revision.wrapping_add(1);
                    }
                    if s(&value, "id") == self.selected_right {
                        self.right = Some(value);
                        self.diff_revision = self.diff_revision.wrapping_add(1);
                    }
                }
            }
            WorkbenchRequestKey::RunComparisons(id)
                if id == &self.selected_left || id == &self.selected_right =>
            {
                self.comparisons = value.as_array().cloned().unwrap_or_default();
                if let Some(c) = self.comparisons.iter().find(|c| {
                    s(c, "leftCaptureId") == self.selected_left
                        && s(c, "rightCaptureId") == self.selected_right
                }) {
                    if !self.dirty {
                        self.choice = s(c, "choice").to_owned();
                        self.notes = s(c, "notes").to_owned();
                    }
                    self.revision = c["revision"].as_u64().unwrap_or_default();
                }
            }
            WorkbenchRequestKey::RunReconcile(_) => {
                if let Some(run) = value.get("run") {
                    self.replace_run(s(run, "id"), run.clone());
                }
                self.changed = true;
            }
            WorkbenchRequestKey::RunSaveComparison => {
                self.revision = value["revision"].as_u64().unwrap_or(self.revision);
                self.dirty = false;
                self.status = Some("Comparison saved".into());
                self.error = None;
            }
            WorkbenchRequestKey::RunCaptureLater(_) => {
                self.status = Some("Later capture saved".into());
                self.changed = true;
            }
            WorkbenchRequestKey::RunTurns(instance) => {
                self.turns_by_instance.insert(
                    instance.clone(),
                    value["executions"]
                        .as_array()
                        .or(value.as_array())
                        .cloned()
                        .unwrap_or_default(),
                );
            }
            _ => {}
        }
    }
    pub(crate) fn apply_error(&mut self, key: &WorkbenchRequestKey, error: &BackendError) -> bool {
        match key {
            WorkbenchRequestKey::RunCreate => {
                self.creating = false;
                self.status = Some("Run record was not created; no agent was started.".into());
                self.error = Some(error.message.clone());
                return true;
            }
            WorkbenchRequestKey::RunCreateWorktree(instance)
            | WorkbenchRequestKey::RunCreateSetup(instance)
            | WorkbenchRequestKey::RunCreateConnect(instance)
            | WorkbenchRequestKey::RunCreateCodexThread(instance)
            | WorkbenchRequestKey::RunCreateCodexSubmit(instance)
            | WorkbenchRequestKey::RunCreateRuntimeSession(instance)
            | WorkbenchRequestKey::RunCreateRuntimeSubmit(instance) => {
                self.fail_instance(instance, error.message.clone());
                return true;
            }
            WorkbenchRequestKey::RunCreateFailure(_) => {
                self.error = Some(format!(
                    "Could not record launch failure: {}",
                    error.message
                ));
                self.finish_instance();
                return true;
            }
            _ => {}
        }
        if matches!(key, WorkbenchRequestKey::RunsList) {
            self.loading = false;
        }
        if matches!(key, WorkbenchRequestKey::RunCodexModels) {
            self.models_loading = false;
        }
        if matches!(key, WorkbenchRequestKey::RunSaveComparison)
            && (error.code == "revision_conflict"
                || error.message.to_ascii_lowercase().contains("conflict"))
        {
            self.error=Some("The saved comparison changed elsewhere. Your draft is still here; use Reload saved notes to compare revisions.".into());
            return true;
        }
        self.error = Some(error.message.clone());
        true
    }

    fn advance_launch(&mut self) {
        let Some(active) = self.active_launch.as_mut() else {
            return;
        };
        let Some(instance) = active.run["instances"]
            .as_array()
            .and_then(|a| a.get(active.index))
            .cloned()
        else {
            self.active_launch = None;
            self.creating = false;
            self.status = Some("Run instances have been prepared and submitted.".into());
            self.changed = true;
            return;
        };
        let id = s(&instance, "id").to_owned();
        if active.phase != LaunchPhase::Begin {
            return;
        }
        if s(&active.run, "mode") == "worktree" {
            active.phase = LaunchPhase::AwaitWorktree;
            let branch = format!(
                "monitor-run-{}-{}",
                s(&active.run, "id").chars().take(8).collect::<String>(),
                active.index + 1
            );
            self.queued.push_back(PanelEffect::Request { key: WorkbenchRequestKey::RunCreateWorktree(id), request: mutate_workspace("add_worktree", json!({"parentId":s(&active.run,"workspaceId"),"branch":branch,"name":branch,"copyAgentsMd":true,"runContext":{"runId":s(&active.run,"id"),"instanceId":s(&instance,"id")}})) });
        } else {
            self.launch_agent();
        }
    }

    fn launch_agent(&mut self) {
        let Some(active) = self.active_launch.as_mut() else {
            return;
        };
        let Some(instance) = active.run["instances"]
            .as_array()
            .and_then(|a| a.get(active.index))
            .cloned()
        else {
            return;
        };
        let id = s(&instance, "id").to_owned();
        if let Some(profile_id) = instance["requested"]["runtimeProfileId"].as_str() {
            active.phase = LaunchPhase::AwaitRuntimeSession;
            let revision = instance["requested"]["runtime"]["profileRevision"]
                .as_u64()
                .unwrap_or_default();
            self.queued.push_back(PanelEffect::Request { key: WorkbenchRequestKey::RunCreateRuntimeSession(id), request: mutate_runtime(RuntimeMethod::CreateSession, json!({"workspaceId":active.workspace_id,"profileId":profile_id,"expectedProfileRevision":revision})) });
            return;
        }
        let connected = self
            .workspaces
            .get(&active.workspace_id)
            .and_then(|w| w["connected"].as_bool())
            .unwrap_or(false);
        if !connected {
            active.phase = LaunchPhase::AwaitConnect;
            self.queued.push_back(PanelEffect::Request {
                key: WorkbenchRequestKey::RunCreateConnect(id),
                request: mutate_workspace("connect_workspace", json!({"id":active.workspace_id})),
            });
        } else {
            active.phase = LaunchPhase::AwaitThread;
            self.queue_start_thread(&id);
        }
    }

    fn queue_start_thread(&mut self, instance_id: &str) {
        let Some(active) = self.active_launch.as_ref() else {
            return;
        };
        self.queued.push_back(PanelEffect::Request { key: WorkbenchRequestKey::RunCreateCodexThread(instance_id.into()), request: mutate_codex("start_thread", json!({"workspaceId":active.workspace_id,"runContext":{"runId":s(&active.run,"id"),"instanceId":instance_id}})) });
    }

    fn queue_codex_submit(&mut self, instance_id: &str, thread_id: String) {
        let Some(active) = self.active_launch.as_ref() else {
            return;
        };
        let Some(instance) = active.run["instances"]
            .as_array()
            .and_then(|a| a.get(active.index))
        else {
            return;
        };
        self.queued.push_back(PanelEffect::Request { key: WorkbenchRequestKey::RunCreateCodexSubmit(instance_id.into()), request: mutate_codex("send_user_message", json!({"workspaceId":active.workspace_id,"threadId":thread_id,"text":s(&active.run,"prompt"),"images":active.images,"model":instance["requested"]["model"],"effort":instance["requested"]["effort"],"serviceTier":instance["requested"]["serviceTier"],"runContext":{"runId":s(&active.run,"id"),"instanceId":instance_id}})) });
    }

    fn queue_runtime_submit(&mut self, instance_id: &str, session_id: String) {
        let Some(active) = self.active_launch.as_ref() else {
            return;
        };
        let Some(instance) = active.run["instances"]
            .as_array()
            .and_then(|a| a.get(active.index))
        else {
            return;
        };
        let options = if instance["requested"]["runtime"]["kind"] == "codex" {
            json!({"model":instance["requested"]["model"],"effort":instance["requested"]["effort"],"serviceTier":instance["requested"]["serviceTier"]})
        } else {
            json!({})
        };
        let mut prompt = Vec::new();
        if !s(&active.run, "prompt").is_empty() {
            prompt.push(json!({"type":"text","text":s(&active.run,"prompt")}));
        }
        let is_acp = instance["requested"]["runtime"]["kind"] != "codex";
        for image in &active.images {
            if let Some((mime, data)) = decode_data_url(image) {
                if is_acp {
                    prompt.push(json!({"type":"image","mimeType":mime,"data":data}));
                } else {
                    prompt.push(json!({"type":"image","url":image}));
                }
            }
        }
        self.queued.push_back(PanelEffect::Request { key: WorkbenchRequestKey::RunCreateRuntimeSubmit(instance_id.into()), request: mutate_runtime(RuntimeMethod::Submit, json!({"requestId":Uuid::new_v4().to_string(),"sessionId":session_id,"prompt":prompt,"options":options,"runContext":{"runId":s(&active.run,"id"),"instanceId":instance_id}})) });
    }

    fn fail_instance(&mut self, instance_id: &str, message: String) {
        let Some(active) = self.active_launch.as_mut() else {
            self.error = Some(message);
            return;
        };
        active.phase = LaunchPhase::AwaitFailure;
        self.error = Some(message.clone());
        self.queued.push_back(PanelEffect::Request {
            key: WorkbenchRequestKey::RunCreateFailure(instance_id.into()),
            request: mutate_runs(
                "record_run_failure",
                json!({"instanceId":instance_id,"message":message}),
            ),
        });
    }

    fn finish_instance(&mut self) {
        if let Some(active) = self.active_launch.as_mut() {
            active.index += 1;
            active.phase = LaunchPhase::Begin;
        }
        self.advance_launch();
    }
    fn replace_run(&mut self, id: &str, run: Value) {
        if let Some(pos) = self.runs.iter().position(|r| s(r, "id") == id) {
            self.runs[pos] = run;
        }
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn null_if_empty(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        Value::String(value.trim().to_owned())
    }
}
fn create_run_effect(intent: CreateRunIntent, images: Vec<String>) -> PanelEffect {
    PanelEffect::Request {
        key: WorkbenchRequestKey::RunCreate,
        request: mutate_runs(
            "create_run",
            json!({
                "requestId":intent.request_id,
                "workspaceId":intent.workspace_id,
                "title":intent.title,
                "prompt":intent.prompt,
                "mode":intent.mode,
                "images":images,
                "instances":intent.instances
            }),
        ),
    }
}

fn encode_image_paths(paths: &str) -> Result<Vec<String>, String> {
    paths
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| {
            if path.starts_with("data:image/") {
                return Ok(path.to_owned());
            }
            let bytes =
                std::fs::read(path).map_err(|e| format!("Could not read image {path}: {e}"))?;
            if bytes.len() > 50 * 1024 * 1024 {
                return Err(format!("Image exceeds 50 MiB: {path}"));
            }
            let mime = match std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
                .as_str()
            {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                "bmp" => "image/bmp",
                "tif" | "tiff" => "image/tiff",
                _ => return Err(format!("Unsupported image extension: {path}")),
            };
            Ok(format!(
                "data:{mime};base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ))
        })
        .collect()
}
fn decode_data_url(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix("data:")?;
    let (mime, data) = rest.split_once(";base64,")?;
    Some((mime, data))
}
fn build_run_title(prompt: &str) -> String {
    let title = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return "Image run".into();
    }
    let mut chars = title.chars();
    let clipped: String = chars.by_ref().take(64).collect();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}
fn instance_id(run: &Value, index: usize) -> &str {
    run["instances"]
        .as_array()
        .and_then(|items| items.get(index))
        .and_then(|item| item["id"].as_str())
        .unwrap_or_default()
}
fn opt(v: &Value) -> &str {
    v.as_str().unwrap_or("unavailable")
}
fn capture_picker(ui: &mut Ui, side: &'static str, runs: &[Value], selected: &mut String) -> bool {
    let before = selected.clone();
    egui::ComboBox::new(("run-capture-picker", side), side)
        .selected_text(if selected.is_empty() {
            "Select"
        } else {
            selected.as_str()
        })
        .show_ui(ui, |ui| {
            for run in runs {
                if let Some(instances) = run["instances"].as_array() {
                    for instance in instances {
                        if let Some(captures) = instance["captures"].as_array() {
                            for cap in captures {
                                let id = s(cap, "id");
                                if !id.is_empty() {
                                    ui.selectable_value(
                                        selected,
                                        id.to_owned(),
                                        format!(
                                            "{} · {} · #{} · {}",
                                            s(run, "title"),
                                            instance["ordinal"].as_u64().unwrap_or(0) + 1,
                                            s(cap, "label"),
                                            id
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        });
    before != *selected
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use egui_kittest::Harness;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn scope(endpoint_id: &str, connection_epoch: u64) -> BackendScope {
        BackendScope {
            endpoint_id: endpoint_id.into(),
            connection_epoch,
            store_id: Some("store-a".into()),
        }
    }

    #[test]
    fn capture_pickers_are_independent_and_keep_stable_widget_ids() {
        let runs = vec![json!({
            "title":"Run",
            "instances":[{"ordinal":0,"captures":[
                {"id":"capture-a","label":"Initial"},
                {"id":"capture-b","label":"Later"}
            ]}]
        })];
        let selected = Rc::new(RefCell::new((String::new(), String::new())));
        let ui_selection = selected.clone();
        let mut harness = Harness::new_ui(move |ui| {
            let mut selected = ui_selection.borrow_mut();
            let (left, right) = &mut *selected;
            ui.horizontal(|ui| {
                capture_picker(ui, "Left capture", &runs, left);
                capture_picker(ui, "Right capture", &runs, right);
            });
        });

        let left = harness.get_by_role_and_label(Role::ComboBox, "Left capture");
        let right = harness.get_by_role_and_label(Role::ComboBox, "Right capture");
        let left_id = left.accesskit_node().id();
        let right_id = right.accesskit_node().id();
        assert_ne!(left_id, right_id);

        left.click();
        harness.run();
        harness.get_by_label_contains("capture-a").click();
        harness.run();
        assert_eq!(selected.borrow().0, "capture-a");
        assert!(selected.borrow().1.is_empty());
        assert_eq!(
            harness
                .get_by_role_and_label(Role::ComboBox, "Left capture")
                .accesskit_node()
                .id(),
            left_id
        );

        harness
            .get_by_role_and_label(Role::ComboBox, "Right capture")
            .click();
        harness.run();
        harness.get_by_label_contains("capture-b").click();
        harness.run();
        assert_eq!(selected.borrow().0, "capture-a");
        assert_eq!(selected.borrow().1, "capture-b");
        assert_eq!(
            harness
                .get_by_role_and_label(Role::ComboBox, "Right capture")
                .accesskit_node()
                .id(),
            right_id
        );

        harness
            .get_by_role_and_label(Role::ComboBox, "Left capture")
            .click();
        harness.run();
        harness.get_by_label_contains("capture-b").click();
        harness.run();
        assert_eq!(selected.borrow().0, "capture-b");
        assert_eq!(selected.borrow().1, "capture-b");
        assert_eq!(
            harness
                .get_by_role_and_label(Role::ComboBox, "Left capture")
                .accesskit_node()
                .id(),
            left_id
        );
        assert_eq!(
            harness
                .get_by_role_and_label(Role::ComboBox, "Right capture")
                .accesskit_node()
                .id(),
            right_id
        );
    }

    #[test]
    fn worktree_launch_records_run_before_preparation_and_keeps_run_context() {
        let mut panel = RunsPanel::default();
        let record = json!({"id":"run-1","workspaceId":"parent","mode":"worktree","prompt":"compare approaches","instances":[{"id":"instance-1","requested":{}}]});
        panel.apply(&WorkbenchRequestKey::RunCreate, record);
        assert_eq!(panel.queued.len(), 1);
        let effect = panel.queued.pop_front().unwrap();
        let PanelEffect::Request { key, request } = effect else {
            panic!("expected request")
        };
        assert_eq!(
            key,
            WorkbenchRequestKey::RunCreateWorktree("instance-1".into())
        );
        assert_eq!(request.rpc().method, "add_worktree");
        assert_eq!(request.rpc().params["runContext"]["runId"], "run-1");
        assert_eq!(
            request.rpc().params["runContext"]["instanceId"],
            "instance-1"
        );
        assert!(matches!(
            request,
            crate::client::contracts::BackendRequest::Workspaces(_)
        ));
    }

    #[test]
    fn a_failed_launch_is_recorded_before_advancing_to_the_next_instance() {
        let mut panel = RunsPanel::default();
        panel
            .workspaces
            .insert("w".into(), json!({"id":"w","connected":true}));
        panel.apply(&WorkbenchRequestKey::RunCreate, json!({"id":"r","workspaceId":"w","mode":"local","prompt":"p","instances":[{"id":"i1","requested":{}},{"id":"i2","requested":{}}]}));
        assert_eq!(panel.queued.len(), 1);
        panel.queued.clear();
        panel.fail_instance("i1", "thread creation failed".into());
        let effect = panel.queued.pop_front().unwrap();
        let PanelEffect::Request { key, request } = effect else {
            panic!("expected failure receipt")
        };
        assert_eq!(key, WorkbenchRequestKey::RunCreateFailure("i1".into()));
        assert_eq!(request.rpc().method, "record_run_failure");
        assert_eq!(request.rpc().params["message"], "thread creation failed");
        panel.apply(
            &WorkbenchRequestKey::RunCreateFailure("i1".into()),
            json!({"ok":true}),
        );
        assert_eq!(panel.active_launch.as_ref().unwrap().index, 1);
    }

    #[test]
    fn runtime_launch_uses_profile_revision_and_run_context() {
        let mut panel = RunsPanel::default();
        panel.apply(&WorkbenchRequestKey::RunCreate, json!({"id":"r","workspaceId":"w","mode":"local","prompt":"hello","instances":[{"id":"i","requested":{"runtimeProfileId":"p","runtime":{"profileId":"p","profileRevision":7,"kind":"acp"}}}]}));
        let PanelEffect::Request { key, request } =
            panel.queued.pop_front().expect("create session request")
        else {
            panic!("request")
        };
        assert_eq!(
            key,
            WorkbenchRequestKey::RunCreateRuntimeSession("i".into())
        );
        assert_eq!(request.rpc().params["profileId"], "p");
        assert_eq!(request.rpc().params["expectedProfileRevision"], 7);
        panel.apply(&key, json!({"sessionId":"s"}));
        let PanelEffect::Request { key, request } =
            panel.queued.pop_front().expect("submit request")
        else {
            panic!("request")
        };
        assert_eq!(key, WorkbenchRequestKey::RunCreateRuntimeSubmit("i".into()));
        assert_eq!(request.rpc().params["runContext"]["runId"], "r");
        assert_eq!(request.rpc().params["runContext"]["instanceId"], "i");
        assert_eq!(request.rpc().params["prompt"][0]["text"], "hello");
    }

    #[test]
    fn codex_launch_uses_returned_thread_and_records_initial_submission_context() {
        let mut panel = RunsPanel::default();
        panel
            .workspaces
            .insert("w".into(), json!({"id":"w","connected":true}));
        panel.apply(&WorkbenchRequestKey::RunCreate, json!({"id":"r","workspaceId":"w","mode":"local","prompt":"hello","instances":[{"id":"i","requested":{"model":"gpt-x","effort":"high"}}]}));
        let PanelEffect::Request { key, request } =
            panel.queued.pop_front().expect("thread request")
        else {
            panic!("request")
        };
        assert_eq!(request.rpc().method, "start_thread");
        assert_eq!(request.rpc().params["runContext"]["instanceId"], "i");
        panel.apply(&key, json!({"result":{"thread":{"id":"t"}}}));
        let PanelEffect::Request { key, request } =
            panel.queued.pop_front().expect("message request")
        else {
            panic!("request")
        };
        assert_eq!(key, WorkbenchRequestKey::RunCreateCodexSubmit("i".into()));
        assert_eq!(request.rpc().method, "send_user_message");
        assert_eq!(request.rpc().params["threadId"], "t");
        assert_eq!(request.rpc().params["model"], "gpt-x");
        assert_eq!(request.rpc().params["effort"], "high");
        assert_eq!(request.rpc().params["runContext"]["runId"], "r");
    }

    #[test]
    fn image_file_is_embedded_and_projected_as_acp_data_content() {
        let dir = std::env::temp_dir().join(format!("monitor-workbench-image-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp directory");
        let path = dir.join("sample.png");
        std::fs::write(&path, b"png bytes").expect("write fixture");
        let encoded = encode_image_paths(path.to_str().expect("utf8 path")).expect("encode image");
        let (mime, data) = decode_data_url(&encoded[0]).expect("data URL");
        assert_eq!(mime, "image/png");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(data)
                .expect("decode image"),
            b"png bytes"
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn image_preparation_completion_is_discarded_after_backend_scope_change() {
        let original = scope("local", 1);
        let current = scope("daemon", 2);
        let (sender, receiver) = mpsc::channel();
        let mut panel = RunsPanel::default();
        panel.image_job = Some(receiver);
        panel.creating = true;
        panel.image_paths_draft = "/tmp/keep.png".into();
        sender
            .send(ImageJobCompletion {
                intent: CreateRunIntent {
                    request_id: "request-1".into(),
                    workspace_id: "workspace-1".into(),
                    scope: original,
                    title: "title".into(),
                    prompt: "prompt".into(),
                    mode: "local".into(),
                    instances: vec![json!({"id":"instance-1"})],
                },
                images: Ok(vec!["data:image/png;base64,cG5n".into()]),
            })
            .expect("send fixture result");

        panel.poll_image_job(&current);

        assert!(!panel.creating);
        assert!(panel.image_job.is_none());
        assert_eq!(panel.image_paths_draft, "/tmp/keep.png");
        assert!(panel.queued.is_empty());
        assert!(panel
            .error
            .as_deref()
            .is_some_and(|error| error.contains("No run was submitted")));
    }

    #[test]
    fn successful_image_preparation_queues_run_creation_with_captured_scope_input() {
        let scope = scope("local", 1);
        let (sender, receiver) = mpsc::channel();
        let mut panel = RunsPanel::default();
        panel.image_job = Some(receiver);
        panel.creating = true;
        sender
            .send(ImageJobCompletion {
                intent: CreateRunIntent {
                    request_id: "request-1".into(),
                    workspace_id: "workspace-1".into(),
                    scope: scope.clone(),
                    title: "captured title".into(),
                    prompt: "captured prompt".into(),
                    mode: "local".into(),
                    instances: vec![json!({"runtimeProfileId":"profile-1"})],
                },
                images: Ok(vec!["data:image/png;base64,cG5n".into()]),
            })
            .expect("send fixture result");

        panel.poll_image_job(&scope);
        let PanelEffect::Request { key, request } =
            panel.queued.pop_front().expect("create effect")
        else {
            panic!("expected backend request")
        };
        assert_eq!(key, WorkbenchRequestKey::RunCreate);
        assert_eq!(request.rpc().method, "create_run");
        assert_eq!(request.rpc().params["requestId"], "request-1");
        assert_eq!(request.rpc().params["workspaceId"], "workspace-1");
        assert_eq!(request.rpc().params["prompt"], "captured prompt");
        assert_eq!(request.rpc().params["title"], "captured title");
        assert_eq!(
            request.rpc().params["images"][0],
            "data:image/png;base64,cG5n"
        );
        assert!(panel.creating);
    }

    #[test]
    fn codex_runtime_profile_selection_enables_native_model_catalog() {
        let mut panel = RunsPanel::default();
        panel.profiles = vec![json!({"id":"codex-profile","kind":"codex"})];
        panel.selected_agents.clear();
        panel.selected_agents.insert("codex-profile".into());
        assert!(panel.needs_codex_model());
        panel.selected_agents.clear();
        panel.selected_agents.insert("acp-profile".into());
        assert!(!panel.needs_codex_model());
    }

    #[test]
    fn opening_and_refreshing_an_acp_only_run_form_never_queries_codex() {
        let mut panel = RunsPanel::default();
        panel.profiles = vec![json!({"id":"opencode","kind":"acp","name":"OpenCode"})];
        panel.selected_agents.clear();
        panel.selected_agents.insert("opencode".into());
        let scope = scope("local", 1);
        let context = egui::Context::default();
        let mut effects = Vec::new();

        let mut first_frame = context.run_ui(Default::default(), |ui| {
            panel.show(ui, &mut effects, Some("workspace-1"), &scope);
        });
        first_frame.textures_delta.clear();
        assert!(!effects.iter().any(is_codex_request));

        panel.apply(
            &WorkbenchRequestKey::RunsList,
            json!({"runs":[],"nextCursor":null}),
        );
        panel.changed = true;
        let mut refresh_frame = context.run_ui(Default::default(), |ui| {
            panel.show(ui, &mut effects, Some("workspace-1"), &scope);
        });
        refresh_frame.textures_delta.clear();
        assert!(!effects.iter().any(is_codex_request));
        assert!(!panel.models_loading);
    }

    #[test]
    fn codex_default_run_does_not_require_the_explicit_model_catalog() {
        let mut panel = RunsPanel::default();
        let scope = scope("local", 1);
        let context = egui::Context::default();
        let mut effects = Vec::new();
        let mut output = context.run_ui(Default::default(), |ui| {
            panel.show(ui, &mut effects, Some("workspace-1"), &scope);
        });
        output.textures_delta.clear();

        assert!(panel.selected_agents.contains("codex"));
        assert!(panel.codex_models.is_empty());
        assert!(!effects.iter().any(is_codex_request));
    }

    fn is_codex_request(effect: &PanelEffect) -> bool {
        matches!(
            effect,
            PanelEffect::Request {
                request: crate::client::contracts::BackendRequest::Codex(_),
                ..
            }
        )
    }

    #[test]
    fn comparison_revision_conflict_keeps_unsaved_notes_and_choice() {
        let mut panel = RunsPanel::default();
        panel.notes = "my unsaved notes".into();
        panel.choice = "left".into();
        panel.dirty = true;
        panel.revision = 4;
        assert!(panel.apply_error(
            &WorkbenchRequestKey::RunSaveComparison,
            &BackendError {
                code: "backend_error".into(),
                message: "conflict: comparison notes changed; reload before saving".into()
            }
        ));
        assert_eq!(panel.notes, "my unsaved notes");
        assert_eq!(panel.choice, "left");
        assert_eq!(panel.revision, 4);
        assert!(panel.dirty);
    }

    #[test]
    fn old_comparison_lookup_is_ignored_after_capture_selection_changes() {
        let mut panel = RunsPanel::default();
        panel.selected_left = "capture-new".into();
        panel.selected_right = "capture-other".into();
        panel.apply(
            &WorkbenchRequestKey::RunComparisons("capture-old".into()),
            json!([{"notes":"stale"}]),
        );
        assert!(panel.comparisons.is_empty());
    }
}
