use super::*;

const PENDING_BROWSER_SESSION_ID: &str = "codex-monitor-browser-pending-session";

fn antigravity_preset_recipe() -> (&'static str, &'static str, &'static [&'static str]) {
    (
        "Google Antigravity ACP 1.4.1",
        "./agy_acp_server.par",
        &["--uid="],
    )
}

#[derive(Clone)]
struct PendingBrowserSession {
    scope: crate::client::BackendScope,
    catalog_id: String,
    store_id: Option<String>,
    session_id: Option<String>,
    native_session_id: Option<String>,
    error: Option<String>,
}

impl Default for PendingBrowserSession {
    fn default() -> Self {
        Self {
            scope: crate::client::BackendScope {
                endpoint_id: String::new(),
                connection_epoch: 0,
                store_id: None,
            },
            catalog_id: String::new(),
            store_id: None,
            session_id: None,
            native_session_id: None,
            error: None,
        }
    }
}

fn browser_scope_matches(
    captured: &crate::client::BackendScope,
    active: &crate::client::BackendScope,
    runtime_store_id: Option<&str>,
) -> bool {
    if captured.endpoint_id != active.endpoint_id
        || captured.connection_epoch != active.connection_epoch
    {
        return false;
    }
    runtime_store_id
        .or(active.store_id.as_deref())
        .is_none_or(|store_id| captured.store_id.as_deref() == Some(store_id))
}

fn catalog_attach_action(catalog_id: Option<&str>, associated: bool) -> Option<Action> {
    catalog_id.map(|catalog_id| Action::OpenCatalog {
        catalog_id: catalog_id.to_owned(),
        add_workspace: !associated,
    })
}

fn catalog_attach_button(
    ui: &mut egui::Ui,
    catalog_id: Option<&str>,
    associated: bool,
    enabled: bool,
) -> Option<Action> {
    let response = ui.add_enabled(
        enabled && catalog_id.is_some(),
        egui::Button::new(if associated {
            "Open conversation"
        } else {
            "Add workspace and open"
        }),
    );
    response
        .clicked()
        .then(|| catalog_attach_action(catalog_id, associated))
        .flatten()
}

fn pending_attach_is_selected(
    pending: &PendingBrowserSession,
    active_scope: &crate::client::BackendScope,
    runtime_store_id: Option<&str>,
    catalog: &[Value],
    selected: Option<&ConversationKey>,
) -> bool {
    if pending.error.is_some()
        || !browser_scope_matches(&pending.scope, active_scope, runtime_store_id)
    {
        return false;
    }
    let Some(row) = catalog
        .iter()
        .find(|row| row["catalogId"].as_str() == Some(pending.catalog_id.as_str()))
    else {
        return false;
    };
    let Some(session_id) = row["sessionId"].as_str() else {
        return false;
    };
    let Some(store_id) = row["storeId"].as_str() else {
        return false;
    };
    if pending
        .store_id
        .as_deref()
        .is_some_and(|expected| expected != store_id)
        || pending
            .scope
            .store_id
            .as_deref()
            .is_some_and(|expected| expected != store_id)
        || pending
            .session_id
            .as_deref()
            .is_some_and(|expected| expected != session_id)
        || pending
            .native_session_id
            .as_deref()
            .is_some_and(|expected| row["nativeSessionId"].as_str() != Some(expected))
    {
        return false;
    }
    selected.is_some_and(|key| {
        key.endpoint_id == pending.scope.endpoint_id
            && matches!(
                &key.conversation,
                ConversationRef::Acp {
                    store_id: selected_store,
                    session_id: selected_session,
                    ..
                } if selected_store == store_id && selected_session == session_id
            )
    })
}

pub(super) struct ProfileEditor {
    pub id: Option<String>,
    pub revision: u64,
    pub name: String,
    pub executable: String,
    pub args: String,
    pub cwd: String,
    pub preset: String,
    initialized: bool,
    pub environment: String,
    pub filesystem_policy: String,
    pub command_policy: String,
    pub additional_roots: String,
}
impl Default for ProfileEditor {
    fn default() -> Self {
        Self {
            id: None,
            revision: 0,
            name: String::new(),
            executable: String::new(),
            args: String::new(),
            cwd: String::new(),
            preset: "generic".into(),
            initialized: false,
            environment: String::new(),
            filesystem_policy: "disabled".into(),
            command_policy: "disabled".into(),
            additional_roots: String::new(),
        }
    }
}
impl ProfileEditor {
    fn from_profile(profile: &Value) -> Self {
        let mut editor = Self {
            id: profile["id"].as_str().map(str::to_owned),
            revision: profile["revision"].as_u64().unwrap_or(0),
            name: profile["name"].as_str().unwrap_or("").to_owned(),
            executable: profile["executable"].as_str().unwrap_or("").to_owned(),
            args: profile["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default(),
            cwd: profile["workingDirectory"]
                .as_str()
                .unwrap_or("")
                .to_owned(),
            preset: match profile["presetId"].as_str() {
                Some("antigravity") | Some("antigravity-acp") => "antigravity-acp",
                Some(id) => id,
                None if profile["kind"].as_str() == Some("open_code_acp") => "opencode",
                None => "generic",
            }
            .to_owned(),
            initialized: true,
            ..Self::default()
        };
        if let Some(environment) = profile["environment"].as_object() {
            editor.environment = environment
                .iter()
                .map(|(key, value)| {
                    let value = match value["kind"].as_str() {
                        Some("backendEnv") => format!("${}", value["name"].as_str().unwrap_or("")),
                        _ => value["value"].as_str().unwrap_or("").to_owned(),
                    };
                    format!("{key}={value}")
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        editor.filesystem_policy = profile
            .pointer("/clientPolicy/filesystem")
            .and_then(Value::as_str)
            .unwrap_or("disabled")
            .to_owned();
        editor.command_policy = profile
            .pointer("/clientPolicy/commandExecution")
            .and_then(Value::as_str)
            .unwrap_or("disabled")
            .to_owned();
        editor.additional_roots = profile
            .pointer("/clientPolicy/additionalRoots")
            .and_then(Value::as_array)
            .map(|roots| {
                roots
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        editor
    }
}
impl NativeApp {
    pub(super) fn show_browser(&mut self, ctx: &egui::Context) {
        if !self.browser_open {
            return;
        }
        let mut pending_session = ctx.data(|data| {
            data.get_temp::<PendingBrowserSession>(egui::Id::new(PENDING_BROWSER_SESSION_ID))
        });
        if let Some(pending) = pending_session.as_mut() {
            if pending.error.is_none()
                && !browser_scope_matches(
                    &pending.scope,
                    self.controller.scope(),
                    self.controller.state.runtime_store_id.as_deref(),
                )
            {
                pending.error = Some(
                    "Backend changed while opening this session. The discovered row is still available; try again.".into(),
                );
            }
            if pending.error.is_none() && self.controller.state.error.is_some() {
                pending.error = self.controller.state.error.take();
            }
            if pending_attach_is_selected(
                pending,
                self.controller.scope(),
                self.controller.state.runtime_store_id.as_deref(),
                &self.controller.state.catalog,
                self.controller.state.selected.as_ref(),
            ) {
                self.browser_open = false;
                ctx.data_mut(|data| {
                    data.remove_temp::<PendingBrowserSession>(egui::Id::new(
                        PENDING_BROWSER_SESSION_ID,
                    ));
                });
                return;
            }
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::new(PENDING_BROWSER_SESSION_ID), pending.clone())
            });
        }
        let attach_in_flight = pending_session
            .as_ref()
            .is_some_and(|pending| pending.error.is_none());
        let mut open = true;
        egui::Window::new("Browse agent sessions")
            .open(&mut open)
            .default_size([850.0, 560.0])
            .show(ctx, |ui| {
                if let Some(error) = pending_session
                    .as_ref()
                    .and_then(|pending| pending.error.as_deref())
                {
                    ui.colored_label(theme::ERROR, format!("Could not open session: {error}"));
                    ui.label("The discovered session remains in the list. Retry it or cancel by closing this browser.");
                } else if attach_in_flight {
                    ui.label("Attaching session and refreshing its conversation…");
                }
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.text_edit_singleline(&mut self.browser_search);
                    if ui.button("Refresh sources").clicked() {
                        self.act(Action::DiscoverAll);
                    }
                    if ui.button("Add agent profile").clicked() {
                        self.profile_editor = Some(ProfileEditor::default());
                    }
                });
                ui.collapsing("Configured ACP profiles", |ui| {
                    for profile in self.controller.state.profiles.clone().into_iter()
                        .filter(|profile| profile["kind"].as_str() != Some("codex")) {
                        let id = profile["id"].as_str().unwrap_or("").to_owned();
                        let name = profile["name"].as_str().unwrap_or("ACP agent").to_owned();
                        ui.horizontal(|ui| {
                            ui.label(&name);
                            if ui.small_button("Edit").clicked() { self.profile_editor = Some(ProfileEditor::from_profile(&profile)); }
                            if ui.small_button("Connection / sign in").clicked() {
                                let cwd = self.selected_workspace.as_deref().and_then(|workspace_id|
                                    self.controller.state.workspaces.iter().find(|workspace| workspace.id == workspace_id).map(|workspace| workspace.path.clone()));
                                let workspace_id = self.selected_workspace.clone();
                                self.acp_connections.open_profile(&mut self.controller, id, cwd, workspace_id);
                            }
                        });
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .selectable_label(self.browser_profile.is_none(), "All profiles")
                        .clicked()
                    {
                        self.browser_profile = None;
                    }
                    for profile in self
                        .controller
                        .state
                        .profiles
                        .clone()
                        .into_iter()
                        .filter(|p| p["kind"].as_str() != Some("codex"))
                    {
                        let id = profile["id"].as_str().unwrap_or("");
                        if ui
                            .selectable_label(
                                self.browser_profile.as_deref() == Some(id),
                                profile["name"].as_str().unwrap_or(id),
                            )
                            .clicked()
                        {
                            self.browser_profile = Some(id.into());
                        }
                    }
                    for profile in self.controller.state.profiles.clone().into_iter()
                        .filter(|profile| profile["kind"].as_str() != Some("codex")) {
                        let id = profile["id"].as_str().unwrap_or("").to_owned();
                        let name = profile["name"].as_str().unwrap_or("ACP agent").to_owned();
                        if ui.small_button(format!("Connect · {name}")).clicked() {
                            let cwd = self.selected_workspace.as_deref().and_then(|workspace_id|
                                self.controller.state.workspaces.iter().find(|workspace| workspace.id == workspace_id).map(|workspace| workspace.path.clone()));
                            let workspace_id = self.selected_workspace.clone();
                            self.acp_connections.open_profile(&mut self.controller, id, cwd, workspace_id);
                        }
                    }
                });
                for source in self
                    .controller
                    .state
                    .discovery
                    .values()
                    .cloned()
                    .collect::<Vec<_>>()
                {
                    if let Some(error) = source.error {
                        ui.colored_label(theme::ERROR, format!("{}: {error}", source.profile_id));
                    }
                    if source.status.as_deref() == Some("unsupported") {
                        ui.label(format!(
                            "{}: discovery unavailable; known sessions remain available",
                            source.profile_id
                        ));
                    }
                    if source.cursor.is_some()
                        && ui
                            .button(format!("Load more · {}", source.profile_id))
                            .clicked()
                    {
                        self.act(Action::AddDiscoveryPage {
                            profile_id: source.profile_id,
                        });
                    }
                }
                ui.separator();
                let search = self.browser_search.to_lowercase();
                egui::ScrollArea::vertical()
                    .max_height(340.0)
                    .show(ui, |ui| {
                        for row in self.controller.state.catalog.clone() {
                            let profile = row["profile"]["id"].as_str().unwrap_or("");
                            if row["profile"]["kind"].as_str() == Some("codex")
                                || row["presentation"]["hidden"].as_bool() == Some(true)
                                || self
                                    .browser_profile
                                    .as_ref()
                                    .is_some_and(|selected| selected != profile)
                            {
                                continue;
                            }
                            let title = row["presentation"]["titleOverride"]
                                .as_str()
                                .or_else(|| row["title"].as_str())
                                .or_else(|| row["nativeSessionId"].as_str())
                                .unwrap_or("Agent session");
                            let cwd = row["cwd"].as_str().unwrap_or("");
                            if !format!("{title} {cwd}").to_lowercase().contains(&search) {
                                continue;
                            }
                            let associated = self.controller.state.workspaces.iter().any(|w| {
                                w.path == cwd || row["workspaceId"].as_str() == Some(&w.id)
                            });
                            ui.push_id(row["catalogId"].as_str().unwrap_or(""), |ui| {
                                ui.horizontal(|ui| {
                                    ui.vertical(|ui| {
                                        ui.strong(title);
                                        ui.label(RichText::new(cwd).small().color(theme::MUTED));
                                        ui.label(row["profile"]["name"].as_str().unwrap_or("ACP"));
                                    });
                                    if let Some(action) = catalog_attach_button(
                                        ui,
                                        row["catalogId"].as_str(),
                                        associated,
                                        !attach_in_flight,
                                    ) {
                                        let store_id = row["storeId"].as_str().map(str::to_owned);
                                        let mut scope = self.controller.scope().clone();
                                        scope.store_id = store_id.clone().or_else(|| {
                                            self.controller.state.runtime_store_id.clone()
                                        }).or(scope.store_id);
                                        let mut pending = PendingBrowserSession {
                                            scope,
                                            catalog_id: row["catalogId"].as_str().unwrap_or_default().to_owned(),
                                            store_id,
                                            session_id: row["sessionId"].as_str().map(str::to_owned),
                                            native_session_id: row["nativeSessionId"].as_str().map(str::to_owned),
                                            error: None,
                                        };
                                        self.controller.state.error = None;
                                        if !browser_scope_matches(
                                            &pending.scope,
                                            self.controller.scope(),
                                            self.controller.state.runtime_store_id.as_deref(),
                                        ) {
                                            pending.error = Some(
                                                "Backend changed before this session could be opened. The discovered row is still available.".into(),
                                            );
                                        } else if let Err(error) = self.controller.handle(action) {
                                            pending.error = Some(error);
                                        }
                                        ctx.data_mut(|data| {
                                            data.insert_temp(
                                                egui::Id::new(PENDING_BROWSER_SESSION_ID),
                                                pending,
                                            )
                                        });
                                    }
                                });
                            });
                            ui.separator();
                        }
                    });
                ui.add_enabled_ui(!attach_in_flight, |ui| ui.collapsing("Open session by ID", |ui| {
                    egui::ComboBox::from_id_salt("open-by-id-profile")
                        .selected_text(if self.open_id_profile.is_empty() {
                            "Choose profile"
                        } else {
                            &self.open_id_profile
                        })
                        .show_ui(ui, |ui| {
                            for profile in &self.controller.state.profiles {
                                if profile["kind"].as_str() != Some("codex") {
                                    let id = profile["id"].as_str().unwrap_or("");
                                    ui.selectable_value(
                                        &mut self.open_id_profile,
                                        id.to_owned(),
                                        profile["name"].as_str().unwrap_or(id),
                                    );
                                }
                            }
                        });
                    ui.label("Native session ID");
                    ui.text_edit_singleline(&mut self.open_id_native);
                    ui.label("Directory on the executing backend");
                    ui.text_edit_singleline(&mut self.open_id_cwd);
                    if ui.button("Find session").clicked() {
                        ctx.data_mut(|data| {
                            data.remove_temp::<PendingBrowserSession>(egui::Id::new(
                                PENDING_BROWSER_SESSION_ID,
                            ));
                        });
                        self.act(Action::OpenSessionById {
                            profile_id: self.open_id_profile.clone(),
                            cwd: self.open_id_cwd.clone(),
                            native_session_id: self.open_id_native.clone(),
                        });
                    }
                }));
            });
        if attach_in_flight && !open {
            open = true;
        }
        if !open {
            ctx.data_mut(|data| {
                data.remove_temp::<PendingBrowserSession>(egui::Id::new(
                    PENDING_BROWSER_SESSION_ID,
                ));
            });
        }
        self.browser_open = open;
    }
    pub(super) fn show_profile_editor(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.profile_editor.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Agent profile")
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                let previous_preset = form.preset.clone();
                egui::ComboBox::from_label("Agent preset")
                    .selected_text(match form.preset.as_str() {
                        "opencode" => "OpenCode",
                        "claude" => "Claude ACP adapter",
                        "gemini" => "Gemini CLI",
                        "antigravity-acp" | "antigravity" => "Antigravity",
                        _ => "Generic ACP",
                    })
                    .show_ui(ui, |ui| {
                        for (id, label) in [
                            ("generic", "Generic ACP"),
                            ("opencode", "OpenCode"),
                            ("claude", "Claude ACP adapter"),
                            ("gemini", "Gemini CLI"),
                            ("antigravity-acp", "Antigravity"),
                        ] {
                            ui.selectable_value(&mut form.preset, id.into(), label);
                        }
                    });
                let preset_changed = previous_preset != form.preset;
                if preset_changed || !form.initialized {
                    form.initialized = true;
                    match form.preset.as_str() {
                        "opencode" => { form.name = "OpenCode".into(); form.executable = "opencode".into(); form.args.clear(); }
                        "claude" => { form.name = "Claude".into(); form.executable = "claude-agent-acp".into(); form.args.clear(); }
                        "gemini" => { form.name = "Gemini CLI".into(); form.executable = "gemini".into(); form.args = "--acp".into(); }
                        "antigravity-acp" | "antigravity" => {
                            let (name, executable, args) = antigravity_preset_recipe();
                            form.name = name.into();
                            form.executable = executable.into();
                            form.args = args.join("\n");
                        }
                        _ => {}
                    }
                }
                ui.label("Name");
                ui.text_edit_singleline(&mut form.name);
                ui.label("Executable");
                ui.text_edit_singleline(&mut form.executable);
                ui.label("Arguments · one exact argument per line");
                ui.add(egui::TextEdit::multiline(&mut form.args).desired_rows(3));
                ui.label("Launch directory (optional)");
                ui.text_edit_singleline(&mut form.cwd);
                if form.preset != "generic" {
                    ui.label("Preset arguments are supplied by this launch recipe. You can edit them below.");
                }
                if matches!(form.preset.as_str(), "antigravity-acp" | "antigravity") {
                    ui.label("Use the installed agy_acp_server.par path on the executing backend. The preset uses --uid= with an empty value and does not guess an installation UID.");
                }
                if form.preset == "gemini" && form.command_policy == "disabled" {
                    ui.label("Gemini CLI 0.64 requires terminal callbacks. Choose Ask or Allow for command callbacks before connecting.");
                }
                ui.label("Environment · KEY=value or KEY=$BACKEND_VARIABLE per line");
                ui.weak("Literal values are stored in the profile; use backend variables for credentials.");
                ui.add(egui::TextEdit::multiline(&mut form.environment).desired_rows(2));
                ui.label("Filesystem callbacks");
                egui::ComboBox::from_id_salt("acp-profile-filesystem")
                    .selected_text(&form.filesystem_policy)
                    .show_ui(ui, |ui| { for option in ["disabled", "read_only", "read_write"] { ui.selectable_value(&mut form.filesystem_policy, option.into(), option); } });
                ui.label("Command execution callbacks");
                egui::ComboBox::from_id_salt("acp-profile-commands")
                    .selected_text(&form.command_policy)
                    .show_ui(ui, |ui| { for option in ["disabled", "ask", "allow"] { ui.selectable_value(&mut form.command_policy, option.into(), option); } });
                if form.command_policy == "allow" {
                    ui.colored_label(theme::ERROR, "Allowed agent commands run with the executing backend account's permissions.");
                }
                ui.label("Additional filesystem roots · one backend path per line");
                ui.add(egui::TextEdit::multiline(&mut form.additional_roots).desired_rows(2));
                save = ui
                    .add_enabled(
                        !form.name.trim().is_empty() && !form.executable.trim().is_empty(),
                        egui::Button::new("Save agent profile"),
                    )
                    .clicked();
            });
        if save {
            let id = form
                .id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            let environment: serde_json::Map<String, Value> = form
                .environment
                .lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    let key = key.trim();
                    if key.is_empty() {
                        return None;
                    }
                    let value = value.trim();
                    let source = value
                        .strip_prefix('$')
                        .filter(|name| !name.is_empty())
                        .map(|name| json!({"kind":"backendEnv","name":name}))
                        .unwrap_or_else(|| json!({"kind":"literal","value":value}));
                    Some((key.to_owned(), source))
                })
                .collect();
            let kind = if form.preset == "opencode" {
                "open_code_acp"
            } else {
                "acp"
            };
            let profile = json!({"id":id,"revision":form.revision,"name":form.name,"kind":kind,"presetId":if form.preset == "generic" { None::<String> } else { Some(form.preset.clone()) },"executable":form.executable,"args":form.args.lines().map(str::to_owned).collect::<Vec<_>>(),"workingDirectory":if form.cwd.trim().is_empty(){None}else{Some(&form.cwd)},"environment":environment,"clientPolicy":{"filesystem":form.filesystem_policy,"commandExecution":form.command_policy,"additionalRoots":form.additional_roots.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect::<Vec<_>>()}});
            let request = BackendRequest::Runtime(RuntimeRequest::mutation(
                RuntimeMethod::SaveProfile,
                json!({"profile":profile,"expectedRevision":form.revision}),
                None,
            ));
            match self.controller.request(request) {
                Ok(id) => {
                    self.after_request.insert(id, RootRefresh::Profiles);
                }
                Err(error) => self
                    .notices
                    .push(format!("Could not save profile: {error:?}")),
            }
        } else if open {
            self.profile_editor = Some(form);
        }
    }
}

#[cfg(test)]
mod preset_tests {
    use super::antigravity_preset_recipe;

    #[test]
    fn antigravity_preset_matches_the_configured_acp_recipe() {
        let (name, executable, args) = antigravity_preset_recipe();
        assert_eq!(name, "Google Antigravity ACP 1.4.1");
        assert_eq!(executable, "./agy_acp_server.par");
        assert_eq!(args, &["--uid="]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        kittest::{NodeT as _, Queryable as _},
        Harness,
    };

    fn scope(endpoint: &str, epoch: u64, store: Option<&str>) -> crate::client::BackendScope {
        crate::client::BackendScope {
            endpoint_id: endpoint.into(),
            connection_epoch: epoch,
            store_id: store.map(str::to_owned),
        }
    }

    #[test]
    fn unbound_discovery_row_click_dispatches_attach_by_catalog_id() {
        let mut effects = Vec::new();
        let mut harness = Harness::new_ui(|ui| {
            if let Some(effect) = catalog_attach_button(ui, Some("catalog-new"), false, true) {
                effects.push(effect);
            }
        });
        harness.get_by_label("Add workspace and open").click();
        harness.run();
        drop(harness);

        assert!(matches!(
            effects.as_slice(),
            [Action::OpenCatalog {
                catalog_id,
                add_workspace: true
            }] if catalog_id == "catalog-new"
        ));
    }

    #[test]
    fn browser_closes_only_after_same_scoped_catalog_binding_is_selected() {
        let active = scope("local:/data", 4, None);
        let pending = PendingBrowserSession {
            scope: scope("local:/data", 4, Some("store-a")),
            catalog_id: "catalog-new".into(),
            store_id: Some("store-a".into()),
            session_id: None,
            native_session_id: Some("native-7".into()),
            error: None,
        };
        let bound_row = json!({
            "catalogId":"catalog-new",
            "storeId":"store-a",
            "sessionId":"monitor-session-7",
            "nativeSessionId":"native-7"
        });
        let selected = ConversationKey {
            endpoint_id: "local:/data".into(),
            conversation: ConversationRef::Acp {
                workspace_id: "workspace-1".into(),
                store_id: "store-a".into(),
                session_id: "monitor-session-7".into(),
            },
        };

        assert!(pending_attach_is_selected(
            &pending,
            &active,
            Some("store-a"),
            std::slice::from_ref(&bound_row),
            Some(&selected),
        ));
        assert!(!pending_attach_is_selected(
            &pending,
            &active,
            Some("store-a"),
            &[json!({
                "catalogId":"catalog-new",
                "storeId":"store-a",
                "sessionId":null,
                "nativeSessionId":"native-7"
            })],
            Some(&selected),
        ));

        let failed = PendingBrowserSession {
            error: Some("attach rejected".into()),
            ..pending.clone()
        };
        assert!(!pending_attach_is_selected(
            &failed,
            &active,
            Some("store-a"),
            std::slice::from_ref(&bound_row),
            Some(&selected),
        ));
        assert!(!pending_attach_is_selected(
            &pending,
            &scope("daemon:other", 4, None),
            Some("store-a"),
            std::slice::from_ref(&bound_row),
            Some(&selected),
        ));
        assert!(!browser_scope_matches(
            &pending.scope,
            &active,
            Some("store-b"),
        ));
    }
}
