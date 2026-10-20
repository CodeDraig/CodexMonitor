use super::*;

impl NativeApp {
    pub(super) fn show_elicitation(
        &mut self,
        ui: &mut egui::Ui,
        key: &ConversationKey,
        requests: &[Value],
    ) {
        for request in requests {
            ui.group(|ui| {
                let params = &request["params"];
                ui.strong("MCP server request");
                ui.label(
                    params["message"]
                        .as_str()
                        .unwrap_or("Provide the requested information"),
                );
                let request_id = request["requestId"].clone();
                let mut content = serde_json::Map::new();
                let schema = &params["requestedSchema"];
                let required = schema["required"].as_array().cloned().unwrap_or_default();
                let mut valid = true;
                if let Some(properties) = schema["properties"].as_object() {
                    for (name, field) in properties {
                        ui.label(field["title"].as_str().unwrap_or(name));
                        if let Some(description) = field["description"].as_str() {
                            ui.small(description);
                        }
                        let storage = format!(
                            "{}:{request_id}:elicitation:{name}",
                            serde_json::to_string(key).unwrap_or_default()
                        );
                        let draft = self
                            .user_input_answers
                            .entry(storage)
                            .or_insert_with(|| field["default"].as_str().unwrap_or("").to_owned());
                        let value = if let Some(options) = field["enum"].as_array() {
                            egui::ComboBox::from_id_salt((
                                "elicitation",
                                key,
                                request_id.to_string(),
                                name,
                            ))
                            .selected_text(draft.clone())
                            .show_ui(ui, |ui| {
                                for option in options {
                                    if let Some(option) = option.as_str() {
                                        ui.selectable_value(draft, option.to_owned(), option);
                                    }
                                }
                            });
                            Value::String(draft.clone())
                        } else if field["type"] == "boolean" {
                            let mut checked = draft == "true";
                            if ui.checkbox(&mut checked, name).changed() {
                                *draft = checked.to_string();
                            }
                            Value::Bool(checked)
                        } else {
                            ui.add(
                                egui::TextEdit::singleline(draft)
                                    .password(field["format"] == "password"),
                            );
                            if field["type"] == "number" || field["type"] == "integer" {
                                match serde_json::from_str::<Value>(draft) {
                                    Ok(value) if value.is_number() => value,
                                    _ => {
                                        valid = false;
                                        Value::Null
                                    }
                                }
                            } else {
                                Value::String(draft.clone())
                            }
                        };
                        if required.iter().any(|r| r.as_str() == Some(name))
                            && value.as_str() == Some("")
                        {
                            valid = false;
                        }
                        content.insert(name.clone(), value);
                    }
                }
                if let Some(url) = params["url"].as_str() {
                    if ui.button("Open request URL").clicked() {
                        self.host(HostAction::OpenUrl { url: url.into() }, None);
                    }
                }
                ui.horizontal(|ui| {
                    for (label, action) in [
                        ("Submit", "accept"),
                        ("Decline", "decline"),
                        ("Cancel", "cancel"),
                    ] {
                        if ui
                            .add_enabled(action != "accept" || valid, egui::Button::new(label))
                            .clicked()
                        {
                            let workspace_id = match &key.conversation {
                                ConversationRef::Codex { workspace_id, .. }
                                | ConversationRef::Acp { workspace_id, .. } => workspace_id.clone(),
                            };
                            self.act(Action::CodexElicitation {
                                workspace_id,
                                request_id: request_id.clone(),
                                result: if action == "accept" {
                                    json!({"action":action,"content":content})
                                } else {
                                    json!({"action":action})
                                },
                            });
                        }
                    }
                });
            });
        }
    }
    pub(super) fn show_codex_controls(&mut self, ui: &mut egui::Ui, key: &ConversationKey) {
        let ConversationRef::Codex { workspace_id, .. } = &key.conversation else {
            return;
        };
        let Some(controls) = self
            .controller
            .state
            .codex_controls_by_workspace
            .get(workspace_id)
            .cloned()
        else {
            return;
        };
        let mut changes = Vec::new();
        ui.horizontal_wrapped(|ui| {
            let mut model = controls.selected_model.clone();
            ui.label("Model");
            egui::ComboBox::from_id_salt(("codex-model", key))
                .selected_text(model.as_deref().unwrap_or("Default"))
                .show_ui(ui, |ui| {
                    for option in &controls.models {
                        let id = option["model"]
                            .as_str()
                            .or_else(|| option["id"].as_str())
                            .unwrap_or("");
                        ui.selectable_value(
                            &mut model,
                            Some(id.into()),
                            option["displayName"]
                                .as_str()
                                .or_else(|| option["name"].as_str())
                                .unwrap_or(id),
                        );
                    }
                });
            if model != controls.selected_model {
                changes.push(("model", json!(model)));
            }
            if let Some(selected) = controls
                .models
                .iter()
                .find(|m| m["model"].as_str().or_else(|| m["id"].as_str()) == model.as_deref())
            {
                if let Some(efforts) = selected["supportedReasoningEfforts"].as_array() {
                    let mut effort = controls.selected_effort.clone();
                    ui.label("Effort");
                    egui::ComboBox::from_id_salt(("codex-effort", key))
                        .selected_text(effort.as_deref().unwrap_or("Default"))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut effort, None, "Default");
                            for option in efforts {
                                let value = option["reasoningEffort"]
                                    .as_str()
                                    .or_else(|| option.as_str())
                                    .unwrap_or("");
                                ui.selectable_value(&mut effort, Some(value.into()), value);
                            }
                        });
                    if effort != controls.selected_effort {
                        changes.push(("effort", json!(effort)));
                    }
                }
            }
            let mut access = controls.selected_access_mode.clone();
            ui.label("Access");
            egui::ComboBox::from_id_salt(("codex-access", key))
                .selected_text(if access.is_empty() {
                    "Default"
                } else {
                    &access
                })
                .show_ui(ui, |ui| {
                    for (mode, label) in [
                        ("read-only", "Read only"),
                        ("current", "Workspace"),
                        ("full-access", "Full access"),
                    ] {
                        ui.selectable_value(&mut access, mode.into(), label);
                    }
                });
            if access != controls.selected_access_mode {
                changes.push(("accessMode", json!(access)));
            }
            if !controls.collaboration_modes.is_empty() {
                let mut selected = controls.selected_collaboration_mode.clone();
                ui.label("Mode");
                egui::ComboBox::from_id_salt(("codex-collaboration", key))
                    .selected_text(
                        selected
                            .as_ref()
                            .and_then(|m| m["name"].as_str().or_else(|| m["mode"].as_str()))
                            .unwrap_or("Default"),
                    )
                    .show_ui(ui, |ui| {
                        for mode in &controls.collaboration_modes {
                            ui.selectable_value(
                                &mut selected,
                                Some(mode.clone()),
                                mode["name"]
                                    .as_str()
                                    .or_else(|| mode["mode"].as_str())
                                    .unwrap_or("Mode"),
                            );
                        }
                    });
                if selected != controls.selected_collaboration_mode {
                    changes.push(("collaborationMode", json!(selected)));
                }
            }
        });
        for (name, value) in changes {
            self.act(Action::SetCodexOption {
                workspace_id: workspace_id.clone(),
                name: name.into(),
                value,
            });
        }
    }
    pub(super) fn show_user_input(
        &mut self,
        ui: &mut egui::Ui,
        key: &ConversationKey,
        requests: &[Value],
    ) {
        for request in requests {
            let request_id = request["requestId"].clone();
            let questions = request["params"]["questions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            ui.group(|ui| {
                ui.strong("Agent question");
                let mut answers = serde_json::Map::new();
                for question in questions {
                    let id = question["id"].as_str().unwrap_or("question");
                    let storage = format!(
                        "{}:{request_id}:{id}",
                        serde_json::to_string(key).unwrap_or_default()
                    );
                    ui.label(
                        question["question"]
                            .as_str()
                            .or_else(|| question["header"].as_str())
                            .unwrap_or("Response"),
                    );
                    let value = self.user_input_answers.entry(storage).or_default();
                    if let Some(options) = question["options"].as_array() {
                        for option in options {
                            let label = option["label"].as_str().unwrap_or("");
                            ui.radio_value(value, label.into(), label)
                                .on_hover_text(option["description"].as_str().unwrap_or(""));
                        }
                    }
                    ui.text_edit_singleline(value);
                    answers.insert(id.into(), json!({"answers":[value]}));
                }
                if ui.button("Submit answers").clicked() {
                    self.act(Action::CodexUserInput {
                        request_id: request_id.clone(),
                        answers: Value::Object(answers),
                    });
                }
            });
        }
    }
}
