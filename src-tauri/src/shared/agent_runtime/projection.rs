//! Lossless protocol evidence plus a small shared presentation vocabulary.
use super::types::RuntimeKind;
use serde_json::{json, Value};
pub(crate) fn normalize(kind: &RuntimeKind, raw: &Value) -> (String, Value) {
    if raw["method"] == "runtime/begin" {
        return ("execution_submitted".into(), raw["params"].clone());
    }
    if raw["method"] == "runtime/steered" {
        return ("input_steered".into(), raw["params"].clone());
    }

    if *kind == RuntimeKind::Codex {
        let method = raw["method"].as_str().unwrap_or("response");
        let p = &raw["params"];
        return match method {
            "item/agentMessage/delta" => (
                "message_delta".into(),
                json!({"role":"assistant","itemId":p["itemId"],"text":p["delta"]}),
            ),
            "item/completed" | "item/started" => ("item".into(), p["item"].clone()),
            "turn/completed" => ("execution_completed".into(), p["turn"].clone()),
            "thread/tokenUsage/updated" => (
                "usage".into(),
                json!({"scope":"execution","value":p["tokenUsage"]["last"]}),
            ),
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                ("permission".into(), raw.clone())
            }
            _ => (method.into(), p.clone()),
        };
    }
    if raw["method"] == "session/request_permission" {
        return ("permission".into(), raw["params"].clone());
    }
    let u = &raw["params"]["update"];
    match u["sessionUpdate"].as_str().unwrap_or("") {
        "agent_message_chunk" | "user_message_chunk" | "agent_thought_chunk" => (
            "message_delta".into(),
            json!({"role":if u["sessionUpdate"]=="user_message_chunk" {"user"} else if u["sessionUpdate"]=="agent_thought_chunk" {"reasoning"} else {"assistant"},"text":u["content"]["text"],"content":u["content"],"itemId":u.get("messageId").or_else(||u.get("itemId")),"annotations":u["annotations"],"native":u}),
        ),
        "tool_call" | "tool_call_update" => ("tool_patch".into(), u.clone()),
        "usage_update" => ("usage".into(), json!({"scope":"session","value":u})),
        "config_option_update" | "current_mode_update" => ("configuration".into(), u.clone()),
        _ => (
            u["sessionUpdate"].as_str().unwrap_or("protocol").into(),
            u.clone(),
        ),
    }
}
/// Merge partial updates while retaining omitted fields. Explicit null stays explicit.
pub(crate) fn patch(target: &mut Value, update: &Value) {
    if let (Some(t), Some(u)) = (target.as_object_mut(), update.as_object()) {
        for (k, v) in u {
            t.insert(k.clone(), v.clone());
        }
    }
}
pub(crate) fn capture_items(events: &[super::types::RuntimeEvent]) -> Vec<Value> {
    let mut result: Vec<Value> = Vec::new();
    for event in events {
        match event.kind.as_str() {
            "message_delta" => {
                let role = event.payload["role"].as_str().unwrap_or("assistant");
                let text = event.payload["text"].as_str().unwrap_or("");
                // Codex starts a native item before streaming its deltas. Keep
                // that item's identity so completion replaces the accumulated
                // text instead of leaving a second copy in the capture.
                if let Some(id) = event.payload["itemId"].as_str() {
                    if let Some(item) = result.iter_mut().find(|v| v["id"] == id) {
                        let mut current = item["text"].as_str().unwrap_or("").to_string();
                        current.push_str(text);
                        item["text"] = json!(current);
                        continue;
                    }
                }
                if let Some(last) = result
                    .last_mut()
                    .filter(|v| v["kind"] == "message" && v["role"] == role)
                {
                    let mut current = last["text"].as_str().unwrap_or("").to_string();
                    current.push_str(text);
                    last["text"] = json!(current);
                } else {
                    result.push(json!({"kind":"message","id":event.payload.get("itemId").cloned().unwrap_or_else(||json!(format!("message:{}",event.sequence))),"role":role,"text":text}));
                }
            }
            "tool_patch" => {
                let id = event.payload["toolCallId"].clone();
                if let Some(previous) = result.iter_mut().find(|v| v["toolCallId"] == id) {
                    let mut update = event.payload.clone();
                    if let Some(k) = update.as_object_mut().and_then(|o| o.remove("kind")) {
                        update["nativeKind"] = k;
                    }
                    patch(previous, &update);
                } else {
                    let mut item = event.payload.clone();
                    item["nativeKind"] = item["kind"].clone();
                    item["kind"] = json!("tool");
                    result.push(item);
                }
            }
            "item" => {
                let id = event.payload["id"].clone();
                if let Some(previous) = result.iter_mut().find(|v| v["id"] == id) {
                    *previous = event.payload.clone();
                } else {
                    result.push(event.payload.clone());
                }
            }
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_tool_patch_keeps_nested_terminal_content_for_live_and_replay() {
        let terminal_content = json!([{"type":"terminal","terminalId":"terminal-42"}]);
        let raw = json!({
            "method":"session/update",
            "params":{"update":{
                "sessionUpdate":"tool_call_update",
                "toolCallId":"tool-42",
                "kind":"execute",
                "content":terminal_content.clone()
            }}
        });

        // Live updates and replay both pass through normalize, then the same
        // patch reducer. Keep the opaque nested ACP block intact in either path.
        let (live_kind, mut live) = normalize(&RuntimeKind::Acp, &raw);
        let (replay_kind, replay) = normalize(&RuntimeKind::Acp, &raw);
        assert_eq!(live_kind, "tool_patch");
        assert_eq!(replay_kind, "tool_patch");
        assert_eq!(live["content"], terminal_content);
        patch(
            &mut live,
            &json!({"toolCallId":"tool-42","status":"completed"}),
        );
        assert_eq!(live["content"][0]["terminalId"], "terminal-42");
        assert_eq!(replay["content"][0]["terminalId"], "terminal-42");
    }
}
