use super::turn_input::build_turn_input_items;
use super::workspace_context::get_session_clone;
use super::workspace_context::resolve_workspace_path_core;
use crate::backend::app_server::session::WorkspaceSession;
use crate::types::WorkspaceEntry;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub(crate) fn insert_optional_nullable_string(
    params: &mut Map<String, Value>,
    key: &str,
    value: Option<Option<String>>,
) {
    if let Some(value) = value {
        params.insert(key.to_string(), json!(value));
    }
}

fn protocol_collaboration_mode(
    selected: Value,
    model: Option<&str>,
    effort: Option<&str>,
) -> Option<Value> {
    let object = selected.as_object()?;
    let mode = object.get("mode").and_then(Value::as_str)?;
    let preset_settings = object.get("settings").and_then(Value::as_object);
    let protocol_settings = preset_settings.is_some();
    let settings_model = if protocol_settings {
        preset_settings
            .and_then(|settings| settings.get("model"))
            .and_then(Value::as_str)
            .or(model)
    } else {
        model.or_else(|| object.get("model").and_then(Value::as_str))
    }?;
    let reasoning_effort = if protocol_settings {
        preset_settings
            .and_then(|settings| settings.get("reasoning_effort"))
            .cloned()
            .or_else(|| effort.map(|value| json!(value)))
    } else {
        effort
            .map(|value| json!(value))
            .or_else(|| object.get("reasoning_effort").cloned())
    }
    .unwrap_or(Value::Null);
    let developer_instructions = preset_settings
        .and_then(|settings| settings.get("developer_instructions"))
        .cloned()
        .unwrap_or(Value::Null);

    Some(json!({
        "mode": mode,
        "settings": {
            "model": settings_model,
            "reasoning_effort": reasoning_effort,
            "developer_instructions": developer_instructions,
        }
    }))
}

fn build_turn_start_params(
    thread_id: String,
    input: Vec<Value>,
    cwd: String,
    approval_policy: &str,
    sandbox_policy: Value,
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<Option<String>>,
    collaboration_mode: Option<Value>,
) -> Value {
    let mut params = Map::new();
    params.insert("threadId".to_string(), json!(thread_id));
    params.insert("input".to_string(), json!(input));
    params.insert("cwd".to_string(), json!(cwd));
    params.insert("approvalPolicy".to_string(), json!(approval_policy));
    params.insert("sandboxPolicy".to_string(), sandbox_policy);
    params.insert("model".to_string(), json!(model));
    params.insert("effort".to_string(), json!(effort));
    insert_optional_nullable_string(&mut params, "serviceTier", service_tier);
    if let Some(selected) = collaboration_mode {
        if !selected.is_null() {
            if let Some(mode) =
                protocol_collaboration_mode(selected, model.as_deref(), effort.as_deref())
            {
                params.insert("collaborationMode".to_string(), mode);
            }
        }
    }
    Value::Object(params)
}

pub(crate) async fn send_user_message_core(
    tracking: Option<crate::shared::runs_core::recording::Tracking>,
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    workspace_id: String,
    thread_id: String,
    text: String,
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<Option<String>>,
    access_mode: Option<String>,
    images: Option<Vec<String>>,
    app_mentions: Option<Vec<Value>>,
    collaboration_mode: Option<Value>,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let workspace_path = resolve_workspace_path_core(workspaces, &workspace_id).await?;
    let access_mode = access_mode.unwrap_or_else(|| "current".to_string());
    let sandbox_policy = match access_mode.as_str() {
        "full-access" => json!({ "type": "dangerFullAccess" }),
        "read-only" => json!({ "type": "readOnly" }),
        _ => json!({
            "type": "workspaceWrite",
            "writableRoots": [workspace_path.clone()],
            "networkAccess": true
        }),
    };

    let approval_policy = if access_mode == "full-access" {
        "never"
    } else {
        "on-request"
    };

    let input = build_turn_input_items(text, images, app_mentions)?;

    let params = build_turn_start_params(
        thread_id,
        input,
        workspace_path,
        approval_policy,
        sandbox_policy,
        model,
        effort,
        service_tier,
        collaboration_mode,
    );
    crate::shared::runs_core::recording::tracked_request(
        &session,
        &workspace_id,
        "turn/start",
        params,
        tracking,
    )
    .await
}

pub(crate) async fn turn_steer_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    thread_id: String,
    turn_id: String,
    text: String,
    images: Option<Vec<String>>,
    app_mentions: Option<Vec<Value>>,
) -> Result<Value, String> {
    if turn_id.trim().is_empty() {
        return Err("missing active turn id".to_string());
    }
    let session = get_session_clone(sessions, &workspace_id).await?;
    let input = build_turn_input_items(text, images, app_mentions)?;
    let params = json!({
        "threadId": thread_id,
        "expectedTurnId": turn_id,
        "input": input
    });
    session
        .send_request_for_workspace(&workspace_id, "turn/steer", params)
        .await
}

pub(crate) async fn turn_interrupt_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    thread_id: String,
    turn_id: String,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let params = json!({ "threadId": thread_id, "turnId": turn_id });
    session
        .send_request_for_workspace(&workspace_id, "turn/interrupt", params)
        .await
}

pub(crate) async fn start_review_core(
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    workspace_id: String,
    thread_id: String,
    target: Value,
    delivery: Option<String>,
) -> Result<Value, String> {
    let session = get_session_clone(sessions, &workspace_id).await?;
    let mut params = Map::new();
    params.insert("threadId".to_string(), json!(thread_id));
    params.insert("target".to_string(), target);
    if let Some(delivery) = delivery {
        params.insert("delivery".to_string(), json!(delivery));
    }
    session
        .send_request_for_workspace(&workspace_id, "review/start", Value::Object(params))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_start_matches_generated_schema_contract_fixture() {
        let params = build_turn_start_params(
            "thread".into(),
            vec![json!({ "type": "text", "text": "hello" })],
            "/tmp/codex-monitor".into(),
            "on-request",
            json!({
                "type": "workspaceWrite",
                "writableRoots": ["/tmp/codex-monitor"],
                "networkAccess": true
            }),
            None,
            None,
            None,
            Some(
                json!({ "name": "Plan", "mode": "plan", "model": "fixture-model", "reasoning_effort": "high" }),
            ),
        );
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/app-server-contracts.json"
        ))
        .expect("contract fixture is valid JSON");
        assert_eq!(params, fixture["TurnStartParams"][0]);
    }

    #[test]
    fn collaboration_preset_uses_selected_model_when_preset_has_none() {
        assert_eq!(
            protocol_collaboration_mode(
                json!({"mode":"default"}),
                Some("selected-model"),
                Some("medium")
            ),
            Some(json!({
                "mode":"default",
                "settings": {
                    "model":"selected-model",
                    "reasoning_effort":"medium",
                    "developer_instructions":null
                }
            }))
        );
        assert_eq!(
            protocol_collaboration_mode(json!({"mode":"default"}), None, None),
            None
        );
    }

    #[test]
    fn collaboration_mode_preserves_an_already_nested_protocol_value() {
        let raw = json!({
            "mode":"plan",
            "settings": {
                "model":"mode-model",
                "reasoning_effort":"high",
                "developer_instructions":"Keep the plan concise"
            }
        });
        assert_eq!(
            protocol_collaboration_mode(raw.clone(), Some("selected-model"), Some("low")),
            Some(raw)
        );
    }

    #[test]
    fn turn_start_uses_native_model_and_effort_over_mask_defaults() {
        for (access_mode, expected_approval, expected_sandbox) in [
            (
                "current",
                "on-request",
                json!({
                    "type":"workspaceWrite",
                    "writableRoots":["/workspace"],
                    "networkAccess":true
                }),
            ),
            ("read-only", "on-request", json!({"type":"readOnly"})),
            ("full-access", "never", json!({"type":"dangerFullAccess"})),
        ] {
            let approval = if access_mode == "full-access" {
                "never"
            } else {
                "on-request"
            };
            let sandbox = match access_mode {
                "read-only" => json!({"type":"readOnly"}),
                "full-access" => json!({"type":"dangerFullAccess"}),
                _ => json!({
                    "type":"workspaceWrite",
                    "writableRoots":["/workspace"],
                    "networkAccess":true
                }),
            };
            assert_eq!(approval, expected_approval);
            assert_eq!(sandbox, expected_sandbox);
            let params = build_turn_start_params(
                "thread".into(),
                vec![json!({"type":"text","text":"hello"})],
                "/workspace".into(),
                approval,
                sandbox,
                Some("model-x".into()),
                Some("high".into()),
                Some(Some("priority".into())),
                Some(json!({"mode":"plan","model":"preset-model","reasoning_effort":"medium"})),
            );
            assert_eq!(params["model"], "model-x");
            assert_eq!(params["effort"], "high");
            assert_eq!(params["serviceTier"], "priority");
            assert_eq!(params["approvalPolicy"], expected_approval);
            assert_eq!(params["sandboxPolicy"], expected_sandbox);
            assert_eq!(params["collaborationMode"]["settings"]["model"], "model-x");
            assert_eq!(
                params["collaborationMode"]["settings"]["reasoning_effort"],
                "high"
            );
        }
    }

    #[test]
    fn insert_optional_nullable_string_omits_missing_and_preserves_null() {
        let mut params = Map::new();

        insert_optional_nullable_string(&mut params, "serviceTier", None);
        assert!(!params.contains_key("serviceTier"));

        insert_optional_nullable_string(&mut params, "serviceTier", Some(None));
        assert_eq!(params.get("serviceTier"), Some(&Value::Null));

        insert_optional_nullable_string(&mut params, "serviceTier", Some(Some("fast".to_string())));
        assert_eq!(params.get("serviceTier"), Some(&json!("fast")));
    }
}
