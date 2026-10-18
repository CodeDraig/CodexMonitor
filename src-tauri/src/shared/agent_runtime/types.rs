use crate::shared::runs_core::types::{ExecutionOutcome, RunContext, SubmissionOutcome};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
        format!(
            "{}:{:x}",
            self.id,
            Sha256::digest(
                serde_json::to_vec(&serde_json::json!([
                    self.executable,
                    self.launch_args(),
                    self.working_directory
                ]))
                .expect("serializable launch configuration")
            )
        )
    }
    pub fn reference(&self) -> RuntimeRef {
        RuntimeRef {
            profile_id: self.id.clone(),
            profile_revision: self.revision,
            kind: self.kind.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
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
