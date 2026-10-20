//! Local managed-daemon and Tailscale controls for desktop preferences.
//!
//! This panel emits host actions only after an explicit user gesture. In
//! particular, opening preferences never starts or stops the daemon.

use crate::{
    desktop::platform::{HostAction, HostResult, HostResultValue},
    types::AppSettings,
};
use egui::{Color32, Ui};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DaemonPanelPurpose {
    TailscaleStatus,
    CommandPreview,
    DaemonStatus,
    Start,
    Stop,
}

#[derive(Clone, Debug)]
pub(crate) struct DaemonPanelEffect {
    pub(crate) purpose: DaemonPanelPurpose,
    pub(crate) action: HostAction,
}

#[derive(Default)]
pub(crate) struct DaemonPanel {
    pending: HashMap<Uuid, DaemonPanelPurpose>,
    daemon: Option<Value>,
    tailscale: Option<Value>,
    preview: Option<Value>,
    error: Option<String>,
    status: Option<String>,
    confirm_stop: bool,
}

impl DaemonPanel {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn show(&mut self, ui: &mut Ui, settings: &AppSettings) -> Vec<DaemonPanelEffect> {
        let mut effects = Vec::new();
        ui.heading("Managed daemon on this computer");
        ui.weak("These controls manage the daemon on this desktop host, regardless of the selected conversation backend.");

        if let Some(error) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }

        let busy = !self.pending.is_empty();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh local status"))
                .clicked()
            {
                self.error = None;
                self.status = None;
                effects.extend(Self::refresh_effects(settings));
            }
            if busy {
                ui.spinner();
                ui.label("Working…");
            }
        });

        ui.separator();
        ui.label("Daemon");
        match self.daemon.as_ref() {
            Some(daemon) => {
                let state = string_field(daemon, "state").unwrap_or("unknown");
                let color = match state {
                    "running" => Color32::LIGHT_GREEN,
                    "error" => Color32::LIGHT_RED,
                    _ => ui.visuals().text_color(),
                };
                ui.colored_label(color, format!("State: {}", state));
                if let Some(pid) = daemon.get("pid").and_then(Value::as_u64) {
                    ui.label(format!("Process ID: {pid}"));
                }
                if let Some(address) = string_field(daemon, "listenAddr") {
                    ui.label(format!("Listening address: {address}"));
                }
                if let Some(error) = nonempty_string_field(daemon, "lastError") {
                    ui.colored_label(Color32::LIGHT_RED, format!("Daemon error: {error}"));
                }
                if state == "running" {
                    if !self.confirm_stop {
                        if ui
                            .add_enabled(!busy, egui::Button::new("Stop daemon…"))
                            .clicked()
                        {
                            self.confirm_stop = true;
                        }
                    } else {
                        ui.colored_label(
                            Color32::YELLOW,
                            "Stop the local daemon? Remote clients may disconnect.",
                        );
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(!busy, egui::Button::new("Confirm stop"))
                                .clicked()
                            {
                                self.confirm_stop = false;
                                effects.push(DaemonPanelEffect {
                                    purpose: DaemonPanelPurpose::Stop,
                                    action: HostAction::ManagedDaemonStop {
                                        settings: settings.clone(),
                                    },
                                });
                            }
                            if ui.button("Cancel").clicked() {
                                self.confirm_stop = false;
                            }
                        });
                    }
                } else if state == "stopped" || state == "error" {
                    let token_available = self
                        .preview
                        .as_ref()
                        .and_then(|preview| preview.get("tokenConfigured"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    if ui
                        .add_enabled(!busy && token_available, egui::Button::new("Start daemon"))
                        .clicked()
                    {
                        self.error = None;
                        effects.push(DaemonPanelEffect {
                            purpose: DaemonPanelPurpose::Start,
                            action: HostAction::ManagedDaemonStart {
                                settings: settings.clone(),
                            },
                        });
                    }
                    if !token_available {
                        ui.weak("Configure a remote backend token before starting the daemon.");
                    }
                }
            }
            None => {
                ui.weak("Status has not been loaded. Select Refresh local status.");
            }
        }

        ui.separator();
        ui.label("Tailscale");
        match self.tailscale.as_ref() {
            Some(tailscale) => {
                let installed = bool_field(tailscale, "installed");
                let running = bool_field(tailscale, "running");
                ui.label(format!(
                    "{} · {}",
                    if installed {
                        "Installed"
                    } else {
                        "Not installed"
                    },
                    if running {
                        "Connected"
                    } else {
                        "Not connected"
                    }
                ));
                if let Some(host) = nonempty_string_field(tailscale, "suggestedRemoteHost")
                    .or_else(|| nonempty_string_field(tailscale, "dnsName"))
                {
                    ui.label(format!("Address: {host}"));
                }
                if let Some(message) = nonempty_string_field(tailscale, "message") {
                    ui.weak(message);
                }
            }
            None => {
                ui.weak("Tailscale status has not been loaded.");
            }
        }

        ui.separator();
        ui.label("Redacted launch command preview");
        if let Some(preview) = self.preview.as_ref() {
            if let Some(path) = nonempty_string_field(preview, "daemonPath") {
                ui.monospace(path);
            }
            if let Some(args) = preview.get("args").and_then(Value::as_array) {
                ui.monospace(redacted_args(args));
            }
            let token_configured = bool_field(preview, "tokenConfigured");
            ui.weak(if token_configured {
                "Authentication token is configured; its value is hidden."
            } else {
                "No authentication token is configured."
            });
        } else {
            ui.weak("Command preview has not been loaded.");
        }

        effects
    }

    pub(crate) fn track(&mut self, id: Uuid, purpose: DaemonPanelPurpose) {
        self.pending.insert(id, purpose);
    }

    /// Returns true only when this panel owns the correlated host response.
    pub(crate) fn handle_result(&mut self, result: &HostResult) -> bool {
        let Some(purpose) = self.pending.remove(&result.request_id) else {
            return false;
        };
        match &result.result {
            Err(error) => {
                self.error = Some(error.clone());
                self.status = None;
            }
            Ok(value) => match (purpose, value) {
                (DaemonPanelPurpose::TailscaleStatus, HostResultValue::TailscaleStatus(value)) => {
                    self.tailscale = Some(value.clone());
                    self.error = None;
                }
                (
                    DaemonPanelPurpose::CommandPreview,
                    HostResultValue::ManagedDaemonCommandPreview(value),
                ) => {
                    self.preview = Some(value.clone());
                    self.error = None;
                }
                (
                    DaemonPanelPurpose::DaemonStatus
                    | DaemonPanelPurpose::Start
                    | DaemonPanelPurpose::Stop,
                    HostResultValue::ManagedDaemonStatus(value),
                ) => {
                    self.daemon = Some(value.clone());
                    self.status = Some(match purpose {
                        DaemonPanelPurpose::Start => "Local daemon start request completed.".into(),
                        DaemonPanelPurpose::Stop => "Local daemon stopped.".into(),
                        _ => "Local daemon status refreshed.".into(),
                    });
                    self.error = None;
                }
                _ => {
                    self.error =
                        Some("The local host returned an unexpected daemon response.".into());
                    self.status = None;
                }
            },
        }
        true
    }

    fn refresh_effects(settings: &AppSettings) -> Vec<DaemonPanelEffect> {
        vec![
            DaemonPanelEffect {
                purpose: DaemonPanelPurpose::TailscaleStatus,
                action: HostAction::TailscaleStatus,
            },
            DaemonPanelEffect {
                purpose: DaemonPanelPurpose::CommandPreview,
                action: HostAction::ManagedDaemonCommandPreview {
                    settings: settings.clone(),
                },
            },
            DaemonPanelEffect {
                purpose: DaemonPanelPurpose::DaemonStatus,
                action: HostAction::ManagedDaemonStatus {
                    settings: settings.clone(),
                },
            },
        ]
    }
}

fn string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn nonempty_string_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    string_field(value, key)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn redacted_args(args: &[Value]) -> String {
    let mut rendered = Vec::with_capacity(args.len());
    let mut hide_next = false;
    for arg in args {
        let text = arg.as_str().unwrap_or("<non-text argument>");
        if hide_next {
            rendered.push("<redacted>".to_owned());
            hide_next = false;
        } else {
            rendered.push(text.to_owned());
            hide_next = text == "--token" || text == "--auth-token";
        }
    }
    rendered.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_requests_only_local_host_status_and_redacts_token_arguments() {
        let settings: AppSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        let effects = DaemonPanel::refresh_effects(&settings);
        assert_eq!(effects.len(), 3);
        assert!(matches!(effects[0].action, HostAction::TailscaleStatus));
        assert!(matches!(
            effects[1].action,
            HostAction::ManagedDaemonCommandPreview { .. }
        ));
        assert!(matches!(
            effects[2].action,
            HostAction::ManagedDaemonStatus { .. }
        ));
        assert_eq!(
            redacted_args(&[
                Value::from("--listen"),
                Value::from("127.0.0.1:4732"),
                Value::from("--token"),
                Value::from("secret"),
            ]),
            "--listen 127.0.0.1:4732 --token <redacted>"
        );
    }

    #[test]
    fn results_are_correlated_and_start_stop_update_daemon_snapshot() {
        let mut panel = DaemonPanel::new();
        let request_id = Uuid::new_v4();
        panel.track(request_id, DaemonPanelPurpose::Start);
        let unrelated = HostResult {
            request_id: Uuid::new_v4(),
            result: Ok(HostResultValue::Completed),
        };
        assert!(!panel.handle_result(&unrelated));

        let result = HostResult {
            request_id,
            result: Ok(HostResultValue::ManagedDaemonStatus(serde_json::json!({
                "state": "running",
                "pid": 1234,
                "listenAddr": "127.0.0.1:4732"
            }))),
        };
        assert!(panel.handle_result(&result));
        assert_eq!(panel.daemon.as_ref().unwrap()["state"], "running");
        assert!(panel
            .status
            .as_deref()
            .unwrap()
            .contains("start request completed"));
        assert!(!panel.handle_result(&result));
    }
}
