use crate::backend::events::{AppServerEvent, EventSink, TerminalExit, TerminalOutput};
use std::path::PathBuf;
use tokio::sync::broadcast;

#[derive(Clone)]
pub(super) struct DaemonEventSink {
    pub(super) run_store_path: PathBuf,
    pub(super) tx: broadcast::Sender<DaemonEvent>,
}

#[derive(Clone)]
pub(super) enum DaemonEvent {
    Runtime(serde_json::Value),
    AppServer(AppServerEvent),
    ResyncRequired {
        stream: String,
    },
    #[allow(dead_code)]
    TerminalOutput(TerminalOutput),
    #[allow(dead_code)]
    TerminalExit(TerminalExit),
}

impl EventSink for DaemonEventSink {
    fn run_store_path(&self) -> Option<PathBuf> {
        Some(self.run_store_path.clone())
    }
    fn emit_runtime_event(&self, event: serde_json::Value) {
        let _ = self.tx.send(DaemonEvent::Runtime(event));
    }
    fn emit_app_server_event(&self, event: AppServerEvent) {
        let _ = self.tx.send(DaemonEvent::AppServer(event));
    }

    fn emit_terminal_output(&self, event: TerminalOutput) {
        let _ = self.tx.send(DaemonEvent::TerminalOutput(event));
    }

    fn emit_terminal_exit(&self, event: TerminalExit) {
        let _ = self.tx.send(DaemonEvent::TerminalExit(event));
    }
}

use serde_json::json;

pub(super) fn emit_background_thread_hide(
    event_sink: &DaemonEventSink,
    workspace_id: &str,
    thread_id: &str,
) {
    event_sink.emit_app_server_event(AppServerEvent {
        workspace_id: workspace_id.to_string(),
        message: json!({
            "method": "codex/backgroundThread",
            "params": {
                "threadId": thread_id,
                "action": "hide"
            }
        }),
    });
}
