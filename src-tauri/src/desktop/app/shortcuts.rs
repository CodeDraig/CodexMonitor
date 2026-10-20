//! Native menu bar and configurable keyboard shortcuts.

use super::NativeApp;
use crate::{
    client::{contracts::CodexRequest, ConversationKey, ConversationRef},
    desktop::{controller::Action, platform::HostAction, workbench::WorkbenchPanel},
};
use egui::{self, Key, Modifiers, Ui};
use serde_json::{json, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedShortcut {
    key: String,
    cmd: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

fn parse_shortcut(value: Option<&str>) -> Option<ParsedShortcut> {
    let parts: Vec<_> = value?
        .split('+')
        .map(|part| part.trim().to_ascii_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    let key = parts.last()?.clone();
    if ["cmd", "meta", "ctrl", "control", "alt", "option", "shift"].contains(&key.as_str()) {
        return None;
    }
    let mut shortcut = ParsedShortcut {
        key,
        cmd: parts.iter().any(|part| part == "cmd" || part == "meta"),
        ctrl: parts.iter().any(|part| part == "ctrl" || part == "control"),
        alt: parts.iter().any(|part| part == "alt" || part == "option"),
        shift: parts.iter().any(|part| part == "shift"),
    };
    // The React shortcut parser translates cmd+ctrl into ctrl+alt on
    // non-macOS hosts because the physical Meta key is not portable there.
    if !cfg!(target_os = "macos") && shortcut.cmd && shortcut.ctrl {
        shortcut.cmd = false;
        shortcut.alt = true;
    }
    Some(shortcut)
}

fn key_name(key: Key) -> String {
    match key {
        Key::Space => "space".into(),
        Key::Escape => "escape".into(),
        Key::Enter => "enter".into(),
        Key::Tab => "tab".into(),
        Key::Backspace => "backspace".into(),
        Key::Delete => "delete".into(),
        Key::ArrowUp => "arrowup".into(),
        Key::ArrowDown => "arrowdown".into(),
        Key::ArrowLeft => "arrowleft".into(),
        Key::ArrowRight => "arrowright".into(),
        Key::F1 => "f1".into(),
        Key::F2 => "f2".into(),
        Key::F3 => "f3".into(),
        Key::F4 => "f4".into(),
        Key::F5 => "f5".into(),
        Key::F6 => "f6".into(),
        Key::F7 => "f7".into(),
        Key::F8 => "f8".into(),
        Key::F9 => "f9".into(),
        Key::F10 => "f10".into(),
        Key::F11 => "f11".into(),
        Key::F12 => "f12".into(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

fn matches_shortcut(key: Key, modifiers: Modifiers, value: Option<&str>) -> bool {
    let Some(shortcut) = parse_shortcut(value) else {
        return false;
    };
    let modifiers_match = if cfg!(target_os = "macos") {
        modifiers.mac_cmd == shortcut.cmd && modifiers.ctrl == shortcut.ctrl
    } else {
        modifiers.command == (shortcut.cmd || shortcut.ctrl)
    };
    key_name(key) == shortcut.key
        && modifiers_match
        && modifiers.alt == shortcut.alt
        && modifiers.shift == shortcut.shift
}

fn configured(shortcut: &Option<String>) -> Option<&str> {
    shortcut.as_deref()
}

impl NativeApp {
    /// Draw the native menu bar. The app settings remain the source of shortcut
    /// labels and behavior; menu commands call the same shell actions.
    pub(super) fn show_menus(&mut self, ui: &mut Ui) {
        ui.menu_button("File", |ui| {
            if ui.button("New workspace…").clicked() {
                self.add_workspace();
                ui.close();
            }
            if ui
                .button(format!(
                    "New agent ({})",
                    display_shortcut(self.settings.new_agent_shortcut.as_deref())
                ))
                .clicked()
            {
                self.start_new_agent();
                ui.close();
            }
            if ui
                .button(format!(
                    "New worktree agent… ({})",
                    display_shortcut(self.settings.new_worktree_agent_shortcut.as_deref())
                ))
                .clicked()
            {
                self.start_workspace_copy(true);
                ui.close();
            }
            if ui
                .button(format!(
                    "New clone agent… ({})",
                    display_shortcut(self.settings.new_clone_agent_shortcut.as_deref())
                ))
                .clicked()
            {
                self.start_workspace_copy(false);
                ui.close();
            }
            ui.separator();
            if ui.button("Close window").clicked() {
                self.host(HostAction::CloseWindow, None);
                ui.close();
            }
            if ui.button("Quit").clicked() {
                self.host(HostAction::QuitApplication, None);
                ui.close();
            }
        });
        ui.menu_button("View", |ui| {
            let collapsed = self
                .presentation
                .panels
                .get("sidebarCollapsed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if ui
                .button(format!(
                    "{} ({})",
                    if collapsed {
                        "Show workspaces"
                    } else {
                        "Hide workspaces"
                    },
                    display_shortcut(self.settings.toggle_projects_sidebar_shortcut.as_deref())
                ))
                .clicked()
            {
                self.presentation
                    .panels
                    .insert("sidebarCollapsed".into(), json!(!collapsed));
                self.presentation_dirty = true;
                ui.close();
            }
            if ui
                .button(format!(
                    "Git workbench ({})",
                    display_shortcut(self.settings.toggle_git_sidebar_shortcut.as_deref())
                ))
                .clicked()
            {
                self.toggle_workbench(WorkbenchPanel::Git);
                ui.close();
            }
            if ui.button("Settings").clicked() {
                self.workbench_open = true;
                self.workbench.active = WorkbenchPanel::Settings;
                ui.close();
            }
            if ui
                .button(format!(
                    "Terminal ({})",
                    display_shortcut(self.settings.toggle_terminal_shortcut.as_deref())
                ))
                .clicked()
            {
                self.terminal_open = !self.terminal_open;
                ui.close();
            }
            if ui.button("Focus composer").clicked() {
                Self::focus_composer(ui.ctx(), self.controller.state.selected.as_ref());
                ui.close();
            }
            ui.separator();
            ui.menu_button("Theme", |ui| {
                for (label, value) in [("System", "system"), ("Dark", "dark"), ("Light", "light")] {
                    if ui
                        .selectable_label(self.settings.theme == value, label)
                        .clicked()
                    {
                        self.settings.theme = value.into();
                        super::theme::apply(ui.ctx(), &self.settings);
                        self.persist_settings();
                        ui.close();
                    }
                }
            });
            ui.menu_button("Scale", |ui| {
                for scale in [0.8_f64, 0.9, 1.0, 1.1, 1.2, 1.3] {
                    if ui
                        .selectable_label(
                            (self.settings.ui_scale - scale).abs() < 0.01,
                            format!("{}%", (scale * 100.0).round() as i32),
                        )
                        .clicked()
                    {
                        self.settings.ui_scale = scale;
                        ui.ctx().set_zoom_factor(scale as f32);
                        self.persist_settings();
                        ui.close();
                    }
                }
            });
        });
        ui.menu_button("Help", |ui| {
            if ui.button("About / Updates…").clicked() {
                self.update_open = true;
                ui.close();
            }
        });
    }

    /// Apply one configured shortcut for each key press. Triggering is
    /// independent of which native panel currently owns keyboard focus.
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|input| input.events.clone());
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let settings = &self.settings;
            if matches_shortcut(key, modifiers, configured(&settings.new_agent_shortcut)) {
                self.start_new_agent();
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.new_worktree_agent_shortcut),
            ) {
                self.start_workspace_copy(true);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.new_clone_agent_shortcut),
            ) {
                self.start_workspace_copy(false);
            } else if matches_shortcut(key, modifiers, configured(&settings.interrupt_shortcut)) {
                if self
                    .controller
                    .state
                    .selected_conversation()
                    .is_some_and(|c| c.busy)
                {
                    self.act(Action::Stop);
                }
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.toggle_terminal_shortcut),
            ) {
                self.terminal_open = !self.terminal_open;
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.toggle_git_sidebar_shortcut),
            ) {
                self.toggle_workbench(WorkbenchPanel::Git);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.toggle_projects_sidebar_shortcut),
            ) {
                self.toggle_sidebar();
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.toggle_debug_panel_shortcut),
            ) {
                self.debug_open = !self.debug_open;
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.cycle_agent_next_shortcut),
            ) {
                self.cycle_agent(1);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.cycle_agent_prev_shortcut),
            ) {
                self.cycle_agent(-1);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.cycle_workspace_next_shortcut),
            ) {
                self.cycle_workspace(1);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.cycle_workspace_prev_shortcut),
            ) {
                self.cycle_workspace(-1);
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.archive_thread_shortcut),
            ) {
                self.archive_selected();
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.composer_model_shortcut),
            ) {
                self.cycle_composer_setting("model");
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.composer_access_shortcut),
            ) {
                self.cycle_composer_setting("access");
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.composer_reasoning_shortcut),
            ) {
                self.cycle_composer_setting("effort");
            } else if matches_shortcut(
                key,
                modifiers,
                configured(&settings.composer_collaboration_shortcut),
            ) {
                self.cycle_composer_setting("mode");
            }
        }
    }

    fn start_new_agent(&mut self) {
        if let Some(workspace_id) = self.selected_workspace.clone().or_else(|| {
            self.controller
                .state
                .workspaces
                .first()
                .map(|w| w.id.clone())
        }) {
            self.selected_workspace = Some(workspace_id.clone());
            self.agent_chooser = Some(workspace_id);
        } else {
            self.notices
                .push("Add a workspace before creating an agent.".into());
        }
    }

    fn start_workspace_copy(&mut self, worktree: bool) {
        let Some(workspace_id) = self.selected_workspace.clone() else {
            self.notices.push("Select a workspace first.".into());
            return;
        };
        if worktree {
            self.workspace_dialogs.begin_worktree(workspace_id);
        } else {
            self.workspace_dialogs.begin_clone(workspace_id);
        }
    }

    fn toggle_sidebar(&mut self) {
        let collapsed = self
            .presentation
            .panels
            .get("sidebarCollapsed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.presentation
            .panels
            .insert("sidebarCollapsed".into(), json!(!collapsed));
        self.presentation_dirty = true;
    }

    fn toggle_workbench(&mut self, panel: WorkbenchPanel) {
        if self.workbench_open && self.workbench.active == panel {
            self.workbench_open = false;
        } else {
            self.workbench_open = true;
            self.workbench.active = panel;
        }
    }

    fn focus_composer(ctx: &egui::Context, selected: Option<&ConversationKey>) {
        if let Some(key) = selected {
            ctx.memory_mut(|memory| memory.request_focus(egui::Id::new(("composer", key))));
        }
    }

    fn cycle_workspace(&mut self, direction: isize) {
        let workspaces = &self.controller.state.workspaces;
        if workspaces.is_empty() {
            return;
        }
        let current = self
            .selected_workspace
            .as_deref()
            .and_then(|selected| workspaces.iter().position(|w| w.id == selected))
            .unwrap_or(0);
        let next = (current as isize + direction).rem_euclid(workspaces.len() as isize) as usize;
        let workspace_id = workspaces[next].id.clone();
        self.selected_workspace = Some(workspace_id.clone());
        if let Some(thread_id) = self
            .controller
            .state
            .active_thread_by_workspace
            .get(&workspace_id)
            .cloned()
        {
            self.act(Action::SelectCodex {
                workspace_id,
                thread_id,
            });
        } else {
            self.controller.state.selected = None;
            self.act(Action::LoadWorkspaceThreads { workspace_id });
        }
    }

    fn cycle_agent(&mut self, direction: isize) {
        let Some(workspace_id) = self.selected_workspace.clone() else {
            return;
        };
        let mut keys = Vec::new();
        let endpoint = self.controller.scope().endpoint_id.clone();
        if let Some(rows) = self
            .controller
            .state
            .threads_by_workspace
            .get(&workspace_id)
        {
            for row in rows {
                keys.push(ConversationKey {
                    endpoint_id: endpoint.clone(),
                    conversation: ConversationRef::Codex {
                        workspace_id: workspace_id.clone(),
                        thread_id: row.id.clone(),
                    },
                });
            }
        }
        for key in self.controller.state.conversations.keys() {
            if matches!(&key.conversation, ConversationRef::Acp { workspace_id: ws, .. } if ws == &workspace_id)
                && !keys.contains(key)
            {
                keys.push(key.clone());
            }
        }
        if keys.is_empty() {
            return;
        }
        let current = self
            .controller
            .state
            .selected
            .as_ref()
            .and_then(|key| keys.iter().position(|candidate| candidate == key))
            .unwrap_or(if direction > 0 { keys.len() - 1 } else { 0 });
        let next = (current as isize + direction).rem_euclid(keys.len() as isize) as usize;
        self.open_conversation(keys[next].clone());
    }

    fn archive_selected(&mut self) {
        let Some(key) = self.controller.state.selected.clone() else {
            return;
        };
        match key.conversation {
            ConversationRef::Codex {
                workspace_id,
                thread_id,
            } => {
                let request = crate::client::BackendRequest::Codex(CodexRequest::mutation(
                    "archive_thread",
                    json!({"workspaceId": workspace_id, "threadId": thread_id}),
                    None,
                ));
                if let Err(error) = self.controller.request(request) {
                    self.notices
                        .push(format!("Could not archive conversation: {error:?}"));
                }
                self.controller.state.selected = None;
            }
            ConversationRef::Acp { session_id, .. } => {
                let revision = self
                    .controller
                    .state
                    .selected_conversation()
                    .and_then(|c| c.session.as_ref())
                    .and_then(|session| session["metadataRevision"].as_i64())
                    .unwrap_or(0);
                self.act(Action::UpdateMetadata {
                    session_id,
                    expected_revision: revision,
                    patch: json!({"hidden": true}),
                });
                self.controller.state.selected = None;
            }
        }
    }

    fn cycle_composer_setting(&mut self, setting: &str) {
        let Some(key) = self.controller.state.selected.clone() else {
            return;
        };
        let conversation = self.controller.state.selected_conversation().cloned();
        match key.conversation {
            ConversationRef::Codex { workspace_id, .. } => {
                let Some(controls) = self
                    .controller
                    .state
                    .codex_controls_by_workspace
                    .get(&workspace_id)
                    .cloned()
                else {
                    return;
                };
                let (name, values, selected) = match setting {
                    "model" => {
                        let values: Vec<_> = controls
                            .models
                            .iter()
                            .filter_map(|v| {
                                v["model"]
                                    .as_str()
                                    .or_else(|| v["id"].as_str())
                                    .map(str::to_owned)
                            })
                            .collect();
                        (
                            "model",
                            values,
                            controls
                                .selected_model
                                .clone()
                                .or(controls.configured_model.clone()),
                        )
                    }
                    "effort" => {
                        let model = controls.models.iter().find(|v| {
                            v["model"].as_str().or_else(|| v["id"].as_str())
                                == controls
                                    .selected_model
                                    .as_deref()
                                    .or(controls.configured_model.as_deref())
                        });
                        let values = model
                            .and_then(|m| m["supportedReasoningEfforts"].as_array())
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(|v| {
                                        v["reasoningEffort"]
                                            .as_str()
                                            .or_else(|| v.as_str())
                                            .map(str::to_owned)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        ("effort", values, controls.selected_effort.clone())
                    }
                    "access" => (
                        "accessMode",
                        vec!["read-only".into(), "current".into(), "full-access".into()],
                        Some(controls.selected_access_mode.clone()),
                    ),
                    "mode" => {
                        let values = controls
                            .collaboration_modes
                            .iter()
                            .filter_map(|v| {
                                v["name"]
                                    .as_str()
                                    .or_else(|| v["mode"].as_str())
                                    .map(str::to_owned)
                            })
                            .collect();
                        let selected = controls
                            .selected_collaboration_mode
                            .as_ref()
                            .and_then(|v| v["name"].as_str().or_else(|| v["mode"].as_str()))
                            .map(str::to_owned);
                        ("collaborationMode", values, selected)
                    }
                    _ => return,
                };
                if values.is_empty() {
                    return;
                }
                let next = next_value(&values, selected.as_deref());
                let value = if name == "collaborationMode" {
                    controls
                        .collaboration_modes
                        .iter()
                        .find(|v| {
                            v["name"].as_str().or_else(|| v["mode"].as_str()) == Some(next.as_str())
                        })
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    json!(next)
                };
                self.act(Action::SetCodexOption {
                    workspace_id,
                    name: name.into(),
                    value,
                });
            }
            ConversationRef::Acp { .. } => {
                let Some(session) = conversation.and_then(|c| c.session) else {
                    return;
                };
                let config = &session["configuration"];
                let selector = match setting {
                    "model" => "model",
                    "effort" => "effort",
                    "access" => "access",
                    "mode" => "mode",
                    _ => return,
                };
                let options = config["configOptions"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let option = options.iter().find(|option| {
                    let id = option["id"].as_str().unwrap_or("").to_ascii_lowercase();
                    id.contains(selector)
                });
                let (id, values, current) = if let Some(option) = option {
                    (
                        option["id"].as_str().unwrap_or("").to_owned(),
                        option["options"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v["value"].as_str().map(str::to_owned))
                            .collect::<Vec<String>>(),
                        option["currentValue"].as_str().map(str::to_owned),
                    )
                } else if selector == "model" {
                    (
                        "model".to_owned(),
                        config["models"]["availableModels"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v["modelId"].as_str().map(str::to_owned))
                            .collect(),
                        config["models"]["currentModelId"]
                            .as_str()
                            .map(str::to_owned),
                    )
                } else if selector == "mode" {
                    (
                        "mode".to_owned(),
                        config["modes"]["availableModes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|v| v["id"].as_str().map(str::to_owned))
                            .collect(),
                        config["modes"]["currentModeId"].as_str().map(str::to_owned),
                    )
                } else {
                    return;
                };
                if values.is_empty() {
                    return;
                }
                self.act(Action::Configure {
                    option_id: id,
                    value: next_value(&values, current.as_deref()),
                });
            }
        }
    }
}

fn next_value(values: &[String], current: Option<&str>) -> String {
    let next = current
        .and_then(|current| values.iter().position(|value| value == current))
        .map_or(0, |index| (index + 1) % values.len());
    values[next].clone()
}

fn display_shortcut(value: Option<&str>) -> String {
    let Some(shortcut) = parse_shortcut(value) else {
        return "no shortcut".into();
    };
    let mut parts = Vec::new();
    if shortcut.cmd {
        parts.push(if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl"
        });
    }
    if shortcut.ctrl {
        parts.push(if cfg!(target_os = "macos") {
            "⌃"
        } else {
            "Ctrl"
        });
    }
    if shortcut.alt {
        parts.push(if cfg!(target_os = "macos") {
            "⌥"
        } else {
            "Alt"
        });
    }
    if shortcut.shift {
        parts.push(if cfg!(target_os = "macos") {
            "⇧"
        } else {
            "Shift"
        });
    }
    parts.push(match shortcut.key.as_str() {
        "arrowup" => "↑",
        "arrowdown" => "↓",
        "arrowleft" => "←",
        "arrowright" => "→",
        "escape" => "Esc",
        "space" => "Space",
        key => key,
    });
    parts.join(if cfg!(target_os = "macos") { "" } else { "+" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_parser_keeps_user_modifier_configuration() {
        assert_eq!(
            parse_shortcut(Some("ctrl+alt+shift+arrowdown")),
            Some(ParsedShortcut {
                key: "arrowdown".into(),
                cmd: false,
                ctrl: true,
                alt: true,
                shift: true,
            })
        );
        assert_eq!(parse_shortcut(None), None);
    }

    #[test]
    fn shortcut_match_requires_exact_modifiers() {
        assert!(matches_shortcut(
            Key::N,
            Modifiers {
                ctrl: true,
                command: true,
                ..Modifiers::default()
            },
            Some("ctrl+n")
        ));
        assert!(!matches_shortcut(
            Key::N,
            Modifiers {
                ctrl: true,
                command: true,
                shift: true,
                ..Modifiers::default()
            },
            Some("ctrl+n")
        ));
    }

    #[test]
    fn cycling_wraps_and_uses_first_value_when_unset() {
        let values = vec!["one".into(), "two".into()];
        assert_eq!(next_value(&values, Some("two")), "one");
        assert_eq!(next_value(&values, None), "one");
    }
}
