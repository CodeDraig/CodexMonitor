use super::{
    capture,
    store::{with_store, Result},
    types::*,
};
use crate::backend::app_server::WorkspaceSession;
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::mpsc;

#[derive(Clone)]
pub(crate) struct Tracking {
    pub path: PathBuf,
    pub context: RunContext,
}

pub(crate) async fn tracked_request(
    session: &Arc<WorkspaceSession>,
    workspace: &str,
    method: &str,
    params: Value,
    tracking: Option<Tracking>,
) -> Result<Value> {
    let Some(tracking) = tracking else {
        return session
            .send_request_for_workspace(workspace, method, params)
            .await;
    };
    let context = tracking.context.clone();
    let input = params.clone();
    let phase = method.to_string();
    let workspace_owned = workspace.to_string();
    let op = with_store(tracking.path.clone(), move |store| {
        let instance = store.validate_context(&context)?;
        let run = store.get_run(&context.run_id)?;
        if instance.workspace_id.as_deref() != Some(&workspace_owned) {
            return Err("Tracked workspace does not match instance".into());
        }
        if phase == "turn/start" && instance.thread_id.as_deref() != input["threadId"].as_str() {
            return Err("Tracked thread does not match instance".into());
        }
        let disposition = store.begin_operation(&context, &phase, input.clone())?;
        if matches!(disposition, OperationDisposition::Execute { .. }) {
            let baseline = if phase == "turn/start" {
                capture::git_snapshot(
                    input["cwd"].as_str().unwrap_or_default(),
                    run.base_oid.as_deref(),
                )
            } else {
                Value::Null
            };
            store.update_instance(&context.instance_id, |i| {
                i.preparation = phase.clone();
                if phase == "turn/start" {
                    i.submission = Some(input);
                    i.baseline = baseline;
                    i.submission_outcome = SubmissionOutcome::Submitting;
                }
                Ok(())
            })?;
        }
        Ok(disposition)
    })
    .await?;
    let operation_id =
        match op {
            OperationDisposition::Execute { id } => id,
            OperationDisposition::Completed { receipt } => return receipt_result(receipt),
            OperationDisposition::Unresolved { .. } => return Err(
                "Run operation has an unresolved receipt; reconcile before starting another run"
                    .into(),
            ),
        };
    if method == "turn/start" {
        let thread = params["threadId"]
            .as_str()
            .ok_or("Missing tracked thread")?;
        register(session, thread, tracking.clone()).await;
    }
    let response = session
        .send_request_for_workspace(workspace, method, params)
        .await;
    let receipt = loop {
        let saved = response.clone();
        let phase = method.to_string();
        let context = tracking.context.clone();
        let operation_id = operation_id.clone();
        let result = with_store(tracking.path.clone(), move |store| {
        let mut receipt=match saved {Ok(value)=>json!({"response":value}),Err(error)=>json!({"transportError":error})};
        let instance=store.finish_operation(&operation_id,receipt.clone(),|i| {
            let response=&receipt["response"];
            let explicit_error=response.get("error").is_some();
            if phase=="thread/start" {
                if let Some(thread)=response.pointer("/result/thread/id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    i.thread_id=Some(thread.into()); i.preparation="thread_created".into();
                } else { i.error=Some(receipt.to_string()); i.preparation=if explicit_error {"failed"} else {"unknown"}.into(); }
            } else {
                if let Some(turn)=response.pointer("/result/turn/id").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                    if i.initial_turn_id.as_deref().is_some_and(|t|t!=turn) { return Err("Initial turn receipt conflicts with recorded event".into()); }
                    i.initial_turn_id=Some(turn.into()); i.submission_outcome=SubmissionOutcome::Accepted;
                    if matches!(i.execution_outcome,ExecutionOutcome::NotStarted|ExecutionOutcome::Unknown) { i.execution_outcome=ExecutionOutcome::Running; }
                    i.preparation="submitted".into();
                } else if i.initial_turn_id.is_none() {
                    i.submission_outcome=if explicit_error {SubmissionOutcome::Rejected} else {SubmissionOutcome::Unknown};
                    i.error=Some(receipt.to_string());
                }
            }
            Ok(())
        })?;
        if phase=="turn/start" {
            receipt["response"]["runReceipt"]=json!({"runId":context.run_id,"instanceId":context.instance_id,"outcome":instance.submission_outcome,"threadId":instance.thread_id,"turnId":instance.initial_turn_id,"message":instance.error});
        }
        Ok(receipt)
        }).await;
        if result
            .as_ref()
            .err()
            .is_some_and(|error| super::store::is_busy(error))
        {
            // Retain the actual response. Never repeat the external request to retry a write.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        break result?;
    };
    receipt_result(receipt)
}

fn receipt_result(receipt: Value) -> Result<Value> {
    if let Some(error) = receipt["transportError"].as_str() {
        Err(error.into())
    } else {
        Ok(receipt["response"].clone())
    }
}

/// Keep the event at the head of its recorder queue until SQLite accepts it.
/// Only typed SQLite BUSY failures are retried; validation and I/O failures surface.
pub(crate) async fn persist_event(tracking: &Tracking, event: &Value) -> Result<bool> {
    let mut warned = false;
    loop {
        let context = tracking.context.clone();
        let event = event.clone();
        let result = with_store(tracking.path.clone(), move |store| {
            store.record_event(&context, &event)
        })
        .await;
        if result
            .as_ref()
            .err()
            .is_some_and(|error| super::store::is_busy(error))
        {
            if !warned {
                eprintln!("Run recording waiting for busy SQLite store; event retained");
                warned = true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        return result;
    }
}

pub(crate) async fn register(session: &Arc<WorkspaceSession>, thread: &str, tracking: Tracking) {
    let mut recorders = session.run_recorders.lock().await;
    if recorders.contains_key(thread) {
        return;
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    recorders.insert(thread.to_string(), tx);
    let weak = Arc::downgrade(session);
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let method = event["method"].as_str().unwrap_or_default().to_string();
            let turn = event
                .pointer("/params/turnId")
                .or_else(|| event.pointer("/params/turn/id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let Some(turn) = turn else { continue };
            let terminal = method == "turn/completed";
            let saved = persist_event(&tracking, &event).await;
            match saved {
                Ok(true) if terminal => {
                    if let Some(session) = weak.upgrade() {
                        if let Err(error) = capture_turn(
                            &session,
                            tracking.clone(),
                            turn,
                            CaptureKind::InitialTurn,
                            None,
                            "Initial turn".into(),
                        )
                        .await
                        {
                            let t = tracking.clone();
                            let _ = with_store(t.path, move |store| {
                                store.update_instance(&t.context.instance_id, |i| {
                                    i.error = Some(error);
                                    Ok(())
                                })
                            })
                            .await;
                        }
                    }
                }
                Err(error) => eprintln!("Run recording failed: {error}"),
                _ => {}
            }
        }
    });
}

pub(crate) async fn capture_turn(
    session: &Arc<WorkspaceSession>,
    tracking: Tracking,
    turn_id: String,
    kind: CaptureKind,
    request_id: Option<String>,
    label: String,
) -> Result<RunCapture> {
    loop {
        let result = capture_turn_once(
            session,
            tracking.clone(),
            turn_id.clone(),
            kind.clone(),
            request_id.clone(),
            label.clone(),
        )
        .await;
        if result
            .as_ref()
            .err()
            .is_some_and(|error| super::store::is_busy(error))
        {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            continue;
        }
        return result;
    }
}

async fn capture_turn_once(
    session: &Arc<WorkspaceSession>,
    tracking: Tracking,
    turn_id: String,
    kind: CaptureKind,
    request_id: Option<String>,
    label: String,
) -> Result<RunCapture> {
    let context = tracking.context.clone();
    let instance = with_store(tracking.path.clone(), move |store| {
        store.validate_context(&context)
    })
    .await?;
    let workspace = instance
        .workspace_id
        .as_deref()
        .ok_or("Run workspace is unavailable")?;
    let thread = instance
        .thread_id
        .as_deref()
        .ok_or("Run thread is unavailable")?;
    let history = crate::shared::codex_core::history::load_session_turns(session, workspace, thread)
    .await;
    let (turn, errors) = match history {
        Ok(turns) => match turns
            .into_iter()
            .find(|t| t["id"].as_str() == Some(&turn_id))
        {
            Some(turn) => (Some(turn), vec![]),
            None => (None, vec!["Requested turn is missing from history".into()]),
        },
        Err(error) => (None, vec![error]),
    };
    let request_id = request_id.unwrap_or_else(|| format!("initial:{}", instance.id));
    with_store(tracking.path, move |store| {
        let run = store.get_run(&instance.run_id)?;
        let git = capture::git_snapshot(
            instance.workspace_path.as_deref().unwrap_or_default(),
            run.base_oid.as_deref(),
        );
        let events = store.read_evidence(&instance.id, &turn_id)?;
        if instance.initial_turn_id.as_deref() == Some(&turn_id) {
            if let Some(ref turn) = turn {
                store.update_instance(&instance.id, |i| {
                    i.execution_outcome =
                        capture::outcome(turn["status"].as_str().unwrap_or_default());
                    Ok(())
                })?;
            }
        }
        let capture = capture::assemble(
            &instance, turn_id, kind, request_id, label, turn, events, git, errors,
        );
        store.append_capture(capture)
    })
    .await
}

#[cfg(test)]
#[path = "recording_tests.rs"]
mod tests;

/// Restore recording when a backend creates its app-server, even without a visible UI.
/// An uncertain operation is never submitted again.
pub(crate) fn restore(session: Arc<WorkspaceSession>, path: PathBuf) {
    if !path.exists() {
        return;
    }
    tokio::spawn(async move {
        let instances = match with_store(path.clone(), |store| store.recoverable_instances()).await
        {
            Ok(instances) => instances,
            Err(error) => {
                eprintln!("Run recovery failed: {error}");
                return;
            }
        };
        for instance in instances {
            // A fresh server cannot correlate a future event with an unanswered old
            // request. A later follow-up must never become the primary result.
            if instance.initial_turn_id.is_none()
                && matches!(instance.submission_outcome, SubmissionOutcome::Submitting)
            {
                let instance_id = instance.id.clone();
                let _ = with_store(path.clone(), move |store| {
                    store.update_instance(&instance_id, |i| {
                        if i.initial_turn_id.is_none() {
                            i.submission_outcome = SubmissionOutcome::Unknown;
                        }
                        Ok(())
                    })
                })
                .await;
            }
            let (Some(workspace), Some(thread)) = (&instance.workspace_id, &instance.thread_id)
            else {
                continue;
            };
            session
                .register_workspace_with_path(workspace, instance.workspace_path.as_deref())
                .await;
            let tracking = Tracking {
                path: path.clone(),
                context: RunContext {
                    run_id: instance.run_id.clone(),
                    instance_id: instance.id.clone(),
                },
            };
            register(&session, thread, tracking.clone()).await;
            let Some(turn) = instance.initial_turn_id.as_ref() else {
                continue;
            };
            let history = crate::shared::codex_core::history::load_session_turns(&session, workspace, thread)
            .await;
            match history {
                Ok(turns) => {
                    let Some(found) = turns.iter().find(|t| t["id"].as_str() == Some(turn)) else {
                        continue;
                    };
                    let status = capture::outcome(found["status"].as_str().unwrap_or_default());
                    let terminal = matches!(
                        status,
                        ExecutionOutcome::Completed
                            | ExecutionOutcome::Failed
                            | ExecutionOutcome::Interrupted
                    );
                    let instance_id = instance.id.clone();
                    let _ = with_store(path.clone(), move |store| {
                        store.update_instance(&instance_id, |i| {
                            i.execution_outcome = status;
                            Ok(())
                        })
                    })
                    .await;
                    if terminal
                        && !instance
                            .captures
                            .iter()
                            .any(|c| c.turn_id == *turn && c.complete)
                    {
                        let kind = if instance.captures.is_empty() {
                            CaptureKind::InitialTurn
                        } else {
                            CaptureKind::Recovered
                        };
                        let request = (kind == CaptureKind::Recovered).then(|| {
                            format!("recovered:{}:{}", instance.id, instance.captures.len())
                        });
                        if let Err(error) = capture_turn(
                            &session,
                            tracking,
                            turn.clone(),
                            kind,
                            request,
                            "Recovered initial turn".into(),
                        )
                        .await
                        {
                            let instance_id = instance.id.clone();
                            let _ = with_store(path.clone(), move |store| {
                                store.update_instance(&instance_id, |i| {
                                    i.error = Some(error);
                                    Ok(())
                                })
                            })
                            .await;
                        }
                    }
                }
                Err(error) => {
                    let instance_id = instance.id.clone();
                    let _ = with_store(path.clone(), move |store| {
                        store.update_instance(&instance_id, |i| {
                            i.error = Some(format!("Recovery: {error}"));
                            Ok(())
                        })
                    })
                    .await;
                }
            }
        }
    });
}
