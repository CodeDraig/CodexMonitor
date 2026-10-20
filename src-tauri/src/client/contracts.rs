use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BackendScope {
    pub endpoint_id: String,
    pub connection_epoch: u64,
    pub store_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum ConversationRef {
    Codex {
        workspace_id: String,
        thread_id: String,
    },
    Acp {
        workspace_id: String,
        store_id: String,
        session_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConversationKey {
    pub endpoint_id: String,
    pub conversation: ConversationRef,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestSafety {
    ReadOnly,
    Mutation { request_id: Option<Uuid> },
}

#[derive(Clone, Debug)]
pub(crate) struct RpcRequest {
    pub method: String,
    pub params: Value,
    pub safety: RequestSafety,
}

impl RpcRequest {
    pub(crate) fn read(method: impl Into<String>, params: Value) -> Self {
        Self {
            method: method.into(),
            params,
            safety: RequestSafety::ReadOnly,
        }
    }

    pub(crate) fn mutation(
        method: impl Into<String>,
        params: Value,
        request_id: Option<Uuid>,
    ) -> Self {
        Self {
            method: method.into(),
            params,
            safety: RequestSafety::Mutation { request_id },
        }
    }
}

macro_rules! rpc_category {
    ($name:ident) => {
        #[derive(Clone, Debug)]
        pub(crate) struct $name(pub RpcRequest);
        impl $name {
            pub(crate) fn read(method: impl Into<String>, params: Value) -> Self {
                Self(RpcRequest::read(method, params))
            }
            pub(crate) fn mutation(
                method: impl Into<String>,
                params: Value,
                request_id: Option<Uuid>,
            ) -> Self {
                Self(RpcRequest::mutation(method, params, request_id))
            }
        }
    };
}

rpc_category!(WorkspaceRequest);
rpc_category!(CodexRequest);
rpc_category!(RunsRequest);
rpc_category!(GitRequest);
rpc_category!(FilesRequest);
rpc_category!(PromptsRequest);
rpc_category!(SettingsRequest);

impl CodexRequest {
    /// Set one Codex feature flag using the stable Tauri/daemon method shape.
    pub(crate) fn set_feature_flag(feature_key: impl Into<String>, enabled: bool) -> Self {
        Self::mutation(
            "set_codex_feature_flag",
            serde_json::json!({"featureKey":feature_key.into(),"enabled":enabled}),
            None,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuntimeMethod {
    Profiles,
    Catalog,
    DiscoverSessions,
    OpenSessionById,
    AttachSession,
    LoadConversationHistory,
    ReadConversation,
    ReadConversationAttachment,
    UpdateConversationMetadata,
    SaveProfile,
    Session,
    Sessions,
    CreateSession,
    LoadSession,
    Submit,
    Execution,
    History,
    Events,
    Steer,
    Cancel,
    Permission,
    Configure,
    Disconnect,
    Capture,
}

impl RuntimeMethod {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Profiles => "profiles",
            Self::Catalog => "catalog",
            Self::DiscoverSessions => "discover_sessions",
            Self::OpenSessionById => "open_session_by_id",
            Self::AttachSession => "attach_session",
            Self::LoadConversationHistory => "load_conversation_history",
            Self::ReadConversation => "read_conversation",
            Self::ReadConversationAttachment => "read_conversation_attachment",
            Self::UpdateConversationMetadata => "update_conversation_metadata",
            Self::SaveProfile => "save_profile",
            Self::Session => "session",
            Self::Sessions => "sessions",
            Self::CreateSession => "create_session",
            Self::LoadSession => "load_session",
            Self::Submit => "submit",
            Self::Execution => "execution",
            Self::History => "history",
            Self::Events => "events",
            Self::Steer => "steer",
            Self::Cancel => "cancel",
            Self::Permission => "permission",
            Self::Configure => "configure",
            Self::Disconnect => "disconnect",
            Self::Capture => "capture",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeRequest {
    pub method: RuntimeMethod,
    pub params: Value,
    pub safety: RequestSafety,
}

impl RuntimeRequest {
    pub(crate) fn read(method: RuntimeMethod, params: Value) -> Self {
        Self {
            method,
            params,
            safety: RequestSafety::ReadOnly,
        }
    }
    pub(crate) fn mutation(method: RuntimeMethod, params: Value, request_id: Option<Uuid>) -> Self {
        Self {
            method,
            params,
            safety: RequestSafety::Mutation { request_id },
        }
    }
    pub(crate) fn as_rpc(&self) -> RpcRequest {
        RpcRequest {
            method: self.method.as_str().into(),
            params: self.params.clone(),
            safety: self.safety,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum BackendRequest {
    Workspaces(WorkspaceRequest),
    Codex(CodexRequest),
    Runtime(RuntimeRequest),
    Runs(RunsRequest),
    Git(GitRequest),
    Files(FilesRequest),
    Prompts(PromptsRequest),
    Settings(SettingsRequest),
}

impl BackendRequest {
    pub(crate) fn rpc(&self) -> RpcRequest {
        match self {
            Self::Workspaces(v) => v.0.clone(),
            Self::Codex(v) => v.0.clone(),
            Self::Runtime(v) => v.as_rpc(),
            Self::Runs(v) => v.0.clone(),
            Self::Git(v) => v.0.clone(),
            Self::Files(v) => v.0.clone(),
            Self::Prompts(v) => v.0.clone(),
            Self::Settings(v) => v.0.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RequestEnvelope {
    pub correlation_id: Uuid,
    pub scope: BackendScope,
    pub request: BackendRequest,
}

impl RequestEnvelope {
    pub(crate) fn new(scope: BackendScope, request: BackendRequest) -> Self {
        Self {
            correlation_id: Uuid::new_v4(),
            scope,
            request,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackendError {
    pub code: String,
    pub message: String,
}

impl From<String> for BackendError {
    fn from(message: String) -> Self {
        let code = message
            .split(':')
            .next()
            .filter(|s| {
                [
                    "unsupported",
                    "auth_required",
                    "runtime_busy",
                    "revision_conflict",
                    "transport_lost",
                    "invalid_input",
                    "unauthorized",
                ]
                .contains(s)
            })
            .unwrap_or("backend_error")
            .to_string();
        Self { code, message }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventKind {
    Runtime,
    AppServer,
    TerminalOutput,
    TerminalExit,
}

#[derive(Clone, Debug)]
pub(crate) struct BackendEvent {
    pub kind: EventKind,
    pub payload: Value,
    pub sequence: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EventStream {
    Runtime,
    AppServer,
    Terminal,
}

#[derive(Debug)]
pub(crate) enum BackendMessage {
    Reply {
        correlation_id: Uuid,
        scope: BackendScope,
        result: Result<Value, BackendError>,
    },
    Event {
        scope: BackendScope,
        event: BackendEvent,
    },
    ResyncRequired {
        scope: BackendScope,
        stream: EventStream,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_methods_are_explicit_and_wire_names_are_stable() {
        let methods = [
            RuntimeMethod::Profiles,
            RuntimeMethod::DiscoverSessions,
            RuntimeMethod::AttachSession,
            RuntimeMethod::ReadConversation,
            RuntimeMethod::Submit,
            RuntimeMethod::Cancel,
        ];
        let names = methods.map(RuntimeMethod::as_str);
        assert_eq!(
            names,
            [
                "profiles",
                "discover_sessions",
                "attach_session",
                "read_conversation",
                "submit",
                "cancel"
            ]
        );
    }

    #[test]
    fn request_safety_and_correlation_are_preserved() {
        let scope = BackendScope {
            endpoint_id: "local".into(),
            connection_epoch: 8,
            store_id: Some("store".into()),
        };
        let req = BackendRequest::Runs(RunsRequest::mutation(
            "save_run_comparison",
            json!({"input":{"requestId":"stable"}}),
            Some(Uuid::nil()),
        ));
        let envelope = RequestEnvelope::new(scope.clone(), req.clone());
        assert_ne!(envelope.correlation_id, Uuid::nil());
        assert_eq!(envelope.scope, scope);
        assert_eq!(
            req.rpc().safety,
            RequestSafety::Mutation {
                request_id: Some(Uuid::nil())
            }
        );
        assert_eq!(req.rpc().params["input"]["requestId"], "stable");
    }

    #[test]
    fn conversation_keys_separate_codex_and_acp_identity() {
        let endpoint_id = "daemon-a".to_string();
        let codex = ConversationKey {
            endpoint_id: endpoint_id.clone(),
            conversation: ConversationRef::Codex {
                workspace_id: "w".into(),
                thread_id: "same-native-id".into(),
            },
        };
        let acp = ConversationKey {
            endpoint_id,
            conversation: ConversationRef::Acp {
                workspace_id: "w".into(),
                store_id: "s".into(),
                session_id: "same-native-id".into(),
            },
        };
        assert_ne!(codex, acp);
    }
}
