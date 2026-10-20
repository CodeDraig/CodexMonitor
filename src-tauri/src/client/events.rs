use super::contracts::{BackendEvent, BackendMessage, BackendScope, EventKind, EventStream};
use crate::backend::events::{AppServerEvent, EventSink, TerminalExit, TerminalOutput};
use serde_json::{json, Value};
use std::sync::{mpsc, Arc, Mutex};

pub(crate) type Wake = Arc<dyn Fn() + Send + Sync + 'static>;

#[derive(Clone)]
pub(crate) struct EventInbox {
    tx: mpsc::Sender<BackendMessage>,
    scope: Arc<Mutex<BackendScope>>,
    wake: Wake,
}

impl EventInbox {
    pub(crate) fn new(scope: BackendScope, wake: Wake) -> (Self, mpsc::Receiver<BackendMessage>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                tx,
                scope: Arc::new(Mutex::new(scope)),
                wake,
            },
            rx,
        )
    }

    pub(crate) fn scope(&self) -> BackendScope {
        self.scope.lock().expect("scope lock poisoned").clone()
    }

    pub(crate) fn set_scope(&self, scope: BackendScope) {
        *self.scope.lock().expect("scope lock poisoned") = scope;
    }

    pub(crate) fn push_event(&self, kind: EventKind, payload: Value) {
        self.push_event_for(self.scope(), kind, payload);
    }

    pub(crate) fn push_event_for(
        &self,
        origin_scope: BackendScope,
        kind: EventKind,
        payload: Value,
    ) {
        let current = self.scope();
        if origin_scope != current {
            return;
        }
        let scope = origin_scope;
        let stream = match kind {
            EventKind::Runtime => EventStream::Runtime,
            EventKind::AppServer => EventStream::AppServer,
            _ => EventStream::Terminal,
        };
        let sequence = if kind == EventKind::Runtime {
            payload.get("sequence").and_then(Value::as_u64)
        } else {
            None
        };
        let _ = self.tx.send(BackendMessage::Event {
            scope,
            event: BackendEvent {
                kind,
                payload,
                sequence,
            },
        });
        (self.wake)();
    }

    pub(crate) fn push_resync(&self, stream: EventStream) {
        self.push_resync_for(self.scope(), stream);
    }

    pub(crate) fn push_resync_for(&self, origin_scope: BackendScope, stream: EventStream) {
        if origin_scope != self.scope() {
            return;
        }
        let _ = self.tx.send(BackendMessage::ResyncRequired {
            scope: origin_scope,
            stream,
        });
        (self.wake)();
    }

    pub(crate) fn reply(
        &self,
        correlation_id: uuid::Uuid,
        scope: BackendScope,
        result: Result<Value, String>,
    ) {
        if scope != self.scope() {
            return;
        }
        let result = result.map_err(crate::client::contracts::BackendError::from);
        let _ = self.tx.send(BackendMessage::Reply {
            correlation_id,
            scope,
            result,
        });
        (self.wake)();
    }
}

#[derive(Clone)]
pub(crate) struct ChannelEventSink {
    inbox: EventInbox,
    origin_scope: BackendScope,
    run_store_path: Option<std::path::PathBuf>,
}

impl ChannelEventSink {
    pub(crate) fn new(inbox: EventInbox, run_store_path: Option<std::path::PathBuf>) -> Self {
        let origin_scope = inbox.scope();
        Self {
            inbox,
            origin_scope,
            run_store_path,
        }
    }
}

impl EventSink for ChannelEventSink {
    fn run_store_path(&self) -> Option<std::path::PathBuf> {
        self.run_store_path.clone()
    }
    fn emit_runtime_event(&self, event: Value) {
        self.inbox
            .push_event_for(self.origin_scope.clone(), EventKind::Runtime, event);
    }
    fn emit_app_server_event(&self, event: AppServerEvent) {
        self.inbox.push_event_for(
            self.origin_scope.clone(),
            EventKind::AppServer,
            json!({"workspaceId":event.workspace_id,"message":event.message}),
        );
    }
    fn emit_terminal_output(&self, event: TerminalOutput) {
        self.inbox.push_event_for(self.origin_scope.clone(), EventKind::TerminalOutput, json!({"workspaceId":event.workspace_id,"terminalId":event.terminal_id,"data":event.data}));
    }
    fn emit_terminal_exit(&self, event: TerminalExit) {
        self.inbox.push_event_for(
            self.origin_scope.clone(),
            EventKind::TerminalExit,
            json!({"workspaceId":event.workspace_id,"terminalId":event.terminal_id}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scope(epoch: u64) -> BackendScope {
        BackendScope {
            endpoint_id: "daemon".into(),
            connection_epoch: epoch,
            store_id: Some("s".into()),
        }
    }

    #[test]
    fn global_runtime_sequence_gaps_and_reordering_do_not_drop_session_events() {
        let (inbox, rx) = EventInbox::new(scope(1), Arc::new(|| {}));
        for (session_id, sequence) in [("a", 4), ("b", 7), ("a", 2), ("c", 19)] {
            inbox.push_event(
                EventKind::Runtime,
                json!({"storeId":"s","sessionId":session_id,"sequence":sequence}),
            );
        }
        let sequences: Vec<_> = (0..4)
            .map(|_| match rx.recv().unwrap() {
                BackendMessage::Event { event, .. } => event.sequence.unwrap(),
                other => panic!("unexpected control message: {other:?}"),
            })
            .collect();
        assert_eq!(sequences, [4, 7, 2, 19]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn explicit_transport_lag_marker_requests_resync() {
        let (inbox, rx) = EventInbox::new(scope(1), Arc::new(|| {}));
        inbox.push_event(
            EventKind::Runtime,
            json!({"storeId":"s","sessionId":"a","sequence":4}),
        );
        inbox.push_resync(EventStream::Runtime);
        inbox.push_event(
            EventKind::Runtime,
            json!({"storeId":"s","sessionId":"a","sequence":9}),
        );
        assert!(matches!(
            rx.recv().unwrap(),
            BackendMessage::Event {
                event: BackendEvent {
                    sequence: Some(4),
                    ..
                },
                ..
            }
        ));
        assert!(matches!(
            rx.recv().unwrap(),
            BackendMessage::ResyncRequired {
                stream: EventStream::Runtime,
                ..
            }
        ));
        assert!(matches!(
            rx.recv().unwrap(),
            BackendMessage::Event {
                event: BackendEvent {
                    sequence: Some(9),
                    ..
                },
                ..
            }
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn events_from_prior_connection_epoch_are_discarded() {
        let (inbox, rx) = EventInbox::new(scope(2), Arc::new(|| {}));
        inbox.push_event_for(scope(1), EventKind::AppServer, json!({"message":{}}));
        inbox.push_event_for(scope(2), EventKind::AppServer, json!({"message":{}}));
        let BackendMessage::Event {
            scope: delivered, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected event")
        };
        assert_eq!(delivered.connection_epoch, 2);
        assert!(rx.try_recv().is_err());
    }
}
