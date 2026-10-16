use super::{
    parse_optional_nullable_string, parse_optional_string, parse_optional_string_array,
    parse_optional_value, parse_string,
};
use crate::shared::codex_core;
use crate::shared::runs_core::types::RunContext;
use crate::shared::runs_rpc;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "send_user_message" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let text = match parse_string(params, "text") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let model = parse_optional_string(params, "model");
            let effort = parse_optional_string(params, "effort");
            let service_tier = parse_optional_nullable_string(params, "serviceTier");
            let access_mode = parse_optional_string(params, "accessMode");
            let images = parse_optional_string_array(params, "images");
            let app_mentions = parse_optional_value(params, "appMentions")
                .and_then(|value| value.as_array().cloned());
            let collaboration_mode = parse_optional_value(params, "collaborationMode");
            Some(
                state
                    .send_user_message(
                        match serde_json::from_value(
                            params.get("runContext").cloned().unwrap_or(Value::Null),
                        ) {
                            Ok(value) => value,
                            Err(e) => return Some(Err(e.to_string())),
                        },
                        workspace_id,
                        thread_id,
                        text,
                        model,
                        effort,
                        service_tier,
                        access_mode,
                        images,
                        app_mentions,
                        collaboration_mode,
                    )
                    .await,
            )
        }
        "turn_interrupt" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let turn_id = match parse_string(params, "turnId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(state.turn_interrupt(workspace_id, thread_id, turn_id).await)
        }
        "turn_steer" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let turn_id = match parse_string(params, "turnId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let text = match parse_string(params, "text") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let images = parse_optional_string_array(params, "images");
            let app_mentions = parse_optional_value(params, "appMentions")
                .and_then(|value| value.as_array().cloned());
            Some(
                state
                    .turn_steer(workspace_id, thread_id, turn_id, text, images, app_mentions)
                    .await,
            )
        }
        "start_review" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let thread_id = match parse_string(params, "threadId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let target = match params
                .as_object()
                .and_then(|map| map.get("target"))
                .cloned()
                .ok_or("missing `target`")
            {
                Ok(value) => value,
                Err(err) => return Some(Err(err.to_string())),
            };
            let delivery = parse_optional_string(params, "delivery");
            Some(
                state
                    .start_review(workspace_id, thread_id, target, delivery)
                    .await,
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn send_user_message(
        &self,
        run_context: Option<RunContext>,
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
        codex_core::turns::send_user_message_core(
            runs_rpc::tracking(&self.storage_path, run_context),
            &self.sessions,
            &self.workspaces,
            workspace_id,
            thread_id,
            text,
            model,
            effort,
            service_tier,
            access_mode,
            images,
            app_mentions,
            collaboration_mode,
        )
        .await
    }

    pub(crate) async fn turn_steer(
        &self,
        workspace_id: String,
        thread_id: String,
        turn_id: String,
        text: String,
        images: Option<Vec<String>>,
        app_mentions: Option<Vec<Value>>,
    ) -> Result<Value, String> {
        codex_core::turns::turn_steer_core(
            &self.sessions,
            workspace_id,
            thread_id,
            turn_id,
            text,
            images,
            app_mentions,
        )
        .await
    }

    pub(crate) async fn turn_interrupt(
        &self,
        workspace_id: String,
        thread_id: String,
        turn_id: String,
    ) -> Result<Value, String> {
        codex_core::turns::turn_interrupt_core(&self.sessions, workspace_id, thread_id, turn_id)
            .await
    }

    pub(crate) async fn start_review(
        &self,
        workspace_id: String,
        thread_id: String,
        target: Value,
        delivery: Option<String>,
    ) -> Result<Value, String> {
        codex_core::turns::start_review_core(
            &self.sessions,
            workspace_id,
            thread_id,
            target,
            delivery,
        )
        .await
    }
}
