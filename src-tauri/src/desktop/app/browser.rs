use super::*;

const PENDING_BROWSER_SESSION_ID: &str = "codex-monitor-browser-pending-session";

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
    pub preset: bool,
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
            preset: false,
        }
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
                if ui
                    .checkbox(&mut form.preset, "OpenCode launch preset")
                    .changed()
                    && form.preset
                {
                    form.name = "OpenCode".into();
                    form.executable = "opencode".into();
                }
                ui.label("Name");
                ui.text_edit_singleline(&mut form.name);
                ui.label("Executable");
                ui.text_edit_singleline(&mut form.executable);
                ui.label("Arguments · one exact argument per line");
                ui.add(egui::TextEdit::multiline(&mut form.args).desired_rows(3));
                ui.label("Launch directory (optional)");
                ui.text_edit_singleline(&mut form.cwd);
                ui.label(if form.preset {
                    "The OpenCode preset appends acp."
                } else {
                    "Generic ACP executes exactly the configured arguments."
                });
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
            let profile = json!({"id":id,"revision":form.revision,"name":form.name,"kind":if form.preset{"open_code_acp"}else{"acp"},"executable":form.executable,"args":form.args.lines().map(str::to_owned).collect::<Vec<_>>(),"workingDirectory":if form.cwd.trim().is_empty(){None}else{Some(&form.cwd)}});
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
