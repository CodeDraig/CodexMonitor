use super::{
    parse_optional_bool, parse_optional_string, parse_optional_u32, serialize_ok, serialize_result,
};
use crate::events::emit_background_thread_hide;
use crate::shared::codex_aux_core;
use crate::shared::git_rpc;
use crate::shared::git_ui_core;
use crate::types::GitCommitDiff;
use crate::types::GitFileDiff;
use crate::types::GitHubIssuesResponse;
use crate::types::GitHubPullRequestComment;
use crate::types::GitHubPullRequestDiff;
use crate::types::GitHubPullRequestsResponse;
use crate::types::GitLogResponse;
use crate::DaemonState;
use serde_json::Value;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        git_rpc::METHOD_GET_GIT_STATUS => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(state.get_git_status(request.workspace_id).await)
        }
        git_rpc::METHOD_INIT_GIT_REPO => {
            let request = parse_request_or_err!(params, git_rpc::InitGitRepoRequiredRequest);
            let force = parse_optional_bool(params, "force").unwrap_or(false);
            Some(
                state
                    .init_git_repo(request.workspace_id, request.branch, force)
                    .await,
            )
        }
        git_rpc::METHOD_CREATE_GITHUB_REPO => {
            let request = parse_request_or_err!(params, git_rpc::CreateGitHubRepoRequiredRequest);
            let branch = parse_optional_string(params, "branch");
            Some(
                state
                    .create_github_repo(
                        request.workspace_id,
                        request.repo,
                        request.visibility,
                        branch,
                    )
                    .await,
            )
        }
        git_rpc::METHOD_LIST_GIT_ROOTS => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            let depth = parse_optional_u32(params, "depth").map(|value| value as usize);
            Some(serialize_result(state.list_git_roots(request.workspace_id, depth)).await)
        }
        git_rpc::METHOD_GET_GIT_DIFFS => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.get_git_diffs(request.workspace_id)).await)
        }
        git_rpc::METHOD_GET_GIT_LOG => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            let limit = parse_optional_u32(params, "limit").map(|value| value as usize);
            Some(serialize_result(state.get_git_log(request.workspace_id, limit)).await)
        }
        git_rpc::METHOD_GET_GIT_COMMIT_DIFF => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceShaRequest);
            Some(
                serialize_result(state.get_git_commit_diff(request.workspace_id, request.sha))
                    .await,
            )
        }
        git_rpc::METHOD_GET_GIT_REMOTE => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.get_git_remote(request.workspace_id)).await)
        }
        git_rpc::METHOD_STAGE_GIT_FILE => {
            let request = parse_request_or_err!(params, git_rpc::WorkspacePathRequest);
            Some(serialize_ok(state.stage_git_file(request.workspace_id, request.path)).await)
        }
        git_rpc::METHOD_STAGE_GIT_ALL => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.stage_git_all(request.workspace_id)).await)
        }
        git_rpc::METHOD_UNSTAGE_GIT_FILE => {
            let request = parse_request_or_err!(params, git_rpc::WorkspacePathRequest);
            Some(serialize_ok(state.unstage_git_file(request.workspace_id, request.path)).await)
        }
        git_rpc::METHOD_REVERT_GIT_FILE => {
            let request = parse_request_or_err!(params, git_rpc::WorkspacePathRequest);
            Some(serialize_ok(state.revert_git_file(request.workspace_id, request.path)).await)
        }
        git_rpc::METHOD_REVERT_GIT_ALL => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.revert_git_all(request.workspace_id)).await)
        }
        git_rpc::METHOD_COMMIT_GIT => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceMessageRequest);
            Some(serialize_ok(state.commit_git(request.workspace_id, request.message)).await)
        }
        git_rpc::METHOD_PUSH_GIT => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.push_git(request.workspace_id)).await)
        }
        git_rpc::METHOD_PULL_GIT => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.pull_git(request.workspace_id)).await)
        }
        git_rpc::METHOD_FETCH_GIT => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.fetch_git(request.workspace_id)).await)
        }
        git_rpc::METHOD_SYNC_GIT => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.sync_git(request.workspace_id)).await)
        }
        git_rpc::METHOD_GET_GITHUB_ISSUES => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.get_github_issues(request.workspace_id)).await)
        }
        git_rpc::METHOD_GET_GITHUB_PULL_REQUESTS => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.get_github_pull_requests(request.workspace_id)).await)
        }
        git_rpc::METHOD_GET_GITHUB_PULL_REQUEST_DIFF => {
            let request = parse_request_or_err!(params, git_rpc::GitHubPullRequestRequest);
            Some(
                serialize_result(
                    state.get_github_pull_request_diff(request.workspace_id, request.pr_number),
                )
                .await,
            )
        }
        git_rpc::METHOD_GET_GITHUB_PULL_REQUEST_COMMENTS => {
            let request = parse_request_or_err!(params, git_rpc::GitHubPullRequestRequest);
            Some(
                serialize_result(
                    state.get_github_pull_request_comments(request.workspace_id, request.pr_number),
                )
                .await,
            )
        }
        git_rpc::METHOD_CHECKOUT_GITHUB_PULL_REQUEST => {
            let request = parse_request_or_err!(params, git_rpc::GitHubPullRequestRequest);
            Some(
                serialize_ok(
                    state.checkout_github_pull_request(request.workspace_id, request.pr_number),
                )
                .await,
            )
        }
        git_rpc::METHOD_LIST_GIT_BRANCHES => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            Some(state.list_git_branches(request.workspace_id).await)
        }
        git_rpc::METHOD_CHECKOUT_GIT_BRANCH => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceNameRequest);
            Some(serialize_ok(state.checkout_git_branch(request.workspace_id, request.name)).await)
        }
        git_rpc::METHOD_CREATE_GIT_BRANCH => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceNameRequest);
            Some(serialize_ok(state.create_git_branch(request.workspace_id, request.name)).await)
        }
        git_rpc::METHOD_GENERATE_COMMIT_MESSAGE => {
            let request = parse_request_or_err!(params, git_rpc::WorkspaceIdRequest);
            let commit_message_model_id = parse_optional_string(params, "commitMessageModelId");
            Some(
                state
                    .generate_commit_message(request.workspace_id, commit_message_model_id)
                    .await
                    .map(Value::String),
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn get_git_status(&self, workspace_id: String) -> Result<Value, String> {
        git_ui_core::get_git_status_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn init_git_repo(
        &self,
        workspace_id: String,
        branch: String,
        force: bool,
    ) -> Result<Value, String> {
        git_ui_core::init_git_repo_core(&self.workspaces, workspace_id, branch, force).await
    }

    pub(crate) async fn create_github_repo(
        &self,
        workspace_id: String,
        repo: String,
        visibility: String,
        branch: Option<String>,
    ) -> Result<Value, String> {
        git_ui_core::create_github_repo_core(
            &self.workspaces,
            workspace_id,
            repo,
            visibility,
            branch,
        )
        .await
    }

    pub(crate) async fn list_git_roots(
        &self,
        workspace_id: String,
        depth: Option<usize>,
    ) -> Result<Vec<String>, String> {
        git_ui_core::list_git_roots_core(&self.workspaces, workspace_id, depth).await
    }

    pub(crate) async fn get_git_diffs(
        &self,
        workspace_id: String,
    ) -> Result<Vec<GitFileDiff>, String> {
        git_ui_core::get_git_diffs_core(&self.workspaces, &self.app_settings, workspace_id).await
    }

    pub(crate) async fn get_git_log(
        &self,
        workspace_id: String,
        limit: Option<usize>,
    ) -> Result<GitLogResponse, String> {
        git_ui_core::get_git_log_core(&self.workspaces, workspace_id, limit).await
    }

    pub(crate) async fn get_git_commit_diff(
        &self,
        workspace_id: String,
        sha: String,
    ) -> Result<Vec<GitCommitDiff>, String> {
        git_ui_core::get_git_commit_diff_core(
            &self.workspaces,
            &self.app_settings,
            workspace_id,
            sha,
        )
        .await
    }

    pub(crate) async fn get_git_remote(
        &self,
        workspace_id: String,
    ) -> Result<Option<String>, String> {
        git_ui_core::get_git_remote_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn stage_git_file(
        &self,
        workspace_id: String,
        path: String,
    ) -> Result<(), String> {
        git_ui_core::stage_git_file_core(&self.workspaces, workspace_id, path).await
    }

    pub(crate) async fn stage_git_all(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::stage_git_all_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn unstage_git_file(
        &self,
        workspace_id: String,
        path: String,
    ) -> Result<(), String> {
        git_ui_core::unstage_git_file_core(&self.workspaces, workspace_id, path).await
    }

    pub(crate) async fn revert_git_file(
        &self,
        workspace_id: String,
        path: String,
    ) -> Result<(), String> {
        git_ui_core::revert_git_file_core(&self.workspaces, workspace_id, path).await
    }

    pub(crate) async fn revert_git_all(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::revert_git_all_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn commit_git(
        &self,
        workspace_id: String,
        message: String,
    ) -> Result<(), String> {
        git_ui_core::commit_git_core(&self.workspaces, workspace_id, message).await
    }

    pub(crate) async fn push_git(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::push_git_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn pull_git(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::pull_git_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn fetch_git(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::fetch_git_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn sync_git(&self, workspace_id: String) -> Result<(), String> {
        git_ui_core::sync_git_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn get_github_issues(
        &self,
        workspace_id: String,
    ) -> Result<GitHubIssuesResponse, String> {
        git_ui_core::get_github_issues_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn get_github_pull_requests(
        &self,
        workspace_id: String,
    ) -> Result<GitHubPullRequestsResponse, String> {
        git_ui_core::get_github_pull_requests_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn get_github_pull_request_diff(
        &self,
        workspace_id: String,
        pr_number: u64,
    ) -> Result<Vec<GitHubPullRequestDiff>, String> {
        git_ui_core::get_github_pull_request_diff_core(&self.workspaces, workspace_id, pr_number)
            .await
    }

    pub(crate) async fn get_github_pull_request_comments(
        &self,
        workspace_id: String,
        pr_number: u64,
    ) -> Result<Vec<GitHubPullRequestComment>, String> {
        git_ui_core::get_github_pull_request_comments_core(
            &self.workspaces,
            workspace_id,
            pr_number,
        )
        .await
    }

    pub(crate) async fn checkout_github_pull_request(
        &self,
        workspace_id: String,
        pr_number: u64,
    ) -> Result<(), String> {
        git_ui_core::checkout_github_pull_request_core(&self.workspaces, workspace_id, pr_number)
            .await
    }

    pub(crate) async fn list_git_branches(&self, workspace_id: String) -> Result<Value, String> {
        git_ui_core::list_git_branches_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn checkout_git_branch(
        &self,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        git_ui_core::checkout_git_branch_core(&self.workspaces, workspace_id, name).await
    }

    pub(crate) async fn create_git_branch(
        &self,
        workspace_id: String,
        name: String,
    ) -> Result<(), String> {
        git_ui_core::create_git_branch_core(&self.workspaces, workspace_id, name).await
    }

    pub(crate) async fn generate_commit_message(
        &self,
        workspace_id: String,
        commit_message_model_id: Option<String>,
    ) -> Result<String, String> {
        let repo_root = git_ui_core::resolve_repo_root_for_workspace_core(
            &self.workspaces,
            workspace_id.clone(),
        )
        .await?;
        let diff = git_ui_core::collect_workspace_diff_core(&repo_root)?;
        let commit_message_prompt = {
            let settings = self.app_settings.lock().await;
            settings.commit_message_prompt.clone()
        };
        codex_aux_core::generate_commit_message_core(
            &self.sessions,
            &self.workspaces,
            workspace_id,
            &diff,
            &commit_message_prompt,
            commit_message_model_id.as_deref(),
            |workspace_id, thread_id| {
                emit_background_thread_hide(&self.event_sink, workspace_id, thread_id);
            },
        )
        .await
    }
}
