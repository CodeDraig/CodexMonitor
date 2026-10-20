use super::*;
use crate::client::BackendScope;

const RENAME_EDITOR_ID: &str = "codex-monitor-sidebar-rename-editor";
const REMOVE_WORKSPACE_ID: &str = "codex-monitor-sidebar-remove-workspace";

#[derive(Clone)]
struct RemoveWorkspaceConfirmation {
    scope: BackendScope,
    workspace_id: String,
    workspace_name: String,
    workspace_path: String,
    is_worktree: bool,
    removed_workspace_ids: Vec<String>,
    backend_label: String,
}

impl Default for RemoveWorkspaceConfirmation {
    fn default() -> Self {
        Self {
            scope: BackendScope {
                endpoint_id: String::new(),
                connection_epoch: 0,
                store_id: None,
            },
            workspace_id: String::new(),
            workspace_name: String::new(),
            workspace_path: String::new(),
            is_worktree: false,
            removed_workspace_ids: Vec::new(),
            backend_label: String::new(),
        }
    }
}

#[derive(Clone)]
struct RenameEditor {
    title: String,
    target: RenameTarget,
}

#[derive(Clone)]
enum RenameTarget {
    Codex {
        scope: BackendScope,
        workspace_id: String,
        thread_id: String,
    },
    Acp {
        scope: BackendScope,
        catalog_id: String,
        workspace_id: String,
        workspace_path: String,
        session_id: String,
        expected_revision: i64,
    },
}

impl Default for RenameTarget {
    fn default() -> Self {
        Self::Codex {
            scope: BackendScope {
                endpoint_id: String::new(),
                connection_epoch: 0,
                store_id: None,
            },
            workspace_id: String::new(),
            thread_id: String::new(),
        }
    }
}

impl Default for RenameEditor {
    fn default() -> Self {
        Self {
            title: String::new(),
            target: RenameTarget::default(),
        }
    }
}

fn set_thread_organize_mode(presentation: &mut UiStateV1, value: &str) -> bool {
    if !matches!(value, "by_project" | "by_project_activity" | "threads_only")
        || presentation.thread_list_organize_mode.as_deref() == Some(value)
    {
        return false;
    }
    presentation.thread_list_organize_mode = Some(value.into());
    true
}

fn set_thread_sort_key(presentation: &mut UiStateV1, value: &str) -> bool {
    if !matches!(value, "updated_at" | "created_at")
        || presentation.thread_list_sort_key.as_deref() == Some(value)
    {
        return false;
    }
    presentation.thread_list_sort_key = Some(value.into());
    true
}

fn scope_matches(captured: &BackendScope, active: &BackendScope) -> bool {
    captured == active
}

fn runtime_scope(scope: &BackendScope, runtime_store_id: Option<&str>) -> BackendScope {
    let mut current = scope.clone();
    if let Some(store_id) = runtime_store_id.or(current.store_id.as_deref()) {
        current.store_id = Some(store_id.to_owned());
    }
    current
}

fn rename_target_matches(
    target: &RenameTarget,
    active_scope: &BackendScope,
    runtime_store_id: Option<&str>,
    catalog: &[Value],
) -> bool {
    match target {
        RenameTarget::Codex { scope, .. } => scope_matches(scope, active_scope),
        RenameTarget::Acp {
            scope,
            catalog_id,
            workspace_id,
            workspace_path,
            session_id,
            expected_revision,
        } => {
            let current_scope = runtime_scope(active_scope, runtime_store_id);
            scope_matches(scope, &current_scope)
                && catalog.iter().any(|row| {
                    row["catalogId"].as_str() == Some(catalog_id)
                        && row["storeId"].as_str() == scope.store_id.as_deref()
                        && row["sessionId"].as_str() == Some(session_id)
                        && row["workspaceId"]
                            .as_str()
                            .map(|row_workspace| row_workspace == workspace_id)
                            .unwrap_or_else(|| row["cwd"].as_str() == Some(workspace_path))
                        && row["revision"].as_i64() == Some(*expected_revision)
                })
        }
    }
}

fn visible_thread_depth<T>(
    thread: &T,
    rows: &[T],
    id: impl Fn(&T) -> String,
    parent_id: impl Fn(&T) -> Option<String>,
) -> usize {
    let mut visited = std::collections::HashSet::new();
    visited.insert(id(thread));
    let mut current = thread;
    let mut depth = 0;
    while let Some(parent) = parent_id(current) {
        if !visited.insert(parent.clone()) {
            break;
        }
        let Some(parent_row) = rows.iter().find(|row| id(row) == parent) else {
            break;
        };
        depth += 1;
        current = parent_row;
    }
    depth
}

impl NativeApp {
    pub(super) fn show_sidebar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let width = self
            .presentation
            .panels
            .get("sidebarWidth")
            .and_then(Value::as_f64)
            .unwrap_or(280.0) as f32;
        let sidebar_collapsed = self
            .presentation
            .panels
            .get("sidebarCollapsed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut sidebar_expanded = !sidebar_collapsed;
        let panel = egui::Panel::left("projects-sidebar")
            .resizable(true)
            .default_size(width)
            .size_range(190.0..=500.0)
            .show_collapsible(ui, &mut sidebar_expanded, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("WORKSPACES");
                    if ui
                        .button("+")
                        .on_hover_text("Add workspace")
                        .clicked()
                    {
                        self.add_workspace();
                    }
                    ui.menu_button("Organize", |ui| {
                        ui.label("Group workspaces");
                        for (label, value) in [
                            ("By project", "by_project"),
                            ("By project activity", "by_project_activity"),
                            ("Thread list", "threads_only"),
                        ] {
                            if ui
                                .selectable_label(
                                    self.presentation.thread_list_organize_mode.as_deref()
                                        .unwrap_or("by_project")
                                        == value,
                                    label,
                                )
                                .clicked()
                            {
                                self.presentation_dirty |=
                                    set_thread_organize_mode(&mut self.presentation, value);
                                ui.close();
                            }
                        }
                        ui.separator();
                        ui.label("Sort conversations by");
                        for (label, value) in [("Updated", "updated_at"), ("Created", "created_at")] {
                            if ui
                                .selectable_label(
                                    self.presentation.thread_list_sort_key.as_deref()
                                        .unwrap_or("updated_at")
                                        == value,
                                    label,
                                )
                                .clicked()
                            {
                                self.presentation_dirty |=
                                    set_thread_sort_key(&mut self.presentation, value);
                                ui.close();
                            }
                        }
                    });
                });
                if ui.button("Browse agent sessions").clicked() {
                    self.browser_open = true;
                    self.act(Action::DiscoverAll);
                }
                ui.separator();

                let mut workspaces = self.controller.state.workspaces.clone();
                if self.presentation.thread_list_organize_mode.as_deref()
                    == Some("by_project_activity")
                {
                    let activity = |workspace_id: &str| {
                        self.controller
                            .state
                            .threads_by_workspace
                            .get(workspace_id)
                            .and_then(|rows| rows.iter().map(|row| row.updated_at).max())
                    };
                    workspaces.sort_by(|a, b| {
                        let a_activity = activity(&a.id);
                        let b_activity = activity(&b.id);
                        b_activity
                            .is_some()
                            .cmp(&a_activity.is_some())
                            .then_with(|| b_activity.cmp(&a_activity))
                            .then_with(|| a.name.cmp(&b.name))
                    });
                }
                egui::ScrollArea::vertical()
                    .id_salt("workspace-sidebar-scroll")
                    .show(ui, |ui| {
                        for workspace in workspaces {
                            let endpoint_id = self.controller.scope().endpoint_id.clone();
                            let sidebar_identity = (endpoint_id.clone(), workspace.id.clone());
                            ui.push_id(sidebar_identity, |ui| {
                                let collapse_key = format!("workspace:{}", workspace.id);
                                let legacy_collapse_key = workspace.id.clone();
                                let collapsed = self
                                    .presentation
                                    .collapsed_groups
                                    .get(&collapse_key)
                                    .or_else(|| {
                                        self.presentation
                                            .collapsed_groups
                                            .get(&legacy_collapse_key)
                                    })
                                    .copied()
                                    .unwrap_or(false);
                                let threads_only = self
                                    .presentation
                                    .thread_list_organize_mode
                                    .as_deref()
                                    == Some("threads_only");
                                ui.horizontal(|ui| {
                                    if !threads_only {
                                        if ui
                                            .small_button(if collapsed { "▸" } else { "▾" })
                                            .on_hover_text(if collapsed {
                                                "Expand workspace"
                                            } else {
                                                "Collapse workspace"
                                            })
                                            .clicked()
                                        {
                                            self.presentation
                                                .collapsed_groups
                                                .insert(collapse_key.clone(), !collapsed);
                                            self.presentation_dirty = true;
                                        }
                                    }
                                    if !threads_only {
                                        let workspace_button = ui.selectable_label(
                                            self.selected_workspace.as_ref() == Some(&workspace.id),
                                            RichText::new(&workspace.name).strong(),
                                        );
                                        if workspace_button.clicked() {
                                            self.selected_workspace = Some(workspace.id.clone());
                                            self.controller.state.selected = None;
                                            self.act(Action::LoadWorkspaceThreads {
                                                workspace_id: workspace.id.clone(),
                                            });
                                        }
                                    }
                                    if !threads_only && ui.small_button("New agent").clicked() {
                                        self.agent_chooser = Some(workspace.id.clone());
                                    }
                                    if ui.small_button("⋯").on_hover_text("Workspace actions").clicked() {
                                        let child_workspaces: Vec<_> = self
                                            .controller
                                            .state
                                            .workspaces
                                            .iter()
                                            .filter(|child| {
                                                child.raw["parentId"].as_str()
                                                    == Some(workspace.id.as_str())
                                                    && child.raw["kind"].as_str()
                                                        == Some("worktree")
                                            })
                                            .collect();
                                        let mut removed_workspace_ids = vec![workspace.id.clone()];
                                        removed_workspace_ids.extend(
                                            child_workspaces.iter().map(|child| child.id.clone()),
                                        );
                                        let backend_label = if matches!(
                                            self.settings.backend_mode,
                                            BackendMode::Remote
                                        ) {
                                            active_remote_backend(&self.settings).name
                                        } else {
                                            "Local backend".into()
                                        };
                                        let confirmation = RemoveWorkspaceConfirmation {
                                            scope: self.controller.scope().clone(),
                                            workspace_id: workspace.id.clone(),
                                            workspace_name: workspace.name.clone(),
                                            workspace_path: workspace.path.clone(),
                                            is_worktree: workspace.raw["kind"].as_str()
                                                == Some("worktree"),
                                            removed_workspace_ids,
                                            backend_label,
                                        };
                                        ctx.data_mut(|data| {
                                            data.insert_temp(
                                                egui::Id::new(REMOVE_WORKSPACE_ID),
                                                confirmation,
                                            )
                                        });
                                    }
                                });
                                if collapsed && !threads_only {
                                    return;
                                }
                                if !threads_only {
                                    ui.label(
                                        RichText::new(&workspace.path)
                                            .small()
                                            .color(theme::MUTED),
                                    );
                                }

                                let source_rows = self
                                    .controller
                                    .state
                                    .threads_by_workspace
                                    .get(&workspace.id)
                                    .cloned()
                                    .unwrap_or_default();
                                let ordered_rows = order_codex_rows(
                                    source_rows,
                                    &endpoint_id,
                                    &workspace.id,
                                    &self.presentation,
                                    |row| row.id.clone(),
                                    |row| row.parent_id.clone(),
                                    |row| row.updated_at,
                                    |row| {
                                        row.raw["createdAt"]
                                            .as_i64()
                                            .or_else(|| row.raw["created_at"].as_i64())
                                            .unwrap_or(row.updated_at)
                                    },
                                );
                                for thread in &ordered_rows {
                                    let depth = visible_thread_depth(
                                        thread,
                                        &ordered_rows,
                                        |row| row.id.clone(),
                                        |row| row.parent_id.clone(),
                                    );
                                    let scoped_key = codex_key(
                                        &endpoint_id,
                                        &workspace.id,
                                        &thread.id,
                                    );
                                    let title = custom_name(
                                        &self.presentation,
                                        &endpoint_id,
                                        &workspace.id,
                                        &thread.id,
                                    )
                                    .unwrap_or_else(|| {
                                        if thread.title.is_empty() {
                                            "Untitled conversation".into()
                                        } else {
                                            thread.title.clone()
                                        }
                                    });
                                    let pinned = pin_timestamp(
                                        &self.presentation,
                                        &endpoint_id,
                                        &workspace.id,
                                        &thread.id,
                                    )
                                    .is_some();
                                    let selected = self.controller.state.selected.as_ref().is_some_and(
                                        |key| {
                                            key.endpoint_id == endpoint_id
                                                && matches!(
                                                    &key.conversation,
                                                    ConversationRef::Codex { workspace_id, thread_id }
                                                        if workspace_id == &workspace.id && thread_id == &thread.id
                                                )
                                        },
                                    );
                                    let activity_key = ConversationKey {
                                        endpoint_id: endpoint_id.clone(),
                                        conversation: ConversationRef::Codex {
                                            workspace_id: workspace.id.clone(),
                                            thread_id: thread.id.clone(),
                                        },
                                    };
                                    let activity = self.controller.state.conversations.get(&activity_key);
                                    let processing = activity
                                        .map(|state| state.busy || state.pending_submission)
                                        .unwrap_or(thread.processing);
                                    let unread = activity
                                        .map(|state| state.unread)
                                        .unwrap_or(thread.unread);
                                    let label = format!(
                                        "{}{}{}{}{}{}",
                                        "  ".repeat(depth),
                                        if depth > 0 { "↳ " } else { "" },
                                        if pinned { "★ " } else { "" },
                                        if processing {
                                            "● "
                                        } else if unread {
                                            "• "
                                        } else {
                                            ""
                                        },
                                        title,
                                        if threads_only {
                                            format!(" · {}", workspace.name)
                                        } else {
                                            String::new()
                                        }
                                    );
                                    let row = ui.selectable_label(selected, label).on_hover_text("Codex");
                                    if row.clicked() {
                                        self.open_conversation(ConversationKey {
                                            endpoint_id: endpoint_id.clone(),
                                            conversation: ConversationRef::Codex {
                                                workspace_id: workspace.id.clone(),
                                                thread_id: thread.id.clone(),
                                            },
                                        });
                                    }
                                    row.context_menu(|ui| {
                                        if ui.button("Rename conversation").clicked() {
                                            open_rename_editor(
                                                &ctx,
                                                title.clone(),
                                                RenameTarget::Codex {
                                                    scope: self.controller.scope().clone(),
                                                    workspace_id: workspace.id.clone(),
                                                    thread_id: thread.id.clone(),
                                                },
                                            );
                                            ui.close();
                                        }
                                        if ui.button(if pinned { "Unpin" } else { "Pin" }).clicked() {
                                            toggle_codex_pin(
                                                &mut self.presentation,
                                                &endpoint_id,
                                                &workspace.id,
                                                &thread.id,
                                                pinned,
                                            );
                                            self.presentation_dirty = true;
                                            ui.close();
                                        }
                                        if ui.button("Hide conversation").clicked() {
                                            self.act(Action::HideThread {
                                                workspace_id: workspace.id.clone(),
                                                thread_id: thread.id.clone(),
                                            });
                                            ui.close();
                                        }
                                    });
                                }

                                if self
                                    .controller
                                    .state
                                    .thread_cursor_by_workspace
                                    .get(&workspace.id)
                                    .is_some_and(Option::is_some)
                                {
                                    let loading = self
                                        .controller
                                        .state
                                        .thread_loading_by_workspace
                                        .contains(&workspace.id);
                                    if ui
                                        .add_enabled(
                                            !loading,
                                            egui::Button::new(if loading {
                                                "Loading older conversations…"
                                            } else {
                                                "Load older conversations"
                                            }),
                                        )
                                        .clicked()
                                    {
                                        self.act(Action::LoadMoreThreads {
                                            workspace_id: workspace.id.clone(),
                                        });
                                    }
                                }

                                let mut acp_rows = self.controller.state.catalog.clone();
                                acp_rows.retain(|row| {
                                    row["profile"]["kind"].as_str() != Some("codex")
                                        && row["presentation"]["hidden"].as_bool() != Some(true)
                                        && (row["workspaceId"].as_str() == Some(&workspace.id)
                                            || row["cwd"].as_str() == Some(&workspace.path))
                                });
                                // Pinned ACP records appear before unpinned records. Their catalog
                                // order already reflects agent activity and remains stable here.
                                acp_rows.sort_by(|a, b| {
                                    let a_pinned = a["presentation"]["pinned"].as_bool() == Some(true);
                                    let b_pinned = b["presentation"]["pinned"].as_bool() == Some(true);
                                    b_pinned.cmp(&a_pinned)
                                });
                                for row in acp_rows {
                                    let title = row["presentation"]["titleOverride"]
                                        .as_str()
                                        .or_else(|| row["title"].as_str())
                                        .or_else(|| row["nativeSessionId"].as_str())
                                        .unwrap_or("Agent conversation");
                                    let agent = row["profile"]["name"].as_str().unwrap_or("ACP");
                                    let session_id = row["sessionId"].as_str();
                                    let selected = self.controller.state.selected.as_ref().is_some_and(
                                        |key| {
                                            key.endpoint_id == endpoint_id
                                                && matches!(
                                                    &key.conversation,
                                                    ConversationRef::Acp {
                                                        workspace_id,
                                                        store_id,
                                                        session_id: selected_session,
                                                    } if workspace_id == &workspace.id
                                                        && store_id == row["storeId"].as_str().unwrap_or("")
                                                        && Some(selected_session.as_str()) == session_id
                                                )
                                        },
                                    );
                                    let pinned = row["presentation"]["pinned"].as_bool() == Some(true);
                                    let activity_key = session_id.map(|session_id| ConversationKey {
                                        endpoint_id: endpoint_id.clone(),
                                        conversation: ConversationRef::Acp {
                                            workspace_id: workspace.id.clone(),
                                            store_id: row["storeId"].as_str().unwrap_or("").to_owned(),
                                            session_id: session_id.to_owned(),
                                        },
                                    });
                                    let activity = activity_key
                                        .as_ref()
                                        .and_then(|key| self.controller.state.conversations.get(key));
                                    let activity_marker = if activity
                                        .is_some_and(|state| state.busy || state.pending_submission)
                                    {
                                        "● "
                                    } else if activity.is_some_and(|state| state.unread) {
                                        "• "
                                    } else {
                                        ""
                                    };
                                    let response = ui.selectable_label(
                                        selected,
                                        format!(
                                            "{}{}{} · {}{}",
                                            if pinned { "★ " } else { "" },
                                            activity_marker,
                                            title,
                                            agent,
                                            if threads_only {
                                                format!(" · {}", workspace.name)
                                            } else {
                                                String::new()
                                            }
                                        ),
                                    );
                                    if response.clicked() {
                                        if let Some(id) = row["catalogId"].as_str() {
                                            self.act(Action::OpenCatalog {
                                                catalog_id: id.to_owned(),
                                                add_workspace: false,
                                            });
                                        }
                                    }
                                    response.context_menu(|ui| {
                                        if let Some(session_id) = session_id {
                                            let revision = row["revision"].as_i64().unwrap_or(0);
                                            if let (Some(catalog_id), Some(store_id)) = (
                                                row["catalogId"].as_str(),
                                                row["storeId"].as_str(),
                                            ) {
                                                if ui.button("Rename conversation").clicked() {
                                                    let mut scope = self.controller.scope().clone();
                                                    scope.store_id = Some(store_id.to_owned());
                                                    open_rename_editor(
                                                        &ctx,
                                                        title.to_owned(),
                                                        RenameTarget::Acp {
                                                            scope,
                                                            catalog_id: catalog_id.to_owned(),
                                                            workspace_id: workspace.id.clone(),
                                                            workspace_path: workspace.path.clone(),
                                                            session_id: session_id.to_owned(),
                                                            expected_revision: revision,
                                                        },
                                                    );
                                                    ui.close();
                                                }
                                            }
                                            let next_pinned = !pinned;
                                            if ui.button(if pinned { "Unpin" } else { "Pin" }).clicked() {
                                                self.act(Action::UpdateMetadata {
                                                    session_id: session_id.to_owned(),
                                                    expected_revision: revision,
                                                    patch: json!({"pinned":next_pinned}),
                                                });
                                                ui.close();
                                            }
                                            if ui.button("Hide conversation").clicked() {
                                                self.act(Action::UpdateMetadata {
                                                    session_id: session_id.to_owned(),
                                                    expected_revision: revision,
                                                    patch: json!({"hidden":true}),
                                                });
                                                ui.close();
                                            }
                                        }
                                    });
                                }
                                ui.add_space(9.0);
                            });
                        }
                    });
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(RichText::new("egui · wgpu").small().color(theme::MUTED));
                });
            });

        let collapsed_now = !sidebar_expanded;
        if collapsed_now != sidebar_collapsed {
            self.presentation
                .panels
                .insert("sidebarCollapsed".into(), json!(collapsed_now));
            self.presentation_dirty = true;
        }
        if let Some(panel) = panel {
            let actual = panel.response.rect.width();
            if (actual - width).abs() > 1.0 {
                self.presentation
                    .panels
                    .insert("sidebarWidth".into(), json!(actual));
                self.presentation_dirty = true;
            }
        }
        self.show_workspace_removal_confirmation(&ctx);
        self.show_sidebar_rename_editor(&ctx);
    }

    fn show_workspace_removal_confirmation(&mut self, ctx: &egui::Context) {
        let id = egui::Id::new(REMOVE_WORKSPACE_ID);
        let Some(confirmation) = ctx.data(|data| data.get_temp::<RemoveWorkspaceConfirmation>(id))
        else {
            return;
        };
        if !scope_matches(&confirmation.scope, self.controller.scope()) {
            ctx.data_mut(|data| data.remove_temp::<RemoveWorkspaceConfirmation>(id));
            self.notices
                .push("Workspace removal was cancelled because the active backend changed.".into());
            return;
        }
        let mut open = true;
        let mut confirm = false;
        let mut cancel = false;
        egui::Window::new("Remove workspace")
            .id(egui::Id::new("sidebar-remove-workspace-window"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Remove {} from Monitor on {}?",
                    confirmation.workspace_name, confirmation.backend_label
                ));
                if confirmation.is_worktree {
                    ui.label(format!(
                        "This stops its active agent session and removes the worktree directory at {}.",
                        confirmation.workspace_path
                    ));
                } else {
                    ui.label("This stops its active agent session and keeps the main repository directory and files.");
                    let child_count = confirmation.removed_workspace_ids.len().saturating_sub(1);
                    if child_count > 0 {
                        ui.colored_label(
                            theme::ERROR,
                            format!(
                                "It also removes {} registered child worktree(s) and their directories.",
                                child_count
                            ),
                        );
                    }
                }
                ui.horizontal(|ui| {
                    confirm = ui
                        .button(if confirmation.is_worktree {
                            "Remove worktree and stop session"
                        } else {
                            "Remove workspace and stop session"
                        })
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });

        if cancel || !open {
            ctx.data_mut(|data| data.remove_temp::<RemoveWorkspaceConfirmation>(id));
            return;
        }
        if confirm {
            if !scope_matches(&confirmation.scope, self.controller.scope()) {
                ctx.data_mut(|data| data.remove_temp::<RemoveWorkspaceConfirmation>(id));
                self.notices.push(
                    "Workspace removal was cancelled because the active backend changed.".into(),
                );
                return;
            }
            let method = if confirmation.is_worktree {
                "remove_worktree"
            } else {
                "remove_workspace"
            };
            let request = BackendRequest::Workspaces(WorkspaceRequest::mutation(
                method,
                json!({"id":confirmation.workspace_id}),
                None,
            ));
            match self.controller.request(request) {
                Ok(request_id) => {
                    self.after_request
                        .insert(request_id, RootRefresh::Workspaces);
                    if self.selected_workspace.as_ref().is_some_and(|selected| {
                        confirmation.removed_workspace_ids.contains(selected)
                    }) {
                        self.selected_workspace = None;
                    }
                    if self
                        .controller
                        .state
                        .selected
                        .as_ref()
                        .is_some_and(|selected| {
                            let workspace_id = match &selected.conversation {
                                ConversationRef::Codex { workspace_id, .. }
                                | ConversationRef::Acp { workspace_id, .. } => workspace_id,
                            };
                            confirmation.removed_workspace_ids.contains(workspace_id)
                        })
                    {
                        self.controller.state.selected = None;
                    }
                }
                Err(error) => self
                    .notices
                    .push(format!("Workspace removal was not queued: {error:?}")),
            }
            ctx.data_mut(|data| data.remove_temp::<RemoveWorkspaceConfirmation>(id));
        }
    }

    fn show_sidebar_rename_editor(&mut self, ctx: &egui::Context) {
        let id = egui::Id::new(RENAME_EDITOR_ID);
        let Some(mut draft) = ctx.data(|data| data.get_temp::<RenameEditor>(id)) else {
            return;
        };
        if !rename_target_matches(
            &draft.target,
            self.controller.scope(),
            self.controller.state.runtime_store_id.as_deref(),
            &self.controller.state.catalog,
        ) {
            ctx.data_mut(|data| data.remove_temp::<RenameEditor>(id));
            self.notices
                .push("Rename was cancelled because the backend or conversation changed.".into());
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Rename conversation")
            .id(egui::Id::new("sidebar-rename-window"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Conversation title");
                let response = ui.text_edit_singleline(&mut draft.title);
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    save = true;
                }
                ui.horizontal(|ui| {
                    save |= ui.button("Save title").clicked();
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            open = false;
        }
        if save {
            let title = draft.title.trim().to_owned();
            match draft.target {
                RenameTarget::Codex {
                    scope,
                    workspace_id,
                    thread_id,
                } => {
                    if !scope_matches(&scope, self.controller.scope()) {
                        self.notices.push("Rename was cancelled because the active backend changed.".into());
                        ctx.data_mut(|data| data.remove_temp::<RenameEditor>(id));
                        return;
                    }
                    let key = codex_key(&scope.endpoint_id, &workspace_id, &thread_id);
                    let legacy_key = legacy_codex_key(&workspace_id, &thread_id);
                    if title.is_empty() {
                        self.presentation.custom_names.remove(&key);
                        self.presentation.custom_names.remove(&legacy_key);
                    } else {
                        self.presentation.custom_names.insert(key, title.clone());
                    }
                    self.presentation_dirty = true;
                    let request = BackendRequest::Codex(CodexRequest::mutation(
                        "set_thread_name",
                        json!({"workspaceId":workspace_id,"threadId":thread_id,"name":title}),
                        None,
                    ));
                    if let Err(error) = self.controller.request(request) {
                        self.notices.push(format!("Could not rename conversation: {error:?}"));
                    }
                }
                RenameTarget::Acp {
                    scope: _,
                    catalog_id: _,
                    workspace_id: _,
                    workspace_path: _,
                    session_id,
                    expected_revision,
                } => self.act(Action::UpdateMetadata {
                    session_id,
                    expected_revision,
                    patch: json!({"titleOverride":if title.is_empty(){Value::Null}else{json!(title)}}),
                }),
            }
            ctx.data_mut(|data| data.remove_temp::<RenameEditor>(id));
        } else if open {
            ctx.data_mut(|data| data.insert_temp(id, draft));
        } else {
            ctx.data_mut(|data| data.remove_temp::<RenameEditor>(id));
        }
    }

    pub(super) fn show_agent_chooser(&mut self, ctx: &egui::Context) {
        let Some(workspace_id) = self.agent_chooser.clone() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Choose an agent")
            .id(egui::Id::new("agent-chooser"))
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Create a conversation with the selected runtime.");
                if ui.button("Codex").clicked() {
                    self.act(Action::CreateConversation {
                        workspace_id: workspace_id.clone(),
                        profile_id: None,
                        prepared_connection: None,
                    });
                    self.agent_chooser = None;
                    self.workbench_open = false;
                }
                for profile in self.controller.state.profiles.clone() {
                    if profile["kind"].as_str() == Some("codex") {
                        continue;
                    }
                    let name = profile["name"].as_str().unwrap_or("ACP agent");
                    let profile_id = profile["id"].as_str().unwrap_or("").to_owned();
                    ui.horizontal(|ui| {
                        if ui.button(format!("New · {name}")).clicked() {
                            self.act(Action::CreateConversation {
                                workspace_id: workspace_id.clone(),
                                profile_id: Some(profile_id.clone()),
                                prepared_connection: None,
                            });
                            self.agent_chooser = None;
                            self.workbench_open = false;
                        }
                        if ui.small_button("Connect / sign in").clicked() {
                            let cwd = self.controller.state.workspaces.iter()
                                .find(|workspace| workspace.id == workspace_id)
                                .map(|workspace| workspace.path.clone());
                            self.acp_connections.open_profile(&mut self.controller, profile_id, cwd, Some(workspace_id.clone()));
                        }
                    });
                }
                ui.separator();
                if ui.button("Configure agent profiles").clicked() {
                    self.profile_editor = Some(browser::ProfileEditor::default());
                    self.agent_chooser = None;
                }
            });
        if !open {
            self.agent_chooser = None;
        }
    }
}

fn open_rename_editor(ctx: &egui::Context, title: String, target: RenameTarget) {
    ctx.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(RENAME_EDITOR_ID),
            RenameEditor { title, target },
        )
    });
}

fn codex_key(endpoint: &str, workspace: &str, thread: &str) -> String {
    format!("{endpoint}:{workspace}:{thread}")
}

fn legacy_codex_key(workspace: &str, thread: &str) -> String {
    format!("{workspace}:{thread}")
}

fn custom_name(
    presentation: &UiStateV1,
    endpoint: &str,
    workspace: &str,
    thread: &str,
) -> Option<String> {
    presentation
        .custom_names
        .get(&codex_key(endpoint, workspace, thread))
        .or_else(|| {
            presentation
                .custom_names
                .get(&legacy_codex_key(workspace, thread))
        })
        .cloned()
        .filter(|name| !name.is_empty())
}

fn pin_timestamp(
    presentation: &UiStateV1,
    endpoint: &str,
    workspace: &str,
    thread: &str,
) -> Option<i64> {
    presentation
        .pinned_threads
        .get(&codex_key(endpoint, workspace, thread))
        .or_else(|| {
            presentation
                .pinned_threads
                .get(&legacy_codex_key(workspace, thread))
        })
        .copied()
}

fn toggle_codex_pin(
    presentation: &mut UiStateV1,
    endpoint: &str,
    workspace: &str,
    thread: &str,
    pinned: bool,
) {
    let key = codex_key(endpoint, workspace, thread);
    let legacy_key = legacy_codex_key(workspace, thread);
    if pinned {
        presentation.pinned_threads.remove(&key);
        presentation.pinned_threads.remove(&legacy_key);
    } else {
        presentation.pinned_threads.insert(key, now_ms() as i64);
    }
}

fn order_codex_rows<T: Clone>(
    rows: Vec<T>,
    endpoint: &str,
    workspace: &str,
    presentation: &UiStateV1,
    id: impl Fn(&T) -> String,
    parent_id: impl Fn(&T) -> Option<String>,
    updated_at: impl Fn(&T) -> i64,
    created_at: impl Fn(&T) -> i64,
) -> Vec<T> {
    use std::collections::{HashMap, HashSet};

    let ids: HashSet<String> = rows.iter().map(&id).collect();
    let original_index: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (id(row), index))
        .collect();
    let mut roots: Vec<_> = rows
        .iter()
        .filter(|row| parent_id(row).is_none_or(|parent| !ids.contains(&parent)))
        .cloned()
        .collect();
    roots.sort_by(|a, b| {
        let a_id = id(a);
        let b_id = id(b);
        let a_pin = pin_timestamp(presentation, endpoint, workspace, &a_id);
        let b_pin = pin_timestamp(presentation, endpoint, workspace, &b_id);
        match (a_pin, b_pin) {
            (Some(a), Some(b)) if a != b => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            _ => {
                let time = match presentation.thread_list_sort_key.as_deref() {
                    Some("created_at") => created_at(b).cmp(&created_at(a)),
                    _ => updated_at(b).cmp(&updated_at(a)),
                };
                time.then_with(|| original_index[&a_id].cmp(&original_index[&b_id]))
            }
        }
    });

    fn append_subtree<T: Clone>(
        root_id: String,
        rows: &[T],
        id: &impl Fn(&T) -> String,
        parent_id: &impl Fn(&T) -> Option<String>,
        seen: &mut HashSet<String>,
        ordered: &mut Vec<T>,
    ) {
        let mut pending = vec![root_id];
        while let Some(parent) = pending.pop() {
            if !seen.insert(parent.clone()) {
                continue;
            }
            if let Some(row) = rows.iter().find(|row| id(row) == parent) {
                ordered.push(row.clone());
                let children: Vec<String> = rows
                    .iter()
                    .filter(|row| parent_id(row).as_deref() == Some(parent.as_str()))
                    .map(id)
                    .collect();
                pending.extend(children.into_iter().rev());
            }
        }
    }

    let mut ordered = Vec::with_capacity(rows.len());
    let mut seen = HashSet::new();
    for root in &roots {
        append_subtree(id(root), &rows, &id, &parent_id, &mut seen, &mut ordered);
    }

    // Rows left over after visiting ordinary roots can form a parent cycle. Pick
    // one stable member of that cycle as a temporary root, then traverse its
    // children normally. This keeps the walk bounded and still places ordinary
    // descendants after their visible parents; only the cycle's closing edge
    // is necessarily broken.
    for row in &rows {
        let start = id(row);
        if seen.contains(&start) {
            continue;
        }
        let mut path = Vec::new();
        let mut positions = HashMap::new();
        let mut cursor = start.clone();
        let promoted = loop {
            if seen.contains(&cursor) {
                break start.clone();
            }
            if let Some(cycle_start) = positions.get(&cursor).copied() {
                break path[cycle_start..]
                    .iter()
                    .min_by_key(|member| original_index.get(*member).copied().unwrap_or(usize::MAX))
                    .cloned()
                    .unwrap_or(start.clone());
            }
            positions.insert(cursor.clone(), path.len());
            path.push(cursor.clone());
            let Some(parent) = rows
                .iter()
                .find(|candidate| id(candidate) == cursor)
                .and_then(|candidate| parent_id(candidate))
                .filter(|parent| ids.contains(parent))
            else {
                break cursor;
            };
            cursor = parent;
        };
        append_subtree(promoted, &rows, &id, &parent_id, &mut seen, &mut ordered);
    }
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend_scope(
        endpoint_id: &str,
        connection_epoch: u64,
        store_id: Option<&str>,
    ) -> BackendScope {
        BackendScope {
            endpoint_id: endpoint_id.into(),
            connection_epoch,
            store_id: store_id.map(str::to_owned),
        }
    }

    #[derive(Clone)]
    struct Row {
        id: String,
        parent_id: Option<String>,
        updated_at: i64,
        created_at: i64,
    }

    fn row(id: &str, parent_id: Option<&str>, updated_at: i64, created_at: i64) -> Row {
        Row {
            id: id.into(),
            parent_id: parent_id.map(str::to_owned),
            updated_at,
            created_at,
        }
    }

    #[test]
    fn sidebar_organize_and_sort_choices_update_only_supported_persisted_values() {
        let mut state = UiStateV1::empty();
        assert!(set_thread_organize_mode(&mut state, "by_project_activity"));
        assert_eq!(
            state.thread_list_organize_mode.as_deref(),
            Some("by_project_activity")
        );
        assert!(!set_thread_organize_mode(&mut state, "unknown"));
        assert!(!set_thread_organize_mode(&mut state, "by_project_activity"));

        assert!(set_thread_sort_key(&mut state, "created_at"));
        assert_eq!(state.thread_list_sort_key.as_deref(), Some("created_at"));
        assert!(!set_thread_sort_key(&mut state, "random"));
        assert!(!set_thread_sort_key(&mut state, "created_at"));
    }

    #[test]
    fn stale_dialog_scope_rejects_endpoint_epoch_and_store_changes_before_mutation() {
        let captured = backend_scope("daemon-a", 7, Some("store-a"));
        let mismatched_scopes = [
            backend_scope("daemon-b", 7, Some("store-a")),
            backend_scope("daemon-a", 8, Some("store-a")),
            backend_scope("daemon-a", 7, Some("store-b")),
        ];

        for active in mismatched_scopes {
            let mut mutation_sent = false;
            if scope_matches(&captured, &active) {
                mutation_sent = true;
            }
            assert!(
                !mutation_sent,
                "stale dialog scope must not dispatch a mutation"
            );
        }
    }

    #[test]
    fn acp_rename_requires_current_scope_and_exact_catalog_identity() {
        let active = backend_scope("local:data", 3, None);
        let target = RenameTarget::Acp {
            scope: backend_scope("local:data", 3, Some("store-a")),
            catalog_id: "catalog-a".into(),
            workspace_id: "workspace-a".into(),
            workspace_path: "/repo/a".into(),
            session_id: "session-a".into(),
            expected_revision: 12,
        };
        let row = json!({
            "catalogId":"catalog-a",
            "storeId":"store-a",
            "workspaceId":"workspace-a",
            "sessionId":"session-a",
            "revision":12
        });
        assert!(rename_target_matches(
            &target,
            &active,
            Some("store-a"),
            std::slice::from_ref(&row),
        ));

        let mismatches = [
            (
                backend_scope("daemon-b", 3, None),
                Some("store-a"),
                vec![row.clone()],
            ),
            (
                backend_scope("local:data", 4, None),
                Some("store-a"),
                vec![row.clone()],
            ),
            (active.clone(), Some("store-b"), vec![row.clone()]),
            (
                active.clone(),
                Some("store-a"),
                vec![json!({
                    "catalogId":"catalog-b",
                    "storeId":"store-a",
                    "workspaceId":"workspace-a",
                    "sessionId":"session-a",
                    "revision":12
                })],
            ),
        ];
        for (active_scope, runtime_store, catalog) in mismatches {
            let mut mutation_sent = false;
            if rename_target_matches(&target, &active_scope, runtime_store, &catalog) {
                mutation_sent = true;
            }
            assert!(
                !mutation_sent,
                "stale ACP identity must not dispatch a mutation"
            );
        }
    }

    #[test]
    fn imported_legacy_names_and_pins_are_fallbacks_for_native_scoped_keys() {
        let mut state = UiStateV1::empty();
        state.custom_names.insert(
            legacy_codex_key("workspace", "thread"),
            "Imported title".into(),
        );
        state
            .pinned_threads
            .insert(legacy_codex_key("workspace", "thread"), 42);

        assert_eq!(
            custom_name(&state, "endpoint", "workspace", "thread").as_deref(),
            Some("Imported title")
        );
        assert_eq!(
            pin_timestamp(&state, "endpoint", "workspace", "thread"),
            Some(42)
        );
    }

    #[test]
    fn pin_order_moves_roots_without_detaching_children() {
        let mut state = UiStateV1::empty();
        state
            .pinned_threads
            .insert(codex_key("endpoint", "workspace", "second"), 1);
        let ordered = order_codex_rows(
            vec![
                row("first", None, 30, 10),
                row("child", Some("first"), 20, 5),
                row("second", None, 10, 20),
            ],
            "endpoint",
            "workspace",
            &state,
            |row| row.id.clone(),
            |row| row.parent_id.clone(),
            |row| row.updated_at,
            |row| row.created_at,
        );

        let ids: Vec<_> = ordered.into_iter().map(|row| row.id).collect();
        assert_eq!(
            ids,
            vec!["second".to_owned(), "first".to_owned(), "child".to_owned()]
        );
    }

    #[test]
    fn child_first_input_is_flattened_parent_first_at_every_depth() {
        let state = UiStateV1::empty();
        let ordered = order_codex_rows(
            vec![
                row("grandchild", Some("child"), 1, 1),
                row("sibling", Some("root"), 1, 1),
                row("child", Some("root"), 1, 1),
                row("root", None, 1, 1),
            ],
            "endpoint",
            "workspace",
            &state,
            |row| row.id.clone(),
            |row| row.parent_id.clone(),
            |row| row.updated_at,
            |row| row.created_at,
        );

        let ids: Vec<_> = ordered.into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["root", "sibling", "child", "grandchild"]);
    }

    #[test]
    fn missing_or_hidden_parent_promotes_child_to_root_in_input_order() {
        let state = UiStateV1::empty();
        // The caller passes only visible rows, so a hidden parent is equivalent
        // to an unavailable parent for sidebar placement.
        let ordered = order_codex_rows(
            vec![
                row("orphan", Some("hidden-parent"), 1, 1),
                row("other-root", None, 1, 1),
            ],
            "endpoint",
            "workspace",
            &state,
            |row| row.id.clone(),
            |row| row.parent_id.clone(),
            |row| row.updated_at,
            |row| row.created_at,
        );

        let ids: Vec<_> = ordered.into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["orphan", "other-root"]);
    }

    #[test]
    fn cycle_recovery_is_bounded_and_keeps_descendants_after_promoted_parent() {
        let state = UiStateV1::empty();
        let ordered = order_codex_rows(
            vec![
                row("descendant", Some("cycle-a"), 1, 1),
                row("cycle-a", Some("cycle-b"), 1, 1),
                row("cycle-b", Some("cycle-a"), 1, 1),
            ],
            "endpoint",
            "workspace",
            &state,
            |row| row.id.clone(),
            |row| row.parent_id.clone(),
            |row| row.updated_at,
            |row| row.created_at,
        );

        let ids: Vec<_> = ordered.into_iter().map(|row| row.id).collect();
        assert_eq!(ids, vec!["cycle-a", "descendant", "cycle-b"]);
    }
}
