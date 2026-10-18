use super::{serialize_ok, serialize_result, serialize_value};
use crate::backend::app_server::spawn_workspace_session;
use crate::shared::git_core;
use crate::shared::runs_core::types::RunContext;
use crate::shared::workspace_rpc;
use crate::shared::workspaces_core;
use crate::shared::worktree_core;
use crate::storage::read_workspaces;
use crate::types::WorkspaceInfo;
use crate::types::WorkspaceSettings;
use crate::types::WorktreeSetupStatus;
#[cfg(target_os = "macos")]
use crate::workspace_macos;
use crate::workspace_settings::apply_workspace_settings_update;
use crate::DaemonState;
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
    client_version: &str,
) -> Option<Result<Value, String>> {
    match method {
        "list_workspaces" => Some(serialize_value(state.list_workspaces().await)),
        "is_workspace_path_dir" => {
            let request = parse_request_or_err!(params, workspace_rpc::IsWorkspacePathDirRequest);
            Some(serialize_value(
                state.is_workspace_path_dir(request.path).await,
            ))
        }
        "add_workspace" => {
            let request = parse_request_or_err!(params, workspace_rpc::AddWorkspaceRequest);
            Some(
                serialize_result(state.add_workspace(request.path, client_version.to_string()))
                    .await,
            )
        }
        "add_workspace_from_git_url" => {
            let request =
                parse_request_or_err!(params, workspace_rpc::AddWorkspaceFromGitUrlRequest);
            Some(
                serialize_result(state.add_workspace_from_git_url(
                    request.url,
                    request.destination_path,
                    request.target_folder_name,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "add_worktree" => {
            let request = parse_request_or_err!(params, workspace_rpc::AddWorktreeRequest);
            Some(
                serialize_result(state.add_worktree(
                    request.parent_id,
                    request.branch,
                    request.name,
                    request.copy_agents_md,
                    request.run_context,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "worktree_setup_status" => {
            let request = parse_request_or_err!(params, workspace_rpc::WorkspaceIdRequest);
            Some(serialize_result(state.worktree_setup_status(request.workspace_id)).await)
        }
        "worktree_setup_mark_ran" => {
            let request = parse_request_or_err!(params, workspace_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.worktree_setup_mark_ran(request.workspace_id)).await)
        }
        "connect_workspace" => {
            let request = parse_request_or_err!(params, workspace_rpc::IdRequest);
            Some(
                serialize_ok(state.connect_workspace(request.id, client_version.to_string())).await,
            )
        }
        "set_workspace_runtime_codex_args" => {
            let request =
                parse_request_or_err!(params, workspace_rpc::SetWorkspaceRuntimeCodexArgsRequest);
            Some(
                serialize_result(state.set_workspace_runtime_codex_args(
                    request.workspace_id,
                    request.codex_args,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "remove_workspace" => {
            let request = parse_request_or_err!(params, workspace_rpc::IdRequest);
            Some(serialize_ok(state.remove_workspace(request.id)).await)
        }
        "remove_worktree" => {
            let request = parse_request_or_err!(params, workspace_rpc::IdRequest);
            Some(serialize_ok(state.remove_worktree(request.id)).await)
        }
        "rename_worktree" => {
            let request = parse_request_or_err!(params, workspace_rpc::RenameWorktreeRequest);
            Some(
                serialize_result(state.rename_worktree(
                    request.id,
                    request.branch,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "rename_worktree_upstream" => {
            let request =
                parse_request_or_err!(params, workspace_rpc::RenameWorktreeUpstreamRequest);
            Some(
                serialize_ok(state.rename_worktree_upstream(
                    request.id,
                    request.old_branch,
                    request.new_branch,
                ))
                .await,
            )
        }
        "update_workspace_settings" => {
            let request =
                parse_request_or_err!(params, workspace_rpc::UpdateWorkspaceSettingsRequest);
            Some(
                serialize_result(state.update_workspace_settings(
                    request.id,
                    request.settings,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "add_clone" => {
            let request = parse_request_or_err!(params, workspace_rpc::AddCloneRequest);
            Some(
                serialize_result(state.add_clone(
                    request.source_workspace_id,
                    request.copies_folder,
                    request.copy_name,
                    client_version.to_string(),
                ))
                .await,
            )
        }
        "apply_worktree_changes" => {
            let request = parse_request_or_err!(params, workspace_rpc::WorkspaceIdRequest);
            Some(serialize_ok(state.apply_worktree_changes(request.workspace_id)).await)
        }
        "open_workspace_in" => {
            let request = parse_request_or_err!(params, workspace_rpc::OpenWorkspaceInRequest);
            Some(
                serialize_ok(state.open_workspace_in(
                    request.path,
                    request.app,
                    request.args,
                    request.command,
                    request.line,
                    request.column,
                ))
                .await,
            )
        }
        "get_open_app_icon" => {
            let request = parse_request_or_err!(params, workspace_rpc::GetOpenAppIconRequest);
            Some(serialize_result(state.get_open_app_icon(request.app_name)).await)
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn sync_workspaces_from_storage(&self) {
        if let Err(e)=workspaces_core::sync_workspaces_from_storage_core(&self.storage_path,&self.workspaces,&self.sessions).await {eprintln!("daemon: failed to refresh workspace registrations: {e}");}
    }

    pub(crate) async fn list_workspaces(&self) -> Vec<WorkspaceInfo> {
        self.sync_workspaces_from_storage().await;
        workspaces_core::list_workspaces_core(&self.workspaces, &self.sessions).await
    }

    pub(crate) async fn is_workspace_path_dir(&self, path: String) -> bool {
        workspaces_core::is_workspace_path_dir_core(&path)
    }

    pub(crate) async fn add_workspace(
        &self,
        path: String,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        let client_version = client_version.clone();
        workspaces_core::add_workspace_core(
            path,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn add_workspace_from_git_url(
        &self,
        url: String,
        destination_path: String,
        target_folder_name: Option<String>,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        let client_version = client_version.clone();
        workspaces_core::add_workspace_from_git_url_core(
            url,
            destination_path,
            target_folder_name,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn add_worktree(
        &self,
        parent_id: String,
        branch: String,
        name: Option<String>,
        copy_agents_md: bool,
        run_context: Option<RunContext>,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        let client_version = client_version.clone();
        workspaces_core::add_worktree_core(
            parent_id,
            branch,
            name,
            copy_agents_md,
            run_context,
            &self.data_dir,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            |value| worktree_core::sanitize_worktree_name(value),
            |root, name| worktree_core::unique_worktree_path_strict(root, name),
            |root, branch_name| {
                let root = root.clone();
                let branch_name = branch_name.to_string();
                async move { git_core::git_branch_exists(&root, &branch_name).await }
            },
            Some(|root: &PathBuf, branch_name: &str| {
                let root = root.clone();
                let branch_name = branch_name.to_string();
                async move { git_core::git_find_remote_tracking_branch_local(&root, &branch_name).await }
            }),
            |root, args| {
                workspaces_core::run_git_command_unit(root, args, git_core::run_git_command_owned)
            },
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(entry, default_bin, codex_args, codex_home, client_version.clone(), self.event_sink.clone())
            },
        )
        .await
    }

    pub(crate) async fn worktree_setup_status(
        &self,
        workspace_id: String,
    ) -> Result<WorktreeSetupStatus, String> {
        workspaces_core::worktree_setup_status_core(&self.workspaces, &workspace_id, &self.data_dir)
            .await
    }

    pub(crate) async fn worktree_setup_mark_ran(&self, workspace_id: String) -> Result<(), String> {
        workspaces_core::worktree_setup_mark_ran_core(
            &self.workspaces,
            &workspace_id,
            &self.data_dir,
        )
        .await
    }

    pub(crate) async fn remove_workspace(&self, id: String) -> Result<(), String> {
        workspaces_core::remove_workspace_core(
            id,
            &self.workspaces,
            &self.sessions,
            &self.storage_path,
            |root, args| {
                workspaces_core::run_git_command_unit(root, args, git_core::run_git_command_owned)
            },
            |error| git_core::is_missing_worktree_error(error),
            |path| {
                std::fs::remove_dir_all(path)
                    .map_err(|err| format!("Failed to remove worktree folder: {err}"))
            },
            true,
            true,
        )
        .await
    }

    pub(crate) async fn remove_worktree(&self, id: String) -> Result<(), String> {
        workspaces_core::remove_worktree_core(
            id,
            &self.workspaces,
            &self.sessions,
            &self.storage_path,
            |root, args| {
                workspaces_core::run_git_command_unit(root, args, git_core::run_git_command_owned)
            },
            |error| git_core::is_missing_worktree_error(error),
            |path| {
                std::fs::remove_dir_all(path)
                    .map_err(|err| format!("Failed to remove worktree folder: {err}"))
            },
        )
        .await
    }

    pub(crate) async fn rename_worktree(
        &self,
        id: String,
        branch: String,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        let client_version = client_version.clone();
        workspaces_core::rename_worktree_core(
            id,
            branch,
            &self.data_dir,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            |entry| Ok(PathBuf::from(entry.path.clone())),
            |root, name| {
                let root = root.clone();
                let name = name.to_string();
                async move {
                    git_core::unique_branch_name_live(&root, &name, None)
                        .await
                        .map(|(branch_name, _was_suffixed)| branch_name)
                }
            },
            |value| worktree_core::sanitize_worktree_name(value),
            |root, name, current| {
                worktree_core::unique_worktree_path_for_rename(root, name, current)
            },
            |root, args| {
                workspaces_core::run_git_command_unit(root, args, git_core::run_git_command_owned)
            },
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn rename_worktree_upstream(
        &self,
        id: String,
        old_branch: String,
        new_branch: String,
    ) -> Result<(), String> {
        workspaces_core::rename_worktree_upstream_core(
            id,
            old_branch,
            new_branch,
            &self.workspaces,
            |entry| Ok(PathBuf::from(entry.path.clone())),
            |root, branch_name| {
                let root = root.clone();
                let branch_name = branch_name.to_string();
                async move { git_core::git_branch_exists(&root, &branch_name).await }
            },
            |root, branch_name| {
                let root = root.clone();
                let branch_name = branch_name.to_string();
                async move { git_core::git_find_remote_for_branch_live(&root, &branch_name).await }
            },
            |root, remote| {
                let root = root.clone();
                let remote = remote.to_string();
                async move { git_core::git_remote_exists(&root, &remote).await }
            },
            |root, remote, branch_name| {
                let root = root.clone();
                let remote = remote.to_string();
                let branch_name = branch_name.to_string();
                async move {
                    git_core::git_remote_branch_exists_live(&root, &remote, &branch_name).await
                }
            },
            |root, args| {
                workspaces_core::run_git_command_unit(root, args, git_core::run_git_command_owned)
            },
        )
        .await
    }

    pub(crate) async fn update_workspace_settings(
        &self,
        id: String,
        settings: WorkspaceSettings,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        let client_version = client_version.clone();
        workspaces_core::update_workspace_settings_core(
            id,
            settings,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            |workspaces, workspace_id, next_settings| {
                apply_workspace_settings_update(workspaces, workspace_id, next_settings)
            },
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn connect_workspace(
        &self,
        id: String,
        client_version: String,
    ) -> Result<(), String> {
        {
            let sessions = self.sessions.lock().await;
            if sessions.contains_key(&id) {
                return Ok(());
            }
        }

        let client_version = client_version.clone();
        workspaces_core::connect_workspace_core(
            id,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            move |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn set_workspace_runtime_codex_args(
        &self,
        workspace_id: String,
        codex_args: Option<String>,
        client_version: String,
    ) -> Result<workspaces_core::WorkspaceRuntimeCodexArgsResult, String> {
        workspaces_core::set_workspace_runtime_codex_args_core(
            workspace_id,
            codex_args,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            move |entry, default_bin, next_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    next_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn add_clone(
        &self,
        source_workspace_id: String,
        copies_folder: String,
        copy_name: String,
        client_version: String,
    ) -> Result<WorkspaceInfo, String> {
        workspaces_core::add_clone_core(
            source_workspace_id,
            copy_name,
            copies_folder,
            &self.workspaces,
            &self.sessions,
            &self.app_settings,
            &self.storage_path,
            |entry, default_bin, codex_args, codex_home| {
                spawn_workspace_session(
                    entry,
                    default_bin,
                    codex_args,
                    codex_home,
                    client_version.clone(),
                    self.event_sink.clone(),
                )
            },
        )
        .await
    }

    pub(crate) async fn apply_worktree_changes(&self, workspace_id: String) -> Result<(), String> {
        workspaces_core::apply_worktree_changes_core(&self.workspaces, workspace_id).await
    }

    pub(crate) async fn open_workspace_in(
        &self,
        path: String,
        app: Option<String>,
        args: Vec<String>,
        command: Option<String>,
        line: Option<u32>,
        column: Option<u32>,
    ) -> Result<(), String> {
        workspaces_core::open_workspace_in_core(path, app, args, command, line, column).await
    }

    pub(crate) async fn get_open_app_icon(
        &self,
        app_name: String,
    ) -> Result<Option<String>, String> {
        #[cfg(target_os = "macos")]
        {
            return workspaces_core::get_open_app_icon_core(app_name, |name| {
                workspace_macos::get_open_app_icon_inner(name)
            })
            .await;
        }

        #[cfg(not(target_os = "macos"))]
        {
            workspaces_core::get_open_app_icon_core(app_name, |_name| None).await
        }
    }
}
