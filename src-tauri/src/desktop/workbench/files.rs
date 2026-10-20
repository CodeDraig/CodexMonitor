use super::{mutate_files, read_files, PanelEffect, WorkbenchRequestKey};
use egui::{self, Ui};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub(crate) struct FilesPanel {
    workspace: Option<String>,
    stale: bool,
    paths: Vec<String>,
    search: String,
    selected: Option<String>,
    content: String,
    truncated: bool,
    loading: bool,
    error: Option<String>,
    edit_policy: bool,
    policy_scope: String,
    policy_kind: String,
    dirty: bool,
    status: Option<String>,
    refresh_selected: bool,
}
impl FilesPanel {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
    }
    pub(crate) fn mark_workspace_files_changed(&mut self, paths: &[String], workspace_path: &str) {
        self.stale = true;
        self.refresh_selected = self.selected.as_ref().is_some_and(|selected| {
            !selected.starts_with("policy:")
                && paths.iter().any(|path| {
                    super::workspace_relative_event_matches(path, selected, workspace_path)
                })
        });
        if self.refresh_selected {
            self.loading = true;
            self.error = None;
        }
    }
    #[cfg(test)]
    pub(crate) fn is_stale(&self) -> bool {
        self.stale
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace: Option<&str>,
    ) {
        if self.policy_scope.is_empty() {
            self.policy_scope = "workspace".into();
        }
        if self.policy_kind.is_empty() {
            self.policy_kind = "agents".into();
        }
        if self.workspace.as_deref() != workspace {
            self.workspace = workspace.map(str::to_owned);
            self.paths.clear();
            self.selected = None;
            self.content.clear();
            self.stale = true;
            self.dirty = false;
        }
        ui.horizontal(|ui| {
            ui.heading("Workspace files");
            if ui.button("Refresh tree").clicked() {
                self.stale = true;
            }
        });
        if self.stale {
            if let Some(w) = workspace {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::FilesList,
                    request: read_files("list_workspace_files", json!({"workspaceId":w})),
                });
                self.stale = false;
            }
        }
        if self.refresh_selected {
            self.refresh_selected = false;
            if let (Some(w), Some(path)) = (workspace, self.selected.as_ref()) {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::FileRead(path.clone()),
                    request: read_files(
                        "read_workspace_file",
                        json!({"workspaceId":w,"path":path,"maxBytes":1048576}),
                    ),
                });
            }
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.edit_policy, "Edit approved Codex files");
            if self.edit_policy {
                egui::ComboBox::from_id_salt("file-scope")
                    .selected_text(&self.policy_scope)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.policy_scope,
                            "workspace".into(),
                            "Workspace",
                        );
                        ui.selectable_value(&mut self.policy_scope, "global".into(), "Global");
                    });
                egui::ComboBox::from_id_salt("file-kind")
                    .selected_text(&self.policy_kind)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.policy_kind, "agents".into(), "AGENTS.md");
                        ui.selectable_value(&mut self.policy_kind, "config".into(), "config.toml");
                    });
                if ui.button("Load approved file").clicked() {
                    let scope = self.policy_scope.clone();
                    let kind = self.policy_kind.clone();
                    effects.push(PanelEffect::Request {
                        key: WorkbenchRequestKey::FileRead(format!("policy:{scope}:{kind}")),
                        request: read_files(
                            "file_read",
                            json!({"scope":scope,"kind":kind,"workspaceId":workspace}),
                        ),
                    });
                }
            }
        });
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        ui.horizontal(|ui| {
            ui.label("Find path");
            ui.add(
                egui::TextEdit::singleline(&mut self.search).hint_text("Filter workspace files"),
            );
            if !self.search.is_empty() && ui.button("Clear").clicked() {
                self.search.clear();
            }
        });
        let filtered_paths: Vec<String> = self
            .paths
            .iter()
            .filter(|path| path_matches(path, &self.search))
            .cloned()
            .collect();
        let directory_tree = build_directory_tree(&self.paths);
        let workspace_scope = workspace.unwrap_or_default().to_owned();
        let search_active = !self.search.trim().is_empty();
        ui.columns(2, |cols| {
            egui::ScrollArea::vertical()
                .id_salt("workspace-file-list")
                .max_height(500.0)
                .show(&mut cols[0], |ui| {
                    if search_active {
                        // Search results stay flat so filtering can find files at any
                        // depth without requiring their parent folders to be open.
                        for path in &filtered_paths {
                            render_file_row(
                                ui,
                                path,
                                &workspace_scope,
                                &mut self.selected,
                                &mut self.loading,
                                effects,
                            );
                        }
                    } else {
                        render_directory_tree(
                            ui,
                            &directory_tree,
                            "",
                            &workspace_scope,
                            &mut self.selected,
                            &mut self.loading,
                            effects,
                        );
                    }
                    if filtered_paths.is_empty() {
                        ui.weak("No paths match this filter.");
                    }
                });
            cols[1].vertical(|ui| {
                if let Some(path) = self.selected.as_ref().filter(|path| !path.starts_with("policy:")) {
                    if ui.button("Open in…").clicked() {
                        effects.push(PanelEffect::OpenIn { workspace_id: workspace.unwrap_or_default().to_owned(), path: path.clone() });
                    }
                }
                if self.loading {
                    ui.spinner();
                }
                if self.truncated {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "File was truncated at the backend size limit.",
                    );
                }
                if self.edit_policy
                    && self
                        .selected
                        .as_deref()
                        .is_some_and(|value| value.starts_with("policy:"))
                {
                    if ui
                        .add(egui::TextEdit::multiline(&mut self.content).desired_rows(24).code_editor())
                        .changed()
                    {
                        self.dirty = true;
                    }
                    if ui
                        .add_enabled(self.dirty, egui::Button::new("Save approved file"))
                        .clicked()
                    {
                        let scope = self.policy_scope.clone();
                        let kind = self.policy_kind.clone();
                        effects.push(PanelEffect::Request {
                            key: WorkbenchRequestKey::FileWrite(format!("policy:{scope}:{kind}")),
                            request: mutate_files(
                                "file_write",
                                json!({"scope":scope,"kind":kind,"workspaceId":workspace,"content":self.content}),
                            ),
                        });
                        self.loading = true;
                    }
                } else if self.selected.is_some() {
                    let mut readonly = self.content.as_str();
                    ui.add(
                        egui::TextEdit::multiline(&mut readonly)
                            .desired_rows(24)
                            .code_editor(),
                    );
                } else {
                    ui.label("Choose a file to inspect.");
                }
            });
        });
    }
    pub(crate) fn apply(&mut self, key: &WorkbenchRequestKey, value: Value) {
        match key {
            WorkbenchRequestKey::FilesList => {
                self.paths = value
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
            }
            WorkbenchRequestKey::FileRead(path) => {
                self.loading = false;
                self.error = None;
                if path.starts_with("policy:") {
                    self.content = value["content"].as_str().unwrap_or_default().to_owned();
                    self.truncated = value["truncated"].as_bool().unwrap_or(false);
                    self.selected = Some(path.clone());
                    self.dirty = false;
                } else {
                    self.content = value["content"].as_str().unwrap_or_default().to_owned();
                    self.truncated = value["truncated"].as_bool().unwrap_or(false);
                    self.dirty = false;
                }
            }
            WorkbenchRequestKey::FileWrite(_) => {
                self.loading = false;
                self.dirty = false;
                self.status = Some("File saved".into());
            }
            _ => {}
        }
    }
}


#[derive(Debug, Default)]
struct DirectoryNode {
    directories: BTreeMap<String, DirectoryNode>,
    files: Vec<String>,
}

fn build_directory_tree(paths: &[String]) -> DirectoryNode {
    let mut root = DirectoryNode::default();
    for path in paths {
        let components: Vec<_> = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if components.is_empty() {
            continue;
        }
        let mut current = &mut root;
        for component in &components[..components.len() - 1] {
            current = current
                .directories
                .entry((*component).to_owned())
                .or_default();
        }
        current.files.push(path.clone());
    }
    root
}

fn render_directory_tree(
    ui: &mut Ui,
    node: &DirectoryNode,
    parent_path: &str,
    workspace_scope: &str,
    selected: &mut Option<String>,
    loading: &mut bool,
    effects: &mut Vec<PanelEffect>,
) {
    for (name, directory) in &node.directories {
        let path = if parent_path.is_empty() {
            name.clone()
        } else {
            format!("{parent_path}/{name}")
        };
        egui::CollapsingHeader::new(name)
            .id_salt(("workspace-file-directory", workspace_scope, &path))
            .show(ui, |ui| {
                render_directory_tree(
                    ui,
                    directory,
                    &path,
                    workspace_scope,
                    selected,
                    loading,
                    effects,
                );
            });
    }
    for path in &node.files {
        render_file_row(ui, path, workspace_scope, selected, loading, effects);
    }
}

fn render_file_row(
    ui: &mut Ui,
    path: &str,
    workspace_scope: &str,
    selected: &mut Option<String>,
    loading: &mut bool,
    effects: &mut Vec<PanelEffect>,
) {
    if ui
        .selectable_label(selected.as_deref() == Some(path), path)
        .clicked()
    {
        *selected = Some(path.to_owned());
        *loading = true;
        effects.push(PanelEffect::Request {
            key: WorkbenchRequestKey::FileRead(path.to_owned()),
            request: read_files(
                "read_workspace_file",
                json!({"workspaceId":workspace_scope,"path":path}),
            ),
        });
    }
}

fn path_matches(path: &str, query: &str) -> bool {
    query.trim().is_empty() || path.to_lowercase().contains(&query.trim().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{kittest::Queryable as _, Harness};

    #[test]
    fn path_filter_is_case_insensitive_and_does_not_mutate_path_inventory() {
        let panel = FilesPanel {
            paths: vec!["src/main.rs".into(), "README.md".into()],
            selected: Some("src/main.rs".into()),
            search: "readme".into(),
            ..FilesPanel::default()
        };
        let visible: Vec<_> = panel
            .paths
            .iter()
            .filter(|path| path_matches(path, &panel.search))
            .map(String::as_str)
            .collect();

        assert_eq!(visible, vec!["README.md"]);
        assert_eq!(panel.paths.len(), 2);
        assert_eq!(panel.selected.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn directory_tree_groups_relative_paths_and_keeps_remote_paths_intact() {
        let paths = vec![
            "src/main.rs".to_owned(),
            "src/lib.rs".to_owned(),
            "README.md".to_owned(),
            "nested/docs/guide.md".to_owned(),
        ];
        let tree = build_directory_tree(&paths);

        assert_eq!(tree.files, vec!["README.md"]);
        assert_eq!(
            tree.directories["src"].files,
            vec!["src/main.rs", "src/lib.rs"]
        );
        assert_eq!(
            tree.directories["nested"].directories["docs"].files,
            vec!["nested/docs/guide.md"]
        );
        assert_eq!(paths[3], "nested/docs/guide.md");
    }

    #[test]
    fn directory_tree_folder_expands_and_collapses_its_children() {
        let tree = build_directory_tree(&["src/main.rs".to_owned()]);
        let mut selected = None;
        let mut loading = false;
        let mut effects = Vec::new();
        let mut harness = Harness::new_ui(|ui| {
            render_directory_tree(
                ui,
                &tree,
                "",
                "workspace-1",
                &mut selected,
                &mut loading,
                &mut effects,
            );
        });

        assert!(harness.query_by_label("src/main.rs").is_none());
        harness.get_by_label("src").click();
        harness.run();
        assert!(harness.query_by_label("src/main.rs").is_some());
        harness.get_by_label("src").click();
        harness.run();
        assert!(harness.query_by_label("src/main.rs").is_none());
        drop(harness);
        assert!(selected.is_none());
        assert!(effects.is_empty());
    }

    #[test]
    fn changed_selected_readonly_file_emits_a_fresh_read_request() {
        let mut panel = FilesPanel {
            workspace: Some("workspace-a".into()),
            selected: Some("/work/repo/src/main file.rs".into()),
            content: "old contents".into(),
            ..FilesPanel::default()
        };
        panel.mark_workspace_files_changed(&["src/main file.rs".into()], "/work/repo");
        let effects = std::cell::RefCell::new(Vec::new());
        let mut harness = Harness::new_ui(|ui| {
            panel.show(ui, &mut effects.borrow_mut(), Some("workspace-a"));
        });
        // The pending read intentionally keeps the spinner repainting until a
        // backend reply arrives; inspect bounded frames rather than waiting idle.
        harness.run_steps(2);
        drop(harness);
        let effects = effects.into_inner();
        assert!(effects.iter().any(|effect| matches!(
            effect,
            PanelEffect::Request {
                key: WorkbenchRequestKey::FileRead(path),
                request,
            } if path == "/work/repo/src/main file.rs"
                && request.rpc().params["path"].as_str() == Some(path.as_str())
        )));
        assert_eq!(panel.content, "old contents", "refresh must preserve prior content until the read reply arrives");
    }
}
