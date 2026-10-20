//! Workspace- and backend-scoped terminal tabs for the native shell.

use super::NativeApp;
use crate::desktop::terminal::TerminalPanel;
use egui::{self, Color32};
use serde_json::json;
use std::{path::Path, sync::Arc};

const DEFAULT_TERMINAL_HEIGHT: f32 = 240.0;
const MIN_TERMINAL_HEIGHT: f32 = 100.0;
const MAX_TERMINAL_HEIGHT: f32 = 1200.0;

fn saved_terminal_height(value: Option<&serde_json::Value>) -> f32 {
    value
        .and_then(serde_json::Value::as_f64)
        .filter(|height| height.is_finite())
        .map(|height| (height as f32).clamp(MIN_TERMINAL_HEIGHT, MAX_TERMINAL_HEIGHT))
        .unwrap_or(DEFAULT_TERMINAL_HEIGHT)
}

struct TerminalTab {
    id: String,
    panel: TerminalPanel,
}

/// Terminals for one workspace on one backend connection. The owner keys its
/// collection by backend scope and workspace so switching either cannot reuse
/// a local shell from another context.
pub(super) struct WorkspaceTerminals {
    workspace_id: String,
    workspace_path: String,
    backend_scope: String,
    remote: bool,
    tabs: Vec<TerminalTab>,
    active: usize,
    next_id: u64,
}

impl WorkspaceTerminals {
    pub(super) fn new(
        workspace_id: impl Into<String>,
        workspace_path: impl Into<String>,
        backend_scope: impl Into<String>,
        remote: bool,
    ) -> Self {
        let workspace_id = workspace_id.into();
        let mut value = Self {
            workspace_id,
            workspace_path: workspace_path.into(),
            backend_scope: backend_scope.into(),
            remote,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
        };
        value.add_tab();
        value
    }

    pub(super) fn close_all(&mut self) {
        for tab in &mut self.tabs {
            let _ = tab.panel.close();
        }
        self.tabs.clear();
        self.active = 0;
    }

    fn add_tab(&mut self) {
        let number = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let id = format!("{}", number);
        let panel = if self.remote {
            TerminalPanel::new_remote(&self.workspace_id, format!("terminal-{id}"))
        } else {
            TerminalPanel::new(&self.workspace_id, format!("terminal-{id}"))
        };
        self.tabs.push(TerminalTab { id, panel });
        self.active = self.tabs.len() - 1;
    }

    fn close_active(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let mut tab = self.tabs.remove(self.active.min(self.tabs.len() - 1));
        let _ = tab.panel.close();
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
    }

    fn active_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active)
    }
}

impl NativeApp {
    pub(super) fn show_terminal(&mut self, ui: &mut egui::Ui) {
        if !self.terminal_open {
            return;
        }

        let default_height =
            saved_terminal_height(self.presentation.panels.get("terminalPanelHeight"));
        let response = egui::Panel::bottom("terminal-dock")
            .resizable(true)
            .default_size(default_height)
            .show(ui, |ui| {
                let Some(workspace) = self
                    .controller
                    .state
                    .workspaces
                    .iter()
                    .find(|workspace| Some(&workspace.id) == self.selected_workspace.as_ref())
                    .cloned()
                else {
                    ui.label("Select a workspace for the terminal.");
                    return;
                };

                let scope = self.controller.scope();
                let remote = matches!(
                    self.settings.backend_mode,
                    crate::types::BackendMode::Remote
                );
                // A local PTY belongs to this host and workspace, so returning
                // from a remote backend should reveal the existing shell. A
                // remote terminal context is unavailable today, but retain a
                // distinct scoped dock for each remote connection.
                let backend_scope = if remote {
                    format!(
                        "remote:{}:{}:{}",
                        scope.endpoint_id,
                        scope.store_id.as_deref().unwrap_or(""),
                        scope.connection_epoch
                    )
                } else {
                    format!("local:{}", scope.endpoint_id)
                };
                let key = format!("{backend_scope}:{}:{}", workspace.id, workspace.path);
                let wake = ui.ctx().clone();
                let notices = &mut self.notices;
                let dock = self.terminals.entry(key).or_insert_with(|| {
                    WorkspaceTerminals::new(
                        workspace.id.clone(),
                        workspace.path.clone(),
                        backend_scope.clone(),
                        remote,
                    )
                });
                let workspace_path = dock.workspace_path.clone();

                ui.horizontal(|ui| {
                    ui.strong("Terminal");
                    ui.label(workspace.name.as_str());
                    ui.separator();
                    for (index, tab) in dock.tabs.iter().enumerate() {
                        if ui
                            .selectable_label(index == dock.active, format!("{}", tab.id))
                            .clicked()
                        {
                            dock.active = index;
                        }
                    }
                    if ui.button("+").on_hover_text("New terminal tab").clicked() {
                        dock.add_tab();
                    }
                    if ui
                        .button("×")
                        .on_hover_text("Close active terminal tab")
                        .clicked()
                    {
                        dock.close_active();
                    }
                    if ui.button("Open shell").clicked() {
                        if let Some(tab) = dock.active_mut() {
                            let wake = wake.clone();
                            if let Err(error) = tab.panel.open(
                                Path::new(&workspace_path),
                                100,
                                24,
                                Arc::new(move || wake.request_repaint()),
                            ) {
                                notices.push(error);
                            }
                        }
                    }
                    if ui.button("Close shell").clicked() {
                        if let Some(tab) = dock.active_mut() {
                            if let Err(error) = tab.panel.close() {
                                notices.push(error);
                            }
                        }
                    }
                    if ui.button("Close all").clicked() {
                        dock.close_all();
                    }
                });

                if dock.remote {
                    ui.colored_label(
                        Color32::from_gray(170),
                        TerminalPanel::remote_unavailable_reason(),
                    );
                } else if let Some(tab) = dock.active_mut() {
                    tab.panel.show(ui);
                } else {
                    ui.weak("No terminal tabs. Add one with +.");
                }
            });

        let measured_height = response.response.rect.height();
        let saved_height = self
            .presentation
            .panels
            .get("terminalPanelHeight")
            .and_then(serde_json::Value::as_f64);
        if saved_height.is_none_or(|saved| (measured_height - saved as f32).abs() > 1.0) {
            self.presentation
                .panels
                .insert("terminalPanelHeight".into(), json!(measured_height));
            self.presentation_dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{saved_terminal_height, TerminalPanel, WorkspaceTerminals};
    use serde_json::json;
    use std::{path::Path, sync::Arc};

    #[test]
    fn terminal_tabs_have_independent_numbered_identity_and_close_all() {
        let mut terminals = WorkspaceTerminals::new("workspace", "/tmp", "local:test", false);
        assert_eq!(terminals.tabs.len(), 1);
        assert_eq!(terminals.tabs[0].id, "1");
        terminals.add_tab();
        assert_eq!(terminals.tabs.len(), 2);
        assert_eq!(terminals.tabs[1].id, "2");
        assert_eq!(terminals.active, 1);
        terminals.close_active();
        assert_eq!(terminals.tabs.len(), 1);
        terminals.close_all();
        assert!(terminals.tabs.is_empty());
    }

    #[test]
    fn remote_workspace_tabs_are_marked_unavailable_without_local_shell_fallback() {
        let mut terminals = WorkspaceTerminals::new("remote-w", "/ignored", "remote:r", true);
        assert!(terminals.remote);
        assert_eq!(terminals.workspace_id, "remote-w");
        assert_eq!(terminals.backend_scope, "remote:r");
        let error = terminals.tabs[0]
            .panel
            .open(Path::new("/"), 80, 24, Arc::new(|| {}))
            .unwrap_err();
        assert_eq!(error, TerminalPanel::remote_unavailable_reason());
    }

    #[test]
    fn terminal_height_restores_a_valid_persisted_size_and_bounds_bad_values() {
        assert_eq!(saved_terminal_height(None), 240.0);
        assert_eq!(saved_terminal_height(Some(&json!(360.0))), 360.0);
        assert_eq!(saved_terminal_height(Some(&json!(12.0))), 100.0);
        assert_eq!(saved_terminal_height(Some(&json!(5000.0))), 1200.0);
        assert_eq!(saved_terminal_height(Some(&json!("large"))), 240.0);
    }
}
