use super::{parse_optional_string, parse_optional_u32, serialize_result};
use crate::shared::local_usage_core;
use crate::types::LocalUsageSnapshot;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "local_usage_snapshot" => {
            let days = parse_optional_u32(params, "days");
            let workspace_path = parse_optional_string(params, "workspacePath");
            Some(serialize_result(state.local_usage_snapshot(days, workspace_path)).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn local_usage_snapshot(
        &self,
        days: Option<u32>,
        workspace_path: Option<String>,
    ) -> Result<LocalUsageSnapshot, String> {
        local_usage_core::local_usage_snapshot_core(&self.workspaces, days, workspace_path).await
    }
}
