use super::*;

impl NativeApp {
    pub(super) fn show_composer(&mut self, ui: &mut egui::Ui) {
        let Some(key) = self.controller.state.selected.clone() else {
            return;
        };
        let mut conversation = self
            .controller
            .state
            .selected_conversation()
            .map(|c| c.presentation_snapshot())
            .unwrap_or_default();
        let session = conversation.session.clone().unwrap_or(Value::Null);
        let is_acp = matches!(&key.conversation, ConversationRef::Acp { .. });
        let agent = if is_acp {
            session["profile"]["name"].as_str().unwrap_or("ACP agent")
        } else {
            "Codex"
        };
        let mut recover = None;
        let recovered = self.recovered_drafts.available(&key);
        if !recovered.is_empty() {
            ui.menu_button(format!("Recover draft ({})", recovered.len()), |ui| {
                ui.weak("Saved from a previous connection. Review before sending.");
                for (index, draft) in recovered.iter().enumerate() {
                    if ui
                        .button(format!(
                            "Connection {} · {} characters · {} attachments",
                            draft.scope.connection_epoch,
                            draft.draft.chars().count(),
                            draft.attachments.len()
                        ))
                        .clicked()
                    {
                        recover = Some(index);
                        ui.close();
                    }
                }
            });
        }
        if let Some(recovered) = recover.and_then(|index| self.recovered_drafts.take(&key, index)) {
            let current = self
                .controller
                .state
                .selected_conversation()
                .map(|c| c.draft.clone())
                .unwrap_or_default();
            self.act(Action::SetDraft(format!(
                "{current}{}{}",
                if current.is_empty() || recovered.draft.is_empty() {
                    ""
                } else {
                    "\n"
                },
                recovered.draft
            )));
            self.attachments
                .entry(key.clone())
                .or_default()
                .extend(recovered.attachments);
            conversation = self
                .controller
                .state
                .selected_conversation()
                .map(|c| c.presentation_snapshot())
                .unwrap_or_default();
        }
        if !is_acp {
            self.show_codex_controls(ui, &key);
        } else if let (
            Some(profile_id),
            ConversationRef::Acp { workspace_id, session_id, .. },
        ) = (session["profile"]["id"].as_str(), &key.conversation)
        {
            if ui.button("Connection and sign-in").clicked() {
                let cwd = self.controller.state.workspaces.iter()
                    .find(|workspace| workspace.id == *workspace_id)
                    .map(|workspace| workspace.path.clone())
                    .unwrap_or_default();
                self.acp_connections.open_existing_session(
                    &mut self.controller,
                    profile_id.to_owned(),
                    session_id.clone(),
                    workspace_id.clone(),
                    cwd,
                );
            }
        }
        self.show_user_input(ui, &key, &conversation.user_input_requests);
        let dictation_scope = self.dictation_scope();
        let permission_pending = self.permission_pending();
        for action in
            self.host_controls
                .show(ui, &self.settings, dictation_scope, permission_pending)
        {
            self.host(action, None);
        }
        for action in self.host_controls.show_audio_status(ui) {
            self.host(action, None);
        }
        if is_acp {
            let config = &session["configuration"];
            let mut options = config["configOptions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if options.is_empty() {
                if let Some(models) = config["models"]["availableModels"].as_array() {
                    options.push(json!({"id":"model","name":"Model","type":"select","currentValue":config["models"]["currentModelId"],"options":models.iter().map(|m|json!({"value":m["modelId"],"name":m["name"]})).collect::<Vec<_>>()}));
                }
                if let Some(modes) = config["modes"]["availableModes"].as_array() {
                    options.push(json!({"id":"mode","name":"Mode","type":"select","currentValue":config["modes"]["currentModeId"],"options":modes.iter().map(|m|json!({"value":m["id"],"name":m["name"]})).collect::<Vec<_>>()}));
                }
            }
            ui.horizontal_wrapped(|ui| {
                for option in options {
                    let id = option["id"].as_str().unwrap_or("").to_owned();
                    let name = option["name"].as_str().unwrap_or(&id);
                    let old = option["currentValue"].as_str().unwrap_or("");
                    let mut selected = old.to_owned();
                    let opts = option["options"].as_array().cloned().unwrap_or_default();
                    let display = option_label(&opts, old).unwrap_or_else(|| old.to_owned());
                    ui.add_enabled_ui(
                        !conversation.busy
                            && !conversation.read_only
                            && !conversation.loading
                            && !conversation.configuration_pending,
                        |ui| {
                            ui.label(name);
                            let popup = egui::ComboBox::from_id_salt(("runtime-config", &key, &id))
                                .selected_text(display)
                                .show_ui(ui, |ui| {
                                    for entry in &opts {
                                        if let Some(group) = entry["options"].as_array() {
                                            ui.strong(entry["name"].as_str().unwrap_or("Options"));
                                            for value in group {
                                                ui.selectable_value(
                                                    &mut selected,
                                                    value["value"].as_str().unwrap_or("").into(),
                                                    value["name"].as_str().unwrap_or("Option"),
                                                );
                                            }
                                        } else {
                                            ui.selectable_value(
                                                &mut selected,
                                                entry["value"].as_str().unwrap_or("").into(),
                                                entry["name"].as_str().unwrap_or("Option"),
                                            );
                                        }
                                    }
                                });
                            self.popup_open |= popup.inner.is_some();
                        },
                    );
                    if selected != old {
                        self.act(Action::Configure {
                            option_id: id,
                            value: selected,
                        });
                    }
                }
            });
        }
        for permission in &conversation.permissions {
            ui.group(|ui| {
                ui.strong("Agent permission request");
                ui.label(
                    permission["toolCall"]["title"]
                        .as_str()
                        .unwrap_or("An agent action needs your response"),
                );
                let request_id = permission["requestId"].clone();
                if let Some(options) = permission["options"].as_array() {
                    ui.horizontal_wrapped(|ui| {
                        for option in options {
                            if ui
                                .add_enabled(
                                    !conversation.read_only,
                                    egui::Button::new(option["name"].as_str().unwrap_or("Respond")),
                                )
                                .clicked()
                            {
                                self.act(Action::Permission {
                                    request_id: request_id.clone(),
                                    option_id: option["optionId"].as_str().unwrap_or("").into(),
                                });
                            }
                        }
                    });
                } else {
                    ui.horizontal(|ui| {
                        for (name, value) in [("Allow once", "accept"), ("Reject", "decline")] {
                            if ui
                                .add_enabled(!conversation.read_only, egui::Button::new(name))
                                .clicked()
                            {
                                self.act(Action::Permission {
                                    request_id: request_id.clone(),
                                    option_id: value.into(),
                                });
                            }
                        }
                    });
                }
            });
        }
        if is_acp {
            self.show_client_requests(ui, &key, &conversation);
        }
        if let Some(request) = conversation.uncertain_request {
            ui.colored_label(theme::ERROR,format!("Submission outcome is uncertain ({request}). It will not be resent automatically."));
            if ui.button("Check submission receipt").clicked() {
                self.act(Action::CheckReceipt);
            }
        }
        let label = format!("Message {agent}");
        let mut draft = conversation.draft.clone();
        if let Some(replacement) = self.show_suggestions(ui, &key, &draft) {
            draft = replacement;
            self.act(Action::SetDraft(draft.clone()));
        }
        let editor_id = egui::Id::new(("composer", &key));
        let focused = ui.memory(|m| m.has_focus(editor_id));
        let ime_active = if focused {
            ui.input(|input| ime_suppresses_submit(&mut self.ime_composing, &input.events))
        } else {
            false
        };
        if focused
            && !ime_active
            && (draft.is_empty() || self.prompt_history_cursor.is_navigating())
        {
            let history = presentation::prompt_history(&self.presentation, &key);
            let direction = ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    -1
                } else if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    1
                } else {
                    0
                }
            });
            let recalled = if direction < 0 {
                self.prompt_history_cursor.previous(&history, &draft)
            } else if direction > 0 {
                self.prompt_history_cursor.next(&history)
            } else {
                None
            };
            if let Some(recalled) = recalled {
                draft = recalled;
                self.act(Action::SetDraft(draft.clone()));
            }
        }
        let response = ui.add_enabled(
            !conversation.read_only && conversation.uncertain_request.is_none(),
            egui::TextEdit::multiline(&mut draft)
                .id(egui::Id::new(("composer", &key)))
                .hint_text(&label)
                .return_key(egui::KeyboardShortcut::new(
                    egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                ))
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        response.clone().labelled_by(
            ui.label(RichText::new(&label).small().color(theme::MUTED))
                .id,
        );
        if self.focus_composer {
            response.request_focus();
            self.focus_composer = false;
        }
        if response.changed() {
            self.act(Action::SetDraft(draft.clone()));
        }
        if conversation.loading {
            ui.weak("Loading conversation history. Your draft is kept while sending is paused.");
        }
        let can_submit = !conversation.read_only
            && !conversation.loading
            && !conversation.configuration_pending
            && conversation.uncertain_request.is_none();
        let opposite_shortcut = can_submit
            && response.has_focus()
            && !ime_active
            && ui.input_mut(|input| {
                input.consume_key(
                    egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                ) || input.consume_key(
                    egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                    egui::Key::Enter,
                )
            });
        let shortcut = can_submit
            && response.has_focus()
            && !ime_active
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
            });
        let attached = self.attachments.get(&key).cloned().unwrap_or_default();
        let default_steer =
            conversation.busy && !is_acp && self.settings.follow_up_message_behavior == "steer";
        let mut send = shortcut || opposite_shortcut;
        let mut steer = if opposite_shortcut {
            conversation.busy && !is_acp && !default_steer
        } else {
            default_steer
        };
        ui.horizontal_wrapped(|ui| {
            let supports_files = !is_acp
                || session["capabilities"]["images"].as_bool() == Some(true)
                || session["capabilities"]["audio"].as_bool() == Some(true)
                || session["capabilities"]["embeddedContext"].as_bool() == Some(true);
            if supports_files && ui.button("Attach files (optional)").clicked() {
                self.host(
                    HostAction::PickFiles {
                        multiple: true,
                        filters: if is_acp {
                            vec![]
                        } else {
                            vec![crate::desktop::platform::DialogFilter {
                                name: "Images".into(),
                                extensions: vec![
                                    "png".into(),
                                    "jpg".into(),
                                    "jpeg".into(),
                                    "gif".into(),
                                    "webp".into(),
                                ],
                            }]
                        },
                    },
                    Some(HostPurpose::Attach(key.clone())),
                );
            }
            if (!is_acp || session["capabilities"]["images"] == true)
                && ui.button("Paste image").clicked()
            {
                self.host(
                    HostAction::ReadClipboardImage,
                    Some(HostPurpose::AttachClipboard(key.clone())),
                );
            }
            if !attached.is_empty() {
                ui.label(format!("{} attachments", attached.len()));
                if ui.button("Clear attachments").clicked() {
                    self.attachments.remove(&key);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if conversation.busy
                    && ui
                        .add_enabled(!conversation.read_only, egui::Button::new("Stop"))
                        .clicked()
                {
                    self.act(Action::Stop);
                }
                if ui
                    .add_enabled(
                        can_submit && (!draft.trim().is_empty() || !attached.is_empty()),
                        egui::Button::new(if conversation.busy {
                            if default_steer {
                                "Steer"
                            } else {
                                "Queue message"
                            }
                        } else {
                            "Send"
                        }),
                    )
                    .clicked()
                {
                    send = true;
                }
                if !is_acp
                    && conversation.busy
                    && ui
                        .add_enabled(
                            can_submit,
                            egui::Button::new(if default_steer {
                                "Queue message"
                            } else {
                                "Steer"
                            }),
                        )
                        .clicked()
                {
                    send = true;
                    steer = !default_steer;
                }
            });
        });
        if send {
            let expanded = match self.suggestions.expand_prompt(&key, draft.trim()) {
                Ok(text) => text,
                Err(error) => {
                    self.notices.push(error);
                    return;
                }
            };
            if expanded != draft {
                self.act(Action::SetDraft(expanded));
            }
            match self.controller.handle(Action::Send {
                content: attached,
                options: json!({}),
                steer,
            }) {
                Ok(_) => {
                    self.presentation_dirty |=
                        presentation::record_prompt(&mut self.presentation, &key, &draft);
                    self.prompt_history_cursor.reset();
                    self.attachments.remove(&key);
                }
                Err(error) => {
                    self.act(Action::SetDraft(draft));
                    self.notices.push(error);
                }
            }
        }
        if !conversation.queue.is_empty() {
            ui.collapsing(
                format!("{} queued messages", conversation.queue.len()),
                |ui| {
                    for entry in &conversation.queue {
                        ui.horizontal(|ui| {
                            ui.label(&entry.text);
                            if ui.small_button("Remove").clicked() {
                                self.act(Action::RemoveQueued(entry.request_id));
                            }
                        });
                    }
                },
            );
        }
    }

    fn show_client_requests(
        &mut self,
        ui: &mut egui::Ui,
        key: &crate::client::ConversationKey,
        conversation: &crate::desktop::controller::ConversationState,
    ) {
        if conversation.client_requests_waiting {
            ui.weak("Waiting for the foreground ACP connection.");
        }
        if let Some(error) = &conversation.client_requests_error {
            ui.colored_label(theme::ERROR, format!("ACP client request status: {error}"));
        }
        for request in &conversation.client_requests {
            let id = request["clientRequestId"].clone();
            let kind = request["kind"].as_str().unwrap_or("approval");
            let enabled = !conversation.read_only
                && conversation.client_connection_id.is_some()
                && conversation.client_connection_generation.is_some();
            ui.group(|ui| {
                match kind {
                    "url" => {
                        ui.strong("Agent sign-in / URL consent");
                        if let Some(message) = request["message"].as_str() {
                            ui.label(message);
                        }
                        if let Some(url) = request["url"].as_str() {
                            ui.label("Requested URL");
                            ui.add(egui::Label::new(url).wrap().selectable(true));
                            ui.horizontal(|ui| {
                                if ui.button("Open URL").clicked() {
                                    self.host(HostAction::OpenUrl { url: url.to_owned() }, None);
                                }
                                for (label, action) in [("Approve request", "accept"), ("Deny", "decline")] {
                                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                        self.act(Action::RespondClientRequest {
                                            key: key.clone(),
                                            client_request_id: id.clone(),
                                            decision: json!({"action":action}),
                                        });
                                    }
                                }
                            });
                        } else {
                            ui.label("The agent supplied no URL.");
                        }
                    }
                    "command" | "approval" => {
                        ui.strong("Agent client callback approval");
                        if let Some(command) = request["command"].as_str() {
                            ui.label(format!("Command: {command}"));
                        }
                        if let Some(args) = request["args"].as_array() {
                            ui.label(format!("Arguments: {}", args.iter().map(|v| v.as_str().unwrap_or("?")).collect::<Vec<_>>().join(" ")));
                        }
                        if let Some(cwd) = request["cwd"].as_str() {
                            ui.label(format!("Working directory: {cwd}"));
                        }
                        ui.horizontal(|ui| {
                            for (label, action) in [("Allow once", "allow_once"), ("Deny", "deny")] {
                                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                    self.act(Action::RespondClientRequest {
                                        key: key.clone(),
                                        client_request_id: id.clone(),
                                        decision: json!({"action":action}),
                                    });
                                }
                            }
                        });
                    }
                    _ => {
                        ui.strong("Agent client request");
                        if let Some(message) = request["message"].as_str() {
                            ui.label(message);
                        }
                        ui.horizontal(|ui| {
                            for (label, action) in [("Accept", "accept"), ("Decline", "decline")] {
                                if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                    self.act(Action::RespondClientRequest {
                                        key: key.clone(),
                                        client_request_id: id.clone(),
                                        decision: json!({"action":action}),
                                    });
                                }
                            }
                        });
                    }
                }
                if !enabled {
                    ui.weak(if conversation.read_only { "This conversation is read-only; callback decisions are unavailable." } else { "Waiting for the foreground ACP connection." });
                }
            });
        }
    }
}

fn option_label(options: &[Value], value: &str) -> Option<String> {
    for option in options {
        if option["value"].as_str() == Some(value) {
            return option["name"].as_str().map(str::to_owned);
        }
        if let Some(children) = option["options"].as_array() {
            if let Some(found) = option_label(children, value) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grouped_configuration_preserves_wire_value() {
        let options = json!([{"name":"Provider","options":[{"name":"Big Pickle","value":"opencode/big-pickle"}]}]);
        assert_eq!(
            option_label(options.as_array().unwrap(), "opencode/big-pickle"),
            Some("Big Pickle".into())
        );
    }
}

/// Commit/clear events suppress Enter for that frame as well as during preedit,
/// preventing the key used to confirm an IME candidate from submitting a turn.
fn ime_suppresses_submit(composing: &mut bool, events: &[egui::Event]) -> bool {
    let mut touched = false;
    for event in events {
        if let egui::Event::Ime(event) = event {
            match event {
                egui::ImeEvent::Preedit { text, .. } => {
                    *composing = !text.is_empty();
                    touched = true;
                }
                egui::ImeEvent::Commit(_) => {
                    *composing = false;
                    touched = true;
                }
                _ => {}
            }
        }
    }
    touched || *composing
}
#[cfg(test)]
mod ime_tests {
    use super::*;
    #[test]
    fn confirming_ime_candidates_never_submits_the_turn() {
        let mut composing = false;
        assert!(ime_suppresses_submit(
            &mut composing,
            &[egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "漢".into(),
                active_range_chars: Some(0..1)
            })]
        ));
        assert!(ime_suppresses_submit(&mut composing, &[]));
        assert!(ime_suppresses_submit(
            &mut composing,
            &[egui::Event::Ime(egui::ImeEvent::Commit("漢字".into()))]
        ));
        assert!(!ime_suppresses_submit(&mut composing, &[]));
    }
}
