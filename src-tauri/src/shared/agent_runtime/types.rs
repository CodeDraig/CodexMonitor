use crate::shared::runs_core::types::{ExecutionOutcome, RunContext, SubmissionOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeKind {
    Codex,
    OpenCodeAcp,
    Acp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeRef {
    pub profile_id: String,
    pub profile_revision: u64,
    pub kind: RuntimeKind,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeProfile {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub kind: RuntimeKind,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// A UI preset identifier. It is presentation/configuration metadata and
    /// deliberately does not affect the identity of an ACP connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
    /// Environment values are resolved only where the agent process runs.
    /// Resolved values are never part of this profile or any RPC response.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, EnvironmentSource>,
    #[serde(default, skip_serializing_if = "AcpClientPolicy::is_default")]
    pub client_policy: AcpClientPolicy,
}

/// A profile environment entry. `backendEnv` stores only a variable name;
/// its value is looked up on the machine that launches the ACP process.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum EnvironmentSource {
    Literal { value: String },
    BackendEnv { name: String },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcpFilesystemPolicy {
    #[default]
    Disabled,
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcpCommandExecutionPolicy {
    #[default]
    Disabled,
    Ask,
    Allow,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct AcpClientPolicy {
    pub filesystem: AcpFilesystemPolicy,
    pub command_execution: AcpCommandExecutionPolicy,
    pub additional_roots: Vec<String>,
}

impl Default for AcpClientPolicy {
    fn default() -> Self {
        Self {
            filesystem: AcpFilesystemPolicy::Disabled,
            command_execution: AcpCommandExecutionPolicy::Disabled,
            additional_roots: Vec::new(),
        }
    }
}

impl AcpClientPolicy {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

impl RuntimeProfile {
    pub fn is_acp(&self) -> bool {
        self.kind != RuntimeKind::Codex
    }
    pub fn launch_args(&self) -> Vec<String> {
        let mut args = self.args.clone();
        if self.kind == RuntimeKind::OpenCodeAcp {
            args.push("acp".into());
        }
        args
    }
    pub fn connection_scope(&self) -> String {
        use sha2::{Digest, Sha256};
        // Presentation fields and revision deliberately do not participate.
        // Keep the original three-element payload for profiles without
        // environment overrides, preserving every legacy scope byte-for-byte.
        let launch_config = if self.environment.is_empty() {
            serde_json::json!([self.executable, self.launch_args(), self.working_directory])
        } else {
            serde_json::json!([
                self.executable,
                self.launch_args(),
                self.working_directory,
                self.environment
            ])
        };
        format!(
            "{}:{:x}",
            self.id,
            Sha256::digest(
                serde_json::to_vec(&launch_config).expect("serializable launch configuration")
            )
        )
    }

    /// Resolve profile environment entries on the backend that will launch
    /// the agent. This return value is transient and must never be serialized
    /// as part of a profile or status response.
    pub(crate) fn resolve_environment(&self) -> Result<BTreeMap<String, String>, String> {
        self.environment
            .iter()
            .map(|(key, source)| {
                let value = match source {
                    EnvironmentSource::Literal { value } => value.clone(),
                    EnvironmentSource::BackendEnv { name } => std::env::var(name).map_err(|_| {
                        format!("invalid_input: required backend environment variable {name} is unavailable")
                    })?,
                };
                Ok((key.clone(), value))
            })
            .collect()
    }
    pub fn reference(&self) -> RuntimeRef {
        RuntimeRef {
            profile_id: self.id.clone(),
            profile_revision: self.revision,
            kind: self.kind.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct RuntimeCapabilities {
    pub load_session: bool,
    pub session_discovery: bool,
    pub images: bool,
    pub audio: bool,
    pub embedded_context: bool,
    pub configuration: bool,
    pub cancellation: bool,
    pub steering: bool,
    pub permissions: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub logout: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeSession {
    pub store_id: String,
    pub runtime: RuntimeRef,
    pub session_id: String,
    pub native_session_id: String,
    pub workspace_id: String,
    pub cwd: String,
    pub profile: RuntimeProfile,
    pub capabilities: RuntimeCapabilities,
    pub configuration: Value,
    pub agent_info: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SubmitExecution {
    pub request_id: String,
    pub session_id: String,
    pub prompt: Vec<Value>,
    #[serde(default)]
    pub options: Value,
    pub run_context: Option<RunContext>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ExecutionTicket {
    pub execution_id: String,
    pub request_id: String,
    pub session_id: String,
    pub submission: SubmissionOutcome,
    pub execution: ExecutionOutcome,
    pub native_turn_id: Option<String>,
    pub input: SubmitExecution,
    #[serde(default)]
    pub configuration: Value,
    #[serde(default)]
    pub runtime: Option<RuntimeRef>,
    #[serde(default)]
    pub agent_info: Value,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RuntimeEvent {
    pub schema_version: u32,
    pub store_id: String,
    pub runtime: RuntimeRef,
    pub session_id: String,
    pub execution_id: Option<String>,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_load_id: Option<String>,
    pub source: String,
    pub kind: String,
    pub payload: Value,
    pub raw: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RuntimeError {
    pub code: String,
    pub message: String,
}
impl From<String> for RuntimeError {
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
                ]
                .contains(s)
            })
            .unwrap_or("runtime_error");
        Self {
            code: code.into(),
            message,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogSession {
    pub workspace_id: Option<String>,
    pub catalog_id: String,
    pub store_id: String,
    pub connection_scope: String,
    pub native_session_id: String,
    pub session_id: Option<String>,
    pub profile: RuntimeProfile,
    pub cwd: String,
    pub title: Option<String>,
    pub updated_at: Option<String>,
    pub metadata: Value,
    pub revision: u64,
    pub presentation: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HistoryLoad {
    pub id: String,
    pub request_id: String,
    pub session_id: String,
    pub state: String,
    pub started_at: i64,
    pub completed_at: Option<i64>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionRole {
    Conversation,
    Discovery,
    Authentication,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceContext {
    pub workspace_id: String,
    pub cwd: String,
}

/// An ACP protocol connection may exist before Monitor has a native session,
/// for example while the user authenticates an agent.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpConnectionContext {
    pub connection_id: String,
    pub generation: u64,
    pub profile: RuntimeRef,
    pub store_id: String,
    pub role: ConnectionRole,
    pub workspace: Option<WorkspaceContext>,
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpAuthMethod {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub details: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConnectionOperationState {
    Pending,
    AwaitingUser,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionOperation {
    pub operation_id: String,
    pub request_id: String,
    pub connection_id: String,
    pub generation: u64,
    pub state: ConnectionOperationState,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionPreparation {
    pub operation_id: String,
    pub connection_id: String,
    pub generation: u64,
    pub state: ConnectionOperationState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpConnectionStatus {
    pub connection_id: String,
    pub generation: u64,
    pub profile_id: String,
    pub agent_info: Value,
    pub capabilities: RuntimeCapabilities,
    pub auth_methods: Vec<AcpAuthMethod>,
    pub auth_state: String,
    pub pending_client_requests: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClientRequestDecision {
    pub connection_id: String,
    pub generation: u64,
    pub client_request_id: Value,
    pub decision: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginTerminalRead {
    pub connection_id: String,
    pub generation: u64,
    pub operation_id: String,
    pub cursor: u64,
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginTerminalWrite {
    pub connection_id: String,
    pub generation: u64,
    pub operation_id: String,
    pub sequence: u64,
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpTerminalRead {
    pub session_id: String,
    pub terminal_id: String,
    pub cursor: Option<u64>,
    pub limit: Option<u32>,
}
