use crate::backend::app_server::{
    session::WorkspaceSession, spawn_workspace_session as spawn_workspace_session_inner,
};
use crate::event_sink::TauriEventSink;
use crate::types::WorkspaceEntry;
use std::{path::PathBuf, sync::Arc};
use tauri::AppHandle;

pub(crate) mod args;
pub(crate) mod config;
pub(crate) mod home;

pub(crate) async fn spawn_workspace_session(
    entry: WorkspaceEntry,
    default_codex_bin: Option<String>,
    codex_args: Option<String>,
    app_handle: AppHandle,
    codex_home: Option<PathBuf>,
) -> Result<Arc<WorkspaceSession>, String> {
    let client_version = app_handle.package_info().version.to_string();
    let event_sink = TauriEventSink::new(app_handle);
    spawn_workspace_session_inner(
        entry,
        default_codex_bin,
        codex_args,
        codex_home,
        client_version,
        event_sink,
    )
    .await
}

pub(crate) mod account;
pub(crate) mod agents;
pub(crate) mod approvals;
pub(crate) mod apps;
pub(crate) mod features;
pub(crate) mod installation;
pub(crate) mod login;
pub(crate) mod mcp;
pub(crate) mod metadata;
pub(crate) mod models;
pub(crate) mod skills;
pub(crate) mod subscriptions;
pub(crate) mod threads;
pub(crate) mod turns;
