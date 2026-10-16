use super::{serialize_ok, serialize_result};
use crate::file_io;
use crate::file_policy;
use crate::shared::files_core;
use crate::shared::workspace_files_core::list_workspace_files_inner;
use crate::shared::workspace_files_core::read_workspace_file_inner;
use crate::shared::workspace_files_core::WorkspaceFileResponse;
use crate::shared::workspace_rpc;
use crate::shared::workspaces_core;
use crate::DaemonState;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileReadRequest {
    scope: file_policy::FileScope,
    kind: file_policy::FileKind,
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileWriteRequest {
    scope: file_policy::FileScope,
    kind: file_policy::FileKind,
    workspace_id: Option<String>,
    content: String,
}

fn parse_file_read_request(params: &Value) -> Result<FileReadRequest, String> {
    serde_json::from_value(params.clone()).map_err(|err| err.to_string())
}

fn parse_file_write_request(params: &Value) -> Result<FileWriteRequest, String> {
    serde_json::from_value(params.clone()).map_err(|err| err.to_string())
}

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "list_workspace_files" => {
            let request = parse_request_or_err!(params, workspace_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.list_workspace_files(request.workspace_id)).await)
        }
        "read_workspace_file" => {
            let request = parse_request_or_err!(params, workspace_rpc::ReadWorkspaceFileRequest);
            Some(
                serialize_result(state.read_workspace_file(request.workspace_id, request.path))
                    .await,
            )
        }
        "file_read" => {
            let request = match parse_file_read_request(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                serialize_result(state.file_read(
                    request.scope,
                    request.kind,
                    request.workspace_id,
                ))
                .await,
            )
        }
        "file_write" => {
            let request = match parse_file_write_request(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                serialize_ok(state.file_write(
                    request.scope,
                    request.kind,
                    request.workspace_id,
                    request.content,
                ))
                .await,
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn list_workspace_files(
        &self,
        workspace_id: String,
    ) -> Result<Vec<String>, String> {
        workspaces_core::list_workspace_files_core(&self.workspaces, &workspace_id, |root| {
            list_workspace_files_inner(root, 20000)
        })
        .await
    }

    pub(crate) async fn read_workspace_file(
        &self,
        workspace_id: String,
        path: String,
    ) -> Result<WorkspaceFileResponse, String> {
        workspaces_core::read_workspace_file_core(
            &self.workspaces,
            &workspace_id,
            &path,
            |root, rel_path| read_workspace_file_inner(root, rel_path),
        )
        .await
    }

    pub(crate) async fn file_read(
        &self,
        scope: file_policy::FileScope,
        kind: file_policy::FileKind,
        workspace_id: Option<String>,
    ) -> Result<file_io::TextFileResponse, String> {
        files_core::file_read_core(&self.workspaces, scope, kind, workspace_id).await
    }

    pub(crate) async fn file_write(
        &self,
        scope: file_policy::FileScope,
        kind: file_policy::FileKind,
        workspace_id: Option<String>,
        content: String,
    ) -> Result<(), String> {
        files_core::file_write_core(&self.workspaces, scope, kind, workspace_id, content).await
    }
}
