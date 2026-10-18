use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunContext {
    pub run_id: String,
    pub instance_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateRun {
    pub request_id: String,
    pub workspace_id: String,
    pub title: String,
    pub prompt: String,
    pub mode: String,
    #[serde(default)]
    pub images: Vec<String>,
    pub instances: Vec<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunRecord {
    pub schema_version: u32,
    pub store_id: String,
    pub id: String,
    pub request_id: String,
    pub workspace_id: String,
    pub workspace_name: String,
    pub workspace_path: String,
    pub title: String,
    pub prompt: String,
    pub mode: String,
    pub created_at: i64,
    pub revision: u32,
    pub base_oid: Option<String>,
    pub baseline: Value,
    pub attachments: Vec<String>,
    pub instances: Vec<RunInstance>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SubmissionOutcome {
    NotSubmitted,
    Submitting,
    Accepted,
    Rejected,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ExecutionOutcome {
    NotStarted,
    Running,
    Completed,
    Failed,
    Interrupted,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CaptureKind {
    InitialTurn,
    Manual,
    Recovered,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunInstance {
    pub id: String,
    pub run_id: String,
    pub ordinal: usize,
    pub requested: Value,
    pub preparation: String,
    pub workspace_id: Option<String>,
    pub workspace_path: Option<String>,
    pub thread_id: Option<String>,
    pub initial_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<crate::shared::agent_runtime::types::RuntimeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_execution_id: Option<String>,
    pub submission_outcome: SubmissionOutcome,
    pub execution_outcome: ExecutionOutcome,
    pub submission: Option<Value>,
    pub baseline: Value,
    pub setup: Value,
    pub error: Option<String>,
    pub captures: Vec<CaptureSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CaptureSummary {
    pub id: String,
    pub kind: CaptureKind,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub label: String,
    pub created_at: i64,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunCapture {
    pub schema_version: u32,
    pub id: String,
    pub instance_id: String,
    pub request_id: String,
    pub kind: CaptureKind,
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    pub label: String,
    pub created_at: i64,
    pub status: ExecutionOutcome,
    pub items: Vec<Value>,
    pub usage: Option<Value>,
    pub observed: Value,
    pub git: Value,
    pub errors: Vec<String>,
    pub complete: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunComparison {
    pub id: String,
    pub left_capture_id: String,
    pub right_capture_id: String,
    pub choice: String,
    pub notes: String,
    pub revision: u32,
    pub updated_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "disposition", rename_all = "snake_case")]
pub(crate) enum OperationDisposition {
    Execute { id: String },
    Completed { receipt: Value },
    Unresolved { id: String },
}
