use crate::DaemonState;
use crate::events::DaemonEvent;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::sync::broadcast;
use tokio::sync::mpsc;

fn serialize_value<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|err| err.to_string())
}

async fn serialize_result<T, Fut>(future: Fut) -> Result<Value, String>
where
    T: Serialize,
    Fut: Future<Output = Result<T, String>>,
{
    future.await.and_then(serialize_value)
}

async fn serialize_ok<Fut>(future: Fut) -> Result<Value, String>
where
    Fut: Future<Output = Result<(), String>>,
{
    future.await.map(|_| json!({ "ok": true }))
}

macro_rules! parse_request_or_err {
    ($params:expr, $ty:ty) => {
        match serde_json::from_value::<$ty>($params.clone()) {
            Ok(value) => value,
            Err(err) => return Some(Err(err.to_string())),
        }
    };
}

pub(super) fn build_error_response(id: Option<u64>, message: &str) -> Option<String> {
    let id = id?;
    Some(
        serde_json::to_string(&json!({
            "id": id,
            "error": { "message": message }
        }))
        .unwrap_or_else(|_| {
            "{\"id\":0,\"error\":{\"message\":\"serialization failed\"}}".to_string()
        }),
    )
}

pub(super) fn build_result_response(id: Option<u64>, result: Value) -> Option<String> {
    let id = id?;
    Some(
        serde_json::to_string(&json!({ "id": id, "result": result })).unwrap_or_else(|_| {
            "{\"id\":0,\"error\":{\"message\":\"serialization failed\"}}".to_string()
        }),
    )
}

fn build_event_notification(event: DaemonEvent) -> Option<String> {
    let payload = match event {
        DaemonEvent::Runtime(payload) => json!({"method":"runtime-event","params":payload}),
        DaemonEvent::AppServer(payload) => json!({
            "method": "app-server-event",
            "params": payload,
        }),
        DaemonEvent::TerminalOutput(payload) => json!({
            "method": "terminal-output",
            "params": payload,
        }),
        DaemonEvent::TerminalExit(payload) => json!({
            "method": "terminal-exit",
            "params": payload,
        }),
        DaemonEvent::ResyncRequired { stream } => json!({
            "method": "resync-required",
            "params": {"stream": stream},
        }),
    };
    serde_json::to_string(&payload).ok()
}

pub(super) fn parse_auth_token(params: &Value) -> Option<String> {
    match params {
        Value::String(value) => Some(value.clone()),
        Value::Object(map) => map
            .get("token")
            .and_then(|value| value.as_str())
            .map(|v| v.to_string()),
        _ => None,
    }
}

pub(super) fn parse_string(value: &Value, key: &str) -> Result<String, String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .ok_or_else(|| format!("missing or invalid `{key}`")),
        _ => Err(format!("missing `{key}`")),
    }
}

pub(super) fn parse_optional_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(|value| value.as_str())
            .map(|v| v.to_string()),
        _ => None,
    }
}

pub(super) fn parse_optional_nullable_string(value: &Value, key: &str) -> Option<Option<String>> {
    match value {
        Value::Object(map) => match map.get(key) {
            Some(Value::Null) => Some(None),
            Some(Value::String(value)) => Some(Some(value.to_string())),
            Some(_) => None,
            None => None,
        },
        _ => None,
    }
}

pub(super) fn parse_optional_u32(value: &Value, key: &str) -> Option<u32> {
    match value {
        Value::Object(map) => map.get(key).and_then(|value| value.as_u64()).and_then(|v| {
            if v > u32::MAX as u64 {
                None
            } else {
                Some(v as u32)
            }
        }),
        _ => None,
    }
}

pub(super) fn parse_optional_bool(value: &Value, key: &str) -> Option<bool> {
    match value {
        Value::Object(map) => map.get(key).and_then(|value| value.as_bool()),
        _ => None,
    }
}

pub(super) fn parse_optional_string_array(value: &Value, key: &str) -> Option<Vec<String>> {
    match value {
        Value::Object(map) => map
            .get(key)
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(|value| value.to_string()))
                    .collect::<Vec<_>>()
            }),
        _ => None,
    }
}

pub(super) fn parse_string_array(value: &Value, key: &str) -> Result<Vec<String>, String> {
    parse_optional_string_array(value, key).ok_or_else(|| format!("missing `{key}`"))
}

pub(super) fn parse_optional_value(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Object(map) => map.get(key).cloned(),
        _ => None,
    }
}

pub(super) async fn handle_rpc_request(
    state: &DaemonState,
    method: &str,
    params: Value,
    client_version: String,
) -> Result<Value, String> {
    dispatcher::dispatch_rpc_request(state, method, &params, &client_version).await
}

pub(super) async fn forward_events(
    mut rx: broadcast::Receiver<DaemonEvent>,
    out_tx_events: mpsc::UnboundedSender<String>,
) {
    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let Some(payload) = build_event_notification(DaemonEvent::ResyncRequired {
                    stream: "all".into(),
                }) else {
                    continue;
                };
                if out_tx_events.send(payload).is_err() {
                    break;
                }
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };

        let Some(payload) = build_event_notification(event) else {
            continue;
        };

        if out_tx_events.send(payload).is_err() {
            break;
        }
    }
}

pub(super) fn spawn_rpc_response_task(
    state: Arc<DaemonState>,
    out_tx: mpsc::UnboundedSender<String>,
    id: Option<u64>,
    method: String,
    params: Value,
    client_version: String,
    request_limiter: Arc<Semaphore>,
) {
    tokio::spawn(async move {
        let Ok(_permit) = request_limiter.acquire_owned().await else {
            return;
        };
        let result = handle_rpc_request(&state, &method, params, client_version).await;
        let response = match result {
            Ok(result) => build_result_response(id, result),
            Err(message) => build_error_response(id, &message),
        };
        if let Some(response) = response {
            let _ = out_tx.send(response);
        }
    });
}

#[path = "rpc/account.rs"]
mod account;
#[path = "rpc/agents.rs"]
mod agents;
#[path = "rpc/approvals.rs"]
mod approvals;
#[path = "rpc/catalog.rs"]
mod catalog;
#[path = "rpc/daemon.rs"]
mod daemon;
#[path = "rpc/dispatcher.rs"]
mod dispatcher;
#[path = "rpc/files.rs"]
mod files;
#[path = "rpc/git.rs"]
mod git;
#[path = "rpc/metadata.rs"]
mod metadata;
#[path = "rpc/prompts.rs"]
mod prompts;
#[path = "rpc/settings.rs"]
mod settings;
#[path = "rpc/threads.rs"]
mod threads;
#[path = "rpc/turns.rs"]
mod turns;
#[path = "rpc/usage.rs"]
mod usage;
#[path = "rpc/workspace.rs"]
mod workspace;

#[cfg(test)]
mod event_notification_tests {
    use super::build_event_notification;
    use crate::events::DaemonEvent;
    use serde_json::json;

    #[test]
    fn resync_notification_uses_the_shared_wire_method() {
        let line = build_event_notification(DaemonEvent::ResyncRequired {
            stream: "all".into(),
        })
        .expect("resync event should serialize");
        let message: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            message,
            json!({"method":"resync-required","params":{"stream":"all"}})
        );
    }
}
