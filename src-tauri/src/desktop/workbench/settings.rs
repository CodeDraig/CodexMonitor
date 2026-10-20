use super::{
    mutate_codex, mutate_settings, read_codex, read_settings, PanelEffect, WorkbenchRequestKey,
};
use crate::client::contracts::{BackendRequest, CodexRequest};
use egui::{self, Ui};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub(crate) struct SettingsPanel {
    value: Option<Value>,
    draft: String,
    stale: bool,
    dirty: bool,
    draft_scope_stale: bool,
    error: Option<String>,
    status: Option<String>,
    codex: HashMap<String, Value>,
    requested: std::collections::HashSet<String>,
    confirm_remove_remote: Option<String>,
    features: Vec<Value>,
    feature_pages: usize,
    feature_updating: Option<(String, bool)>,
    feature_error: Option<String>,
    queued_effects: Vec<PanelEffect>,
    feature_workspace: Option<String>,
}
impl SettingsPanel {
    pub(crate) fn change_workspace(&mut self) {
        self.codex.clear();
        self.requested.clear();
        self.features.clear();
        self.feature_pages = 0;
        self.feature_updating = None;
        self.feature_workspace = None;
    }
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
        self.draft_scope_stale |= self.dirty;
        self.codex.clear();
        self.requested.clear();
        self.features.clear();
        self.feature_pages = 0;
        self.feature_updating = None;
        self.feature_workspace = None;
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace_id: Option<&str>,
    ) {
        self.feature_workspace = workspace_id.map(str::to_owned);
        ui.horizontal(|ui| {
            ui.heading("Application settings");
            if ui.button("Reload").clicked() {
                self.stale = true;
            }
        });
        if self.stale {
            effects.push(PanelEffect::Request {
                key: WorkbenchRequestKey::SettingsLoad,
                request: read_settings("get_app_settings", Value::Null),
            });
            self.stale = false;
        }
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
        }
        if let Some(s) = &self.status {
            ui.label(s);
        }
        if self.draft_scope_stale {
            ui.colored_label(egui::Color32::YELLOW, "This unsaved settings draft belongs to the previous backend. It cannot be saved to the current backend until you discard it.");
            if ui
                .button("Discard old-backend draft and use loaded settings")
                .clicked()
            {
                if let Some(value) = &self.value {
                    self.draft = serde_json::to_string_pretty(value).unwrap_or_default();
                }
                self.dirty = false;
                self.draft_scope_stale = false;
            }
        }
        ui.separator();
        ui.heading("Codex account and workspace catalogs");
        let workspace = workspace_id.unwrap_or_default();
        for (key, label, method, params) in [
            (
                "account",
                "Account",
                "account_read",
                serde_json::json!({"workspaceId":workspace}),
            ),
            (
                "limits",
                "Rate limits",
                "account_rate_limits",
                serde_json::json!({"workspaceId":workspace}),
            ),
            (
                "mcp",
                "MCP servers",
                "list_mcp_server_status",
                serde_json::json!({"workspaceId":workspace,"cursor":null,"limit":100}),
            ),
            (
                "apps",
                "Apps",
                "apps_list",
                serde_json::json!({"workspaceId":workspace,"cursor":null,"limit":100}),
            ),
            (
                "skills",
                "Skills",
                "skills_list",
                serde_json::json!({"workspaceId":workspace}),
            ),
            (
                "features",
                "Experimental features",
                "experimental_feature_list",
                serde_json::json!({"workspaceId":workspace,"cursor":null,"limit":100}),
            ),
            (
                "config",
                "Configured model",
                "get_config_model",
                serde_json::json!({"workspaceId":workspace}),
            ),
        ] {
            let load = self.requested.contains(key);
            ui.horizontal(|ui| {
                ui.label(label);
                if ui
                    .add_enabled(
                        workspace_id.is_some() && !load,
                        egui::Button::new(if self.codex.contains_key(key) {
                            "Refresh"
                        } else {
                            "Load"
                        }),
                    )
                    .clicked()
                {
                    effects.push(PanelEffect::Request {
                        key: match key {
                            "account" => WorkbenchRequestKey::SettingsAccount,
                            "limits" => WorkbenchRequestKey::SettingsRateLimits,
                            "mcp" => WorkbenchRequestKey::SettingsMcp,
                            "apps" => WorkbenchRequestKey::SettingsApps,
                            "skills" => WorkbenchRequestKey::SettingsSkills,
                            "features" => WorkbenchRequestKey::SettingsFeatures,
                            _ => WorkbenchRequestKey::SettingsConfig,
                        },
                        request: read_codex(method, params.clone()),
                    });
                    if key == "features" {
                        self.features.clear();
                        self.feature_pages = 0;
                        self.feature_error = None;
                        self.feature_workspace = workspace_id.map(str::to_owned);
                    }
                    self.requested.insert(key.into());
                }
                if !workspace_id.is_some() {
                    ui.weak("Select a workspace first.");
                }
            });
            if let Some(value) = self.codex.get(key) {
                if key != "features" {
                    ui.collapsing(format!("{label} data"), |ui| {
                        ui.monospace(serde_json::to_string_pretty(value).unwrap_or_default());
                    });
                }
            }
        }
        self.show_features(ui, effects, workspace_id);
        ui.horizontal(|ui| {
            ui.label("Codex sign-in and diagnostics");
            if ui
                .add_enabled(
                    workspace_id.is_some() && !self.requested.contains("login"),
                    egui::Button::new("Sign in to Codex"),
                )
                .clicked()
            {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::SettingsLogin,
                    request: mutate_codex(
                        "codex_login",
                        serde_json::json!({"workspaceId":workspace}),
                    ),
                });
                self.requested.insert("login".into());
            }
            if ui
                .add_enabled(
                    self.requested.contains("login"),
                    egui::Button::new("Cancel sign-in"),
                )
                .clicked()
            {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::SettingsLoginCancel,
                    request: mutate_codex(
                        "codex_login_cancel",
                        serde_json::json!({"workspaceId":workspace}),
                    ),
                });
                self.requested.insert("login_cancel".into());
            }
            if ui
                .add_enabled(
                    !self.requested.contains("doctor"),
                    egui::Button::new("Run diagnostics"),
                )
                .clicked()
            {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::SettingsDoctor,
                    request: read_codex(
                        "codex_doctor",
                        serde_json::json!({"workspaceId":workspace}),
                    ),
                });
                self.requested.insert("doctor".into());
            }
        });
        for key in ["login", "login_cancel", "doctor"] {
            if let Some(value) = self.codex.get(key) {
                ui.collapsing(format!("{key} result"), |ui| {
                    if key == "login" {
                        if let Some(auth_url) = value["authUrl"].as_str() {
                            ui.hyperlink_to("Open Codex sign-in", auth_url);
                        }
                    }
                    ui.monospace(serde_json::to_string_pretty(value).unwrap_or_default());
                });
            }
        }
        ui.weak("Runtime profiles are managed by the agent-profile controls in the sidebar. Account, rate limits, MCP servers, apps, and skills are read-only catalogs here. Feature flags can be changed below. The available Codex auth operations support sign-in and cancelling sign-in; there is no logout operation in the current backend contract.");
        ui.separator();
        if self.draft_scope_stale {
            ui.weak("Common controls are disabled while the old-backend draft is protected.");
        } else if let Ok(mut settings) = serde_json::from_str::<Value>(&self.draft) {
            if settings.is_object() {
                ui.collapsing("Common settings", |ui| {
                    let changed = self.show_common_controls(ui, &mut settings);
                    if changed {
                        self.draft = serde_json::to_string_pretty(&settings).unwrap_or_default();
                        self.dirty = true;
                    }
                });
            } else {
                ui.weak("Common controls need a settings object; repair the advanced JSON below.");
            }
        } else if self.value.is_none() {
            ui.weak("Load application settings to edit common controls.");
        } else {
            ui.weak("The current advanced JSON draft is invalid; repair it below before using common controls.");
        }
        if !self.draft_scope_stale {
            if let Ok(mut settings) = serde_json::from_str::<Value>(&self.draft) {
                if settings.is_object() {
                    ui.collapsing("Open-in targets", |ui| {
                        let changed = self.show_open_app_targets(ui, &mut settings);
                        if changed {
                            self.draft =
                                serde_json::to_string_pretty(&settings).unwrap_or_default();
                            self.dirty = true;
                        }
                    });
                }
            }
        }
        ui.label("Settings are edited as their serialized application configuration. Invalid structures are rejected by the backend.");
        if ui
            .add(
                egui::TextEdit::multiline(&mut self.draft)
                    .code_editor()
                    .desired_rows(26),
            )
            .changed()
        {
            self.dirty = true;
        }
        if ui
            .add_enabled(
                self.dirty && !self.draft_scope_stale,
                egui::Button::new("Save settings"),
            )
            .clicked()
        {
            match serde_json::from_str::<Value>(&self.draft) {
                Ok(value) => effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::SettingsSave,
                    request: mutate_settings(
                        "update_app_settings",
                        serde_json::json!({"settings":value}),
                    ),
                }),
                Err(e) => self.error = Some(format!("Settings JSON is invalid: {e}")),
            }
        }
    }

    fn show_common_controls(&mut self, ui: &mut Ui, settings: &mut Value) -> bool {
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label("Theme");
            let mut theme = settings["theme"].as_str().unwrap_or("system").to_owned();
            egui::ComboBox::from_id_salt("native-settings-theme")
                .selected_text(&theme)
                .show_ui(ui, |ui| {
                    for option in ["system", "light", "dark", "dim"] {
                        ui.selectable_value(&mut theme, option.to_owned(), option);
                    }
                });
            changed |= set_string(settings, "theme", theme);
        });

        ui.horizontal(|ui| {
            ui.label("Backend mode");
            let mut mode = settings["backendMode"]
                .as_str()
                .unwrap_or("local")
                .to_owned();
            egui::ComboBox::from_id_salt("native-settings-backend-mode")
                .selected_text(&mode)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, "local".into(), "Local");
                    ui.selectable_value(&mut mode, "remote".into(), "Remote daemon");
                });
            changed |= set_string(settings, "backendMode", mode);
        });

        changed |= self.show_remote_controls(ui, settings);

        changed |= checkbox_setting(
            ui,
            settings,
            "keepDaemonRunningAfterAppClose",
            "Keep daemon running after app closes",
        );
        changed |= checkbox_setting(
            ui,
            settings,
            "notificationSoundsEnabled",
            "Notification sounds",
        );
        changed |= checkbox_setting(
            ui,
            settings,
            "systemNotificationsEnabled",
            "System notifications",
        );
        changed |= checkbox_setting(
            ui,
            settings,
            "subagentSystemNotificationsEnabled",
            "Include sub-agent notifications",
        );
        changed |= checkbox_setting(
            ui,
            settings,
            "automaticAppUpdateChecksEnabled",
            "Check for updates automatically",
        );

        ui.horizontal(|ui| {
            ui.label("Follow-up behavior");
            let mut behavior = settings["followUpMessageBehavior"]
                .as_str()
                .unwrap_or("queue")
                .to_owned();
            egui::ComboBox::from_id_salt("native-settings-follow-up")
                .selected_text(&behavior)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut behavior, "queue".into(), "Queue");
                    ui.selectable_value(&mut behavior, "steer".into(), "Steer");
                });
            changed |= set_string(settings, "followUpMessageBehavior", behavior);
        });

        changed |= checkbox_setting(
            ui,
            settings,
            "composerFollowUpHintEnabled",
            "Show follow-up hint while processing",
        );
        changed |= checkbox_setting(
            ui,
            settings,
            "dictationEnabled",
            "Enable microphone dictation",
        );
        ui.horizontal(|ui| {
            ui.label("Dictation model");
            let mut model = settings["dictationModelId"]
                .as_str()
                .unwrap_or("base")
                .to_owned();
            egui::ComboBox::from_id_salt("native-settings-dictation-model")
                .selected_text(&model)
                .show_ui(ui, |ui| {
                    for (id, label) in [
                        ("tiny", "Tiny · 75 MB · fastest"),
                        ("base", "Base · 142 MB · balanced"),
                        ("small", "Small · 466 MB · better accuracy"),
                        ("medium", "Medium · 1.5 GB · high accuracy"),
                        ("large-v3", "Large V3 · 3.0 GB · best accuracy"),
                    ] {
                        ui.selectable_value(&mut model, id.to_owned(), label);
                    }
                });
            changed |= set_string(settings, "dictationModelId", model);
        });
        changed |= edit_optional_string_setting(
            ui,
            settings,
            "dictationPreferredLanguage",
            "Preferred dictation language",
        );
        ui.horizontal(|ui| {
            ui.label("Dictation hold key");
            let mut key = settings["dictationHoldKey"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            egui::ComboBox::from_id_salt("native-settings-dictation-key")
                .selected_text(match key.as_str() {
                    "alt" => "Alt / Option",
                    "shift" => "Shift",
                    "control" => "Control",
                    "meta" => "Meta / Command",
                    _ => "Off",
                })
                .show_ui(ui, |ui| {
                    for (id, label) in [
                        ("", "Off"),
                        ("alt", "Alt / Option"),
                        ("shift", "Shift"),
                        ("control", "Control"),
                        ("meta", "Meta / Command"),
                    ] {
                        ui.selectable_value(&mut key, id.to_owned(), label);
                    }
                });
            changed |= set_string(settings, "dictationHoldKey", key);
        });
        changed |= edit_optional_string_setting(
            ui,
            settings,
            "toggleTerminalShortcut",
            "Terminal toggle shortcut",
        );
        changed |= edit_string_setting(ui, settings, "uiFontFamily", "UI font family");
        changed |= edit_string_setting(ui, settings, "codeFontFamily", "Code font family");

        ui.horizontal(|ui| {
            ui.label("Code font size");
            let mut size = settings["codeFontSize"].as_u64().unwrap_or(14).clamp(8, 32) as u32;
            if ui
                .add(egui::Slider::new(&mut size, 8..=32).suffix(" px"))
                .changed()
            {
                settings["codeFontSize"] = Value::from(size);
                changed = true;
            }
        });

        changed |= checkbox_setting(ui, settings, "unifiedExecEnabled", "Enable terminal tools");
        ui.weak("Controls save together with the advanced configuration below. Backend mode and active remote are applied after the saved settings reply.");
        changed
    }

    fn show_features(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace_id: Option<&str>,
    ) {
        ui.separator();
        ui.heading("Codex features");
        ui.label(
            "Stable flags and beta or under-development flags from the selected Codex workspace.",
        );
        if let Some(error) = &self.feature_error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        let loading = self.requested.contains("features");
        if workspace_id.is_none() {
            ui.weak("Connect a workspace to load Codex feature flags.");
            return;
        }
        if loading {
            ui.spinner();
            ui.label(format!(
                "Loading Codex feature flags (page {} of 20)…",
                self.feature_pages + 1
            ));
        }
        let stable: Vec<Value> = self
            .features
            .iter()
            .filter(|feature| {
                let stage = feature_stage(feature);
                stage == Some("stable") && !hidden_feature(feature)
            })
            .cloned()
            .collect();
        let experimental: Vec<Value> = self
            .features
            .iter()
            .filter(|feature| {
                matches!(feature_stage(feature), Some("beta" | "under_development"))
                    && !hidden_feature(feature)
            })
            .cloned()
            .collect();
        let has_rows = !stable.is_empty() || !experimental.is_empty();
        ui.collapsing("Stable features", |ui| {
            if stable.is_empty() && !loading && self.feature_error.is_none() {
                ui.weak("No stable feature flags returned by Codex.");
            }
            for feature in &stable {
                self.show_feature_row(ui, effects, feature);
            }
        });
        ui.collapsing("Experimental features", |ui| {
            if experimental.is_empty() && !loading && self.feature_error.is_none() {
                ui.weak("No beta or under-development flags returned by Codex.");
            }
            for feature in &experimental {
                self.show_feature_row(ui, effects, feature);
            }
        });
        if !has_rows && self.feature_error.is_none() && !loading && self.features.is_empty() {
            ui.weak("Load feature flags using the Load control above.");
        }
    }

    fn show_feature_row(&mut self, ui: &mut Ui, effects: &mut Vec<PanelEffect>, feature: &Value) {
        let name = feature["name"].as_str().unwrap_or_default();
        if name.is_empty() {
            return;
        }
        let enabled = feature["enabled"].as_bool().unwrap_or(false);
        let updating = self
            .feature_updating
            .as_ref()
            .is_some_and(|(updating_name, _)| updating_name == name);
        let settings_feature = matches!(name, "apps" | "unified_exec");
        let can_change = !self.requested.contains("features")
            && self.feature_updating.is_none()
            && !(settings_feature && self.draft_scope_stale);
        let label = feature["displayName"]
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| human_feature_name(name));
        let description = feature_description(feature);
        ui.horizontal(|ui| {
            let mut next = enabled;
            if ui
                .add_enabled(can_change, egui::Checkbox::new(&mut next, &label))
                .changed()
            {
                self.feature_updating = Some((name.to_owned(), next));
                self.requested.insert("feature_update".into());
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::SettingsFeatures,
                    request: BackendRequest::Codex(CodexRequest::set_feature_flag(name, next)),
                });
            }
            if updating {
                ui.spinner();
            }
            ui.weak(description);
        });
        if settings_feature && self.draft_scope_stale {
            ui.weak("Discard the protected previous-backend draft before changing this paired app setting.");
        }
    }

    pub(crate) fn take_effects(&mut self) -> Vec<PanelEffect> {
        std::mem::take(&mut self.queued_effects)
    }

    fn show_open_app_targets(&mut self, ui: &mut Ui, settings: &mut Value) -> bool {
        let mut changed = false;
        if !settings.get("openAppTargets").is_some_and(Value::is_array) {
            settings["openAppTargets"] = Value::Array(Vec::new());
            changed = true;
        }
        ui.label("Customize the Open in menu. Command arguments are whitespace-separated; the selected path is appended by the launcher.");
        let mut next_default: Option<String> = None;
        let mut index = 0;
        while index < settings["openAppTargets"].as_array().unwrap().len() {
            let len = settings["openAppTargets"].as_array().unwrap().len();
            let mut action = 0i8;
            let selected_id = settings["selectedOpenAppId"].as_str().map(str::to_owned);
            let target = &mut settings["openAppTargets"].as_array_mut().unwrap()[index];
            let id = target["id"].as_str().unwrap_or_default().to_owned();
            let mut label = target["label"].as_str().unwrap_or_default().to_owned();
            let mut kind = target["kind"].as_str().unwrap_or("app").to_owned();
            let mut args = target["args"]
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let mut app_name = target["appName"].as_str().unwrap_or_default().to_owned();
            let mut command = target["command"].as_str().unwrap_or_default().to_owned();
            let mut row_changed = false;
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(format!("Target {}", index + 1));
                    ui.label("Label");
                    if ui.text_edit_singleline(&mut label).changed() {
                        row_changed = true;
                    }
                    ui.label("Type");
                    egui::ComboBox::from_id_salt(("open-app-kind", &id))
                        .selected_text(&kind)
                        .show_ui(ui, |ui| {
                            for value in ["app", "command", "finder"] {
                                ui.selectable_value(&mut kind, value.into(), value);
                            }
                        });
                    if kind != target["kind"].as_str().unwrap_or("app") {
                        target["kind"] = Value::String(kind.clone());
                        target["appName"] = if kind == "app" {
                            Value::String(app_name.clone())
                        } else {
                            Value::Null
                        };
                        target["command"] = if kind == "command" {
                            Value::String(command.clone())
                        } else {
                            Value::Null
                        };
                        if kind == "finder" {
                            args.clear();
                        }
                        row_changed = true;
                    }
                    let complete = !label.trim().is_empty()
                        && match kind.as_str() {
                            "app" => !app_name.trim().is_empty(),
                            "command" => !command.trim().is_empty(),
                            _ => kind == "finder",
                        };
                    let is_default = selected_id.as_deref() == Some(id.as_str());
                    if ui
                        .add_enabled(complete, egui::RadioButton::new(is_default, "Default"))
                        .clicked()
                    {
                        next_default = Some(id.clone());
                    }
                    if !complete {
                        ui.colored_label(egui::Color32::YELLOW, "Incomplete");
                    }
                    if ui.add_enabled(index > 0, egui::Button::new("↑")).clicked() {
                        action = -1;
                    }
                    if ui
                        .add_enabled(index + 1 < len, egui::Button::new("↓"))
                        .clicked()
                    {
                        action = 1;
                    }
                    if ui
                        .add_enabled(len > 1, egui::Button::new("Remove"))
                        .clicked()
                    {
                        action = 2;
                    }
                });
                if kind == "app" {
                    ui.horizontal(|ui| {
                        ui.label("App name");
                        if ui.text_edit_singleline(&mut app_name).changed() {
                            row_changed = true;
                        }
                    });
                } else if kind == "command" {
                    ui.horizontal(|ui| {
                        ui.label("Command");
                        if ui.text_edit_singleline(&mut command).changed() {
                            row_changed = true;
                        }
                    });
                }
                if kind != "finder" {
                    ui.horizontal(|ui| {
                        ui.label("Arguments");
                        if ui.text_edit_singleline(&mut args).changed() {
                            row_changed = true;
                        }
                    });
                }
            });
            if label != target["label"].as_str().unwrap_or_default() {
                target["label"] = Value::String(label.trim().to_owned());
                row_changed = true;
            }
            if kind == "app" && app_name != target["appName"].as_str().unwrap_or_default() {
                target["appName"] = optional_string(&app_name);
                row_changed = true;
            }
            if kind == "command" && command != target["command"].as_str().unwrap_or_default() {
                target["command"] = optional_string(&command);
                row_changed = true;
            }
            if kind != "finder" {
                let next_args = split_open_app_args(&args);
                if target["args"].as_array() != Some(&next_args) {
                    target["args"] = Value::Array(next_args);
                    row_changed = true;
                }
            } else if target["args"]
                .as_array()
                .is_some_and(|args| !args.is_empty())
            {
                target["args"] = Value::Array(Vec::new());
                row_changed = true;
            }
            changed |= row_changed;
            match action {
                -1 => {
                    settings["openAppTargets"]
                        .as_array_mut()
                        .unwrap()
                        .swap(index, index - 1);
                    changed = true;
                    index = index.saturating_sub(1);
                }
                1 => {
                    settings["openAppTargets"]
                        .as_array_mut()
                        .unwrap()
                        .swap(index, index + 1);
                    changed = true;
                    index += 1;
                }
                2 => {
                    settings["openAppTargets"]
                        .as_array_mut()
                        .unwrap()
                        .remove(index);
                    changed = true;
                }
                _ => index += 1,
            }
        }
        if let Some(id) = next_default {
            if settings["selectedOpenAppId"].as_str() != Some(&id) {
                settings["selectedOpenAppId"] = Value::String(id);
                changed = true;
            }
        }
        if ui.button("Add Open-in target").clicked() {
            let id = uuid::Uuid::new_v4().to_string();
            settings["openAppTargets"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "id": id,
                    "label": "New App",
                    "kind": "app",
                    "appName": "",
                    "command": null,
                    "args": []
                }));
            settings["selectedOpenAppId"] = Value::String(id);
            changed = true;
        }
        let selected_id = settings["selectedOpenAppId"].as_str().unwrap_or_default();
        let selected_valid = settings["openAppTargets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|target| target["id"].as_str() == Some(selected_id) && open_app_complete(target));
        if !selected_valid {
            if let Some(id) = settings["openAppTargets"]
                .as_array()
                .unwrap()
                .iter()
                .find(|target| open_app_complete(target))
                .and_then(|target| target["id"].as_str())
            {
                settings["selectedOpenAppId"] = Value::String(id.to_owned());
                changed = true;
            }
        }
        changed
    }

    fn show_remote_controls(&mut self, ui: &mut Ui, settings: &mut Value) -> bool {
        let mut changed = false;
        if !settings.get("remoteBackends").is_some_and(Value::is_array) {
            settings["remoteBackends"] = Value::Array(Vec::new());
            changed = true;
        }
        ui.horizontal(|ui| {
            ui.label("Saved remote");
            let remotes = settings["remoteBackends"].as_array().unwrap();
            if remotes.is_empty() {
                ui.weak("No remote backends configured.");
            } else {
                let active = settings["activeRemoteBackendId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let current_name = remotes
                    .iter()
                    .find(|remote| remote["id"].as_str() == Some(&active))
                    .and_then(|remote| remote["name"].as_str())
                    .unwrap_or("Select a remote");
                let mut selected = active;
                egui::ComboBox::from_id_salt("native-settings-remote-target")
                    .selected_text(current_name)
                    .show_ui(ui, |ui| {
                        for remote in remotes.iter() {
                            let id = remote["id"].as_str().unwrap_or_default().to_owned();
                            let name = remote["name"].as_str().unwrap_or(&id).to_owned();
                            ui.selectable_value(&mut selected, id, name);
                        }
                    });
                if selected
                    != settings["activeRemoteBackendId"]
                        .as_str()
                        .unwrap_or_default()
                {
                    settings["activeRemoteBackendId"] = Value::String(selected);
                    changed = true;
                }
            }
            if ui.button("Add remote").clicked() {
                let id = uuid::Uuid::new_v4().to_string();
                settings["remoteBackends"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!({
                        "id": id,
                        "name": "New remote",
                        "provider": "tcp",
                        "host": "127.0.0.1:4732",
                        "token": null,
                        "lastConnectedAtMs": null
                    }));
                settings["activeRemoteBackendId"] = Value::String(id);
                changed = true;
            }
        });

        let active = settings["activeRemoteBackendId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let Some(index) = settings["remoteBackends"]
            .as_array()
            .unwrap()
            .iter()
            .position(|remote| remote["id"].as_str() == Some(&active))
        else {
            return changed;
        };
        let remote = &mut settings["remoteBackends"].as_array_mut().unwrap()[index];
        changed |= edit_string_setting(ui, remote, "name", "Remote name");
        changed |= edit_string_setting(ui, remote, "host", "Remote host");
        ui.horizontal(|ui| {
            ui.label("Remote token");
            let mut token = remote["token"].as_str().unwrap_or_default().to_owned();
            if ui
                .add(egui::TextEdit::singleline(&mut token).password(true))
                .changed()
            {
                remote["token"] = if token.is_empty() {
                    Value::Null
                } else {
                    Value::String(token)
                };
                changed = true;
            }
        });
        if let Some(confirm_id) = &self.confirm_remove_remote {
            if confirm_id == &active {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::YELLOW, "Remove this saved remote?");
                    if ui.button("Confirm remove").clicked() {
                        let remotes = settings["remoteBackends"].as_array_mut().unwrap();
                        remotes.remove(index);
                        settings["activeRemoteBackendId"] = remotes
                            .first()
                            .and_then(|value| value["id"].as_str())
                            .map_or(Value::Null, |id| Value::String(id.to_owned()));
                        self.confirm_remove_remote = None;
                        changed = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_remove_remote = None;
                    }
                });
            }
        } else if ui.button("Remove saved remote…").clicked() {
            self.confirm_remove_remote = Some(active);
        }
        changed
    }
    pub(crate) fn apply(&mut self, key: &WorkbenchRequestKey, value: Value) {
        match key {
            WorkbenchRequestKey::SettingsLoad => {
                self.value = Some(value.clone());
                if !self.draft_scope_stale {
                    self.draft = serde_json::to_string_pretty(&value).unwrap_or_default();
                    self.dirty = false;
                }
                self.error = None;
            }
            WorkbenchRequestKey::SettingsSave => {
                self.value = Some(value.clone());
                self.draft = serde_json::to_string_pretty(&value).unwrap_or_default();
                self.dirty = false;
                self.status = Some("Settings saved".into());
                self.error = None;
            }
            WorkbenchRequestKey::SettingsAccount => {
                self.requested.remove("account");
                self.codex.insert("account".into(), value);
            }
            WorkbenchRequestKey::SettingsRateLimits => {
                self.requested.remove("limits");
                self.codex.insert("limits".into(), value);
            }
            WorkbenchRequestKey::SettingsMcp => {
                self.requested.remove("mcp");
                self.codex.insert("mcp".into(), value);
            }
            WorkbenchRequestKey::SettingsApps => {
                self.requested.remove("apps");
                self.codex.insert("apps".into(), value);
            }
            WorkbenchRequestKey::SettingsSkills => {
                self.requested.remove("skills");
                self.codex.insert("skills".into(), value);
            }
            WorkbenchRequestKey::SettingsFeatures => {
                if let Some((feature_name, enabled)) = self.feature_updating.take() {
                    self.requested.remove("feature_update");
                    if value.get("ok").and_then(Value::as_bool) != Some(true) {
                        self.feature_error =
                            Some("Codex did not confirm the feature update.".into());
                    } else {
                        if let Some(feature) = self
                            .features
                            .iter_mut()
                            .find(|item| item["name"].as_str() == Some(&feature_name))
                        {
                            feature["enabled"] = Value::Bool(enabled);
                        }
                        self.feature_error = None;
                        self.status = Some(format!(
                            "Codex feature {} {}",
                            feature_name,
                            if enabled { "enabled" } else { "disabled" }
                        ));
                        if let Some(setting_key) = feature_app_setting_key(&feature_name) {
                            if !self.draft_scope_stale {
                                if let Ok(mut settings) = serde_json::from_str::<Value>(&self.draft)
                                {
                                    if settings.is_object() {
                                        settings[setting_key] = Value::Bool(enabled);
                                        self.draft = serde_json::to_string_pretty(&settings)
                                            .unwrap_or_default();
                                        self.dirty = true;
                                        self.queued_effects.push(PanelEffect::Request {
                                            key: WorkbenchRequestKey::SettingsSave,
                                            request: mutate_settings(
                                                "update_app_settings",
                                                serde_json::json!({"settings": settings}),
                                            ),
                                        });
                                    }
                                }
                            }
                        }
                    }
                } else {
                    let (items, next_cursor) = parse_feature_page(&value);
                    for feature in items {
                        if !self
                            .features
                            .iter()
                            .any(|item| item["name"] == feature["name"])
                        {
                            self.features.push(feature);
                        }
                    }
                    self.features.sort_by(|left, right| {
                        left["name"]
                            .as_str()
                            .unwrap_or_default()
                            .cmp(right["name"].as_str().unwrap_or_default())
                    });
                    self.feature_pages = self.feature_pages.saturating_add(1);
                    self.codex
                        .insert("features".into(), Value::Array(self.features.clone()));
                    if let (Some(cursor), Some(workspace_id)) =
                        (next_cursor, self.feature_workspace.clone())
                    {
                        if self.feature_pages < 20 {
                            self.queued_effects.push(PanelEffect::Request {
                                key: WorkbenchRequestKey::SettingsFeatures,
                                request: read_codex("experimental_feature_list", serde_json::json!({"workspaceId":workspace_id,"cursor":cursor,"limit":100})),
                            });
                        } else {
                            self.requested.remove("features");
                            self.feature_error =
                                Some("Codex feature list is truncated after 20 pages.".into());
                        }
                    } else {
                        self.requested.remove("features");
                    }
                }
            }
            WorkbenchRequestKey::SettingsDoctor => {
                self.requested.remove("doctor");
                self.codex.insert("doctor".into(), value);
            }
            WorkbenchRequestKey::SettingsLogin => {
                self.requested.remove("login");
                self.codex.insert("login".into(), value);
            }
            WorkbenchRequestKey::SettingsLoginCancel => {
                self.requested.remove("login_cancel");
                self.codex.insert("login_cancel".into(), value);
            }
            WorkbenchRequestKey::SettingsConfig => {
                self.requested.remove("config");
                self.codex.insert("config".into(), value);
            }
            _ => {}
        }
    }
    pub(crate) fn apply_error(
        &mut self,
        key: &WorkbenchRequestKey,
        error: &crate::client::contracts::BackendError,
    ) {
        let item = match key {
            WorkbenchRequestKey::SettingsAccount => Some("account"),
            WorkbenchRequestKey::SettingsRateLimits => Some("limits"),
            WorkbenchRequestKey::SettingsMcp => Some("mcp"),
            WorkbenchRequestKey::SettingsApps => Some("apps"),
            WorkbenchRequestKey::SettingsSkills => Some("skills"),
            WorkbenchRequestKey::SettingsFeatures => Some("features"),
            WorkbenchRequestKey::SettingsConfig => Some("config"),
            WorkbenchRequestKey::SettingsDoctor => Some("doctor"),
            WorkbenchRequestKey::SettingsLogin => Some("login"),
            WorkbenchRequestKey::SettingsLoginCancel => Some("login_cancel"),
            _ => None,
        };
        if let Some(item) = item {
            if item == "features" && self.feature_updating.take().is_some() {
                self.requested.remove("feature_update");
                self.feature_error =
                    Some(format!("Unable to update Codex feature: {}", error.message));
            } else {
                self.requested.remove(item);
                if item == "features" {
                    self.feature_error = Some(error.message.clone());
                }
                self.status = Some(format!("{item}: {}", error.message));
            }
        }
    }
}

fn parse_feature_page(value: &Value) -> (Vec<Value>, Option<String>) {
    let root = value
        .get("result")
        .filter(|result| result.is_object())
        .unwrap_or(value);
    let items = root
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| {
            item["name"]
                .as_str()
                .is_some_and(|name| !name.trim().is_empty())
                && feature_stage(item).is_some()
        })
        .cloned()
        .collect();
    let cursor = root
        .get("nextCursor")
        .and_then(Value::as_str)
        .filter(|cursor| !cursor.is_empty())
        .map(str::to_owned);
    (items, cursor)
}

fn feature_stage(feature: &Value) -> Option<&str> {
    let stage = feature
        .get("stage")
        .and_then(Value::as_str)?
        .trim()
        .to_ascii_lowercase();
    match stage.as_str() {
        "stable" => Some("stable"),
        "beta" => Some("beta"),
        "underdevelopment" | "under_development" => Some("under_development"),
        "deprecated" => Some("deprecated"),
        "removed" => Some("removed"),
        _ => None,
    }
}

fn hidden_feature(feature: &Value) -> bool {
    matches!(
        feature["name"].as_str(),
        Some("personality" | "collab" | "steer" | "collaboration_modes")
    )
}

fn human_feature_name(name: &str) -> String {
    name.split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().to_string() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn feature_description(feature: &Value) -> String {
    for key in ["description", "announcement"] {
        if let Some(text) = feature
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
        {
            return text.to_owned();
        }
    }
    match feature["name"].as_str().unwrap_or_default() {
        "undo" => "Create a ghost commit at each turn.".into(),
        "shell_tool" => "Enable the default shell tool.".into(),
        "unified_exec" => "Use the single unified PTY-backed exec tool.".into(),
        "apps" => "Enable ChatGPT Apps integration.".into(),
        _ => match feature_stage(feature) {
            Some("deprecated") => "Deprecated feature flag.".into(),
            Some("removed") => "Legacy feature flag kept for backward compatibility.".into(),
            _ => format!(
                "Feature key: features.{}",
                feature["name"].as_str().unwrap_or_default()
            ),
        },
    }
}

fn feature_app_setting_key(name: &str) -> Option<&'static str> {
    match name {
        "apps" => Some("experimentalAppsEnabled"),
        "unified_exec" => Some("unifiedExecEnabled"),
        _ => None,
    }
}

fn optional_string(value: &str) -> Value {
    if value.trim().is_empty() {
        Value::Null
    } else {
        Value::String(value.to_owned())
    }
}

fn split_open_app_args(value: &str) -> Vec<Value> {
    value
        .split_whitespace()
        .map(|item| Value::String(item.to_owned()))
        .collect()
}

fn open_app_complete(target: &Value) -> bool {
    let label = target["label"].as_str().unwrap_or_default().trim();
    if label.is_empty() {
        return false;
    }
    match target["kind"].as_str().unwrap_or_default() {
        "app" => !target["appName"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "command" => !target["command"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .is_empty(),
        "finder" => true,
        _ => false,
    }
}

fn set_string(settings: &mut Value, key: &str, value: String) -> bool {
    if settings.get(key).and_then(Value::as_str) == Some(value.as_str()) {
        return false;
    }
    let Some(object) = settings.as_object_mut() else {
        return false;
    };
    object.insert(key.to_owned(), Value::String(value));
    true
}

fn checkbox_setting(ui: &mut Ui, settings: &mut Value, key: &str, label: &str) -> bool {
    let mut value = settings
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or_default();
    let changed = ui.checkbox(&mut value, label).changed();
    if changed {
        if let Some(object) = settings.as_object_mut() {
            object.insert(key.to_owned(), Value::Bool(value));
        }
    }
    changed
}

fn edit_string_setting(ui: &mut Ui, settings: &mut Value, key: &str, label: &str) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut value = settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let changed = ui.text_edit_singleline(&mut value).changed();
        if changed {
            set_string(settings, key, value);
        }
        changed
    })
    .inner
}

fn edit_optional_string_setting(ui: &mut Ui, settings: &mut Value, key: &str, label: &str) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut value = settings
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let changed = ui.text_edit_singleline(&mut value).changed();
        if changed {
            let next = if value.trim().is_empty() {
                Value::Null
            } else {
                Value::String(value)
            };
            if let Some(object) = settings.as_object_mut() {
                object.insert(key.to_owned(), next);
            }
        }
        changed
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_setting_update_preserves_remote_routing_configuration() {
        let mut settings = serde_json::json!({
            "theme": "system",
            "backendMode": "remote",
            "activeRemoteBackendId": "desk-1",
            "remoteBackends": [{
                "id": "desk-1",
                "provider": "tcp",
                "host": "desk.example:4732",
                "token": "kept-secret"
            }]
        });

        assert!(set_string(&mut settings, "theme", "dim".into()));
        assert!(!set_string(&mut settings, "theme", "dim".into()));
        assert_eq!(settings["theme"], "dim");
        assert_eq!(settings["backendMode"], "remote");
        assert_eq!(settings["activeRemoteBackendId"], "desk-1");
        assert_eq!(settings["remoteBackends"][0]["token"], "kept-secret");
    }

    #[test]
    fn remote_settings_shape_can_be_edited_and_keeps_active_target_identity() {
        let mut settings = serde_json::json!({
            "backendMode": "remote",
            "activeRemoteBackendId": "remote-1",
            "remoteBackends": [{
                "id": "remote-1",
                "name": "Workstation",
                "provider": "tcp",
                "host": "10.0.0.8:4732",
                "token": "secret"
            }]
        });

        assert!(set_string(
            &mut settings["remoteBackends"][0],
            "host",
            "workstation.local:4732".into()
        ));
        assert_eq!(settings["backendMode"], "remote");
        assert_eq!(settings["activeRemoteBackendId"], "remote-1");
        assert_eq!(
            settings["remoteBackends"][0]["host"],
            "workstation.local:4732"
        );
        assert_eq!(settings["remoteBackends"][0]["token"], "secret");
    }

    #[test]
    fn backend_switch_preserves_old_draft_and_blocks_implicit_save() {
        let mut panel = SettingsPanel {
            draft: "old draft".into(),
            dirty: true,
            ..SettingsPanel::default()
        };

        panel.mark_stale();
        panel.apply(
            &WorkbenchRequestKey::SettingsLoad,
            serde_json::json!({"theme":"light"}),
        );

        assert_eq!(panel.draft, "old draft");
        assert!(panel.dirty);
        assert!(panel.draft_scope_stale);
        assert_eq!(panel.value, Some(serde_json::json!({"theme":"light"})));
        panel.draft = serde_json::to_string_pretty(panel.value.as_ref().unwrap()).unwrap();
        panel.dirty = false;
        panel.draft_scope_stale = false;
        assert!(!panel.dirty && !panel.draft_scope_stale);
    }

    #[test]
    fn feature_catalog_pagination_and_visibility_match_settings_contract() {
        let (items, cursor) = parse_feature_page(&serde_json::json!({
            "result": {
                "data": [
                    {"name":"stable_flag", "stage":"stable", "enabled":true},
                    {"name":"new_flag", "stage":"underDevelopment", "enabled":false},
                    {"name":"collab", "stage":"beta", "enabled":true},
                    {"name":"unknown", "stage":"future", "enabled":false}
                ],
                "nextCursor":"opaque-cursor"
            }
        }));

        assert_eq!(items.len(), 3);
        assert_eq!(cursor.as_deref(), Some("opaque-cursor"));
        assert_eq!(feature_stage(&items[1]), Some("under_development"));
        assert!(hidden_feature(&items[2]));
        assert_eq!(
            feature_app_setting_key("apps"),
            Some("experimentalAppsEnabled")
        );
        assert_eq!(
            feature_app_setting_key("unified_exec"),
            Some("unifiedExecEnabled")
        );
    }

    #[test]
    fn rendering_a_feature_row_does_not_mutate_without_user_input() {
        let mut panel = SettingsPanel {
            features: vec![serde_json::json!({"name":"apps", "stage":"beta", "enabled":false})],
            ..SettingsPanel::default()
        };
        let context = egui::Context::default();
        let mut effects = Vec::new();
        let mut output = context.run_ui(Default::default(), |ui| {
            let feature = panel.features[0].clone();
            panel.show_feature_row(ui, &mut effects, &feature);
        });
        output.textures_delta.clear();
        assert!(effects.is_empty());
        assert!(panel.feature_updating.is_none());
        assert!(!panel.requested.contains("feature_update"));
    }

    #[test]
    fn successful_apps_flag_change_updates_paired_setting_and_queues_save() {
        let mut panel = SettingsPanel {
            draft: serde_json::json!({"experimentalAppsEnabled":false,"theme":"dark"}).to_string(),
            features: vec![serde_json::json!({"name":"apps","stage":"beta","enabled":false})],
            feature_updating: Some(("apps".into(), true)),
            ..SettingsPanel::default()
        };

        panel.apply(
            &WorkbenchRequestKey::SettingsFeatures,
            serde_json::json!({"ok":true}),
        );

        let settings: Value = serde_json::from_str(&panel.draft).unwrap();
        assert_eq!(settings["experimentalAppsEnabled"], true);
        assert_eq!(settings["theme"], "dark");
        assert!(panel.dirty);
        assert_eq!(panel.features[0]["enabled"], true);
        assert!(matches!(
            panel.queued_effects.as_slice(),
            [PanelEffect::Request {
                key: WorkbenchRequestKey::SettingsSave,
                ..
            }]
        ));
    }

    #[test]
    fn open_in_target_contract_preserves_kind_and_normalizes_whitespace_args() {
        let command_target = serde_json::json!({
            "id":"terminal", "label":"Terminal", "kind":"command",
            "appName":null, "command":"xterm", "args":["--working-directory"]
        });
        assert!(open_app_complete(&command_target));
        assert!(!open_app_complete(
            &serde_json::json!({"label":"Broken","kind":"app","appName":"  "})
        ));
        assert_eq!(
            split_open_app_args("  --reuse-window   --new-window "),
            vec![
                Value::String("--reuse-window".into()),
                Value::String("--new-window".into())
            ]
        );
        assert_eq!(optional_string("  "), Value::Null);
        assert_eq!(optional_string("Editor"), Value::String("Editor".into()));
        assert_eq!(command_target["id"], "terminal");
        assert_eq!(command_target["kind"], "command");
    }
}
