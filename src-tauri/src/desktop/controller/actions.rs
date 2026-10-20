use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) enum ControllerAction {
    Initialize,
    RefreshWorkspaces,
    RefreshCatalog,
    DiscoverAll,
    DiscoverProfile {
        profile_id: String,
        cursor: Option<String>,
    },
    AddDiscoveryPage {
        profile_id: String,
    },
    OpenSessionById {
        profile_id: String,
        cwd: String,
        native_session_id: String,
    },
    OpenCatalog {
        catalog_id: String,
        add_workspace: bool,
    },
    CreateConversation {
        workspace_id: String,
        profile_id: Option<String>,
        prepared_connection: Option<(String, u64)>,
    },
    SelectCodex {
        workspace_id: String,
        thread_id: String,
    },
    LoadWorkspaceThreads {
        workspace_id: String,
    },
    LoadMoreThreads {
        workspace_id: String,
    },
    LoadCodexControls {
        workspace_id: String,
    },
    SetCodexOption {
        workspace_id: String,
        name: String,
        value: Value,
    },
    SelectAcp {
        workspace_id: String,
        store_id: String,
        session_id: String,
    },
    RefreshSelected,
    LoadOlderHistory,
    SetDraft(String),
    Send {
        content: Vec<Value>,
        options: Value,
        steer: bool,
    },
    RemoveQueued(Uuid),
    Stop,
    CheckReceipt,
    Permission {
        request_id: Value,
        option_id: String,
    },
    CodexUserInput {
        request_id: Value,
        answers: Value,
    },
    CodexElicitation {
        workspace_id: String,
        request_id: Value,
        result: Value,
    },
    RespondClientRequest {
        key: crate::client::ConversationKey,
        client_request_id: Value,
        decision: Value,
    },
    Configure {
        option_id: String,
        value: String,
    },
    SetConfigurationPopup(bool),
    SetReviewPromptOpen(bool),
    UpdateMetadata {
        session_id: String,
        expected_revision: i64,
        patch: Value,
    },
    HideThread {
        workspace_id: String,
        thread_id: String,
    },
    SetVisible(bool),
    /// Used by side panels; replies not consumed by the conversation controller are surfaced to the shell.
    Request(crate::client::BackendRequest),
}
