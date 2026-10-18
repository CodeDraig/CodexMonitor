use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::backend::app_server::session::WorkspaceSession;
use crate::codex::args::resolve_workspace_codex_args;
use crate::codex::home::resolve_workspace_codex_home;
use crate::shared::process_core::kill_child_process_tree;
use crate::types::{AppSettings, WorkspaceEntry};

use super::connect::workspace_session_spawn_lock;
use super::helpers::resolve_entry_and_parent;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WorkspaceRuntimeCodexArgsResult {
    pub(crate) applied_codex_args: Option<String>,
    pub(crate) respawned: bool,
}

pub(crate) async fn set_workspace_runtime_codex_args_core<F, Fut>(
    workspace_id: String,
    codex_args_override: Option<String>,
    workspaces: &Mutex<HashMap<String, WorkspaceEntry>>,
    sessions: &Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    app_settings: &Mutex<AppSettings>,
    spawn_session: F,
) -> Result<WorkspaceRuntimeCodexArgsResult, String>
where
    F: Fn(WorkspaceEntry, Option<String>, Option<String>, Option<PathBuf>) -> Fut,
    Fut: Future<Output = Result<Arc<WorkspaceSession>, String>>,
{
    let (entry, parent_entry) = resolve_entry_and_parent(workspaces, &workspace_id).await?;
    let _spawn_guard = workspace_session_spawn_lock().lock().await;

    let (default_bin, resolved_args) = {
        let settings = app_settings.lock().await;
        (
            settings.codex_bin.clone(),
            resolve_workspace_codex_args(&entry, parent_entry.as_ref(), Some(&settings)),
        )
    };

    let target_args = codex_args_override
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or(resolved_args);

    // If we are not connected, we can't respawn. Treat this as a no-op success; callers
    // should call again after connecting.
    let (workspace_connected, current_session) = {
        let sessions = sessions.lock().await;
        (
            sessions.contains_key(&entry.id),
            sessions.get(&entry.id).cloned(),
        )
    };
    if !workspace_connected {
        return Ok(WorkspaceRuntimeCodexArgsResult {
            applied_codex_args: target_args,
            respawned: false,
        });
    }

    let Some(current_session) = current_session else {
        return Ok(WorkspaceRuntimeCodexArgsResult {
            applied_codex_args: target_args,
            respawned: false,
        });
    };

    if current_session.codex_args == target_args {
        return Ok(WorkspaceRuntimeCodexArgsResult {
            applied_codex_args: target_args,
            respawned: false,
        });
    }

    // Keep new requests from entering the old process between the idle check and
    // replacement. References obtained before replacement must fail after retirement.
    let mut activity = current_session.activity.lock().await;
    if activity.is_busy() {
        return Err("Cannot change Codex runtime arguments while the shared session has active turns or pending requests".to_string());
    }
    let codex_home = resolve_workspace_codex_home(&entry, parent_entry.as_ref());
    let new_session =
        spawn_session(entry.clone(), default_bin, target_args.clone(), codex_home).await?;
    activity.retired = true;
    let workspace_ids = {
        let mut sessions = sessions.lock().await;
        let keys: Vec<String> = sessions.iter().filter(|(_, candidate)| Arc::ptr_eq(candidate, &current_session)).map(|(key, _)| key.clone()).collect();
        for key in &keys {
            sessions.insert(key.clone(), Arc::clone(&new_session));
        }
        keys
    };
    let workspace_paths = {
        let workspaces = workspaces.lock().await;
        workspace_ids
            .iter()
            .map(|workspace_id| {
                let path = workspaces
                    .get(workspace_id)
                    .map(|entry| entry.path.clone())
                    .unwrap_or_default();
                (workspace_id.clone(), path)
            })
            .collect::<Vec<_>>()
    };
    for (workspace_id, workspace_path) in &workspace_paths {
        let path = if workspace_path.is_empty() {
            None
        } else {
            Some(workspace_path.as_str())
        };
        new_session
            .register_workspace_with_path(workspace_id, path)
            .await;
    }
    let mut child = current_session.child.lock().await;
    kill_child_process_tree(&mut child).await;

    Ok(WorkspaceRuntimeCodexArgsResult {
        applied_codex_args: target_args,
        respawned: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Stdio;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use tokio::process::Command;

    use crate::types::{WorkspaceKind, WorkspaceSettings};

    fn make_workspace_entry(id: &str) -> WorkspaceEntry {
        WorkspaceEntry {
            id: id.to_string(),
            name: id.to_string(),
            path: "/tmp".to_string(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: WorkspaceSettings::default(),
        }
    }

    fn make_session(_entry: WorkspaceEntry, codex_args: Option<String>) -> WorkspaceSession {
        let mut cmd = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "more"]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "cat"]);
            cmd
        };

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().expect("spawn dummy child");
        let stdin = child.stdin.take().expect("dummy child stdin");

        WorkspaceSession {
            codex_args,
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            activity: Mutex::new(Default::default()),
            pending: Mutex::new(HashMap::new()),
            request_context: Mutex::new(HashMap::new()),
            thread_workspace: Mutex::new(HashMap::new()),
            hidden_thread_ids: Mutex::new(HashSet::new()),
            next_id: AtomicU64::new(0),
            launch_identity: None,
            initialization: Default::default(),
            runtime_subscribers: Default::default(),
            run_recorders: Mutex::new(HashMap::new()),
            background_thread_callbacks: Mutex::new(HashMap::new()),
            owner_workspace_id: "test-owner".to_string(),
            workspace_ids: Mutex::new(HashSet::from(["test-owner".to_string()])),
            workspace_roots: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn runtime_change_rejects_shared_activity_without_spawning_or_killing() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for pending in [false, true] {
                let entry = make_workspace_entry("ws-1");
                let other = make_workspace_entry("ws-2");
                let workspaces = Mutex::new(HashMap::from([
                    (entry.id.clone(), entry.clone()),
                    (other.id.clone(), other.clone()),
                ]));
                let session = Arc::new(make_session(entry.clone(), Some("--old".into())));
                if pending {
                    session
                        .activity
                        .lock()
                        .await
                        .begin_request(1, "turn/start", Some("other-thread".into()))
                        .unwrap();
                } else {
                    session.activity.lock().await.observe(
                        &serde_json::json!({"method":"turn/started"}),
                        Some("other-thread".into()),
                    );
                }
                let sessions = Mutex::new(HashMap::from([
                    (entry.id.clone(), session.clone()),
                    (other.id.clone(), session.clone()),
                ]));
                let settings = Mutex::new(AppSettings::default());
                let result = set_workspace_runtime_codex_args_core(
                    entry.id.clone(),
                    Some("--new".into()),
                    &workspaces,
                    &sessions,
                    &settings,
                    |_, _, _, _| async { panic!("must not spawn while busy") },
                )
                .await;
                assert!(result
                    .unwrap_err()
                    .contains("active turns or pending requests"));
                assert!(Arc::ptr_eq(
                    sessions.lock().await.get(&other.id).unwrap(),
                    &session
                ));
                assert!(session.child.lock().await.try_wait().unwrap().is_none());
                assert!(!session.activity.lock().await.retired);
                kill_child_process_tree(&mut *session.child.lock().await).await;
            }
        });
    }

    #[test]
    fn set_workspace_runtime_codex_args_is_noop_when_workspace_not_connected() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let entry = make_workspace_entry("ws-1");
            let workspaces = Mutex::new(HashMap::from([(entry.id.clone(), entry.clone())]));
            let sessions = Mutex::new(HashMap::<String, Arc<WorkspaceSession>>::new());
            let app_settings = Mutex::new(AppSettings::default());

            let spawn_calls = Arc::new(AtomicUsize::new(0));
            let spawn_calls_ref = spawn_calls.clone();

            let result = set_workspace_runtime_codex_args_core(
                entry.id.clone(),
                Some("  --profile dev  ".to_string()),
                &workspaces,
                &sessions,
                &app_settings,
                move |entry, _bin, args, _home| {
                    let spawn_calls_ref = spawn_calls_ref.clone();
                    async move {
                        spawn_calls_ref.fetch_add(1, Ordering::SeqCst);
                        Ok(Arc::new(make_session(entry, args)))
                    }
                },
            )
            .await
            .expect("core call succeeds");

            assert_eq!(
                result,
                WorkspaceRuntimeCodexArgsResult {
                    applied_codex_args: Some("--profile dev".to_string()),
                    respawned: false
                }
            );
            assert_eq!(spawn_calls.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn set_workspace_runtime_codex_args_is_noop_when_args_match() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let entry = make_workspace_entry("ws-1");
            let workspaces = Mutex::new(HashMap::from([(entry.id.clone(), entry.clone())]));
            let current_session = Arc::new(make_session(entry.clone(), Some("--same".to_string())));
            let sessions = Mutex::new(HashMap::from([(entry.id.clone(), current_session.clone())]));
            let app_settings = Mutex::new(AppSettings::default());

            let spawn_calls = Arc::new(AtomicUsize::new(0));
            let spawn_calls_ref = spawn_calls.clone();

            let result = set_workspace_runtime_codex_args_core(
                entry.id.clone(),
                Some("--same".to_string()),
                &workspaces,
                &sessions,
                &app_settings,
                move |entry, _bin, args, _home| {
                    let spawn_calls_ref = spawn_calls_ref.clone();
                    async move {
                        spawn_calls_ref.fetch_add(1, Ordering::SeqCst);
                        Ok(Arc::new(make_session(entry, args)))
                    }
                },
            )
            .await
            .expect("core call succeeds");

            assert_eq!(
                result,
                WorkspaceRuntimeCodexArgsResult {
                    applied_codex_args: Some("--same".to_string()),
                    respawned: false
                }
            );
            assert_eq!(spawn_calls.load(Ordering::SeqCst), 0);
        });
    }

    #[test]
    fn set_workspace_runtime_codex_args_respawns_when_args_change() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let entry = make_workspace_entry("ws-1");
            let workspaces = Mutex::new(HashMap::from([(entry.id.clone(), entry.clone())]));
            let current_session = Arc::new(make_session(entry.clone(), Some("--old".to_string())));
            let sessions = Mutex::new(HashMap::from([(entry.id.clone(), current_session.clone())]));
            let app_settings = Mutex::new(AppSettings::default());

            let spawn_calls = Arc::new(AtomicUsize::new(0));
            let spawn_calls_ref = spawn_calls.clone();

            let result = set_workspace_runtime_codex_args_core(
                entry.id.clone(),
                Some("--new".to_string()),
                &workspaces,
                &sessions,
                &app_settings,
                move |entry, _bin, args, _home| {
                    let spawn_calls_ref = spawn_calls_ref.clone();
                    async move {
                        spawn_calls_ref.fetch_add(1, Ordering::SeqCst);
                        Ok(Arc::new(make_session(entry, args)))
                    }
                },
            )
            .await
            .expect("core call succeeds");

            assert_eq!(
                result,
                WorkspaceRuntimeCodexArgsResult {
                    applied_codex_args: Some("--new".to_string()),
                    respawned: true
                }
            );
            assert_eq!(spawn_calls.load(Ordering::SeqCst), 1);

            let next = sessions
                .lock()
                .await
                .get(&entry.id)
                .expect("session updated")
                .codex_args
                .clone();
            assert_eq!(next, Some("--new".to_string()));
            assert!(current_session
                .send_request("turn/start", serde_json::json!({"threadId":"stale"}))
                .await
                .unwrap_err()
                .contains("replaced"));
            let new_session = sessions.lock().await.get(&entry.id).cloned().unwrap();
            kill_child_process_tree(&mut *new_session.child.lock().await).await;
        });
    }
    #[test]
    fn runtime_change_preserves_an_unrelated_process_and_its_activity() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let entry=make_workspace_entry("selected");let other_entry=make_workspace_entry("unrelated");
            let current=Arc::new(make_session(entry.clone(),Some("--old".into())));
            let other=Arc::new(make_session(other_entry.clone(),Some("--unrelated".into())));
            other.activity.lock().await.begin_request(99,"turn/start",Some("other-turn".into())).unwrap();
            let workspaces=Mutex::new(HashMap::from([(entry.id.clone(),entry.clone()),(other_entry.id.clone(),other_entry.clone())]));
            let sessions=Mutex::new(HashMap::from([(entry.id.clone(),current),(other_entry.id.clone(),other.clone())]));
            set_workspace_runtime_codex_args_core(entry.id.clone(),Some("--new".into()),&workspaces,&sessions,&Mutex::new(AppSettings::default()),|entry,_,args,_|async move{Ok(Arc::new(make_session(entry,args)))}).await.unwrap();
            assert!(Arc::ptr_eq(sessions.lock().await.get(&other_entry.id).unwrap(),&other));
            assert!(!other.activity.lock().await.retired);assert!(other.child.lock().await.try_wait().unwrap().is_none());
            for session in sessions.lock().await.values(){kill_child_process_tree(&mut *session.child.lock().await).await;}
        });
    }

}
