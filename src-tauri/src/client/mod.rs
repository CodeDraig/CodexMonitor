pub(crate) mod contracts;
pub(crate) mod events;
pub(crate) mod local;
pub(crate) mod remote;

pub(crate) use contracts::*;
use events::EventInbox;
use std::{
    future::Future,
    pin::Pin,
    sync::{mpsc, Arc, Mutex},
    thread,
};

pub(crate) type ClientFuture<'a> =
    Pin<Box<dyn Future<Output = Result<serde_json::Value, String>> + Send + 'a>>;

pub(crate) trait BackendDriver: Send + Sync + 'static {
    fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnqueueError {
    QueueFull,
    Disconnected,
}

#[derive(Clone)]
pub(crate) struct BackendHandle {
    tx: tokio::sync::mpsc::Sender<RequestEnvelope>,
    rx: Arc<Mutex<mpsc::Receiver<BackendMessage>>>,
    inbox: EventInbox,
}

impl BackendHandle {
    pub(crate) fn spawn<D: BackendDriver>(
        driver: D,
        scope: BackendScope,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (inbox, rx) = EventInbox::new(scope.clone(), Arc::new(wake));
        Self::spawn_parts(driver, scope, inbox, rx)
    }

    pub(crate) fn spawn_parts<D: BackendDriver>(
        driver: D,
        _scope: BackendScope,
        inbox: EventInbox,
        rx: mpsc::Receiver<BackendMessage>,
    ) -> Self {
        let (tx, mut request_rx) = tokio::sync::mpsc::channel::<RequestEnvelope>(128);
        let current_scope = inbox.clone();
        let driver = Arc::new(driver);
        thread::Builder::new()
            .name("codex-monitor-backend-client".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        eprintln!("backend client runtime failed: {err}");
                        return;
                    }
                };
                runtime.block_on(async move {
                    while let Some(envelope) = request_rx.recv().await {
                        if envelope.scope != current_scope.scope() {
                            continue;
                        }
                        let correlation_id = envelope.correlation_id;
                        let request_scope = envelope.scope;
                        let driver = driver.clone();
                        let inbox = current_scope.clone();
                        tokio::spawn(async move {
                            let codex_rpc = matches!(&envelope.request, BackendRequest::Codex(_));
                            let result = driver
                                .execute(envelope.request)
                                .await
                                .and_then(|value| check_codex_rpc_result(codex_rpc, value));
                            inbox.reply(correlation_id, request_scope, result);
                        });
                    }
                });
            })
            .expect("spawn backend client worker");
        Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
            inbox,
        }
    }

    /// Enqueue without blocking the UI thread. Mutations are sent once; an
    /// ambiguous disconnect is reported to the caller for reconciliation.
    pub(crate) fn enqueue(&self, envelope: RequestEnvelope) -> Result<(), EnqueueError> {
        match self.tx.try_send(envelope) {
            Ok(()) => Ok(()),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Err(EnqueueError::QueueFull),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                Err(EnqueueError::Disconnected)
            }
        }
    }

    pub(crate) fn try_recv(&self) -> Result<BackendMessage, mpsc::TryRecvError> {
        self.rx
            .lock()
            .expect("backend inbox lock poisoned")
            .try_recv()
    }

    pub(crate) fn set_scope(&self, scope: BackendScope) {
        self.inbox.set_scope(scope);
    }
}

/// Codex operations retain their app-server response envelope inside the local
/// result or the daemon's outer result. A rejected inner request must reach the
/// controller as a failure rather than acknowledging a turn that never started.
fn check_codex_rpc_result(
    codex_rpc: bool,
    value: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if codex_rpc && (value.get("id").is_some() || value.get("jsonrpc").is_some()) {
        if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .or_else(|| error.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            return Err(format!("codex_rpc_error: {message}"));
        }
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::{BackendScope, RequestEnvelope, WorkspaceRequest};
    use serde_json::json;
    use std::time::Duration;
    use tokio::sync::Notify;

    struct GatedDriver {
        started: mpsc::Sender<()>,
        release: Arc<Notify>,
    }

    impl BackendDriver for GatedDriver {
        fn execute<'a>(&'a self, _request: BackendRequest) -> ClientFuture<'a> {
            let started = self.started.clone();
            let release = self.release.clone();
            Box::pin(async move {
                let _ = started.send(());
                release.notified().await;
                Ok(json!({"ok":true}))
            })
        }
    }

    fn scope(epoch: u64) -> BackendScope {
        BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: epoch,
            store_id: None,
        }
    }

    #[test]
    fn delayed_reply_from_prior_epoch_is_discarded_without_replaying_request() {
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new(Notify::new());
        let handle = BackendHandle::spawn(
            GatedDriver {
                started: started_tx,
                release: release.clone(),
            },
            scope(1),
            || {},
        );
        let request = BackendRequest::Workspaces(WorkspaceRequest::mutation(
            "add_workspace",
            json!({"path":"/tmp"}),
            None,
        ));
        let envelope = RequestEnvelope::new(scope(1), request);
        handle.enqueue(envelope).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        handle.set_scope(scope(2));
        release.notify_one();
        std::thread::sleep(Duration::from_millis(30));
        assert!(matches!(handle.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    struct ReplyDriver(serde_json::Value);

    impl BackendDriver for ReplyDriver {
        fn execute<'a>(&'a self, _request: BackendRequest) -> ClientFuture<'a> {
            let value = self.0.clone();
            Box::pin(async move { Ok(value) })
        }
    }

    #[test]
    fn codex_inner_rpc_rejection_is_a_failed_reply_without_changing_success_shapes() {
        let rejection = json!({"id":8,"error":{"code":-32602,"message":"turn input rejected"}});
        let handle = BackendHandle::spawn(ReplyDriver(rejection), scope(1), || {});
        let request = BackendRequest::Codex(CodexRequest::mutation(
            "send_user_message",
            json!({"workspaceId":"w","threadId":"t","text":"fixture"}),
            None,
        ));
        let envelope = RequestEnvelope::new(scope(1), request);
        let correlation_id = envelope.correlation_id;
        handle.enqueue(envelope).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let message = loop {
            if let Ok(message) = handle.try_recv() {
                break message;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "reply was not delivered"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        match message {
            BackendMessage::Reply {
                correlation_id: id,
                result: Err(error),
                ..
            } => {
                assert_eq!(id, correlation_id);
                assert_eq!(error.message, "codex_rpc_error: turn input rejected");
            }
            other => panic!("expected a rejected turn, got {other:?}"),
        }
        let success = json!({"id":8,"result":{"turn":{"id":"turn-1"}}});
        assert_eq!(
            check_codex_rpc_result(true, success.clone()).unwrap(),
            success
        );
        let domain_status = json!({"error":"doctor could not find binary"});
        assert_eq!(
            check_codex_rpc_result(true, domain_status.clone()).unwrap(),
            domain_status
        );
        let other_domain = json!({"id":"capture","error":"recorded failure"});
        assert_eq!(
            check_codex_rpc_result(false, other_domain.clone()).unwrap(),
            other_domain
        );
    }
}
