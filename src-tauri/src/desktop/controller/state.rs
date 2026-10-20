use crate::client::{BackendScope, ConversationKey, ConversationRef};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct WorkspaceState {
    pub id: String,
    pub path: String,
    pub name: String,
    pub raw: Value,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ThreadSummary {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub path: String,
    pub parent_id: Option<String>,
    pub updated_at: i64,
    pub processing: bool,
    pub unread: bool,
    pub pinned: bool,
    pub hidden: bool,
    pub raw: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CodexControls {
    pub models: Vec<Value>,
    pub collaboration_modes: Vec<Value>,
    pub configured_model: Option<String>,
    pub selected_model: Option<String>,
    pub selected_effort: Option<String>,
    pub selected_access_mode: String,
    pub selected_service_tier: Option<String>,
    pub selected_collaboration_mode: Option<Value>,
}

impl Default for CodexControls {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            collaboration_modes: Vec::new(),
            configured_model: None,
            selected_model: None,
            selected_effort: None,
            selected_access_mode: "current".into(),
            selected_service_tier: None,
            selected_collaboration_mode: None,
        }
    }
}

impl CodexControls {
    pub(crate) fn send_options(&self) -> Value {
        serde_json::json!({
            "model": self.selected_model,
            "effort": self.selected_effort,
            "accessMode": self.selected_access_mode,
            "serviceTier": self.selected_service_tier,
            "collaborationMode": self.selected_collaboration_mode,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct DiscoverySource {
    pub profile_id: String,
    pub cursor: Option<String>,
    pub loading: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub sessions: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ConversationState {
    pub key: Option<ConversationKey>,
    pub title: String,
    pub items: Vec<Value>,
    pub generation: Option<String>,
    /// Advances when an authoritative conversation snapshot replaces prior items.
    /// The shell uses it to restore a scroll anchor after refresh/replay.
    pub history_revision: u64,
    /// Advances for every materialized item mutation; unlike history_revision,
    /// this also covers appends and live event rows and is used by diff caches.
    pub items_revision: u64,
    pub cursor: Option<Value>,
    pub load_state: Option<String>,
    pub coverage: Option<String>,
    pub stale: bool,
    pub read_only: bool,
    pub loading: bool,
    pub busy: bool,
    /// A live Codex completion arrived. Keep queued follow-ups blocked until
    /// its submission acknowledgement and a fresh thread snapshot settle.
    pub completion_ready: bool,
    /// A live event arrived while this conversation was not selected.
    pub unread: bool,
    /// A submit/steer was enqueued but has not been acknowledged yet. A stale
    /// history read must not erase its busy anchor.
    pub pending_submission: bool,
    /// Newly created ACP sessions may not be loadable until their first turn
    /// has been persisted by the agent.
    pub awaiting_first_turn: bool,
    /// True after one automatic connection/replay probe. A manual refresh may
    /// retry the probe when connection startup failed before capabilities arrived.
    pub capability_probe_attempted: bool,
    pub config_popup_open: bool,
    /// A provider configuration update has been enqueued and its reply is pending.
    pub configuration_pending: bool,
    pub last_refresh_ms: Option<u64>,
    pub last_error: Option<String>,
    /// Informational reply produced by a local slash command, separate from errors.
    pub command_output: Option<String>,
    pub draft: String,
    pub queue: VecDeque<QueuedPrompt>,
    pub uncertain_request: Option<Uuid>,
    pub uncertain_prompt: Option<QueuedPrompt>,
    pub pending_execution_id: Option<String>,
    pub active_turn_id: Option<String>,
    pub session: Option<Value>,
    pub permissions: Vec<Value>,
    pub user_input_requests: Vec<Value>,
    /// Client callbacks requested by the currently bound ACP foreground client.
    /// These are distinct from agent permission requests and user-input elicitation.
    pub client_requests: Vec<Value>,
    pub client_connection_id: Option<String>,
    pub client_connection_generation: Option<u64>,
    pub client_requests_pending: bool,
    pub client_requests_waiting: bool,
    pub client_requests_poll_due_ms: u64,
    pub client_requests_error: Option<String>,
}

impl ConversationState {
    /// Copy display/control metadata for the egui shell without copying the
    /// potentially large transcript. Content widgets receive `items` by borrow.
    pub(crate) fn presentation_snapshot(&self) -> Self {
        Self {
            key: self.key.clone(),
            title: self.title.clone(),
            items: Vec::new(),
            generation: self.generation.clone(),
            history_revision: self.history_revision,
            items_revision: self.items_revision,
            cursor: self.cursor.clone(),
            load_state: self.load_state.clone(),
            coverage: self.coverage.clone(),
            stale: self.stale,
            read_only: self.read_only,
            loading: self.loading,
            busy: self.busy,
            completion_ready: self.completion_ready,
            unread: self.unread,
            pending_submission: self.pending_submission,
            awaiting_first_turn: self.awaiting_first_turn,
            capability_probe_attempted: self.capability_probe_attempted,
            config_popup_open: self.config_popup_open,
            configuration_pending: self.configuration_pending,
            last_refresh_ms: self.last_refresh_ms,
            last_error: self.last_error.clone(),
            command_output: self.command_output.clone(),
            draft: self.draft.clone(),
            queue: self
                .queue
                .iter()
                .map(|entry| QueuedPrompt {
                    request_id: entry.request_id,
                    text: entry.text.clone(),
                    content: Vec::new(),
                    options: Value::Null,
                })
                .collect(),
            uncertain_request: self.uncertain_request,
            // The composer does not render a retry payload; avoid copying its
            // potentially large text, attachments, and options into UI snapshots.
            uncertain_prompt: None,
            pending_execution_id: self.pending_execution_id.clone(),
            active_turn_id: self.active_turn_id.clone(),
            session: self.session.clone(),
            permissions: self.permissions.clone(),
            user_input_requests: self.user_input_requests.clone(),
            client_requests: self.client_requests.clone(),
            client_connection_id: self.client_connection_id.clone(),
            client_connection_generation: self.client_connection_generation,
            client_requests_pending: self.client_requests_pending,
            client_requests_waiting: self.client_requests_waiting,
            client_requests_poll_due_ms: self.client_requests_poll_due_ms,
            client_requests_error: self.client_requests_error.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct QueuedPrompt {
    pub request_id: Uuid,
    pub text: String,
    pub content: Vec<Value>,
    pub options: Value,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ControllerState {
    pub scope: Option<BackendScope>,
    pub runtime_store_id: Option<String>,
    pub workspaces: Vec<WorkspaceState>,
    pub profiles: Vec<Value>,
    pub catalog: Vec<Value>,
    pub discovery: HashMap<String, DiscoverySource>,
    pub threads_by_workspace: HashMap<String, Vec<ThreadSummary>>,
    pub codex_controls_by_workspace: HashMap<String, CodexControls>,
    /// MCP server elicitation requests are workspace-level App Server requests;
    /// they need not identify a thread.
    pub codex_elicitations_by_workspace: HashMap<String, Vec<Value>>,
    pub thread_cursor_by_workspace: HashMap<String, Option<String>>,
    pub thread_loading_by_workspace: HashSet<String>,
    pub hidden_threads_by_workspace: HashMap<String, HashSet<String>>,
    pub parents_by_thread: HashMap<(String, String), String>,
    pub active_thread_by_workspace: HashMap<String, String>,
    pub selected: Option<ConversationKey>,
    pub conversations: HashMap<ConversationKey, ConversationState>,
    pub error: Option<String>,
    pub initialized: bool,
    pub browser_open: bool,
    pub review_prompt_open: bool,
}

impl ControllerState {
    pub(crate) fn select(&mut self, key: ConversationKey) {
        self.selected = Some(key.clone());
        let conversation =
            self.conversations
                .entry(key.clone())
                .or_insert_with(|| ConversationState {
                    key: Some(key),
                    ..ConversationState::default()
                });
        conversation.unread = false;
        if let Some(ConversationKey {
            conversation:
                ConversationRef::Codex {
                    workspace_id,
                    thread_id,
                },
            ..
        }) = &self.selected
        {
            self.active_thread_by_workspace
                .insert(workspace_id.clone(), thread_id.clone());
            if let Some(summary) = self
                .threads_by_workspace
                .get_mut(workspace_id)
                .and_then(|rows| rows.iter_mut().find(|row| row.id == *thread_id))
            {
                summary.unread = false;
            }
        }
    }

    pub(crate) fn selected_conversation(&self) -> Option<&ConversationState> {
        self.selected
            .as_ref()
            .and_then(|key| self.conversations.get(key))
    }

    pub(crate) fn selected_conversation_mut(&mut self) -> Option<&mut ConversationState> {
        self.selected
            .clone()
            .and_then(|key| self.conversations.get_mut(&key))
    }

    /// Reconciles a partial thread-list response while retaining ordered local anchors.
    /// Hidden IDs win over every incoming payload.
    pub(crate) fn reconcile_threads(
        &mut self,
        workspace_id: &str,
        incoming: Vec<ThreadSummary>,
        processing_ids: &HashSet<String>,
    ) {
        let hidden = self
            .hidden_threads_by_workspace
            .entry(workspace_id.to_owned())
            .or_default();
        let old = self
            .threads_by_workspace
            .get(workspace_id)
            .cloned()
            .unwrap_or_default();
        let mut seen = HashSet::new();
        let mut ordered: Vec<_> = incoming
            .into_iter()
            .filter(|thread| {
                !thread.id.is_empty()
                    && !hidden.contains(&thread.id)
                    && seen.insert(thread.id.clone())
            })
            .collect();
        let active = self.active_thread_by_workspace.get(workspace_id);
        let required: HashSet<String> = active
            .into_iter()
            .cloned()
            .chain(processing_ids.iter().cloned())
            .chain(
                old.iter()
                    .filter(|t| t.processing || active == Some(&t.id))
                    .map(|t| t.id.clone()),
            )
            .collect();
        let mut ancestors = HashSet::new();
        for id in required.iter() {
            let mut current = id.as_str();
            while let Some(parent) = self
                .parents_by_thread
                .get(&(workspace_id.to_owned(), current.to_owned()))
            {
                if !ancestors.insert(parent.clone()) {
                    break;
                }
                current = parent;
            }
        }
        // Local anchors are appended only when omitted by a partial payload.
        for row in old
            .into_iter()
            .filter(|row| required.contains(&row.id) || ancestors.contains(&row.id))
        {
            if !hidden.contains(&row.id) && seen.insert(row.id.clone()) {
                ordered.push(row);
            }
        }
        // Required active/processing IDs omitted entirely still remain represented.
        for id in required {
            if !hidden.contains(&id) && seen.insert(id.clone()) {
                ordered.push(ThreadSummary {
                    id: id.clone(),
                    workspace_id: workspace_id.to_owned(),
                    processing: processing_ids.contains(&id),
                    ..ThreadSummary::default()
                });
            }
        }
        self.threads_by_workspace
            .insert(workspace_id.to_owned(), ordered);
        let present: HashSet<String> = self
            .threads_by_workspace
            .get(workspace_id)
            .into_iter()
            .flatten()
            .map(|t| t.id.clone())
            .collect();
        // Keep parent links, but only present parents are used by the view to nest children.
        for thread in self
            .threads_by_workspace
            .get_mut(workspace_id)
            .into_iter()
            .flatten()
        {
            if let Some(parent) = self
                .parents_by_thread
                .get(&(workspace_id.to_owned(), thread.id.clone()))
            {
                thread.parent_id = present.contains(parent).then(|| parent.clone());
            }
        }
    }

    pub(crate) fn hide_thread(&mut self, workspace_id: &str, thread_id: &str) {
        self.hidden_threads_by_workspace
            .entry(workspace_id.to_owned())
            .or_default()
            .insert(thread_id.to_owned());
        if let Some(rows) = self.threads_by_workspace.get_mut(workspace_id) {
            rows.retain(|row| row.id != thread_id);
        }
    }

    pub(crate) fn key_for_ref(&self, reference: ConversationRef) -> Option<ConversationKey> {
        self.scope.as_ref().map(|scope| ConversationKey {
            endpoint_id: scope.endpoint_id.clone(),
            conversation: reference,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str) -> ThreadSummary {
        ThreadSummary {
            id: id.into(),
            workspace_id: "w".into(),
            title: id.into(),
            ..ThreadSummary::default()
        }
    }

    #[test]
    fn partial_thread_refresh_keeps_order_and_required_anchors_but_hidden_ids_win() {
        let mut state = ControllerState::default();
        state.threads_by_workspace.insert(
            "w".into(),
            vec![row("active"), row("processing"), row("old")],
        );
        state
            .active_thread_by_workspace
            .insert("w".into(), "active".into());
        state.hide_thread("w", "hidden");
        state
            .threads_by_workspace
            .get_mut("w")
            .unwrap()
            .push(row("hidden"));
        let processing = HashSet::from(["processing".to_owned()]);
        state.reconcile_threads(
            "w",
            vec![row("new-a"), row("new-b"), row("hidden")],
            &processing,
        );
        let ids: Vec<_> = state.threads_by_workspace["w"]
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, ["new-a", "new-b", "active", "processing"]);
    }

    #[test]
    fn children_are_roots_when_parent_is_not_in_the_visible_snapshot() {
        let mut state = ControllerState::default();
        state
            .parents_by_thread
            .insert(("w".into(), "child".into()), "parent".into());
        state.reconcile_threads("w", vec![row("child")], &HashSet::new());
        assert_eq!(state.threads_by_workspace["w"][0].parent_id, None);
        state.reconcile_threads("w", vec![row("parent"), row("child")], &HashSet::new());
        assert_eq!(
            state.threads_by_workspace["w"][1].parent_id.as_deref(),
            Some("parent")
        );
    }

    #[test]
    fn conversation_state_and_draft_keys_include_endpoint_store_and_native_session() {
        let make = |endpoint: &str, store: &str, session: &str| ConversationKey {
            endpoint_id: endpoint.into(),
            conversation: ConversationRef::Acp {
                workspace_id: "w".into(),
                store_id: store.into(),
                session_id: session.into(),
            },
        };
        let mut state = ControllerState::default();
        let a = make("local", "store-a", "native-1");
        let b = make("daemon", "store-b", "native-1");
        state.select(a.clone());
        state.selected_conversation_mut().unwrap().draft = "local draft".into();
        state.select(b.clone());
        state.selected_conversation_mut().unwrap().draft = "daemon draft".into();
        assert_eq!(state.conversations[&a].draft, "local draft");
        assert_eq!(state.conversations[&b].draft, "daemon draft");
    }

    #[test]
    fn codex_control_selection_is_materialized_as_turn_options() {
        let controls = CodexControls {
            selected_model: Some("model-2".into()),
            selected_effort: Some("high".into()),
            selected_access_mode: "read-only".into(),
            selected_service_tier: Some("fast".into()),
            selected_collaboration_mode: Some(serde_json::json!({"mode":"plan"})),
            ..CodexControls::default()
        };
        assert_eq!(
            controls.send_options(),
            serde_json::json!({
                "model":"model-2",
                "effort":"high",
                "accessMode":"read-only",
                "serviceTier":"fast",
                "collaborationMode":{"mode":"plan"}
            })
        );
    }

    #[test]
    fn presentation_snapshot_keeps_metadata_without_cloning_conversation_items() {
        let conversation = ConversationState {
            title: "a conversation".into(),
            items: vec![Value::String("large history row".into()); 10_000],
            generation: Some("generation-4".into()),
            history_revision: 9,
            items_revision: 12,
            busy: true,
            draft: "draft remains local".into(),
            queue: VecDeque::from([QueuedPrompt {
                request_id: Uuid::new_v4(),
                text: "show queued text".into(),
                content: vec![serde_json::json!({"type":"image","data":"base64-payload"})],
                options: serde_json::json!({"large":"option payload sentinel"}),
            }]),
            uncertain_prompt: Some(QueuedPrompt {
                request_id: Uuid::new_v4(),
                text: "uncertain retry body".into(),
                content: vec![serde_json::json!({"type":"image","data":"retry-attachment"})],
                options: serde_json::json!({"option":"uncertain"}),
            }),
            ..ConversationState::default()
        };

        let snapshot = conversation.presentation_snapshot();

        assert!(snapshot.items.is_empty());
        assert_eq!(snapshot.title, "a conversation");
        assert_eq!(snapshot.generation.as_deref(), Some("generation-4"));
        assert_eq!(snapshot.history_revision, 9);
        assert_eq!(snapshot.items_revision, 12);
        assert!(snapshot.busy);
        assert_eq!(snapshot.draft, "draft remains local");
        assert_eq!(snapshot.queue.len(), 1);
        assert_eq!(snapshot.queue[0].text, "show queued text");
        assert_eq!(
            snapshot.queue[0].request_id,
            conversation.queue[0].request_id
        );
        assert!(snapshot.queue[0].content.is_empty());
        assert_eq!(snapshot.queue[0].options, Value::Null);
        assert!(snapshot.uncertain_prompt.is_none());
        assert_eq!(conversation.items.len(), 10_000);
    }
}
