//! Codex conversation presentation preferences shared between the native shell
//! and its versioned UI-state store.

use crate::{
    client::{ConversationKey, ConversationRef},
    shared::ui_state_core::UiStateV1,
};
use serde_json::{json, Value};

const PROMPT_HISTORY_LIMIT: usize = 200;

/// The Codex controls which belong to a conversation rather than a workspace.
/// Optional values deliberately preserve `null`, matching the legacy browser
/// state and allowing an explicit return to an agent default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CodexThreadParams {
    pub model_id: Option<String>,
    pub effort: Option<String>,
    pub access_mode: Option<String>,
    pub service_tier: Option<String>,
    pub collaboration_mode_id: Option<String>,
}

impl CodexThreadParams {
    pub(crate) fn from_selection(
        model_id: Option<String>,
        effort: Option<String>,
        access_mode: Option<String>,
        service_tier: Option<String>,
        collaboration_mode: Option<&Value>,
    ) -> Self {
        let collaboration_mode_id = collaboration_mode.and_then(|mode| {
            mode.get("id")
                .or_else(|| mode.get("mode"))
                .or_else(|| mode.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        Self {
            model_id,
            effort,
            access_mode,
            service_tier,
            collaboration_mode_id,
        }
    }

    fn to_value(&self) -> Value {
        json!({
            "modelId": self.model_id,
            "effort": self.effort,
            "accessMode": self.access_mode,
            "serviceTier": self.service_tier,
            "collaborationModeId": self.collaboration_mode_id,
        })
    }

    fn from_value(value: &Value) -> Self {
        let string = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Self {
            model_id: string("modelId"),
            effort: string("effort"),
            access_mode: string("accessMode"),
            service_tier: string("serviceTier"),
            collaboration_mode_id: string("collaborationModeId"),
        }
    }
}

/// Stable native key and the exact pre-native `workspace:thread` fallback.
/// The fallback supports state imported from the React frontend; all new writes
/// use the endpoint-qualified native key to avoid local/remote collisions.
pub(crate) fn codex_presentation_keys(key: &ConversationKey) -> Option<(String, String)> {
    let ConversationRef::Codex {
        workspace_id,
        thread_id,
    } = &key.conversation
    else {
        return None;
    };
    Some((
        format!("codex:{}:{workspace_id}:{thread_id}", key.endpoint_id),
        format!("{workspace_id}:{thread_id}"),
    ))
}

pub(crate) fn load_codex_params(
    state: &UiStateV1,
    key: &ConversationKey,
) -> Option<CodexThreadParams> {
    let (native_key, legacy_key) = codex_presentation_keys(key)?;
    state
        .thread_codex_params
        .get(&native_key)
        .or_else(|| state.thread_codex_params.get(&legacy_key))
        .map(CodexThreadParams::from_value)
}

pub(crate) fn save_codex_params(
    state: &mut UiStateV1,
    key: &ConversationKey,
    params: CodexThreadParams,
) -> bool {
    let Some((native_key, _)) = codex_presentation_keys(key) else {
        return false;
    };
    state
        .thread_codex_params
        .insert(native_key, params.to_value());
    true
}

fn prompt_history_keys(key: &ConversationKey) -> Option<(String, Option<String>)> {
    match &key.conversation {
        ConversationRef::Codex { .. } => {
            let (native_key, legacy_key) = codex_presentation_keys(key)?;
            Some((format!("prompt:{native_key}"), Some(legacy_key)))
        }
        ConversationRef::Acp {
            store_id,
            session_id,
            ..
        } => {
            // JSON encoding keeps arbitrary native IDs unambiguous while
            // scoping history to the backend, store, and session.
            let identity = serde_json::to_string(&[
                key.endpoint_id.as_str(),
                store_id.as_str(),
                session_id.as_str(),
            ])
            .ok()?;
            Some((format!("prompt:acp:{identity}"), None))
        }
    }
}

/// Persist a submitted prompt under its conversation identity. This mirrors
/// the browser composer: trim, ignore empty values and consecutive duplicates,
/// and retain the newest 200 prompts.
pub(crate) fn record_prompt(state: &mut UiStateV1, key: &ConversationKey, prompt: &str) -> bool {
    let Some((native_key, legacy_key)) = prompt_history_keys(key) else {
        return false;
    };
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return false;
    }
    let migrated = legacy_key
        .as_deref()
        .and_then(|legacy_key| state.prompt_history.get(legacy_key))
        .cloned()
        .unwrap_or_default();
    let history = state.prompt_history.entry(native_key).or_insert(migrated);
    if history.last().is_some_and(|last| last == prompt) {
        return false;
    }
    history.push(prompt.to_owned());
    if history.len() > PROMPT_HISTORY_LIMIT {
        let excess = history.len() - PROMPT_HISTORY_LIMIT;
        history.drain(..excess);
    }
    true
}

pub(crate) fn prompt_history(state: &UiStateV1, key: &ConversationKey) -> Vec<String> {
    let Some((native_key, legacy_key)) = prompt_history_keys(key) else {
        return Vec::new();
    };
    state
        .prompt_history
        .get(&native_key)
        .or_else(|| {
            legacy_key
                .as_deref()
                .and_then(|key| state.prompt_history.get(key))
        })
        .cloned()
        .unwrap_or_default()
}

/// State for the composer's familiar Up/Down prompt recall. The caller should
/// invoke `previous` only when the draft is empty (unless already navigating).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PromptHistoryCursor {
    index: Option<usize>,
    draft_before_history: String,
}

impl PromptHistoryCursor {
    pub(crate) fn is_navigating(&self) -> bool {
        self.index.is_some()
    }

    pub(crate) fn previous(&mut self, history: &[String], current_draft: &str) -> Option<String> {
        if history.is_empty() || (self.index.is_none() && !current_draft.is_empty()) {
            return None;
        }
        let index = self
            .index
            .map(|current| current.saturating_sub(1))
            .unwrap_or_else(|| {
                self.draft_before_history = current_draft.to_owned();
                history.len() - 1
            });
        self.index = Some(index);
        history.get(index).cloned()
    }

    pub(crate) fn next(&mut self, history: &[String]) -> Option<String> {
        let current = self.index?;
        if current + 1 >= history.len() {
            self.index = None;
            return Some(std::mem::take(&mut self.draft_before_history));
        }
        self.index = Some(current + 1);
        history.get(current + 1).cloned()
    }

    pub(crate) fn reset(&mut self) {
        self.index = None;
        self.draft_before_history.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_key(endpoint: &str, workspace: &str, thread: &str) -> ConversationKey {
        ConversationKey {
            endpoint_id: endpoint.into(),
            conversation: ConversationRef::Codex {
                workspace_id: workspace.into(),
                thread_id: thread.into(),
            },
        }
    }

    #[test]
    fn native_keys_isolate_endpoints_and_read_legacy_fallback() {
        let local = codex_key("local", "w", "t");
        let remote = codex_key("remote", "w", "t");
        let mut state = UiStateV1::empty();
        let mut params = CodexThreadParams::default();
        params.model_id = Some("new-model".into());
        assert!(save_codex_params(&mut state, &local, params));
        assert_eq!(load_codex_params(&state, &remote), None);

        state
            .thread_codex_params
            .insert("w:t".into(), json!({"modelId":"legacy","effort":"high"}));
        assert_eq!(
            load_codex_params(&state, &remote),
            Some(CodexThreadParams {
                model_id: Some("legacy".into()),
                effort: Some("high".into()),
                ..CodexThreadParams::default()
            })
        );
        assert_eq!(
            load_codex_params(&state, &local)
                .unwrap()
                .model_id
                .as_deref(),
            Some("new-model")
        );
    }

    #[test]
    fn acp_history_is_scoped_and_codex_params_remain_codex_only() {
        let acp_key = |endpoint: &str, store: &str, session: &str| ConversationKey {
            endpoint_id: endpoint.into(),
            conversation: ConversationRef::Acp {
                workspace_id: "w".into(),
                store_id: store.into(),
                session_id: session.into(),
            },
        };
        let first = acp_key("local", "one", "same:id");
        let same_session_other_store = acp_key("local", "two", "same:id");
        let same_store_other_endpoint = acp_key("daemon", "one", "same:id");
        let mut state = UiStateV1::empty();
        assert!(!save_codex_params(
            &mut state,
            &first,
            CodexThreadParams::default()
        ));
        assert!(record_prompt(&mut state, &first, " prompt "));
        assert_eq!(prompt_history(&state, &first), vec!["prompt"]);
        assert!(prompt_history(&state, &same_session_other_store).is_empty());
        assert!(prompt_history(&state, &same_store_other_endpoint).is_empty());
        assert!(state.thread_codex_params.is_empty());
        assert!(state
            .prompt_history
            .keys()
            .all(|key| key.starts_with("prompt:acp:")));
    }

    #[test]
    fn prompt_history_trims_deduplicates_and_recalls_scratch_draft() {
        let key = codex_key("local", "w", "t");
        let mut state = UiStateV1::empty();
        assert!(!record_prompt(&mut state, &key, "   "));
        assert!(record_prompt(&mut state, &key, " first "));
        assert!(!record_prompt(&mut state, &key, "first"));
        assert!(record_prompt(&mut state, &key, "second"));

        let history = prompt_history(&state, &key);
        let mut cursor = PromptHistoryCursor::default();
        assert!(!cursor.is_navigating());
        assert_eq!(cursor.previous(&history, ""), Some("second".into()));
        assert!(cursor.is_navigating());
        assert_eq!(cursor.previous(&history, "second"), Some("first".into()));
        assert_eq!(cursor.previous(&history, "first"), Some("first".into()));
        assert_eq!(cursor.next(&history), Some("second".into()));
        assert_eq!(cursor.next(&history), Some(String::new()));
        assert!(!cursor.is_navigating());
        assert_eq!(cursor.next(&history), None);

        let mut cursor = PromptHistoryCursor::default();
        assert_eq!(cursor.previous(&history, "unsent draft"), None);
    }

    #[test]
    fn prompt_history_keeps_only_the_newest_two_hundred() {
        let key = codex_key("local", "w", "t");
        let mut state = UiStateV1::empty();
        for index in 0..205 {
            record_prompt(&mut state, &key, &format!("prompt-{index}"));
        }
        let history = prompt_history(&state, &key);
        assert_eq!(history.len(), PROMPT_HISTORY_LIMIT);
        assert_eq!(history.first().map(String::as_str), Some("prompt-5"));
        assert_eq!(history.last().map(String::as_str), Some("prompt-204"));
    }
}
