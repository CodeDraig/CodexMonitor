use super::thread_source::source_subagent_kind;
use serde_json::Value;

#[derive(Debug, Clone)]
pub(super) struct ThreadListEntry {
    pub(super) thread_id: String,
    pub(super) cwd: Option<String>,
    pub(super) is_memory_consolidation: bool,
}

pub(super) fn extract_thread_entries_from_thread_list_result(
    value: &Value,
) -> Vec<ThreadListEntry> {
    fn collect_entries(input: &Value, out: &mut Vec<ThreadListEntry>) {
        if let Some(values) = input.as_array() {
            for value in values {
                collect_entries(value, out);
            }
            return;
        }
        let Some(object) = input.as_object() else {
            return;
        };

        let cwd = object
            .get("cwd")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .or_else(|| {
                object
                    .get("thread")
                    .and_then(|thread| thread.get("cwd"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            });

        let thread_id = object
            .get("threadId")
            .or_else(|| object.get("thread_id"))
            .or_else(|| object.get("id"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .or_else(|| {
                object
                    .get("thread")
                    .and_then(|thread| thread.get("id"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            });
        if let Some(thread_id) = thread_id {
            let source = object
                .get("source")
                .or_else(|| object.get("thread").and_then(|thread| thread.get("source")));
            let is_memory_consolidation = source
                .and_then(source_subagent_kind)
                .is_some_and(|kind| kind == "memory_consolidation");
            out.push(ThreadListEntry {
                thread_id,
                cwd,
                is_memory_consolidation,
            });
        }

        for key in ["threads", "items", "results", "data"] {
            if let Some(values) = object.get(key).and_then(|value| value.as_array()) {
                for value in values {
                    collect_entries(value, out);
                }
            }
        }
    }

    let mut out = Vec::new();
    if let Some(result) = value.get("result") {
        collect_entries(result, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_thread_entries_reads_result_data_items() {
        let value = json!({
            "result": {
                "data": [
                    { "id": "thread-a", "cwd": "/tmp/a" },
                    {
                        "threadId": "thread-b",
                        "cwd": "/tmp/b",
                        "source": { "subAgent": "memory_consolidation" }
                    }
                ]
            }
        });
        let entries = extract_thread_entries_from_thread_list_result(&value);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].thread_id, "thread-a");
        assert_eq!(entries[0].cwd.as_deref(), Some("/tmp/a"));
        assert!(!entries[0].is_memory_consolidation);
        assert_eq!(entries[1].thread_id, "thread-b");
        assert_eq!(entries[1].cwd.as_deref(), Some("/tmp/b"));
        assert!(entries[1].is_memory_consolidation);
    }
}
