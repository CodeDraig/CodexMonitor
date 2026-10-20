//! Small, backend-routed dialogs for creating workspaces and repository copies.
//!
//! The module only gathers input and emits a `BackendRequest`; callers own
//! transport, acknowledgement, refresh, and any post-creation agent flow.

use crate::client::contracts::{BackendRequest, WorkspaceRequest};
use egui;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceCreationKind {
    RemoteDirectory,
    Clone { source_workspace_id: String },
    Worktree { parent_id: String },
    GitUrl,
}

/// Returned beside a successful creation request so the app can refresh the
/// workspace list and optionally open its existing new-agent chooser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceCreationContext {
    pub(crate) kind: WorkspaceCreationKind,
    pub(crate) choose_agent_after_create: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum WorkspaceDialogEffect {
    Request {
        request: BackendRequest,
        completion: WorkspaceCreationContext,
    },
}

#[derive(Clone, Debug)]
enum Dialog {
    RemoteDirectory {
        path: String,
        choose_agent: bool,
    },
    Clone {
        source_workspace_id: String,
        copy_name: String,
        copies_folder: String,
        choose_agent: bool,
    },
    Worktree {
        parent_id: String,
        branch: String,
        name: String,
        copy_agents_md: bool,
        choose_agent: bool,
    },
    GitUrl {
        url: String,
        destination_path: String,
        target_folder_name: String,
        choose_agent: bool,
    },
}

#[derive(Default)]
pub(crate) struct WorkspaceDialogs {
    dialog: Option<Dialog>,
}

impl WorkspaceDialogs {
    /// Ask for a directory path on the currently executing backend. This is
    /// intentionally text-based because a desktop file picker returns a path
    /// from the desktop machine, which may differ from a remote daemon's host.
    pub(crate) fn begin_remote(&mut self) {
        self.dialog = Some(Dialog::RemoteDirectory {
            path: String::new(),
            choose_agent: false,
        });
    }

    pub(crate) fn begin_clone(&mut self, source_workspace_id: impl Into<String>) {
        self.dialog = Some(Dialog::Clone {
            source_workspace_id: source_workspace_id.into(),
            copy_name: String::new(),
            copies_folder: String::new(),
            choose_agent: false,
        });
    }

    pub(crate) fn begin_worktree(&mut self, parent_id: impl Into<String>) {
        self.dialog = Some(Dialog::Worktree {
            parent_id: parent_id.into(),
            branch: String::new(),
            name: String::new(),
            copy_agents_md: true,
            choose_agent: false,
        });
    }

    pub(crate) fn begin_git_url(&mut self) {
        self.dialog = Some(Dialog::GitUrl {
            url: String::new(),
            destination_path: String::new(),
            target_folder_name: String::new(),
            choose_agent: false,
        });
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Vec<WorkspaceDialogEffect> {
        let Some(mut dialog) = self.dialog.take() else {
            return Vec::new();
        };

        let mut open = true;
        let mut effect = None;
        match &mut dialog {
            Dialog::RemoteDirectory { path, choose_agent } => {
                egui::Window::new("Add workspace on backend")
                    .id(egui::Id::new("workspace-add-remote"))
                    .collapsible(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label("Directory path on the selected backend");
                        ui.text_edit_singleline(path);
                        ui.checkbox(choose_agent, "Choose an agent after adding");
                        if ui
                            .add_enabled(
                                !path.trim().is_empty(),
                                egui::Button::new("Add workspace"),
                            )
                            .clicked()
                        {
                            let path = path.trim().to_owned();
                            effect = Some(WorkspaceDialogEffect::Request {
                                request: workspace_mutation("add_workspace", json!({"path":path})),
                                completion: WorkspaceCreationContext {
                                    kind: WorkspaceCreationKind::RemoteDirectory,
                                    choose_agent_after_create: *choose_agent,
                                },
                            });
                        }
                    });
            }
            Dialog::Clone {
                source_workspace_id,
                copy_name,
                copies_folder,
                choose_agent,
            } => {
                egui::Window::new("Clone workspace")
                    .id(egui::Id::new("workspace-clone"))
                    .collapsible(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(format!("Source workspace: {source_workspace_id}"));
                        ui.label("Copy name");
                        ui.text_edit_singleline(copy_name);
                        ui.label("Copies folder on the selected backend");
                        ui.text_edit_singleline(copies_folder);
                        ui.checkbox(choose_agent, "Choose an agent after cloning");
                        let ready =
                            !copy_name.trim().is_empty() && !copies_folder.trim().is_empty();
                        if ui
                            .add_enabled(ready, egui::Button::new("Create clone"))
                            .clicked()
                        {
                            effect = Some(WorkspaceDialogEffect::Request {
                                request: workspace_mutation(
                                    "add_clone",
                                    json!({
                                        "sourceWorkspaceId":source_workspace_id,
                                        "copyName":copy_name.trim(),
                                        "copiesFolder":copies_folder.trim()
                                    }),
                                ),
                                completion: WorkspaceCreationContext {
                                    kind: WorkspaceCreationKind::Clone {
                                        source_workspace_id: source_workspace_id.clone(),
                                    },
                                    choose_agent_after_create: *choose_agent,
                                },
                            });
                        }
                    });
            }
            Dialog::Worktree {
                parent_id,
                branch,
                name,
                copy_agents_md,
                choose_agent,
            } => {
                egui::Window::new("Create worktree")
                    .id(egui::Id::new("workspace-worktree"))
                    .collapsible(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(format!("Parent workspace: {parent_id}"));
                        ui.label("Branch name");
                        ui.text_edit_singleline(branch);
                        ui.label("Workspace name (optional)");
                        ui.text_edit_singleline(name);
                        ui.checkbox(copy_agents_md, "Copy AGENTS.md from parent");
                        ui.checkbox(choose_agent, "Choose an agent after creation");
                        if ui
                            .add_enabled(!branch.trim().is_empty(), egui::Button::new("Create worktree"))
                            .clicked()
                        {
                            let name = name.trim();
                            effect = Some(WorkspaceDialogEffect::Request {
                                request: workspace_mutation(
                                    "add_worktree",
                                    json!({
                                        "parentId":parent_id,
                                        "branch":branch.trim(),
                                        "name":if name.is_empty(){None::<String>}else{Some(name.to_owned())},
                                        "copyAgentsMd":copy_agents_md
                                    }),
                                ),
                                completion: WorkspaceCreationContext {
                                    kind: WorkspaceCreationKind::Worktree {
                                        parent_id: parent_id.clone(),
                                    },
                                    choose_agent_after_create: *choose_agent,
                                },
                            });
                        }
                    });
            }
            Dialog::GitUrl {
                url,
                destination_path,
                target_folder_name,
                choose_agent,
            } => {
                egui::Window::new("Clone Git URL")
                    .id(egui::Id::new("workspace-git-url"))
                    .collapsible(false)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label("Git URL");
                        ui.text_edit_singleline(url);
                        ui.label("Destination directory on the selected backend");
                        ui.text_edit_singleline(destination_path);
                        ui.label("Target folder name (optional)");
                        ui.text_edit_singleline(target_folder_name);
                        ui.checkbox(choose_agent, "Choose an agent after cloning");
                        let ready = !url.trim().is_empty() && !destination_path.trim().is_empty();
                        if ui
                            .add_enabled(ready, egui::Button::new("Clone repository"))
                            .clicked()
                        {
                            let target = target_folder_name.trim();
                            effect = Some(WorkspaceDialogEffect::Request {
                                request: workspace_mutation(
                                    "add_workspace_from_git_url",
                                    json!({
                                        "url":url.trim(),
                                        "destinationPath":destination_path.trim(),
                                        "targetFolderName":if target.is_empty(){None::<String>}else{Some(target.to_owned())}
                                    }),
                                ),
                                completion: WorkspaceCreationContext {
                                    kind: WorkspaceCreationKind::GitUrl,
                                    choose_agent_after_create: *choose_agent,
                                },
                            });
                        }
                    });
            }
        }

        if effect.is_none() && open {
            self.dialog = Some(dialog);
        }
        effect.into_iter().collect()
    }
}

fn workspace_mutation(method: &str, params: serde_json::Value) -> BackendRequest {
    BackendRequest::Workspaces(WorkspaceRequest::mutation(method, params, None))
}
