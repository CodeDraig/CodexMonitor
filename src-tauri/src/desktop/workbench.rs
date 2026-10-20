//! Backend-connected native workbench panels.
//!
//! Requests are emitted as effects so drawing never performs I/O. Call
//! `track_request` with the correlation id returned by the backend controller,
//! then pass replies to `apply_reply`.

use crate::client::contracts::{
    BackendMessage, BackendRequest, BackendScope, ConversationKey, ConversationRef, FilesRequest,
    GitRequest, PromptsRequest, RunsRequest, RuntimeMethod, RuntimeRequest, SettingsRequest,
    WorkspaceRequest,
};
use egui::{self, Ui};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

#[path = "workbench/files.rs"]
mod files;
#[path = "workbench/git.rs"]
mod git;
#[path = "workbench/prompts.rs"]
mod prompts;
#[path = "workbench/runs.rs"]
mod runs;
#[path = "workbench/settings.rs"]
mod settings;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WorkbenchPanel {
    #[default]
    Runs,
    Git,
    Files,
    Prompts,
    Settings,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum WorkbenchRequestKey {
    RunsList,
    RunDetail(String),
    RunCapture(String),
    RunComparisons(String),
    RunReconcile(String),
    RunSaveComparison,
    RunTurns(String),
    RunCaptureLater(String),
    RunProfiles,
    RunWorkspaces,
    RunCodexModels,
    RunRuntimeCatalog(String),
    RunCreate,
    RunCreateWorktree(String),
    RunCreateSetup(String),
    RunCreateConnect(String),
    RunCreateCodexThread(String),
    RunCreateCodexSubmit(String),
    RunCreateRuntimeSession(String),
    RunCreateRuntimeSubmit(String),
    RunCreateFailure(String),
    GitStatus,
    GitDiffs,
    GitLog,
    GitBranches,
    GitIssues,
    GitPullRequests,
    GitPullRequestDiff(u64),
    GitAction(String),
    FilesList,
    FileRead(String),
    FileWrite(String),
    PromptsList,
    PromptWrite,
    PromptDelete(String),
    PromptMove(String),
    SettingsLoad,
    SettingsSave,
    SettingsAccount,
    SettingsRateLimits,
    SettingsMcp,
    SettingsApps,
    SettingsSkills,
    SettingsFeatures,
    SettingsConfig,
    SettingsDoctor,
    SettingsLogin,
    SettingsLoginCancel,
}

#[derive(Clone, Debug)]
pub(crate) enum PanelEffect {
    Request {
        key: WorkbenchRequestKey,
        request: BackendRequest,
    },
    OpenConversation(ConversationKey),
    OpenIn {
        workspace_id: String,
        path: String,
    },
    InsertPrompt {
        prompt: Value,
    },
}

#[derive(Clone, Debug)]
struct PendingRequest {
    key: WorkbenchRequestKey,
    scope: BackendScope,
    workspace_id: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Workbench {
    pub(crate) active: WorkbenchPanel,
    workspace_id: Option<String>,
    scope: Option<BackendScope>,
    pending: HashMap<Uuid, PendingRequest>,
    pub(crate) runs: runs::RunsPanel,
    pub(crate) git: git::GitPanel,
    pub(crate) files: files::FilesPanel,
    pub(crate) prompts: prompts::PromptsPanel,
    pub(crate) settings: settings::SettingsPanel,
    pub(crate) last_error: Option<String>,
}

impl Workbench {
    pub(crate) fn mark_workspace_files_changed(
        &mut self,
        workspace_id: &str,
        workspace_path: &str,
        paths: &[String],
    ) {
        if self.workspace_id.as_deref() == Some(workspace_id) {
            self.pending.retain(|_, pending| {
                match &pending.key {
                    WorkbenchRequestKey::FilesList => false,
                    WorkbenchRequestKey::FileRead(path) if !path.starts_with("policy:") => {
                        !paths.iter().any(|changed| {
                            workspace_relative_event_matches(changed, path, workspace_path)
                        })
                    }
                    WorkbenchRequestKey::GitStatus
                    | WorkbenchRequestKey::GitDiffs
                    | WorkbenchRequestKey::GitLog
                    | WorkbenchRequestKey::GitBranches
                    | WorkbenchRequestKey::GitIssues
                    | WorkbenchRequestKey::GitPullRequests
                    | WorkbenchRequestKey::GitPullRequestDiff(_) => false,
                    _ => true,
                }
            });
            self.files
                .mark_workspace_files_changed(paths, workspace_path);
            self.git.mark_stale();
        }
    }

    pub(crate) fn set_workspace(&mut self, workspace_id: Option<String>) {
        if self.workspace_id != workspace_id {
            self.workspace_id = workspace_id;
            self.pending
                .retain(|_, pending| is_run_launch_key(&pending.key));
            self.runs.change_workspace();
            self.git.reset();
            self.files.reset();
            self.prompts.reset();
            self.settings.change_workspace();
        }
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, scope: &BackendScope) -> Vec<PanelEffect> {
        let mut effects = Vec::new();
        self.sync_scope(scope);

        ui.horizontal(|ui| {
            for (label, panel) in [
                ("Runs", WorkbenchPanel::Runs),
                ("Git", WorkbenchPanel::Git),
                ("Files", WorkbenchPanel::Files),
                ("Prompts", WorkbenchPanel::Prompts),
                ("Settings", WorkbenchPanel::Settings),
            ] {
                if ui.selectable_label(self.active == panel, label).clicked() {
                    self.active = panel;
                }
            }
        });
        if self.workspace_id.is_none() && self.active != WorkbenchPanel::Settings {
            ui.label("Select a workspace to use this panel.");
            return effects;
        }
        match self.active {
            WorkbenchPanel::Runs => {
                self.runs
                    .show(ui, &mut effects, self.workspace_id.as_deref(), scope)
            }
            WorkbenchPanel::Git => self
                .git
                .show(ui, &mut effects, self.workspace_id.as_deref()),
            WorkbenchPanel::Files => {
                self.files
                    .show(ui, &mut effects, self.workspace_id.as_deref())
            }
            WorkbenchPanel::Prompts => {
                self.prompts
                    .show(ui, &mut effects, self.workspace_id.as_deref())
            }
            WorkbenchPanel::Settings => {
                self.settings
                    .show(ui, &mut effects, self.workspace_id.as_deref())
            }
        }
        if let Some(error) = &self.last_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        effects
    }

    pub(crate) fn track_request(
        &mut self,
        id: Uuid,
        key: WorkbenchRequestKey,
        scope: BackendScope,
    ) {
        self.pending.insert(
            id,
            PendingRequest {
                key,
                scope,
                workspace_id: self.workspace_id.clone(),
            },
        );
    }

    /// Root drains these after reply handling on every frame so launch chains
    /// continue even if the user navigates away from the Workbench panel.
    pub(crate) fn take_effects(&mut self, scope: &BackendScope) -> Vec<PanelEffect> {
        self.sync_scope(scope);
        self.runs.poll_image_job(scope);
        let mut effects = self.runs.take_effects();
        effects.extend(self.settings.take_effects());
        effects
    }

    fn sync_scope(&mut self, scope: &BackendScope) {
        if self.scope.as_ref() == Some(scope) {
            return;
        }
        let interrupted = self.runs.has_pending_launch();
        self.scope = Some(scope.clone());
        self.pending.clear();
        self.last_error = None;
        self.runs.mark_stale();
        self.git.reset();
        self.files.mark_stale();
        self.prompts.mark_stale();
        self.settings.mark_stale();
        if interrupted {
            self.runs.mark_scope_interrupted();
        }
    }

    pub(crate) fn apply_reply(&mut self, message: BackendMessage) {
        let BackendMessage::Reply {
            correlation_id,
            scope,
            result,
        } = message
        else {
            return;
        };
        let Some(pending) = self.pending.remove(&correlation_id) else {
            return;
        };
        // A reply from an earlier backend/connection generation must never alter
        // the newly selected workspace's panel state.
        if pending.scope != scope || self.scope.as_ref() != Some(&scope) {
            return;
        }
        if pending.workspace_id != self.workspace_id && !is_run_launch_key(&pending.key) {
            return;
        }
        match result {
            Ok(value) => {
                self.last_error = None;
                self.runs.apply(&pending.key, value.clone());
                self.git.apply(&pending.key, value.clone());
                self.files.apply(&pending.key, value.clone());
                self.prompts.apply(&pending.key, value.clone());
                self.settings.apply(&pending.key, value);
            }
            Err(error) => {
                let message = error.message.clone();
                self.settings.apply_error(&pending.key, &error);
                if self.runs.apply_error(&pending.key, &error) {
                    self.last_error = Some(message);
                } else {
                    self.last_error = Some(message);
                }
            }
        }
    }

    pub(crate) fn open_run_conversation(
        &self,
        workspace_id: String,
        thread_id: String,
    ) -> Option<PanelEffect> {
        let endpoint_id = self.scope.as_ref()?.endpoint_id.clone();
        Some(PanelEffect::OpenConversation(ConversationKey {
            endpoint_id,
            conversation: ConversationRef::Codex {
                workspace_id,
                thread_id,
            },
        }))
    }

    pub(crate) fn open_runtime_conversation(
        &self,
        workspace_id: String,
        store_id: String,
        session_id: String,
    ) -> Option<PanelEffect> {
        let endpoint_id = self.scope.as_ref()?.endpoint_id.clone();
        Some(PanelEffect::OpenConversation(ConversationKey {
            endpoint_id,
            conversation: ConversationRef::Acp {
                workspace_id,
                store_id,
                session_id,
            },
        }))
    }
}

fn is_run_launch_key(key: &WorkbenchRequestKey) -> bool {
    matches!(
        key,
        WorkbenchRequestKey::RunCreate
            | WorkbenchRequestKey::RunCreateWorktree(_)
            | WorkbenchRequestKey::RunCreateSetup(_)
            | WorkbenchRequestKey::RunCreateConnect(_)
            | WorkbenchRequestKey::RunCreateCodexThread(_)
            | WorkbenchRequestKey::RunCreateCodexSubmit(_)
            | WorkbenchRequestKey::RunCreateRuntimeSession(_)
            | WorkbenchRequestKey::RunCreateRuntimeSubmit(_)
            | WorkbenchRequestKey::RunCreateFailure(_)
    )
}

pub(crate) fn workspace_relative_event_matches(
    changed_relative: &str,
    displayed_path: &str,
    workspace_path: &str,
) -> bool {
    let changed = normalize_display_path(changed_relative);
    let displayed = normalize_display_path(displayed_path);
    if changed == displayed {
        return true;
    }
    let workspace = normalize_display_path(workspace_path).trim_end_matches('/').to_owned();
    if workspace.is_empty() {
        return false;
    }
    displayed
        .strip_prefix(&workspace)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .is_some_and(|relative| relative == changed)
}

fn normalize_display_path(path: &str) -> String {
    let path = path.strip_prefix("file://").unwrap_or(path);
    let mut bytes = Vec::with_capacity(path.len());
    let raw = path.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            if let (Some(high), Some(low)) = (hex(raw[index + 1]), hex(raw[index + 2])) {
                bytes.push(high * 16 + low);
                index += 3;
                continue;
            }
        }
        bytes.push(raw[index]);
        index += 1;
    }
    String::from_utf8_lossy(&bytes)
        .replace('\\', "/")
        .trim_end_matches('/')
        .trim_start_matches("./")
        .to_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn read_runs(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Runs(RunsRequest::read(method, params))
}
pub(crate) fn mutate_runs(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Runs(RunsRequest::mutation(method, params, Some(Uuid::new_v4())))
}
pub(crate) fn read_codex(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Codex(crate::client::contracts::CodexRequest::read(method, params))
}
pub(crate) fn mutate_codex(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Codex(crate::client::contracts::CodexRequest::mutation(
        method,
        params,
        Some(Uuid::new_v4()),
    ))
}
pub(crate) fn read_git(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Git(GitRequest::read(method, params))
}
pub(crate) fn mutate_git(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Git(GitRequest::mutation(method, params, Some(Uuid::new_v4())))
}
pub(crate) fn read_files(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Files(FilesRequest::read(method, params))
}
pub(crate) fn mutate_files(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Files(FilesRequest::mutation(method, params, Some(Uuid::new_v4())))
}
pub(crate) fn read_prompts(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Prompts(PromptsRequest::read(method, params))
}
pub(crate) fn mutate_prompts(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Prompts(PromptsRequest::mutation(
        method,
        params,
        Some(Uuid::new_v4()),
    ))
}
pub(crate) fn read_workspace(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Workspaces(WorkspaceRequest::read(method, params))
}
pub(crate) fn mutate_workspace(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Workspaces(WorkspaceRequest::mutation(
        method,
        params,
        Some(Uuid::new_v4()),
    ))
}
pub(crate) fn read_settings(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Settings(SettingsRequest::read(method, params))
}
pub(crate) fn mutate_settings(method: &str, params: Value) -> BackendRequest {
    BackendRequest::Settings(SettingsRequest::mutation(
        method,
        params,
        Some(Uuid::new_v4()),
    ))
}
pub(crate) fn read_runtime(method: RuntimeMethod, params: Value) -> BackendRequest {
    BackendRequest::Runtime(RuntimeRequest::read(method, params))
}
pub(crate) fn mutate_runtime(method: RuntimeMethod, params: Value) -> BackendRequest {
    BackendRequest::Runtime(RuntimeRequest::mutation(
        method,
        params,
        Some(Uuid::new_v4()),
    ))
}
pub(crate) fn workspace_params(workspace_id: Option<&str>) -> Value {
    json!({"workspaceId": workspace_id.unwrap_or_default()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::contracts::BackendError;

    fn scope(endpoint: &str, epoch: u64) -> BackendScope {
        BackendScope {
            endpoint_id: endpoint.into(),
            connection_epoch: epoch,
            store_id: Some("store-a".into()),
        }
    }

    #[test]
    fn stale_backend_scope_reply_cannot_mutate_panels() {
        let mut workbench = Workbench::default();
        let current = scope("daemon", 8);
        let previous = scope("local", 7);
        workbench.scope = Some(current.clone());
        workbench.runs.runs.push(json!({"id":"keep-me"}));
        let id = Uuid::new_v4();
        workbench.track_request(id, WorkbenchRequestKey::RunsList, previous.clone());
        workbench.apply_reply(BackendMessage::Reply {
            correlation_id: id,
            scope: previous,
            result: Ok(json!({"runs":[{"id":"stale"}],"nextCursor":null})),
        });
        assert_eq!(workbench.runs.runs, vec![json!({"id":"keep-me"})]);
    }

    #[test]
    fn workspace_change_discards_pending_panel_replies() {
        let mut workbench = Workbench::default();
        let scope = scope("local", 1);
        workbench.scope = Some(scope.clone());
        workbench.set_workspace(Some("workspace-a".into()));
        let id = Uuid::new_v4();
        workbench.track_request(id, WorkbenchRequestKey::FilesList, scope.clone());
        workbench.set_workspace(Some("workspace-b".into()));
        workbench.last_error = Some("keep".into());
        workbench.apply_reply(BackendMessage::Reply {
            correlation_id: id,
            scope,
            result: Ok(json!(["stale.txt"])),
        });
        assert!(workbench.pending.is_empty());
        assert_eq!(workbench.last_error.as_deref(), Some("keep"));
    }

    #[test]
    fn acp_file_change_invalidates_files_and_git_only_for_current_workspace() {
        let mut workbench = Workbench::default();
        workbench.set_workspace(Some("workspace-a".into()));
        assert!(!workbench.files.is_stale());
        assert!(!workbench.git.is_stale());

        workbench.mark_workspace_files_changed("workspace-b", "/work/repo", &["src/main.rs".into()]);
        assert!(!workbench.files.is_stale());
        assert!(!workbench.git.is_stale());
        workbench.mark_workspace_files_changed("workspace-a", "/work/repo", &["src/main.rs".into()]);
        assert!(workbench.files.is_stale());
        assert!(workbench.git.is_stale());
    }

    #[test]
    fn acp_file_change_drops_old_list_preview_and_git_replies_but_keeps_policy_edits() {
        let mut workbench = Workbench::default();
        let scope = scope("local", 1);
        workbench.scope = Some(scope.clone());
        workbench.set_workspace(Some("workspace-a".into()));
        let list = Uuid::new_v4();
        let preview = Uuid::new_v4();
        let policy = Uuid::new_v4();
        let git = Uuid::new_v4();
        workbench.track_request(list, WorkbenchRequestKey::FilesList, scope.clone());
        workbench.track_request(preview, WorkbenchRequestKey::FileRead("src/main.rs".into()), scope.clone());
        workbench.track_request(policy, WorkbenchRequestKey::FileRead("policy:workspace:agents".into()), scope.clone());
        workbench.track_request(git, WorkbenchRequestKey::GitStatus, scope.clone());

        workbench.mark_workspace_files_changed("workspace-a", "/work/repo", &["src/main.rs".into()]);
        assert_eq!(workbench.pending.len(), 1);
        assert!(workbench.pending.contains_key(&policy));
        workbench.apply_reply(BackendMessage::Reply {
            correlation_id: preview,
            scope: scope.clone(),
            result: Ok(json!({"content":"stale"})),
        });
        assert!(workbench.pending.contains_key(&policy));
    }

    #[test]
    fn workspace_switch_keeps_a_run_launch_chain_and_effects_drain_without_panel_draw() {
        let mut workbench = Workbench::default();
        let scope = scope("local", 1);
        workbench.scope = Some(scope.clone());
        workbench.set_workspace(Some("source".into()));
        let request_id = Uuid::new_v4();
        workbench.track_request(request_id, WorkbenchRequestKey::RunCreate, scope.clone());
        workbench.set_workspace(Some("other".into()));
        workbench.apply_reply(BackendMessage::Reply {
            correlation_id: request_id,
            scope: scope.clone(),
            result: Ok(json!({"id":"run","workspaceId":"source","mode":"local","prompt":"p","instances":[{"id":"i","requested":{}}]})),
        });
        assert!(workbench.runs.has_pending_launch());
        assert_eq!(workbench.take_effects(&scope).len(), 1);
    }
}
