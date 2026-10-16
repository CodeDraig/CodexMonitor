use super::runs_core::{
    self, capture,
    recording::{self, Tracking},
    store::with_store,
    types::*,
};
use crate::{backend::app_server::session::WorkspaceSession, types::WorkspaceEntry};
use base64::Engine;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

pub(crate) const METHODS: &[&str] = &[
    "create_run",
    "list_runs",
    "get_run",
    "update_run_metadata",
    "record_run_setup",
    "record_run_failure",
    "reconcile_run",
    "capture_run_instance",
    "list_run_turns",
    "get_run_capture",
    "save_run_comparison",
    "list_run_comparisons",
];
fn string(input: &Value, key: &str) -> Result<String, String> {
    input[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("Missing {key}"))
}
fn encode(value: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}
pub(crate) async fn dispatch(
    method: &str,
    input: Value,
    path: PathBuf,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
) -> Result<Value, String> {
    if method == "create_run" {
        let request: CreateRun = serde_json::from_value(input).map_err(|e| e.to_string())?;
        let request_id = request.request_id.clone();
        if let Some(existing) =
            with_store(path.clone(), move |store| store.find_request(&request_id)).await?
        {
            return encode(existing);
        }
        let workspace = workspaces
            .lock()
            .await
            .get(&request.workspace_id)
            .cloned()
            .ok_or("Workspace not found")?;
        return with_store(path, move |store| {
            let baseline = capture::git_snapshot(&workspace.path, None);
            let base = baseline["headOid"].as_str().map(str::to_string);
            if request.mode == "worktree" && base.is_none() {
                return Err("Recorded worktrees require a Git HEAD commit".into());
            }
            let images = request
                .images
                .iter()
                .map(|image| {
                    if let Some((prefix, data)) = image.split_once(',').filter(|(prefix, _)| {
                        prefix.starts_with("data:") && prefix.ends_with(";base64")
                    }) {
                        let _ = prefix;
                        base64::engine::general_purpose::STANDARD
                            .decode(data)
                            .map_err(|e| e.to_string())
                    } else {
                        std::fs::read(crate::shared::codex_core::images::normalize_file_path(image))
                            .map_err(|e| e.to_string())
                    }
                })
                .collect::<Result<Vec<_>, _>>()?;
            encode(store.create_run(
                request,
                workspace.name,
                workspace.path,
                base,
                baseline,
                images,
            )?)
        })
        .await;
    }
    if method == "list_run_turns" {
        let id = string(&input, "instanceId")?;
        let instance = with_store(path, move |store| store.instance(&id)).await?;
        let workspace = instance.workspace_id.ok_or("No recorded workspace")?;
        let thread = instance.thread_id.ok_or("No recorded thread")?;
        let session = sessions
            .lock()
            .await
            .get(&workspace)
            .cloned()
            .ok_or("Workspace is disconnected; connect it before selecting a later turn")?;
        let turns = super::codex_core::history::load_session_turns(&session, &workspace, &thread)
        .await?;
        return Ok(json!(turns
            .into_iter()
            .map(|turn| json!({"id":turn["id"],"status":turn["status"]}))
            .collect::<Vec<_>>()));
    }
    if method == "capture_run_instance" {
        let request = string(&input, "requestId")?;
        let instance = string(&input, "instanceId")?;
        let turn = string(&input, "turnId")?;
        if let Some(capture) = with_store(path.clone(), move |store| {
            store.capture_by_request(&request)
        })
        .await?
        {
            if capture.instance_id != instance || capture.turn_id != turn {
                return Err("Capture request identity conflicts with existing capture".into());
            }
            return encode(capture);
        }
    }
    if method == "reconcile_run" || method == "capture_run_instance" {
        let records = if method == "reconcile_run" {
            let run_id = string(&input, "runId")?;
            with_store(path.clone(), move |store| {
                Ok(store.get_run(&run_id)?.instances)
            })
            .await?
        } else {
            let instance_id = string(&input, "instanceId")?;
            vec![with_store(path.clone(), move |store| store.instance(&instance_id)).await?]
        };
        let mut errors = vec![];
        for instance in records {
            let tracking = Tracking {
                path: path.clone(),
                context: RunContext {
                    run_id: instance.run_id.clone(),
                    instance_id: instance.id.clone(),
                },
            };
            let session = if let Some(ref workspace) = instance.workspace_id {
                sessions.lock().await.get(workspace).cloned()
            } else {
                None
            };
            let Some(session) = session else {
                errors.push(format!(
                    "{}: workspace is missing or disconnected",
                    instance.id
                ));
                continue;
            };
            if let Some(ref thread) = instance.thread_id {
                recording::register(&session, thread, tracking.clone()).await;
            }
            let turn = if method == "capture_run_instance" {
                Some(string(&input, "turnId")?)
            } else {
                instance.initial_turn_id.clone()
            };
            let Some(turn) = turn else {
                errors.push(format!(
                    "{}: submission identity remains unresolved",
                    instance.id
                ));
                continue;
            };
            if method == "reconcile_run" {
                // Recovery never guesses an initial turn or relaunches an external operation.
                let Some(ref thread) = instance.thread_id else {
                    continue;
                };
                match super::codex_core::history::load_session_turns(
                    &session, instance.workspace_id.as_deref().unwrap_or_default(), thread,
                )
                .await
                {
                    Ok(turns) => {
                        let Some(found) = turns.iter().find(|t| t["id"].as_str() == Some(&turn))
                        else {
                            errors.push(format!("{}: initial turn missing", instance.id));
                            continue;
                        };
                        let status = capture::outcome(found["status"].as_str().unwrap_or_default());
                        let instance_id = instance.id.clone();
                        let observed = status.clone();
                        with_store(path.clone(), move |store| {
                            store.update_instance(&instance_id, |i| {
                                i.execution_outcome = observed;
                                Ok(())
                            })
                        })
                        .await?;
                        if !matches!(
                            status,
                            ExecutionOutcome::Completed
                                | ExecutionOutcome::Failed
                                | ExecutionOutcome::Interrupted
                        ) {
                            continue;
                        }
                        // Do not generate a new recovered capture on every poll.
                        if instance
                            .captures
                            .iter()
                            .any(|c| c.turn_id == turn && c.complete)
                        {
                            continue;
                        }
                    }
                    Err(error) => {
                        errors.push(format!("{}: {error}", instance.id));
                        // A failed read is not terminal evidence. Leave the primary
                        // slot for the recorder instead of freezing an active turn.
                        continue;
                    }
                }
            }
            let manual = method == "capture_run_instance";
            let kind = if manual {
                CaptureKind::Manual
            } else if instance.captures.is_empty() {
                CaptureKind::InitialTurn
            } else {
                CaptureKind::Recovered
            };
            let request = if manual {
                let id = string(&input, "requestId")?;
                uuid::Uuid::parse_str(&id).map_err(|e| e.to_string())?;
                Some(id)
            } else if kind == CaptureKind::Recovered {
                Some(format!(
                    "recovered:{}:{}",
                    instance.id,
                    instance.captures.len()
                ))
            } else {
                None
            };
            let label = input["label"]
                .as_str()
                .unwrap_or(if manual {
                    "Manual capture"
                } else {
                    "Recovered initial turn"
                })
                .to_string();
            match recording::capture_turn(&session, tracking, turn, kind, request, label).await {
                Ok(capture) if manual => return encode(capture),
                Ok(_) => {}
                Err(error) => errors.push(error),
            }
        }
        if method == "capture_run_instance" {
            return Err(errors.join("; "));
        }
        let run_id = string(&input, "runId")?;
        return with_store(path, move |store| {
            Ok(json!({"run":store.get_run(&run_id)?,"errors":errors}))
        })
        .await;
    }
    let method = method.to_string();
    with_store(path, move |store| match method.as_str() {
        "list_runs" => store.list_runs(
            input["workspaceId"].as_str(),
            input["cursor"].as_str(),
            input["limit"].as_u64().unwrap_or(50).min(100) as u32,
        ),
        "get_run" => encode(store.get_run(&string(&input, "runId")?)?),
        "get_run_capture" => encode(store.capture(&string(&input, "captureId")?)?),
        "update_run_metadata" => encode(
            store.update_title(
                &string(&input, "runId")?,
                input["expectedRevision"]
                    .as_u64()
                    .ok_or("Missing expectedRevision")? as u32,
                string(&input, "title")?,
            )?,
        ),
        "record_run_setup" => {
            let observation = input["observation"].clone();
            if ![
                "not_configured",
                "already_dispatched",
                "dispatched",
                "dispatch_failed",
            ]
            .contains(&observation["status"].as_str().unwrap_or_default())
            {
                return Err("Invalid setup observation".into());
            }
            encode(store.update_instance(&string(&input, "instanceId")?, |i| {
                i.setup = observation;
                Ok(())
            })?)
        }
        "record_run_failure" => {
            encode(store.update_instance(&string(&input, "instanceId")?, |i| {
                i.error = Some(string(&input, "message")?);
                if matches!(i.submission_outcome, SubmissionOutcome::NotSubmitted) {
                    i.preparation = "failed".into();
                    i.submission_outcome = SubmissionOutcome::Rejected;
                }
                Ok(())
            })?)
        }
        "save_run_comparison" => encode(
            store.save_comparison(
                string(&input, "leftCaptureId")?,
                string(&input, "rightCaptureId")?,
                string(&input, "choice")?,
                string(&input, "notes")?,
                input["expectedRevision"]
                    .as_u64()
                    .ok_or("Missing expectedRevision")? as u32,
            )?,
        ),
        "list_run_comparisons" => {
            encode(store.comparisons(input["runId"].as_str(), input["captureId"].as_str())?)
        }
        _ => Err(format!("Unknown run command: {method}")),
    })
    .await
}

pub(crate) fn tracking(storage: &std::path::Path, context: Option<RunContext>) -> Option<Tracking> {
    context.map(|context| Tracking {
        path: runs_core::db_path(storage),
        context,
    })
}
