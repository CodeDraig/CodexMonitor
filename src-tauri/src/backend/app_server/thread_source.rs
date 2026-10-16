use serde_json::Value;

fn normalize_subagent_kind(value: &str) -> String {
    let mut normalized = value.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    if let Some(stripped) = normalized.strip_prefix("subagent_") {
        normalized = stripped.to_string();
    } else if let Some(stripped) = normalized.strip_prefix("sub_agent_") {
        normalized = stripped.to_string();
    }
    normalized
}

pub(super) fn source_subagent_kind(source: &Value) -> Option<String> {
    if let Some(raw) = source.as_str() {
        let normalized = normalize_subagent_kind(raw);
        return if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };
    }
    let source_obj = source.as_object()?;
    let sub_agent = source_obj
        .get("subAgent")
        .or_else(|| source_obj.get("sub_agent"))
        .or_else(|| source_obj.get("subagent"))?;

    if let Some(raw) = sub_agent.as_str() {
        let normalized = normalize_subagent_kind(raw);
        return if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };
    }
    let sub_agent_obj = sub_agent.as_object()?;
    if let Some(explicit) = sub_agent_obj
        .get("kind")
        .or_else(|| sub_agent_obj.get("type"))
        .or_else(|| sub_agent_obj.get("name"))
        .or_else(|| sub_agent_obj.get("id"))
        .and_then(Value::as_str)
    {
        let normalized = normalize_subagent_kind(explicit);
        return if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        };
    }

    let candidate_keys: Vec<&String> = sub_agent_obj
        .keys()
        .filter(|key| key.as_str() != "thread_spawn" && key.as_str() != "threadSpawn")
        .collect();
    if candidate_keys.len() != 1 {
        return None;
    }
    let normalized = normalize_subagent_kind(candidate_keys[0]);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub(super) fn thread_started_is_memory_consolidation(value: &Value) -> bool {
    value
        .get("params")
        .and_then(|params| {
            params
                .get("thread")
                .and_then(|thread| thread.get("source"))
                .or_else(|| params.get("source"))
        })
        .and_then(source_subagent_kind)
        .is_some_and(|kind| kind == "memory_consolidation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_subagent_kind_reads_string_variants() {
        assert_eq!(
            source_subagent_kind(&json!("subagent-memory-consolidation")),
            Some("memory_consolidation".to_string())
        );
        assert_eq!(
            source_subagent_kind(&json!("sub_agent_memory_consolidation")),
            Some("memory_consolidation".to_string())
        );
    }

    #[test]
    fn source_subagent_kind_reads_nested_subagent_object_keys() {
        let source = json!({
            "subAgent": {
                "memory_consolidation": {
                    "thread_spawn": { "parent_thread_id": "thread-parent" }
                }
            }
        });
        assert_eq!(
            source_subagent_kind(&source),
            Some("memory_consolidation".to_string())
        );
    }

    #[test]
    fn thread_started_memory_consolidation_detects_thread_source() {
        let value = json!({
            "method": "thread/started",
            "params": {
                "thread": {
                    "id": "thread-1",
                    "source": {
                        "subagent": "memory_consolidation"
                    }
                }
            }
        });
        assert!(thread_started_is_memory_consolidation(&value));
    }

    #[test]
    fn thread_started_memory_consolidation_detects_params_source_fallback() {
        let value = json!({
            "method": "thread/started",
            "params": {
                "threadId": "thread-1",
                "source": {
                    "subAgent": "memory_consolidation"
                }
            }
        });
        assert!(thread_started_is_memory_consolidation(&value));
    }

    #[test]
    fn thread_started_memory_consolidation_rejects_non_memory_subagent() {
        let value = json!({
            "method": "thread/started",
            "params": {
                "thread": {
                    "id": "thread-1",
                    "source": {
                        "subAgent": "review"
                    }
                }
            }
        });
        assert!(!thread_started_is_memory_consolidation(&value));
    }
}
