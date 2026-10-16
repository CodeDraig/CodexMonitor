use std::collections::{HashMap, HashSet};

use serde_json::Value;

#[derive(Default)]
pub(crate) struct SessionActivity {
    pub(crate) retired: bool,
    requests: HashMap<u64, (bool, Option<String>)>,
    active_threads: HashSet<String>,
    completed_threads: HashSet<String>,
}

impl SessionActivity {
    pub(crate) fn is_busy(&self) -> bool {
        !self.requests.is_empty() || !self.active_threads.is_empty()
    }

    pub(crate) fn begin_request(
        &mut self,
        id: u64,
        method: &str,
        thread_id: Option<String>,
    ) -> Result<(), String> {
        if self.retired {
            return Err("Codex session was replaced; retry the request".to_string());
        }
        let starts_turn = matches!(
            method,
            "turn/start" | "review/start" | "thread/compact/start"
        );
        if starts_turn {
            if let Some(thread) = thread_id.as_ref() {
                self.completed_threads.remove(thread);
            }
        }
        self.requests.insert(id, (starts_turn, thread_id));
        Ok(())
    }

    fn complete_thread(&mut self, id: &str) {
        self.active_threads.remove(id);
        if !self.requests.is_empty() {
            self.completed_threads.insert(id.to_owned());
        }
    }

    // Called before routing/filtering events so background and hidden threads count too.
    pub(crate) fn observe(&mut self, value: &Value, thread_id: Option<String>) {
        match value.get("method").and_then(Value::as_str) {
            Some("turn/started") => {
                if let Some(id) = thread_id.as_ref() {
                    self.active_threads.insert(id.clone());
                }
            }
            Some("turn/completed") => {
                if let Some(id) = thread_id.as_ref() {
                    self.complete_thread(id);
                }
            }
            Some("thread/status/changed") => {
                if let Some(id) = thread_id.as_ref() {
                    let status = value
                        .pointer("/params/status/type")
                        .or_else(|| value.pointer("/params/status"))
                        .and_then(Value::as_str);
                    match status {
                        Some("active") => {
                            self.active_threads.insert(id.clone());
                        }
                        Some("idle" | "notLoaded") => self.complete_thread(id),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        // Resuming an already-running thread need not emit another turn/started.
        if value
            .pointer("/result/thread/status/type")
            .and_then(Value::as_str)
            == Some("active")
        {
            if let Some(id) = thread_id.as_ref() {
                self.active_threads.insert(id.clone());
            }
        }
        if value.get("result").is_some() || value.get("error").is_some() {
            if let Some(id) = value.get("id").and_then(Value::as_u64) {
                if let Some((starts_turn, requested_thread)) = self.requests.remove(&id) {
                    // A response may precede turn/started, or follow turn/completed.
                    let response_thread = value
                        .pointer("/result/reviewThreadId")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(thread_id)
                        .or(requested_thread);
                    let terminal = matches!(
                        value.pointer("/result/turn/status").and_then(Value::as_str),
                        Some("completed" | "interrupted" | "failed")
                    );
                    if starts_turn && value.get("error").is_none() && !terminal {
                        if let Some(thread) = response_thread {
                            if !self.completed_threads.contains(&thread) {
                                self.active_threads.insert(thread);
                            }
                        }
                    }
                }
                if self.requests.is_empty() {
                    self.completed_threads.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pending_start_and_response_before_notification_keep_session_busy() {
        let mut state = SessionActivity::default();
        state
            .begin_request(1, "turn/start", Some("thread-other-workspace".into()))
            .unwrap();
        assert!(state.is_busy());
        state.observe(
            &json!({"id":1,"result":{"turn":{"status":"inProgress"}}}),
            None,
        );
        assert!(state.is_busy());
        state.observe(
            &json!({"method":"turn/completed"}),
            Some("thread-other-workspace".into()),
        );
        assert!(!state.is_busy());
    }

    #[test]
    fn completion_before_response_does_not_resurrect_activity() {
        let mut state = SessionActivity::default();
        state
            .begin_request(1, "turn/start", Some("t".into()))
            .unwrap();
        state.observe(&json!({"method":"turn/started"}), Some("t".into()));
        state.observe(&json!({"method":"turn/completed"}), Some("t".into()));
        assert!(state.is_busy(), "response is still pending");
        state.observe(
            &json!({"id":1,"result":{"turn":{"status":"inProgress"}}}),
            None,
        );
        assert!(!state.is_busy());
    }

    #[test]
    fn failed_request_does_not_clear_an_existing_turn() {
        let mut state = SessionActivity::default();
        state.observe(&json!({"method":"turn/started"}), Some("t".into()));
        state
            .begin_request(1, "turn/start", Some("t".into()))
            .unwrap();
        state.observe(&json!({"id":1,"error":{"message":"already running"}}), None);
        assert!(state.is_busy());
        state.observe(&json!({"method":"turn/completed"}), Some("t".into()));
        assert!(!state.is_busy());
    }

    #[test]
    fn resumed_active_thread_and_compaction_stay_busy_until_idle() {
        let mut state = SessionActivity::default();
        state
            .begin_request(1, "thread/resume", Some("t".into()))
            .unwrap();
        state.observe(
            &json!({"id":1,"result":{"thread":{"status":{"type":"active"}}}}),
            Some("t".into()),
        );
        assert!(state.is_busy());
        state.observe(
            &json!({"method":"thread/status/changed","params":{"status":{"type":"idle"}}}),
            Some("t".into()),
        );
        assert!(!state.is_busy());
        state
            .begin_request(2, "thread/compact/start", Some("t".into()))
            .unwrap();
        state.observe(&json!({"id":2,"result":{}}), None);
        assert!(state.is_busy());
        state.observe(
            &json!({"method":"thread/status/changed","params":{"status":"idle"}}),
            Some("t".into()),
        );
        assert!(!state.is_busy());
    }

    #[test]
    fn detached_review_tracks_its_own_thread() {
        let mut state = SessionActivity::default();
        state
            .begin_request(1, "review/start", Some("source".into()))
            .unwrap();
        state.observe(&json!({"id":1,"result":{"reviewThreadId":"review"}}), None);
        state.observe(&json!({"method":"turn/completed"}), Some("review".into()));
        assert!(!state.is_busy());
    }

    #[test]
    fn pending_queries_block_replacement_and_retired_sessions_reject_requests() {
        let mut state = SessionActivity::default();
        state.begin_request(1, "thread/list", None).unwrap();
        assert!(state.is_busy());
        state.observe(&json!({"id":1,"result":{}}), None);
        assert!(!state.is_busy());
        state.retired = true;
        assert!(state
            .begin_request(2, "turn/start", Some("t".into()))
            .is_err());
    }
}
