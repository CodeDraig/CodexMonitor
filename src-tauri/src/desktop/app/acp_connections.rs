//! Native ACP connection and authentication panel. Credentials are entered into
//! the agent's transient login terminal and are never added to the UI snapshot.
use super::*;
use base64::Engine as _;
use std::collections::HashMap;

#[derive(Clone, Debug)]
enum PendingConnectionRequest {
    Prepare { scope: crate::client::BackendScope },
    Status,
    Authenticate,
    Operation,
    Cancel,
    Logout,
    Callback(Value),
    LoginRead,
    LoginWrite { sequence: u64, submitted: String },
    LoginResize,
    LoginClose,
}

fn create_with_connection_label(auth_state: Option<&str>) -> Option<&'static str> {
    match auth_state {
        Some("authenticated") => Some("Create conversation with this authenticated connection"),
        Some("ready") => Some("Create conversation with this connection"),
        Some("unknown") => Some("Create conversation using existing sign-in"),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct AcpConnections {
    pub open: bool,
    profile_id: String,
    cwd: String,
    workspace_id: Option<String>,
    existing_session_id: Option<String>,
    connection_scope: Option<crate::client::BackendScope>,
    retired_prepares: HashMap<Uuid, crate::client::BackendScope>,
    prepare_attempted: bool,
    connection_id: Option<String>,
    generation: Option<u64>,
    prepared_ref: Option<(String, u64)>,
    consumed: bool,
    handoff_requested: bool,
    operation_id: Option<String>,
    login_terminal_operation_id: Option<String>,
    operation_state: Option<String>,
    status: Value,
    error: Option<String>,
    pending: HashMap<Uuid, PendingConnectionRequest>,
    closing_connections: std::collections::HashSet<Uuid>,
    next_status_poll_ms: u64,
    next_operation_poll_ms: u64,
    next_login_poll_ms: u64,
    login_cursor: Option<Value>,
    login_output: String,
    login_input: String,
    uncertain_login_input: Option<String>,
    login_cols: Option<u16>,
    login_rows: Option<u16>,
    input_sequence: u64,
    opened_urls: Vec<String>,
    terminal_login: bool,
}

impl AcpConnections {
    pub fn open_profile(
        &mut self,
        controller: &mut Controller,
        profile_id: String,
        cwd: Option<String>,
        workspace_id: Option<String>,
    ) {
        let next_cwd = cwd.unwrap_or_default();
        if self.profile_id != profile_id
            || self.cwd != next_cwd
            || self.workspace_id != workspace_id
            || self.existing_session_id.is_some()
        {
            let retired = self.cleanup(controller);
            let closing_connections = std::mem::take(&mut self.closing_connections);
            *self = Self::default();
            self.retired_prepares = retired;
            self.closing_connections = closing_connections;
            self.profile_id = profile_id;
        }
        self.cwd = next_cwd;
        self.workspace_id = workspace_id;
        self.existing_session_id = None;
        self.open = true;
    }

    pub fn open_existing_session(
        &mut self,
        controller: &mut Controller,
        profile_id: String,
        session_id: String,
        workspace_id: String,
        cwd: String,
    ) {
        if self.profile_id != profile_id
            || self.existing_session_id.as_deref() != Some(&session_id)
            || self.workspace_id.as_deref() != Some(&workspace_id)
        {
            let retired = self.cleanup(controller);
            let closing_connections = std::mem::take(&mut self.closing_connections);
            *self = Self::default();
            self.retired_prepares = retired;
            self.closing_connections = closing_connections;
            self.profile_id = profile_id;
        }
        self.cwd = cwd;
        self.workspace_id = Some(workspace_id);
        self.existing_session_id = Some(session_id);
        self.connection_scope = Some(controller.scope().clone());
        self.prepare_attempted = true;
        self.consumed = true;
        self.open = true;
    }

    fn enqueue(
        &mut self,
        controller: &mut Controller,
        method: RuntimeMethod,
        params: Value,
        mutation: bool,
        pending: PendingConnectionRequest,
    ) {
        let request = BackendRequest::Runtime(if mutation {
            RuntimeRequest::mutation(method, params, Some(Uuid::new_v4()))
        } else {
            RuntimeRequest::read(method, params)
        });
        match controller.request(request) {
            Ok(id) => {
                self.pending.insert(id, pending);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Could not queue ACP request: {error:?}")),
        }
    }

    fn prepare(&mut self, controller: &mut Controller) {
        self.prepare_attempted = true;
        let scope = controller.scope().clone();
        self.connection_scope = Some(scope.clone());
        let request_id = Uuid::new_v4();
        self.enqueue(
            controller,
            RuntimeMethod::PrepareConnection,
            json!({
                "profileId": self.profile_id,
                "expectedProfileRevision": self.profile_revision(controller),
                "cwd": if self.cwd.trim().is_empty() { None::<String> } else { Some(self.cwd.clone()) },
                "role": "conversation",
                "requestId": request_id,
            }),
            true,
            PendingConnectionRequest::Prepare { scope },
        );
    }

    fn close_connection(&mut self, controller: &mut Controller, connection_id: String, generation: u64) {
        let request = BackendRequest::Runtime(RuntimeRequest::mutation(
            RuntimeMethod::CloseConnection,
            json!({"connectionId":connection_id,"generation":generation}),
            None,
        ));
        match controller.request(request) {
            Ok(id) => { self.closing_connections.insert(id); }
            Err(error) => {
                let message = format!("Could not queue ACP connection cleanup: {error:?}");
                eprintln!("{message}");
                self.error = Some(message);
            }
        }
    }

    fn cleanup(
        &mut self,
        controller: &mut Controller,
    ) -> HashMap<Uuid, crate::client::BackendScope> {
        let mut retired = std::mem::take(&mut self.retired_prepares);
        for (id, pending) in self.pending.drain() {
            if let PendingConnectionRequest::Prepare { scope } = pending {
                retired.insert(id, scope);
            }
        }
        if !self.consumed && self.connection_scope.as_ref() == Some(controller.scope()) {
            if let (Some(operation_id), Some(generation)) =
                (self.login_terminal_operation_id.take(), self.generation)
            {
                self.enqueue(
                    controller,
                    RuntimeMethod::CloseLoginTerminal,
                    json!({"operationId":operation_id,"generation":generation}),
                    true,
                    PendingConnectionRequest::LoginClose,
                );
            }
            if let (Some(connection_id), Some(generation)) =
                (self.connection_id.take(), self.generation.take())
            {
                self.close_connection(controller, connection_id, generation);
            }
        }
        self.connection_id = None;
        self.generation = None;
        self.prepared_ref = None;
        self.existing_session_id = None;
        self.consumed = false;
        self.handoff_requested = false;
        self.operation_id = None;
        self.login_terminal_operation_id = None;
        self.connection_scope = None;
        retired
    }

    fn profile_revision(&self, controller: &Controller) -> u64 {
        controller
            .state
            .profiles
            .iter()
            .find(|profile| profile["id"].as_str() == Some(&self.profile_id))
            .and_then(|profile| profile["revision"].as_u64())
            .unwrap_or(0)
    }

    fn status(&mut self, controller: &mut Controller) {
        let params = if let Some(session_id) = self.existing_session_id.as_ref() {
            json!({"sessionId":session_id})
        } else if let Some(connection_id) = self.connection_id.as_ref() {
            json!({"connectionId":connection_id})
        } else { return; };
        self.enqueue(
            controller,
            RuntimeMethod::ConnectionStatus,
            params,
            false,
            PendingConnectionRequest::Status,
        );
    }

    pub fn apply_result(
        &mut self,
        id: Uuid,
        result: Result<Value, crate::client::contracts::BackendError>,
        controller: &mut Controller,
    ) -> bool {
        if self.closing_connections.remove(&id) {
            match result {
                Ok(value) if value["state"].as_str() == Some("closed") => {}
                Ok(value) => {
                    let message = format!("ACP connection cleanup was not acknowledged: {}", value);
                    eprintln!("{message}");
                    self.error = Some(message);
                }
                Err(error) => {
                    let message = format!("ACP connection cleanup failed: {}", error.message);
                    eprintln!("{message}");
                    self.error = Some(message);
                }
            }
            return true;
        }
        let Some(kind) = self.pending.remove(&id) else {
            let Some(scope) = self.retired_prepares.remove(&id) else {
                return false;
            };
            if &scope == controller.scope() {
                if let Ok(value) = result {
                    if let (Some(connection_id), Some(generation)) =
                        (value["connectionId"].as_str(), value["generation"].as_u64())
                    {
                        self.close_connection(controller, connection_id.to_owned(), generation);
                    }
                }
            }
            return true;
        };
        match result {
            Err(error) => {
                if let PendingConnectionRequest::LoginWrite { submitted, .. } = kind {
                    self.uncertain_login_input = Some(submitted);
                    self.error = Some(error.message);
                } else {
                    self.error = Some(error.message);
                }
            }
            Ok(value) => match kind {
                PendingConnectionRequest::Prepare { .. } => {
                    self.connection_id = value["connectionId"].as_str().map(str::to_owned);
                    self.operation_id = value["operationId"].as_str().map(str::to_owned);
                    self.generation = value["generation"].as_u64();
                    self.operation_state = value["state"].as_str().map(str::to_owned);
                    if self.connection_id.is_none() {
                        self.error = Some("The backend returned no ACP connection ID.".into());
                    } else if let (Some(connection_id), Some(generation)) =
                        (self.connection_id.clone(), self.generation)
                    {
                        self.prepared_ref = Some((connection_id, generation));
                    }
                }
                PendingConnectionRequest::Status => {
                    if let Some(connection_id) = value["connectionId"].as_str() {
                        self.connection_id = Some(connection_id.to_owned());
                    }
                    if value["generation"].as_u64().is_some() {
                        self.generation = value["generation"].as_u64();
                    }
                    if let (Some(connection_id), Some(generation)) =
                        (self.connection_id.clone(), self.generation)
                    {
                        self.prepared_ref = Some((connection_id, generation));
                    }
                    if self.handoff_requested && value["authState"].as_str() == Some("required") {
                        self.handoff_requested = false;
                        self.consumed = false;
                        self.error = Some("The agent requires sign-in before it can create this conversation. The prepared connection is still available.".into());
                    }
                    self.status = value;
                }
                PendingConnectionRequest::Authenticate
                | PendingConnectionRequest::Cancel
                | PendingConnectionRequest::Logout => {
                    if let Some(operation_id) = value["operationId"].as_str() {
                        self.operation_id = Some(operation_id.to_owned());
                        if self.terminal_login {
                            self.login_terminal_operation_id = Some(operation_id.to_owned());
                        }
                    }
                    if let Some(connection_id) = value["connectionId"].as_str() {
                        self.connection_id = Some(connection_id.to_owned());
                    }
                    if let Some(generation) = value["generation"].as_u64() {
                        self.generation = Some(generation);
                    }
                    if let (Some(connection_id), Some(generation)) =
                        (self.connection_id.clone(), self.generation)
                    {
                        self.prepared_ref = Some((connection_id, generation));
                    }
                    if let Some(state) = value["state"].as_str() {
                        self.operation_state = Some(state.to_owned());
                    }
                }
                PendingConnectionRequest::Operation => {
                    let state = value["state"].as_str().unwrap_or("");
                    self.operation_state = Some(state.to_owned());
                    if state == "failed" {
                        self.error = value["error"].as_str().map(str::to_owned);
                    }
                    if matches!(state, "succeeded" | "failed" | "cancelled" | "interrupted") {
                        self.operation_id = None;
                    }
                    if let Some(terminal) = value.get("loginTerminal") {
                        self.login_cursor = terminal.get("cursor").cloned();
                        self.terminal_login = true;
                    }
                }
                PendingConnectionRequest::Callback(_) => {}
                PendingConnectionRequest::LoginRead => {
                    if let Some(output) = value["output"].as_str() {
                        self.login_output.push_str(output);
                        if self.login_output.len() > 64 * 1024 {
                            let mut drain = self.login_output.len() - 64 * 1024;
                            while !self.login_output.is_char_boundary(drain) {
                                drain += 1;
                            }
                            self.login_output.drain(..drain);
                        }
                    }
                    self.login_cursor = value
                        .get("nextCursor")
                        .cloned()
                        .filter(|cursor| !cursor.is_null());
                    if value["state"].as_str().is_some_and(|state| {
                        matches!(state, "succeeded" | "failed" | "cancelled" | "closed")
                    }) {
                        self.operation_id = None;
                    }
                }
                PendingConnectionRequest::LoginWrite {
                    sequence,
                    submitted,
                } => {
                    self.input_sequence = self.input_sequence.max(sequence);
                    if self.login_input == submitted {
                        self.login_input.clear();
                    }
                }
                PendingConnectionRequest::LoginResize | PendingConnectionRequest::LoginClose => {}
            },
        }
        true
    }

    pub(super) fn show(&mut self, app: &mut NativeApp, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if self
            .connection_scope
            .as_ref()
            .is_some_and(|scope| scope != app.controller.scope())
        {
            let retired = self.cleanup(&mut app.controller);
            self.retired_prepares = retired;
            self.operation_state = None;
            self.status = Value::Null;
            self.prepare_attempted = true;
            self.login_input.clear();
            self.login_output.clear();
            self.uncertain_login_input = None;
            self.error =
                Some("The selected backend changed. Connect again on the new backend.".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if self.connection_id.is_none()
            && !self.prepare_attempted
            && !self
                .pending
                .values()
                .any(|p| matches!(p, PendingConnectionRequest::Prepare { .. }))
        {
            self.prepare(&mut app.controller);
        }
        if (self.connection_id.is_some() || self.existing_session_id.is_some())
            && now >= self.next_status_poll_ms
            && !self
                .pending
                .values()
                .any(|p| matches!(p, PendingConnectionRequest::Status))
        {
            self.status(&mut app.controller);
            self.next_status_poll_ms = now.saturating_add(1_000);
        }
        if self.operation_id.is_some()
            && now >= self.next_operation_poll_ms
            && !self
                .pending
                .values()
                .any(|p| matches!(p, PendingConnectionRequest::Operation))
        {
            let operation_id = self.operation_id.clone().unwrap_or_default();
            self.enqueue(
                &mut app.controller,
                RuntimeMethod::ConnectionOperation,
                json!({"operationId":operation_id}),
                false,
                PendingConnectionRequest::Operation,
            );
            self.next_operation_poll_ms = now.saturating_add(300);
        }
        if self.operation_id.is_some()
            && self.terminal_login
            && now >= self.next_login_poll_ms
            && !self
                .pending
                .values()
                .any(|p| matches!(p, PendingConnectionRequest::LoginRead))
        {
            let operation_id = self.operation_id.clone().unwrap_or_default();
            self.enqueue(&mut app.controller, RuntimeMethod::ReadLoginTerminal,
                json!({"operationId":operation_id,"generation":self.generation,"cursor":self.login_cursor.as_ref().and_then(Value::as_u64).unwrap_or(0),"limit":4096}),
                false, PendingConnectionRequest::LoginRead);
            self.next_login_poll_ms = now.saturating_add(250);
        }

        let mut open = true;
        let mut disconnect = false;
        egui::Window::new("ACP connection and sign-in")
            .id(egui::Id::new("acp-connections"))
            .open(&mut open).default_size([620.0, 520.0]).show(ctx, |ui| {
                let profile = app.controller.state.profiles.iter().find(|p| p["id"].as_str() == Some(&self.profile_id));
                ui.heading(profile.and_then(|p| p["name"].as_str()).unwrap_or("ACP agent"));
                ui.label(format!("Executing on {}", app.controller.scope().endpoint_id));
                ui.label("Working directory on that backend");
                ui.add_enabled_ui(self.connection_id.is_none(), |ui| ui.text_edit_singleline(&mut self.cwd));
                if self.connection_id.is_none() && self.existing_session_id.is_none() {
                    if ui.button(if self.prepare_attempted { "Retry connection" } else { "Connect" }).clicked() { self.prepare(&mut app.controller); }
                } else if self.connection_id.is_none() {
                    ui.label("Looking up the existing foreground connection…");
                } else {
                    ui.horizontal(|ui| {
                        ui.label(format!("Connection {} · generation {}", self.connection_id.as_deref().unwrap_or(""), self.generation.unwrap_or(0)));
                        if ui.button("Refresh status").clicked() { self.status(&mut app.controller); }
                        if !self.consumed && ui.button("Disconnect").clicked() { disconnect = true; }
                    });
                    ui.label(format!("Authentication: {}", self.status["authState"].as_str().unwrap_or("connecting")));
                    if let Some(info) = self.status.get("agentInfo") { ui.label(format!("Agent: {}", info["name"].as_str().or_else(|| info["title"].as_str()).unwrap_or("ACP agent"))); }
                    if let Some(caps) = self.status.get("capabilities") { ui.collapsing("Negotiated capabilities", |ui| { ui.code(caps.to_string()); }); }
                    let auth_state = self.status["authState"].as_str().unwrap_or("connecting");
                    if !self.consumed {
                    if let Some(label) = create_with_connection_label(Some(auth_state)) {
                        if let (Some(workspace_id), Some(connection_id), Some(generation)) =
                            (self.workspace_id.clone(), self.connection_id.clone(), self.generation)
                        {
                            if ui.button(label).clicked() {
                                self.prepared_ref = Some((connection_id.clone(), generation));
                                self.consumed = true;
                                self.handoff_requested = true;
                                let prepared_connection = self.prepared_ref.clone();
                                app.act(Action::CreateConversation {
                                    workspace_id,
                                    profile_id: Some(self.profile_id.clone()),
                                    prepared_connection,
                                });
                            }
                        } else {
                            ui.label("Connect this agent from a workspace to create a conversation with this connection.");
                        }
                    }
                    } else if self.handoff_requested {
                        ui.label("Conversation creation was requested. The prepared connection remains open for status or authentication recovery.");
                    }
                    let methods = self.status["authMethods"].as_array().cloned().unwrap_or_default();
                    if !methods.is_empty() {
                        ui.label("Choose an authentication method. Authentication will not replay a prompt.");
                        for method in methods {
                            let id = method["id"].as_str().unwrap_or("").to_owned();
                            let label = method["name"].as_str().or_else(|| method["label"].as_str()).unwrap_or(&id).to_owned();
                            if ui.button(format!("Sign in · {label}")).clicked() {
                                self.terminal_login = method["type"].as_str() == Some("terminal") || method["kind"].as_str() == Some("terminal");
                                let request_id = Uuid::new_v4();
                                self.enqueue(&mut app.controller, RuntimeMethod::Authenticate,
                                    json!({"connectionId":self.connection_id,"generation":self.generation,"methodId":id,"requestId":request_id}),
                                    true, PendingConnectionRequest::Authenticate);
                            }
                        }
                    }
                    if self.operation_id.is_some() {
                        ui.horizontal(|ui| {
                            if self.status["authState"].as_str() == Some("authenticating") { ui.spinner(); }
                            let operation_state = self.operation_state.as_deref().unwrap_or("running");
                            ui.label(if operation_state == "awaiting_user" { "Waiting for sign-in input…" } else { "Authentication operation is running…" });
                            if ui.button("Cancel sign-in").clicked() {
                                self.enqueue(&mut app.controller, RuntimeMethod::CancelConnectionOperation,
                                    json!({"operationId":self.operation_id,"generation":self.generation,"requestId":Uuid::new_v4()}),
                                    true, PendingConnectionRequest::Cancel);
                            }
                        });
                        if self.terminal_login || !self.login_output.is_empty() {
                            let available = ui.available_size();
                            let cols = ((available.x / 8.0).floor() as u16).clamp(20, 240);
                            let rows = ((available.y.min(190.0) / 18.0).floor() as u16).clamp(4, 60);
                            if self.login_cols != Some(cols) || self.login_rows != Some(rows) {
                                if let (Some(operation_id), Some(generation)) = (self.operation_id.clone(), self.generation) {
                                    self.enqueue(&mut app.controller, RuntimeMethod::ResizeLoginTerminal,
                                        json!({"operationId":operation_id,"generation":generation,"columns":cols,"rows":rows}),
                                        true, PendingConnectionRequest::LoginResize);
                                    self.login_cols = Some(cols);
                                    self.login_rows = Some(rows);
                                }
                            }
                            egui::ScrollArea::vertical().max_height(170.0).stick_to_bottom(true).show(ui, |ui| { ui.monospace(&self.login_output); });
                            if let Some(input) = &self.uncertain_login_input {
                                ui.colored_label(theme::ERROR, format!("Input delivery is uncertain. It was not resent. Sequence {} remains reserved.", self.input_sequence.saturating_add(1)));
                                ui.label(egui::RichText::new(input).monospace());
                            } else {
                                ui.add(egui::TextEdit::multiline(&mut self.login_input).desired_rows(2).hint_text("Sign-in input; sent only to the agent terminal. Ctrl+Enter sends."));
                                if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter)) && !self.login_input.is_empty() { self.send_login_input(&mut app.controller, self.login_input.clone()); }
                                if ui.button("Send input").clicked() && !self.login_input.is_empty() { self.send_login_input(&mut app.controller, self.login_input.clone()); }
                            }
                        }
                    }
                    let callbacks = self.status["pendingClientRequests"].as_array().cloned().unwrap_or_default();
                    for callback in callbacks {
                        let request_id = callback["clientRequestId"].clone();
                        let method = callback["method"].as_str().unwrap_or("client request");
                        ui.separator(); ui.strong(format!("Agent request: {method}"));
                        let url = callback["url"].as_str().or_else(|| callback.pointer("/params/url").and_then(Value::as_str)).unwrap_or("");
                        if !url.is_empty() {
                            ui.label(url);
                            if ui.button("Open sign-in link").clicked() && !self.opened_urls.contains(&url.to_owned()) {
                                self.opened_urls.push(url.to_owned());
                                app.host(HostAction::OpenUrl { url: url.to_owned() }, None);
                            }
                            if ui.button("Confirm link consent").clicked() { self.respond_callback(&mut app.controller, request_id.clone(), "accept"); }
                        } else {
                            ui.label(callback["message"].as_str().or_else(|| callback.pointer("/params/message").and_then(Value::as_str)).unwrap_or("The agent requests a client callback."));
                            if let Some(command) = callback["command"].as_str() { ui.code(command); }
                            if let Some(args) = callback["args"].as_array() { ui.code(args.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")); }
                            ui.horizontal(|ui| {
                                if ui.button("Allow once").clicked() { self.respond_callback(&mut app.controller, request_id.clone(), "allow_once"); }
                                if ui.button("Deny").clicked() { self.respond_callback(&mut app.controller, request_id.clone(), "deny"); }
                            });
                        }
                        if ui.button("Cancel request").clicked() { self.respond_callback(&mut app.controller, request_id, "cancel"); }
                    }
                    if self.status["authState"].as_str() == Some("authenticated") && self.status["capabilities"]["logout"].as_bool() == Some(true) && ui.button("Sign out").clicked() {
                        self.enqueue(&mut app.controller, RuntimeMethod::Logout,
                            json!({"connectionId":self.connection_id,"generation":self.generation,"requestId":Uuid::new_v4()}),
                            true, PendingConnectionRequest::Logout);
                    }
                }
                if let Some(error) = &self.error { ui.colored_label(theme::ERROR, error); }
            });
        if disconnect {
            let retired = self.cleanup(&mut app.controller);
            self.retired_prepares = retired;
            self.prepare_attempted = true;
            self.status = Value::Null;
            self.error = Some("Disconnected. The connection can be prepared again.".into());
        }
        if !open {
            let retired = self.cleanup(&mut app.controller);
            self.retired_prepares = retired;
            self.open = false;
        }
    }

    fn send_login_input(&mut self, controller: &mut Controller, input: String) {
        let (Some(operation_id), Some(generation)) = (self.operation_id.clone(), self.generation)
        else {
            return;
        };
        let sequence = self.input_sequence.saturating_add(1);
        let mut data = input.into_bytes();
        data.push(b'\n');
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        self.enqueue(controller, RuntimeMethod::WriteLoginTerminal,
            json!({"operationId":operation_id,"generation":generation,"inputSequence":sequence,"data":encoded}),
            true, PendingConnectionRequest::LoginWrite { sequence, submitted: self.login_input.clone() });
    }

    fn respond_callback(
        &mut self,
        controller: &mut Controller,
        client_request_id: Value,
        action: &str,
    ) {
        let (Some(connection_id), Some(generation)) = (self.connection_id.clone(), self.generation)
        else {
            return;
        };
        self.enqueue(controller, RuntimeMethod::RespondClientRequest,
            json!({"connectionId":connection_id,"generation":generation,"clientRequestId":client_request_id,"decision":{"action":action}}),
            true, PendingConnectionRequest::Callback(client_request_id));
    }
}

impl NativeApp {
    pub(super) fn request_acp_terminal(&mut self, session_id: &str, terminal_id: &str) {
        let request = BackendRequest::Runtime(RuntimeRequest::read(
            RuntimeMethod::ReadAcpTerminal,
            json!({
                "sessionId": session_id,
                "terminalId": terminal_id,
                "limit": 16_384,
            }),
        ));
        match self.controller.request(request) {
            Ok(id) => {
                self.acp_terminal_pending.insert(id, terminal_id.to_owned());
            }
            Err(error) => self
                .acp_terminal_output
                .push_str(&format!("\n[Could not read terminal: {error:?}]")),
        }
    }

    pub(super) fn show_acp_terminal(&mut self, ctx: &egui::Context) {
        let Some((session_id, terminal_id)) = self.acp_terminal_open.clone() else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if !self.acp_terminal_finished
            && now >= self.acp_terminal_next_poll_ms
            && !self
                .acp_terminal_pending
                .values()
                .any(|id| id == &terminal_id)
        {
            self.request_acp_terminal(&session_id, &terminal_id);
            self.acp_terminal_next_poll_ms = now.saturating_add(1_000);
        }
        let mut open = true;
        egui::Window::new(format!("ACP command terminal · {}", terminal_id))
            .id(egui::Id::new(("acp-command-terminal", &session_id, &terminal_id)))
            .open(&mut open).default_size([760.0, 420.0]).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Output is read from the executing backend and remains available after command release.");
                    if ui.button("Read latest output").clicked() { self.request_acp_terminal(&session_id, &terminal_id); }
                });
                if self.acp_terminal_truncated { ui.label("Showing the backend's bounded output tail."); }
                if let Some(exit) = &self.acp_terminal_exit { ui.label(format!("Command finished: {exit}")); }
                egui::ScrollArea::both().stick_to_bottom(true).show(ui, |ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&self.acp_terminal_output).monospace()).selectable(true));
                });
            });
        if !open {
            self.acp_terminal_open = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::create_with_connection_label;

    #[test]
    fn prepared_connection_offer_distinguishes_auth_states() {
        assert_eq!(
            create_with_connection_label(Some("authenticated")),
            Some("Create conversation with this authenticated connection")
        );
        assert_eq!(
            create_with_connection_label(Some("ready")),
            Some("Create conversation with this connection")
        );
        assert_eq!(
            create_with_connection_label(Some("unknown")),
            Some("Create conversation using existing sign-in")
        );
        assert_eq!(create_with_connection_label(Some("required")), None);
        assert_eq!(create_with_connection_label(Some("connecting")), None);
        assert_eq!(create_with_connection_label(None), None);
    }
}
