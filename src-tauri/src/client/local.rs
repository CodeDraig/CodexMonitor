use super::{
    contracts::{BackendRequest, RuntimeMethod},
    events::{ChannelEventSink, EventInbox},
    BackendDriver, BackendHandle, ClientFuture,
};
use crate::{
    backend::{app_server, events::EventSink},
    shared::{host_core::CoreState, runs_core, runs_rpc, runtime_rpc, workspaces_core},
    types::AppSettings,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc};

pub(crate) struct LocalDriver {
    core: Arc<CoreState>,
    sink: ChannelEventSink,
}

impl LocalDriver {
    pub(crate) fn new(core: Arc<CoreState>, inbox: EventInbox) -> Self {
        let sink = ChannelEventSink::new(inbox, Some(runs_core::db_path(&core.storage_path)));
        Self { core, sink }
    }
}

impl BackendDriver for LocalDriver {
    fn execute<'a>(&'a self, request: BackendRequest) -> ClientFuture<'a> {
        Box::pin(async move { self.dispatch(request).await })
    }
}

impl LocalDriver {
    async fn dispatch(&self, request: BackendRequest) -> Result<Value, String> {
        let path = runs_core::db_path(&self.core.storage_path);
        match request {
            BackendRequest::Runtime(request) => {
                runtime_rpc::dispatch(
                    request.method.as_str(),
                    request.params,
                    path,
                    &self.core.runtime_registry,
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    self.sink.clone(),
                )
                .await
            }
            BackendRequest::Runs(request) => {
                runs_rpc::dispatch(
                    &request.0.method,
                    request.0.params,
                    path,
                    &self.core.workspaces,
                    &self.core.sessions,
                )
                .await
            }
            BackendRequest::Settings(request) => match request.0.method.as_str() {
                "get_app_settings" => serde_json::to_value(
                    crate::shared::settings_core::get_app_settings_core(&self.core.app_settings)
                        .await,
                )
                .map_err(|e| e.to_string()),
                "update_app_settings" => {
                    let settings: AppSettings = serde_json::from_value(
                        request
                            .0
                            .params
                            .get("settings")
                            .cloned()
                            .unwrap_or(request.0.params),
                    )
                    .map_err(|e| e.to_string())?;
                    serde_json::to_value(
                        crate::shared::settings_core::update_app_settings_core(
                            settings,
                            &self.core.app_settings,
                            &self.core.settings_path,
                        )
                        .await?,
                    )
                    .map_err(|e| e.to_string())
                }
                other => Err(format!("unsupported: local settings method {other}")),
            },
            BackendRequest::Workspaces(request) => {
                self.workspace(&request.0.method, request.0.params).await
            }
            BackendRequest::Codex(request) => self.codex(&request.0.method, request.0.params).await,
            BackendRequest::Files(request) => self.files(&request.0.method, request.0.params).await,
            BackendRequest::Prompts(request) => {
                self.prompts(&request.0.method, request.0.params).await
            }
            BackendRequest::Git(request) => self.git(&request.0.method, request.0.params).await,
        }
    }

    async fn workspace(&self, method: &str, p: Value) -> Result<Value, String> {
        match method {
            "list_workspaces" => serde_json::to_value(
                workspaces_core::list_workspaces_core(&self.core.workspaces, &self.core.sessions)
                    .await,
            )
            .map_err(|e| e.to_string()),
            "is_workspace_path_dir" => Ok(Value::Bool(
                workspaces_core::is_workspace_path_dir_core(reqstr(&p, "path")?),
            )),
            "add_workspace" => {
                let sink = self.sink.clone();
                let entry = workspaces_core::add_workspace_core(
                    reqstr(&p, "path")?.into(),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    &self.core.storage_path,
                    move |entry, bin, args, home| {
                        let sink = sink.clone();
                        async move {
                            app_server::spawn_workspace_session(
                                entry,
                                bin,
                                args,
                                home,
                                env!("CARGO_PKG_VERSION").to_owned(),
                                sink,
                            )
                            .await
                        }
                    },
                )
                .await?;
                serde_json::to_value(entry).map_err(|e| e.to_string())
            }
            "add_workspace_from_git_url" => {
                let sink = self.sink.clone();
                let entry = workspaces_core::add_workspace_from_git_url_core(
                    reqstr(&p, "url")?.into(),
                    reqstr(&p, "destinationPath")?.into(),
                    p.get("targetFolderName")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    &self.core.storage_path,
                    move |entry, bin, args, home| {
                        let sink = sink.clone();
                        async move {
                            app_server::spawn_workspace_session(
                                entry,
                                bin,
                                args,
                                home,
                                env!("CARGO_PKG_VERSION").to_owned(),
                                sink,
                            )
                            .await
                        }
                    },
                )
                .await?;
                serde_json::to_value(entry).map_err(|e| e.to_string())
            }
            "add_worktree" => {
                let data_dir = self
                    .core
                    .storage_path
                    .parent()
                    .ok_or("storage path has no parent directory")?
                    .to_path_buf();
                let run_context =
                    serde_json::from_value(p.get("runContext").cloned().unwrap_or(Value::Null))
                        .ok();
                let sink = self.sink.clone();
                let info =
                    workspaces_core::add_worktree_core(
                        reqstr(&p, "parentId")?.into(),
                        reqstr(&p, "branch")?.into(),
                        p.get("name").and_then(Value::as_str).map(str::to_owned),
                        p.get("copyAgentsMd")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        run_context,
                        &data_dir,
                        &self.core.workspaces,
                        &self.core.sessions,
                        &self.core.app_settings,
                        &self.core.storage_path,
                        crate::shared::worktree_core::sanitize_worktree_name,
                        crate::shared::worktree_core::unique_worktree_path_strict,
                        |root, branch| {
                            let root = root.clone();
                            let branch = branch.to_owned();
                            async move {
                                crate::shared::git_core::git_branch_exists(&root, &branch).await
                            }
                        },
                        None::<
                            fn(
                                &PathBuf,
                                &str,
                            )
                                -> std::future::Ready<Result<Option<String>, String>>,
                        >,
                        |root, args| {
                            workspaces_core::run_git_command_unit(
                                root,
                                args,
                                crate::shared::git_core::run_git_command_owned,
                            )
                        },
                        move |entry, bin, args, home| {
                            let sink = sink.clone();
                            async move {
                                app_server::spawn_workspace_session(
                                    entry,
                                    bin,
                                    args,
                                    home,
                                    env!("CARGO_PKG_VERSION").to_owned(),
                                    sink,
                                )
                                .await
                            }
                        },
                    )
                    .await?;
                serde_json::to_value(info).map_err(|e| e.to_string())
            }
            "add_clone" => {
                let sink = self.sink.clone();
                let info = workspaces_core::add_clone_core(
                    reqstr(&p, "sourceWorkspaceId")?.into(),
                    reqstr(&p, "copyName")?.into(),
                    reqstr(&p, "copiesFolder")?.into(),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    &self.core.storage_path,
                    move |entry, bin, args, home| {
                        let sink = sink.clone();
                        async move {
                            app_server::spawn_workspace_session(
                                entry,
                                bin,
                                args,
                                home,
                                env!("CARGO_PKG_VERSION").to_owned(),
                                sink,
                            )
                            .await
                        }
                    },
                )
                .await?;
                serde_json::to_value(info).map_err(|e| e.to_string())
            }
            "remove_worktree" => {
                workspaces_core::remove_worktree_core(
                    reqstr(&p, "id")?.into(),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.storage_path,
                    |root, args| {
                        workspaces_core::run_git_command_unit(
                            root,
                            args,
                            crate::shared::git_core::run_git_command_owned,
                        )
                    },
                    crate::shared::git_core::is_missing_worktree_error,
                    |path| {
                        std::fs::remove_dir_all(path)
                            .map_err(|e| format!("Failed to remove worktree folder: {e}"))
                    },
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "apply_worktree_changes" => {
                workspaces_core::apply_worktree_changes_core(
                    &self.core.workspaces,
                    reqstr(&p, "workspaceId")?.into(),
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "worktree_setup_status" => serde_json::to_value(
                workspaces_core::worktree_setup_status_core(
                    &self.core.workspaces,
                    reqstr(&p, "workspaceId")?,
                    &self
                        .core
                        .storage_path
                        .parent()
                        .ok_or("storage path has no parent directory")?
                        .to_path_buf(),
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "worktree_setup_mark_ran" => {
                workspaces_core::worktree_setup_mark_ran_core(
                    &self.core.workspaces,
                    reqstr(&p, "workspaceId")?,
                    &self
                        .core
                        .storage_path
                        .parent()
                        .ok_or("storage path has no parent directory")?
                        .to_path_buf(),
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "set_workspace_runtime_codex_args" => {
                let sink = self.sink.clone();
                let result = workspaces_core::set_workspace_runtime_codex_args_core(
                    reqstr(&p, "workspaceId")?.into(),
                    p.get("codexArgs")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    move |entry, bin, args, home| {
                        let sink = sink.clone();
                        async move {
                            app_server::spawn_workspace_session(
                                entry,
                                bin,
                                args,
                                home,
                                env!("CARGO_PKG_VERSION").to_owned(),
                                sink,
                            )
                            .await
                        }
                    },
                )
                .await?;
                serde_json::to_value(result).map_err(|e| e.to_string())
            }
            "update_workspace_settings" => {
                let settings: crate::types::WorkspaceSettings =
                    serde_json::from_value(p.get("settings").cloned().ok_or("missing settings")?)
                        .map_err(|e| e.to_string())?;
                let info = workspaces_core::update_workspace_settings_core(
                    reqstr(&p, "id")?.into(),
                    settings,
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    &self.core.storage_path,
                    |workspaces, id, settings| match workspaces.get_mut(id) {
                        Some(entry) => {
                            entry.settings = settings;
                            Ok(entry.clone())
                        }
                        None => Err("workspace not found".into()),
                    },
                    |_, _, _, _| std::future::ready(Err("unused settings spawn callback".into())),
                )
                .await?;
                serde_json::to_value(info).map_err(|e| e.to_string())
            }
            "remove_workspace" => {
                workspaces_core::remove_workspace_core(
                    reqstr(&p, "id")?.into(),
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.storage_path,
                    |root, args| {
                        workspaces_core::run_git_command_unit(
                            root,
                            args,
                            crate::shared::git_core::run_git_command_owned,
                        )
                    },
                    crate::shared::git_core::is_missing_worktree_error,
                    |path| {
                        std::fs::remove_dir_all(path)
                            .map_err(|e| format!("Failed to remove worktree folder: {e}"))
                    },
                    true,
                    true,
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "connect_workspace" => {
                let id = reqstr(&p, "id")?.to_owned();
                let sink = self.sink.clone();
                workspaces_core::connect_workspace_core(
                    id,
                    &self.core.workspaces,
                    &self.core.sessions,
                    &self.core.app_settings,
                    move |entry, bin, args, home| {
                        let sink = sink.clone();
                        async move {
                            app_server::spawn_workspace_session(
                                entry,
                                bin,
                                args,
                                home,
                                env!("CARGO_PKG_VERSION").to_owned(),
                                sink,
                            )
                            .await
                        }
                    },
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "list_workspace_files" => {
                let id = reqstr(&p, "workspaceId")?;
                let root = self
                    .core
                    .workspaces
                    .lock()
                    .await
                    .get(id)
                    .map(|w| PathBuf::from(&w.path))
                    .ok_or("workspace not found")?;
                Ok(json!(
                    crate::shared::workspace_files_core::list_workspace_files_inner(&root, 10_000)
                ))
            }
            "read_workspace_file" => {
                let id = reqstr(&p, "workspaceId")?;
                let root = self
                    .core
                    .workspaces
                    .lock()
                    .await
                    .get(id)
                    .map(|w| PathBuf::from(&w.path))
                    .ok_or("workspace not found")?;
                serde_json::to_value(
                    crate::shared::workspace_files_core::read_workspace_file_inner(
                        &root,
                        reqstr(&p, "path")?,
                    )?,
                )
                .map_err(|e| e.to_string())
            }
            "read_workspace_file_binary" => {
                let id = reqstr(&p, "workspaceId")?;
                let path = reqstr(&p, "path")?;
                let response = workspaces_core::read_workspace_file_core(
                    &self.core.workspaces,
                    id,
                    path,
                    crate::shared::workspace_files_core::read_workspace_file_binary_inner,
                )
                .await?;
                serde_json::to_value(response).map_err(|e| e.to_string())
            }
            other => Err(format!("unsupported: local workspace method {other}")),
        }
    }

    async fn codex(&self, method: &str, p: Value) -> Result<Value, String> {
        let workspace = p
            .get("workspaceId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let thread = p
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        use crate::shared::codex_core;
        match method {
            "local_usage_snapshot" => {
                let snapshot = crate::shared::local_usage_core::local_usage_snapshot_core(
                    &self.core.workspaces,
                    p.get("days")
                        .and_then(Value::as_u64)
                        .map(|days| days as u32),
                    p.get("workspacePath")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                )
                .await?;
                serde_json::to_value(snapshot).map_err(|error| error.to_string())
            }
            "get_agents_settings" => {
                serde_json::to_value(crate::shared::agents_config_core::get_agents_settings_core()?)
                    .map_err(|e| e.to_string())
            }
            "create_agent" => {
                let input = p.get("input").cloned().ok_or("missing input")?;
                let input = serde_json::from_value(input).map_err(|e| e.to_string())?;
                serde_json::to_value(crate::shared::agents_config_core::create_agent_core(input)?)
                    .map_err(|e| e.to_string())
            }
            "update_agent" => {
                let input = p.get("input").cloned().ok_or("missing input")?;
                let input = serde_json::from_value(input).map_err(|e| e.to_string())?;
                serde_json::to_value(crate::shared::agents_config_core::update_agent_core(input)?)
                    .map_err(|e| e.to_string())
            }
            "delete_agent" => {
                let input = p.get("input").cloned().ok_or("missing input")?;
                let input = serde_json::from_value(input).map_err(|e| e.to_string())?;
                serde_json::to_value(crate::shared::agents_config_core::delete_agent_core(input)?)
                    .map_err(|e| e.to_string())
            }
            "read_agent_config_toml" => serde_json::to_value(
                crate::shared::agents_config_core::read_agent_config_toml_core(reqstr(
                    &p,
                    "agentName",
                )?)?,
            )
            .map_err(|e| e.to_string()),
            "write_agent_config_toml" => {
                crate::shared::agents_config_core::write_agent_config_toml_core(
                    reqstr(&p, "agentName")?,
                    reqstr(&p, "content")?,
                )?;
                Ok(json!({"ok":true}))
            }
            "model_list" => {
                codex_core::models::model_list_core(&self.core.sessions, workspace).await
            }
            "collaboration_mode_list" => {
                codex_core::models::collaboration_mode_list_core(&self.core.sessions, workspace)
                    .await
            }
            "get_config_model" => {
                codex_core::models::get_config_model_core(&self.core.workspaces, workspace).await
            }
            "list_mcp_server_status" => {
                codex_core::mcp::list_mcp_server_status_core(
                    &self.core.sessions,
                    workspace,
                    p.get("cursor").and_then(Value::as_str).map(str::to_owned),
                    p.get("limit").and_then(Value::as_u64).map(|n| n as u32),
                )
                .await
            }
            "experimental_feature_list" => {
                codex_core::features::experimental_feature_list_core(
                    &self.core.sessions,
                    workspace,
                    p.get("cursor").and_then(Value::as_str).map(str::to_owned),
                    p.get("limit").and_then(Value::as_u64).map(|n| n as u32),
                )
                .await
            }
            "set_codex_feature_flag" => {
                let feature_key = reqstr(&p, "featureKey")?;
                let enabled = p
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or("missing or invalid `enabled`")?;
                codex_core::features::set_feature_flag_core(
                    &self.core.sessions,
                    feature_key,
                    enabled,
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "skills_list" => {
                codex_core::skills::skills_list_core(
                    &self.core.sessions,
                    &self.core.workspaces,
                    workspace,
                )
                .await
            }
            "apps_list" => {
                codex_core::apps::apps_list_core(
                    &self.core.sessions,
                    workspace,
                    p.get("cursor").and_then(Value::as_str).map(str::to_owned),
                    p.get("limit").and_then(Value::as_u64).map(|n| n as u32),
                    p.get("threadId").and_then(Value::as_str).map(str::to_owned),
                )
                .await
            }
            "account_rate_limits" => {
                codex_core::account::account_rate_limits_core(&self.core.sessions, workspace).await
            }
            "account_read" => {
                codex_core::account::account_read_core(
                    &self.core.sessions,
                    &self.core.workspaces,
                    workspace,
                )
                .await
            }
            "codex_login" => {
                codex_core::login::codex_login_core(
                    &self.core.sessions,
                    &self.core.codex_login_cancels,
                    workspace,
                )
                .await
            }
            "codex_login_cancel" => {
                codex_core::login::codex_login_cancel_core(
                    &self.core.sessions,
                    &self.core.codex_login_cancels,
                    workspace,
                )
                .await
            }
            "codex_doctor" => crate::shared::codex_aux_core::codex_doctor_core(
                &self.core.app_settings,
                p.get("codexBin").and_then(Value::as_str).map(str::to_owned),
                p.get("codexArgs")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
            .await
            .and_then(|value| serde_json::to_value(value).map_err(|e| e.to_string())),
            "codex_update" => crate::shared::codex_update_core::codex_update_core(
                &self.core.app_settings,
                p.get("codexBin").and_then(Value::as_str).map(str::to_owned),
                p.get("codexArgs")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            )
            .await
            .and_then(|value| serde_json::to_value(value).map_err(|e| e.to_string())),
            "start_thread" => {
                codex_core::threads::start_thread_core(
                    runs_rpc::tracking(
                        &self.core.storage_path,
                        serde_json::from_value(p.get("runContext").cloned().unwrap_or(Value::Null))
                            .ok(),
                    ),
                    &self.core.sessions,
                    &self.core.workspaces,
                    workspace,
                )
                .await
            }
            "resume_thread" => {
                codex_core::threads::resume_thread_core(&self.core.sessions, workspace, thread)
                    .await
            }
            "read_thread" => {
                codex_core::threads::read_thread_core(&self.core.sessions, workspace, thread).await
            }
            "start_review" => {
                codex_core::turns::start_review_core(
                    &self.core.sessions,
                    workspace,
                    thread,
                    p.get("target").cloned().ok_or("missing target")?,
                    p.get("delivery").and_then(Value::as_str).map(str::to_owned),
                )
                .await
            }
            "list_threads" => {
                codex_core::threads::list_threads_core(
                    &self.core.sessions,
                    workspace,
                    p.get("cursor").and_then(Value::as_str).map(str::to_owned),
                    p.get("limit").and_then(Value::as_u64).map(|n| n as u32),
                    p.get("sortKey").and_then(Value::as_str).map(str::to_owned),
                )
                .await
            }
            "fork_thread" => {
                codex_core::threads::fork_thread_core(&self.core.sessions, workspace, thread).await
            }
            "archive_thread" => {
                codex_core::threads::archive_thread_core(&self.core.sessions, workspace, thread)
                    .await
            }
            "compact_thread" => {
                codex_core::threads::compact_thread_core(&self.core.sessions, workspace, thread)
                    .await
            }
            "set_thread_name" => {
                codex_core::threads::set_thread_name_core(
                    &self.core.sessions,
                    workspace,
                    thread,
                    reqstr(&p, "name")?.to_owned(),
                )
                .await
            }
            "thread_live_subscribe" => {
                codex_core::subscriptions::thread_live_subscribe_core(
                    &self.core.sessions,
                    workspace.clone(),
                    thread.clone(),
                )
                .await?;
                self.sink.emit_app_server_event(crate::backend::events::AppServerEvent{workspace_id:workspace.clone(),message:json!({"method":"thread/live_attached","params":{"workspaceId":workspace,"threadId":thread,"state":"live"}})});
                Ok(json!({"state":"live"}))
            }
            "thread_live_unsubscribe" => {
                codex_core::subscriptions::thread_live_unsubscribe_core(
                    &self.core.sessions,
                    workspace,
                    thread,
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "send_user_message" => {
                let context =
                    serde_json::from_value(p.get("runContext").cloned().unwrap_or(Value::Null))
                        .map_err(|e| e.to_string())?;
                codex_core::turns::send_user_message_core(
                    runs_rpc::tracking(&self.core.storage_path, context),
                    &self.core.sessions,
                    &self.core.workspaces,
                    workspace,
                    thread,
                    reqstr(&p, "text")?.to_owned(),
                    optstr(&p, "model"),
                    optstr(&p, "effort"),
                    p.get("serviceTier").map(|v| {
                        if v.is_null() {
                            None
                        } else {
                            v.as_str().map(str::to_owned)
                        }
                    }),
                    optstr(&p, "accessMode"),
                    p.get("images").and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    }),
                    p.get("appMentions").and_then(Value::as_array).cloned(),
                    p.get("collaborationMode").cloned(),
                )
                .await
            }
            "turn_steer" => {
                codex_core::turns::turn_steer_core(
                    &self.core.sessions,
                    workspace,
                    thread,
                    reqstr(&p, "turnId")?.into(),
                    reqstr(&p, "text")?.into(),
                    p.get("images").and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    }),
                    p.get("appMentions").and_then(Value::as_array).cloned(),
                )
                .await
            }
            "turn_interrupt" => {
                codex_core::turns::turn_interrupt_core(
                    &self.core.sessions,
                    workspace,
                    thread,
                    reqstr(&p, "turnId")?.into(),
                )
                .await
            }
            "respond_to_server_request" => {
                let request_id = p.get("requestId").cloned().ok_or("missing requestId")?;
                let result = p.get("result").cloned().ok_or("missing result")?;
                codex_core::approvals::respond_to_server_request_core(
                    &self.core.sessions,
                    workspace,
                    request_id,
                    result,
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "remember_approval_rule" => {
                let command = p
                    .get("command")
                    .and_then(Value::as_array)
                    .ok_or("missing command")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                codex_core::approvals::remember_approval_rule_core(
                    &self.core.workspaces,
                    workspace,
                    command,
                )
                .await
            }
            "thread/read" | "thread/resume" => {
                let session = self
                    .core
                    .sessions
                    .lock()
                    .await
                    .get(&workspace)
                    .cloned()
                    .ok_or("workspace is not connected")?;
                let mut params = p.clone();
                params.as_object_mut().map(|m| m.remove("workspaceId"));
                session
                    .send_request_for_workspace(&workspace, method, params)
                    .await
            }
            other => Err(format!("unsupported: local Codex method {other}")),
        }
    }

    async fn files(&self, method: &str, p: Value) -> Result<Value, String> {
        let scope: crate::files::policy::FileScope =
            serde_json::from_value(p.get("scope").cloned().unwrap_or(json!("workspace")))
                .map_err(|e| e.to_string())?;
        let kind: crate::files::policy::FileKind =
            serde_json::from_value(p.get("kind").cloned().unwrap_or(json!("agents")))
                .map_err(|e| e.to_string())?;
        let workspace = p
            .get("workspaceId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match method {
            "list_workspace_files" => {
                let workspace_id = workspace.as_deref().ok_or("missing workspaceId")?;
                serde_json::to_value(
                    workspaces_core::list_workspace_files_core(
                        &self.core.workspaces,
                        workspace_id,
                        |root| {
                            crate::shared::workspace_files_core::list_workspace_files_inner(
                                root, 20_000,
                            )
                        },
                    )
                    .await?,
                )
                .map_err(|e| e.to_string())
            }
            "read_workspace_file" => {
                let workspace_id = workspace.as_deref().ok_or("missing workspaceId")?;
                let path = reqstr(&p, "path")?;
                let response = workspaces_core::read_workspace_file_core(
                    &self.core.workspaces,
                    workspace_id,
                    path,
                    crate::shared::workspace_files_core::read_workspace_file_inner,
                )
                .await?;
                serde_json::to_value(response).map_err(|e| e.to_string())
            }
            "read_workspace_file_binary" => {
                let workspace_id = workspace.as_deref().ok_or("missing workspaceId")?;
                let path = reqstr(&p, "path")?;
                let response = workspaces_core::read_workspace_file_core(
                    &self.core.workspaces,
                    workspace_id,
                    path,
                    crate::shared::workspace_files_core::read_workspace_file_binary_inner,
                )
                .await?;
                serde_json::to_value(response).map_err(|e| e.to_string())
            }
            "file_read" => serde_json::to_value(
                crate::shared::files_core::file_read_core(
                    &self.core.workspaces,
                    scope,
                    kind,
                    workspace,
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "file_write" => {
                crate::shared::files_core::file_write_core(
                    &self.core.workspaces,
                    scope,
                    kind,
                    workspace,
                    reqstr(&p, "content")?.to_owned(),
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            other => Err(format!("unsupported: local file method {other}")),
        }
    }

    async fn prompts(&self, method: &str, p: Value) -> Result<Value, String> {
        use crate::shared::prompts_core as core;
        let w = reqstr(&p, "workspaceId")?.to_owned();
        let sp = self.core.settings_path.clone();
        match method {
            "prompts_list" => {
                serde_json::to_value(core::prompts_list_core(&self.core.workspaces, &sp, w).await?)
                    .map_err(|e| e.to_string())
            }
            "prompts_create" => serde_json::to_value(
                core::prompts_create_core(
                    &self.core.workspaces,
                    &sp,
                    w,
                    reqstr(&p, "scope")?.into(),
                    reqstr(&p, "name")?.into(),
                    optstr(&p, "description"),
                    optstr(&p, "argumentHint"),
                    reqstr(&p, "content")?.into(),
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "prompts_update" => serde_json::to_value(
                core::prompts_update_core(
                    &self.core.workspaces,
                    &sp,
                    w,
                    reqstr(&p, "path")?.into(),
                    reqstr(&p, "name")?.into(),
                    optstr(&p, "description"),
                    optstr(&p, "argumentHint"),
                    reqstr(&p, "content")?.into(),
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "prompts_delete" => {
                core::prompts_delete_core(
                    &self.core.workspaces,
                    &sp,
                    w,
                    reqstr(&p, "path")?.into(),
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "prompts_move" => serde_json::to_value(
                core::prompts_move_core(
                    &self.core.workspaces,
                    &sp,
                    w,
                    reqstr(&p, "path")?.into(),
                    reqstr(&p, "scope")?.into(),
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            other => Err(format!("unsupported: local prompt method {other}")),
        }
    }

    async fn git(&self, method: &str, p: Value) -> Result<Value, String> {
        let id = reqstr(&p, "workspaceId")?.to_owned();
        use crate::shared::git_ui_core as git;
        match method {
            "get_git_status" => git::get_git_status_core(&self.core.workspaces, id).await,
            "get_git_diffs" => serde_json::to_value(
                git::get_git_diffs_core(&self.core.workspaces, &self.core.app_settings, id).await?,
            )
            .map_err(|e| e.to_string()),
            "get_git_log" => serde_json::to_value(
                git::get_git_log_core(
                    &self.core.workspaces,
                    id,
                    p.get("limit").and_then(Value::as_u64).map(|n| n as usize),
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "get_git_remote" => {
                serde_json::to_value(git::get_git_remote_core(&self.core.workspaces, id).await?)
                    .map_err(|e| e.to_string())
            }
            "list_git_branches" => git::list_git_branches_core(&self.core.workspaces, id).await,
            "stage_git_file" => {
                git::stage_git_file_core(&self.core.workspaces, id, reqstr(&p, "path")?.into())
                    .await?;
                Ok(json!({"ok":true}))
            }
            "stage_git_all" => {
                git::stage_git_all_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "unstage_git_file" => {
                git::unstage_git_file_core(&self.core.workspaces, id, reqstr(&p, "path")?.into())
                    .await?;
                Ok(json!({"ok":true}))
            }
            "revert_git_file" => {
                git::revert_git_file_core(&self.core.workspaces, id, reqstr(&p, "path")?.into())
                    .await?;
                Ok(json!({"ok":true}))
            }
            "revert_git_all" => {
                git::revert_git_all_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "checkout_git_branch" => {
                git::checkout_git_branch_core(
                    &self.core.workspaces,
                    id,
                    reqstr(&p, "name")?.into(),
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            "create_git_branch" => {
                git::create_git_branch_core(&self.core.workspaces, id, reqstr(&p, "name")?.into())
                    .await?;
                Ok(json!({"ok":true}))
            }
            "commit_git" => {
                git::commit_git_core(&self.core.workspaces, id, reqstr(&p, "message")?.into())
                    .await?;
                Ok(json!({"ok":true}))
            }
            "push_git" => {
                git::push_git_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "pull_git" => {
                git::pull_git_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "fetch_git" => {
                git::fetch_git_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "sync_git" => {
                git::sync_git_core(&self.core.workspaces, id).await?;
                Ok(json!({"ok":true}))
            }
            "get_github_issues" => {
                serde_json::to_value(git::get_github_issues_core(&self.core.workspaces, id).await?)
                    .map_err(|e| e.to_string())
            }
            "get_github_pull_requests" => serde_json::to_value(
                git::get_github_pull_requests_core(&self.core.workspaces, id).await?,
            )
            .map_err(|e| e.to_string()),
            "get_github_pull_request_diff" => serde_json::to_value(
                git::get_github_pull_request_diff_core(
                    &self.core.workspaces,
                    id,
                    p.get("prNumber")
                        .and_then(Value::as_u64)
                        .ok_or("invalid_input: missing or invalid prNumber")?,
                )
                .await?,
            )
            .map_err(|e| e.to_string()),
            "checkout_github_pull_request" => {
                git::checkout_github_pull_request_core(
                    &self.core.workspaces,
                    id,
                    p.get("prNumber")
                        .and_then(Value::as_u64)
                        .ok_or("invalid_input: missing or invalid prNumber")?,
                )
                .await?;
                Ok(json!({"ok":true}))
            }
            other => Err(format!("unsupported: local Git method {other}")),
        }
    }
}

fn reqstr<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("invalid_input: missing {key}"))
}
fn optstr(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub(crate) fn spawn_local(
    core: Arc<CoreState>,
    scope: super::contracts::BackendScope,
    wake: impl Fn() + Send + Sync + 'static,
) -> Result<BackendHandle, String> {
    let (inbox, rx) = EventInbox::new(scope.clone(), Arc::new(wake));
    Ok(BackendHandle::spawn_parts(
        LocalDriver::new(core, inbox.clone()),
        scope,
        inbox,
        rx,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::contracts::{BackendScope, RuntimeRequest};
    use crate::types::{WorkspaceEntry, WorkspaceKind, WorkspaceSettings};

    struct CodexHomeRestore(Option<std::ffi::OsString>);

    impl CodexHomeRestore {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("CODEX_HOME");
            std::env::set_var("CODEX_HOME", path);
            Self(previous)
        }
    }

    impl Drop for CodexHomeRestore {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("CODEX_HOME", value),
                None => std::env::remove_var("CODEX_HOME"),
            }
        }
    }

    #[test]
    fn local_runtime_request_reaches_shared_runtime_core_and_store() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-local-{}",
                uuid::Uuid::new_v4()
            ));
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver {
                core,
                sink: ChannelEventSink::new(inbox, None),
            };
            let result = driver
                .dispatch(BackendRequest::Runtime(RuntimeRequest::read(
                    RuntimeMethod::Profiles,
                    json!({}),
                )))
                .await
                .unwrap();
            assert_eq!(result["schemaVersion"], 3);
            assert!(result["profiles"].is_array());
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn local_usage_request_reaches_shared_usage_core() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let _env_guard = crate::codex::home::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-usage-{}",
                uuid::Uuid::new_v4()
            ));
            let codex_home = data.join("codex-home");
            std::fs::create_dir_all(&codex_home).unwrap();
            let _restore_codex_home = CodexHomeRestore::set(&codex_home);
            assert_eq!(
                std::env::var_os("CODEX_HOME").as_deref(),
                Some(codex_home.as_os_str())
            );
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver::new(core, inbox);
            let result = driver
                .dispatch(BackendRequest::Codex(
                    super::super::contracts::CodexRequest::read(
                        "local_usage_snapshot",
                        json!({"days": 7}),
                    ),
                ))
                .await
                .unwrap();
            assert_eq!(result["days"].as_array().map(Vec::len), Some(7));
            assert!(result["days"].as_array().unwrap().iter().all(|day| {
                day["day"].as_str().is_some()
                    && day["totalTokens"].as_u64() == Some(0)
                    && day["inputTokens"].as_u64() == Some(0)
                    && day["outputTokens"].as_u64() == Some(0)
            }));
            assert_eq!(result["totals"]["last7DaysTokens"].as_u64(), Some(0));
            assert_eq!(result["topModels"].as_array().map(Vec::len), Some(0));
            drop(_restore_codex_home);
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn local_feature_flag_request_uses_shared_core_and_persists_codex_config() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let _env_guard = crate::codex::home::TEST_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-feature-flag-{}",
                uuid::Uuid::new_v4()
            ));
            let codex_home = data.join("codex-home");
            std::fs::create_dir_all(&codex_home).unwrap();
            let _restore_codex_home = CodexHomeRestore::set(&codex_home);
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver::new(core, inbox);
            let response = driver
                .dispatch(BackendRequest::Codex(
                    super::super::contracts::CodexRequest::set_feature_flag("unified_exec", true),
                ))
                .await
                .unwrap();
            assert_eq!(response, json!({"ok":true}));
            let document =
                crate::shared::config_toml_core::load_global_config_document(&codex_home)
                    .unwrap()
                    .1;
            assert_eq!(
                crate::shared::config_toml_core::read_feature_flag(&document, "unified_exec"),
                Some(true)
            );
            drop(_restore_codex_home);
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn binary_workspace_file_request_returns_bytes_from_registered_workspace() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            use base64::{engine::general_purpose::STANDARD, Engine as _};

            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-binary-{}",
                uuid::Uuid::new_v4()
            ));
            let workspace_path = data.join("workspace");
            std::fs::create_dir_all(&workspace_path).unwrap();
            let asset = [0_u8, 1, 2, 127, 128, 254, 255];
            std::fs::write(workspace_path.join("asset.png"), asset).unwrap();
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            core.workspaces.lock().await.insert(
                "workspace-id".into(),
                WorkspaceEntry {
                    id: "workspace-id".into(),
                    name: "workspace".into(),
                    path: workspace_path.to_string_lossy().into_owned(),
                    kind: WorkspaceKind::Main,
                    parent_id: None,
                    worktree: None,
                    settings: WorkspaceSettings::default(),
                },
            );
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver::new(core, inbox);
            let result = driver
                .dispatch(BackendRequest::Files(
                    super::super::contracts::FilesRequest::read(
                        "read_workspace_file_binary",
                        json!({"workspaceId":"workspace-id","path":"asset.png"}),
                    ),
                ))
                .await
                .unwrap();
            assert_eq!(result["mimeType"], "image/png");
            assert_eq!(result["byteLength"], asset.len());
            assert_eq!(
                STANDARD
                    .decode(result["contentBase64"].as_str().unwrap())
                    .unwrap(),
                asset.to_vec()
            );
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn files_panel_list_request_uses_shared_workspace_file_core() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-file-list-{}",
                uuid::Uuid::new_v4()
            ));
            let workspace_path = data.join("workspace");
            std::fs::create_dir_all(workspace_path.join(".git")).unwrap();
            std::fs::write(workspace_path.join("Unicode-π — 雪.md"), "visible").unwrap();
            std::fs::write(workspace_path.join(".git/hidden-index"), "ignored").unwrap();
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            core.workspaces.lock().await.insert(
                "workspace-id".into(),
                WorkspaceEntry {
                    id: "workspace-id".into(),
                    name: "workspace".into(),
                    path: workspace_path.to_string_lossy().into_owned(),
                    kind: WorkspaceKind::Main,
                    parent_id: None,
                    worktree: None,
                    settings: WorkspaceSettings::default(),
                },
            );
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver::new(core, inbox);
            let result = driver
                .dispatch(BackendRequest::Files(
                    super::super::contracts::FilesRequest::read(
                        "list_workspace_files",
                        json!({"workspaceId":"workspace-id"}),
                    ),
                ))
                .await
                .unwrap();
            assert_eq!(result, json!(["Unicode-π — 雪.md"]));
            let git_request = driver
                .dispatch(BackendRequest::Git(
                    super::super::contracts::GitRequest::read(
                        "get_github_pull_request_diff",
                        json!({"workspaceId":"workspace-id"}),
                    ),
                ))
                .await
                .unwrap_err();
            assert!(git_request.starts_with("invalid_input: missing or invalid prNumber"));
            std::fs::remove_dir_all(data).unwrap();
        });
    }

    #[test]
    fn native_git_panel_methods_are_dispatched_to_shared_git_core() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let data = std::env::temp_dir().join(format!(
                "codex-monitor-client-git-contract-{}",
                uuid::Uuid::new_v4()
            ));
            let paths = crate::shared::host_core::HostPaths::from_data_dir(data.clone()).unwrap();
            let core = Arc::new(CoreState::load(paths).unwrap());
            let scope = BackendScope {
                endpoint_id: "local".into(),
                connection_epoch: 1,
                store_id: None,
            };
            let (inbox, _rx) = EventInbox::new(scope, Arc::new(|| {}));
            let driver = LocalDriver::new(core, inbox);
            let cases = [
                (
                    "commit_git",
                    json!({"workspaceId":"missing","message":"safe fixture"}),
                ),
                ("push_git", json!({"workspaceId":"missing"})),
                ("pull_git", json!({"workspaceId":"missing"})),
                ("fetch_git", json!({"workspaceId":"missing"})),
                ("sync_git", json!({"workspaceId":"missing"})),
                ("get_github_issues", json!({"workspaceId":"missing"})),
                ("get_github_pull_requests", json!({"workspaceId":"missing"})),
                (
                    "get_github_pull_request_diff",
                    json!({"workspaceId":"missing","prNumber":1}),
                ),
                (
                    "checkout_github_pull_request",
                    json!({"workspaceId":"missing","prNumber":1}),
                ),
            ];
            for (method, params) in cases {
                let result = driver
                    .dispatch(BackendRequest::Git(
                        super::super::contracts::GitRequest::mutation(method, params, None),
                    ))
                    .await
                    .unwrap_err();
                assert!(
                    !result.starts_with("unsupported: local Git method"),
                    "native Git method {method} must reach shared git core: {result}"
                );
            }
            std::fs::remove_dir_all(data).unwrap();
        });
    }
}
