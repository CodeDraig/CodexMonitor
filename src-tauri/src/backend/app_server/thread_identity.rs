use serde_json::Value;
use std::collections::HashSet;

pub(super) fn extract_thread_id(value: &Value) -> Option<String> {
    fn extract_from_container(container: Option<&Value>) -> Option<String> {
        let container = container?;
        container
            .get("threadId")
            .or_else(|| container.get("thread_id"))
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                container
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
            })
    }

    extract_from_container(value.get("params"))
        .or_else(|| extract_from_container(value.get("result")))
}

fn push_thread_id(out: &mut Vec<String>, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    if let Some(thread_id) = value.as_str().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        out.push(thread_id.to_string());
        return;
    }
    if let Some(values) = value.as_array() {
        for entry in values {
            push_thread_id(out, Some(entry));
        }
    }
}

pub(super) fn extract_related_thread_ids(value: &Value) -> Vec<String> {
    fn collect_agent_thread_ids(value: Option<&Value>, out: &mut Vec<String>) {
        let Some(value) = value else {
            return;
        };
        if let Some(values) = value.as_array() {
            for entry in values {
                collect_agent_thread_ids(Some(entry), out);
            }
            return;
        }
        let Some(record) = value.as_object() else {
            return;
        };
        push_thread_id(
            out,
            record.get("threadId").or_else(|| record.get("thread_id")),
        );
        push_thread_id(out, record.get("id"));
        push_thread_id(
            out,
            record.get("thread").and_then(|thread| {
                thread
                    .get("id")
                    .or_else(|| thread.get("threadId"))
                    .or_else(|| thread.get("thread_id"))
            }),
        );
    }

    fn collect_from_container(container: Option<&Value>, out: &mut Vec<String>) {
        let Some(container) = container.and_then(|value| value.as_object()) else {
            return;
        };
        push_thread_id(out, container.get("agentThreadId"));
        push_thread_id(
            out,
            container
                .get("threadId")
                .or_else(|| container.get("thread_id")),
        );
        push_thread_id(
            out,
            container.get("thread").and_then(|thread| thread.get("id")),
        );
        push_thread_id(
            out,
            container
                .get("params")
                .and_then(|params| params.get("threadId").or_else(|| params.get("thread_id"))),
        );
        push_thread_id(
            out,
            container
                .get("result")
                .and_then(|result| result.get("threadId").or_else(|| result.get("thread_id"))),
        );
        push_thread_id(
            out,
            container
                .get("newThreadId")
                .or_else(|| container.get("new_thread_id")),
        );
        push_thread_id(
            out,
            container
                .get("receiverThreadId")
                .or_else(|| container.get("receiver_thread_id")),
        );
        push_thread_id(
            out,
            container
                .get("receiverThreadIds")
                .or_else(|| container.get("receiver_thread_ids")),
        );
        collect_agent_thread_ids(
            container
                .get("receiverAgents")
                .or_else(|| container.get("receiver_agents")),
            out,
        );
        collect_agent_thread_ids(
            container
                .get("receiverAgent")
                .or_else(|| container.get("receiver_agent")),
            out,
        );
        collect_agent_thread_ids(
            container
                .get("agentStatuses")
                .or_else(|| container.get("agent_statuses")),
            out,
        );
        if let Some(status_map) = container
            .get("statuses")
            .and_then(|value| value.as_object())
        {
            out.extend(
                status_map
                    .keys()
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty()),
            );
        }
        if let Some(item) = container.get("item") {
            collect_from_container(Some(item), out);
        }
    }

    let mut out = Vec::new();
    collect_from_container(value.get("params"), &mut out);
    collect_from_container(value.get("result"), &mut out);
    collect_from_container(Some(value), &mut out);

    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|thread_id| seen.insert(thread_id.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_thread_id_reads_camel_case() {
        let value = json!({ "params": { "threadId": "thread-123" } });
        assert_eq!(extract_thread_id(&value), Some("thread-123".to_string()));
    }

    #[test]
    fn extract_thread_id_reads_snake_case() {
        let value = json!({ "params": { "thread_id": "thread-456" } });
        assert_eq!(extract_thread_id(&value), Some("thread-456".to_string()));
    }

    #[test]
    fn extract_thread_id_reads_hook_notification_thread_id() {
        let value = json!({
            "method": "hook/started",
            "params": {
                "threadId": "thread-hook-1",
                "run": { "id": "hook-1" }
            }
        });
        assert_eq!(extract_thread_id(&value), Some("thread-hook-1".to_string()));
    }

    #[test]
    fn extract_thread_id_returns_none_when_missing() {
        let value = json!({ "params": {} });
        assert_eq!(extract_thread_id(&value), None);
    }

    #[test]
    fn extract_related_thread_ids_reads_spawn_hints_from_item_payloads() {
        let value = json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-parent",
                "item": {
                    "type": "mcpToolCall",
                    "new_thread_id": "thread-child"
                }
            }
        });
        let ids = extract_related_thread_ids(&value);
        assert!(ids.contains(&"thread-parent".to_string()));
        assert!(ids.contains(&"thread-child".to_string()));
    }

    #[test]
    fn extract_related_thread_ids_reads_receiver_agent_references() {
        let value = json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-parent",
                "item": {
                    "type": "collabToolCall",
                    "receiver_agents": [
                        { "thread_id": "thread-child-a" },
                        { "thread": { "id": "thread-child-b" } }
                    ],
                    "statuses": {
                        "thread-child-c": { "status": "running" }
                    }
                }
            }
        });
        let ids = extract_related_thread_ids(&value);
        assert!(ids.contains(&"thread-parent".to_string()));
        assert!(ids.contains(&"thread-child-a".to_string()));
        assert!(ids.contains(&"thread-child-b".to_string()));
        assert!(ids.contains(&"thread-child-c".to_string()));
    }

    #[test]
    fn extract_related_thread_ids_reads_singular_receiver_agent_reference() {
        let value = json!({
            "method": "item/completed",
            "params": {
                "threadId": "thread-parent",
                "item": {
                    "type": "mcpToolCall",
                    "receiver_agent": { "thread_id": "thread-child-single" }
                }
            }
        });
        let ids = extract_related_thread_ids(&value);
        assert!(ids.contains(&"thread-parent".to_string()));
        assert!(ids.contains(&"thread-child-single".to_string()));
    }
}
