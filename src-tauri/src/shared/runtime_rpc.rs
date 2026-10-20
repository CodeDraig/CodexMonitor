//! The desktop and daemon call exactly this dispatcher; profiles are backend-owned.
use super::{
    agent_runtime::{
        registry::{capture_execution, persist},
        types::*,
        RuntimeRegistry,
    },
    runs_core::{store::with_store, types::CaptureKind},
};
use crate::{
    backend::{app_server::session::WorkspaceSession, events::EventSink},
    types::{AppSettings, WorkspaceEntry},
};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use tokio::sync::Mutex;
fn string(v: &Value, key: &str) -> Result<String, String> {
    v[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("invalid_input: missing {key}"))
}
fn encode(v: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(v).map_err(|e| e.to_string())
}
fn prepared_reference(input: &Value) -> Result<Option<(String, u64)>, String> {
    if input.get("connectionId").is_none_or(Value::is_null)
        && input.get("generation").is_none_or(Value::is_null) {
        return Ok(None);
    }
    let id = string(input, "connectionId")?;
    let generation = input["generation"].as_u64().filter(|generation| *generation > 0)
        .ok_or("invalid_input: prepared connection requires a positive generation")?;
    Ok(Some((id, generation)))
}
#[cfg(test)]
#[test]
fn prepared_connection_reference_requires_both_identity_and_generation() {
    assert_eq!(prepared_reference(&json!({})).unwrap(), None);
    assert!(prepared_reference(&json!({"connectionId":"connection"})).is_err());
    assert!(prepared_reference(&json!({"generation":1})).is_err());
    assert!(prepared_reference(&json!({"connectionId":"connection","generation":0})).is_err());
    assert_eq!(prepared_reference(&json!({"connectionId":"connection","generation":2})).unwrap(), Some(("connection".into(),2)));
}
pub(crate) async fn dispatch<E: EventSink>(
    method: &str,
    input: Value,
    path: PathBuf,
    registry: &RuntimeRegistry,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    settings: &Mutex<AppSettings>,
    sink: E,
) -> Result<Value, String> {
    match method {
        "prepare_connection" => {
            match input["role"].as_str() {
                Some("conversation" | "discovery") => {},
                _ => return Err("invalid_input: connection role must be conversation or discovery".into()),
            }
            let id = string(&input, "profileId")?;
            let profile = registry
                .profiles(path.clone(), settings)
                .await?
                .into_iter()
                .find(|p| p.id == id && p.is_acp())
                .ok_or("invalid_input: ACP profile not found")?;
            if input["expectedProfileRevision"].as_u64() != Some(profile.revision) {
                return Err("revision_conflict: runtime profile changed".into());
            }
            let cwd = input["cwd"]
                .as_str()
                .or(profile.working_directory.as_deref())
                .ok_or("invalid_input: prepare_connection requires an absolute directory")?;
            let cwd = std::path::PathBuf::from(cwd);
            if !cwd.is_absolute() {
                return Err("invalid_input: connection directory must be absolute".into());
            }
            let cwd = cwd
                .canonicalize()
                .map_err(|e| format!("invalid_input: connection directory unavailable: {e}"))?;
            let session = input["sessionId"].as_str().map(str::to_owned);
            if let Some(sid) = &session {
                if input["role"] == "discovery" {
                    return Err(
                        "invalid_input: discovery authentication cannot claim a conversation"
                            .into(),
                    );
                }
                let saved_id = sid.clone();
                let saved = with_store(path.clone(), move |s| s.runtime_session(&saved_id)).await?;
                if saved.profile.connection_scope() != profile.connection_scope()
                    || !saved.profile.is_acp()
                {
                    return Err(
                        "invalid_input: connection profile does not match the saved conversation"
                            .into(),
                    );
                }
                let saved_cwd = std::path::Path::new(&saved.cwd)
                    .canonicalize()
                    .map_err(|e| format!("invalid_input: saved conversation directory: {e}"))?;
                if saved_cwd != cwd {
                    return Err(
                        "invalid_input: connection directory does not match the saved conversation"
                            .into(),
                    );
                }
                let sid = sid.clone();
                let owner = registry.owner.clone();
                with_store(path.clone(), move |s| {
                    s.check_runtime_session_available(&sid, &owner)
                })
                .await?;
            }
            registry
                .connections
                .prepare(
                    path,
                    profile,
                    cwd,
                    session,
                    input["role"] == "discovery",
                    string(&input, "requestId")?,
                )
                .await
        }
        "close_connection" => {
            registry
                .connections
                .close_prepared_connection(
                    &string(&input, "connectionId")?,
                    input["generation"]
                        .as_u64()
                        .ok_or("invalid_input: generation required")?,
                )
                .await
        }
        "connection_status" => {
            if input["connectionId"].is_string() {
                registry.connections.status(&string(&input, "connectionId")?).await
            } else {
                registry.connections.status_for_session(&string(&input, "sessionId")?).await
            }
        }
        "connection_operation" => {
            registry
                .connections
                .operation(&string(&input, "operationId")?)
                .await
        }
        "authenticate" => {
            registry
                .connections
                .authenticate(
                    &string(&input, "connectionId")?,
                    input["generation"]
                        .as_u64()
                        .ok_or("invalid_input: generation required")?,
                    &string(&input, "methodId")?,
                    string(&input, "requestId")?,
                )
                .await
        }
        "cancel_connection_operation" => {
            registry
                .connections
                .cancel_operation(
                    &string(&input, "operationId")?,
                    input["generation"]
                        .as_u64()
                        .ok_or("invalid_input: generation required")?,
                )
                .await
        }
        "logout" => {
            registry
                .connections
                .logout(
                    &string(&input, "connectionId")?,
                    input["generation"]
                        .as_u64()
                        .ok_or("invalid_input: generation required")?,
                )
                .await
        }
        "respond_client_request" => {
            registry
                .connections
                .respond(
                    &string(&input, "connectionId")?,
                    input["generation"]
                        .as_u64()
                        .ok_or("invalid_input: generation required")?,
                    &input["clientRequestId"],
                    input["decision"].clone(),
                )
                .await
        }
        "read_login_terminal"
        | "write_login_terminal"
        | "resize_login_terminal"
        | "close_login_terminal" => registry.connections.login_call(method, &input).await,
        "read_acp_terminal" => {
            registry
                .connections
                .read_terminal(
                    path,
                    &string(&input, "sessionId")?,
                    &string(&input, "terminalId")?,
                )
                .await
        }
        "profiles" => {
            let profiles = registry.profiles(path.clone(), settings).await?;
            let info = with_store(path, |s| Ok(s.runtime_store_info())).await?;
            Ok(json!({"profiles":profiles,"storeId":info["storeId"],"schemaVersion":4}))
        }
        "catalog" => with_store(path, |s| s.catalog()).await.and_then(encode),
        "discover_sessions" => {
            let id = string(&input, "profileId")?;
            let profile = registry
                .profiles(path.clone(), settings)
                .await?
                .into_iter()
                .find(|p| p.id == id)
                .ok_or("invalid_input: profile not found")?;
            registry
                .discover(
                    path,
                    profile,
                    input["cwd"].as_str().map(str::to_string),
                    input["cursor"].as_str().map(str::to_string),
                    prepared_reference(&input)?,
                )
                .await
        }
        "open_session_by_id" => {
            let id = string(&input, "profileId")?;
            let profile = registry
                .profiles(path.clone(), settings)
                .await?
                .into_iter()
                .find(|p| p.id == id && p.is_acp())
                .ok_or("invalid_input: ACP profile not found")?;
            let cwd = string(&input, "cwd")?;
            let native_id = string(&input, "nativeSessionId")?;
            if !std::path::Path::new(&cwd).is_absolute() {
                return Err("invalid_input: agent session directory must be absolute".into());
            }
            // Probe on an independent connection: opening by ID must not disturb
            // an active prompt, nor register an unrestorable external session.
            let (client, _events, _callbacks, connection_id) = registry
                .connections
                .spawn_auto(path.clone(), profile.clone(), PathBuf::from(&cwd), true)
                .await?;
            let supported =
                super::agent_runtime::registry::acp_capabilities(&client.initialization)
                    .load_session;
            registry
                .connections
                .close_connection(&connection_id, 1)
                .await?;
            if !supported {
                return Err("unsupported: this agent cannot load existing sessions by ID".into());
            }
            let info = json!({"sessionId":native_id,"cwd":cwd});
            persist(path, move |s| s.catalog_upsert(&profile, &info))
                .await
                .and_then(encode)
        }
        "attach_session" => {
            let cid = string(&input, "catalogId")?;
            let catalog = with_store(path.clone(), move |s| s.catalog_session(&cid)).await?;
            let previous = if let Some(id) = catalog.session_id.clone() {
                Some(with_store(path.clone(), move |s| s.runtime_session(&id)).await?)
            } else {
                None
            };
            let association = &input["workspaceAssociation"];
            let workspace = match association["kind"].as_str() {
                Some("reuse") => {
                    let wid = string(association, "workspaceId")?;
                    let w = workspaces
                        .lock()
                        .await
                        .get(&wid)
                        .cloned()
                        .ok_or("invalid_input: workspace not found")?;
                    let reported = std::fs::canonicalize(&catalog.cwd)
                        .map_err(|e| format!("invalid_input: agent directory unavailable: {e}"))?;
                    if std::fs::canonicalize(&w.path).ok().as_ref() != Some(&reported) {
                        return Err(
                            "invalid_input: workspace does not match agent directory".into()
                        );
                    }
                    w
                }
                Some("add") => {
                    crate::shared::workspaces_core::register_workspace_core(
                        &catalog.cwd,
                        previous.as_ref().map(|s| s.workspace_id.clone()),
                        workspaces,
                        &path.with_file_name("workspaces.json"),
                    )
                    .await?
                }
                _ => {
                    return Err("invalid_input: explicitly reuse or add the agent directory".into())
                }
            };
            let session = RuntimeSession {
                store_id: catalog.store_id.clone(),
                runtime: catalog.profile.reference(),
                session_id: previous
                    .as_ref()
                    .map(|s| s.session_id.clone())
                    .unwrap_or_else(crate::shared::runs_core::id),
                native_session_id: catalog.native_session_id.clone(),
                workspace_id: workspace.id,
                cwd: catalog.cwd.clone(),
                profile: catalog.profile.clone(),
                capabilities: Default::default(),
                configuration: Value::Null,
                agent_info: Value::Null,
            };
            persist(path, move |s| {
                s.attach_catalog(&catalog.catalog_id, &session)
            })
            .await
            .and_then(encode)
        }
        "load_conversation_history" => {
            let sid = string(&input, "sessionId")?;
            let request = string(&input, "requestId")?;
            uuid::Uuid::parse_str(&request).map_err(|e| format!("invalid_input: {e}"))?;
            let session = with_store(path.clone(), move |s| s.runtime_session(&sid)).await?;
            registry
                .load_conversation(
                    path,
                    session,
                    request,
                    sink,
                    prepared_reference(&input)?,
                )
                .await
                .and_then(encode)
        }
        "read_conversation" => {
            let sid = string(&input, "sessionId")?;
            let generation = input["generation"].as_str().map(str::to_string);
            let cursor = input["cursor"].as_u64().unwrap_or(0);
            let limit = input["limit"].as_u64().unwrap_or(200);
            let owner = registry.owner.clone();
            let ready = registry.conversation_ready(&sid).await;
            let mut result = with_store(path, move |s| {
                s.conversation_read(&sid, generation.as_deref(), cursor, limit, &owner)
            })
            .await?;
            result["connectionReady"] = json!(ready);
            Ok(result)
        }
        "read_conversation_attachment" => {
            let sid = string(&input, "sessionId")?;
            let aid = string(&input, "attachmentId")?;
            with_store(path, move |s| s.conversation_attachment(&sid, &aid)).await
        }
        "update_conversation_metadata" => {
            let sid = string(&input, "sessionId")?;
            let expected = input["expectedRevision"]
                .as_u64()
                .ok_or("invalid_input: missing expectedRevision")?;
            persist(path, move |s| {
                s.conversation_metadata(&sid, expected, &input)
            })
            .await
            .and_then(encode)
        }
        "save_profile" => {
            let p: RuntimeProfile = serde_json::from_value(input["profile"].clone())
                .map_err(|e| format!("invalid_input: {e}"))?;
            let expected = input["expectedRevision"]
                .as_u64()
                .ok_or("invalid_input: missing expectedRevision")?;
            persist(path, move |s| s.save_runtime_profile(p.clone(), expected))
                .await
                .and_then(encode)
        }
        "session" => {
            let sid = string(&input, "sessionId")?;
            with_store(path, move |s| s.runtime_session(&sid))
                .await
                .and_then(encode)
        }
        "sessions" => {
            let workspace = string(&input, "workspaceId")?;
            with_store(path, move |s| s.runtime_sessions(&workspace))
                .await
                .and_then(encode)
        }
        "create_session" | "load_session" => {
            let existing = if method == "load_session" {
                let id = string(&input, "sessionId")?;
                Some(with_store(path.clone(), move |s| s.runtime_session(&id)).await?)
            } else {
                None
            };
            let workspace_id = existing
                .as_ref()
                .map(|s| s.workspace_id.clone())
                .unwrap_or(string(&input, "workspaceId").unwrap_or_default());
            let workspace = workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned()
                .ok_or("invalid_input: workspace not found")?;
            let profile = if let Some(s) = &existing {
                s.profile.clone()
            } else {
                let id = string(&input, "profileId")?;
                registry
                    .profiles(path.clone(), settings)
                    .await?
                    .into_iter()
                    .find(|p| p.id == id)
                    .ok_or("invalid_input: runtime profile not found")?
            };
            if let Some(expected) = input["expectedProfileRevision"].as_u64() {
                if profile.revision != expected {
                    return Err("revision_conflict: runtime profile changed; reload profiles before creating a session".into());
                }
            }
            encode(
                registry
                    .create_session(
                        path,
                        workspace,
                        profile,
                        existing,
                        sessions,
                        workspaces,
                        settings,
                        sink,
                        prepared_reference(&input)?,
                    )
                    .await?,
            )
        }
        "submit" => {
            let request: SubmitExecution =
                serde_json::from_value(input).map_err(|e| format!("invalid_input: {e}"))?;
            encode(registry.submit(path, request).await?)
        }
        "execution" => {
            if let Some(id) = input["requestId"].as_str() {
                let id = id.to_string();
                with_store(path, move |s| s.find_runtime_request(&id))
                    .await
                    .and_then(encode)
            } else {
                let id = string(&input, "executionId")?;
                with_store(path, move |s| s.runtime_execution(&id))
                    .await
                    .and_then(encode)
            }
        }
        "history" => {
            let id = string(&input, "sessionId")?;
            registry.check_recording(&id).await?;
            let executions = with_store(path, move |s| {
                s.reconcile_runtime_owner(&id)?;
                s.runtime_history(&id)
            })
            .await?;
            Ok(
                json!({"executions":executions,"complete":false,"scope":"monitor_recorded_executions"}),
            )
        }
        "events" => {
            let id = string(&input, "sessionId")?;
            let after = input["after"].as_u64().unwrap_or(0);
            with_store(path, move |s| s.runtime_events(&id, after))
                .await
                .and_then(encode)
        }
        "steer" => {
            let prompt = input["prompt"]
                .as_array()
                .cloned()
                .ok_or("invalid_input: missing prompt content")?;
            registry
                .steer(
                    path,
                    &string(&input, "sessionId")?,
                    &string(&input, "executionId")?,
                    prompt,
                )
                .await
        }
        "cancel" => {
            registry.cancel(&string(&input, "sessionId")?).await?;
            Ok(Value::Null)
        }
        "permission" => {
            registry
                .permission(
                    &string(&input, "sessionId")?,
                    &string(&input, "requestId")?,
                    input["optionId"].as_str().map(str::to_string),
                )
                .await?;
            Ok(Value::Null)
        }
        "configure" => {
            registry
                .configure(
                    path,
                    &string(&input, "sessionId")?,
                    &string(&input, "optionId")?,
                    input["value"].clone(),
                )
                .await
        }
        "disconnect" => {
            registry
                .disconnect(path, &string(&input, "sessionId")?)
                .await?;
            Ok(Value::Null)
        }
        "capture" => {
            let request = string(&input, "requestId")?;
            uuid::Uuid::parse_str(&request).map_err(|e| format!("invalid_input: {e}"))?;
            encode(
                capture_execution(
                    path,
                    string(&input, "instanceId")?,
                    string(&input, "executionId")?,
                    request,
                    input["label"].as_str().unwrap_or("Later execution").into(),
                    CaptureKind::Manual,
                )
                .await?,
            )
        }
        _ => Err(format!("unsupported: unknown runtime command {method}")),
    }
}
