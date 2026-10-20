use super::*;
use crate::backend::app_server::session::WorkspaceSession;
use crate::shared::host_core::{CoreState, HostPaths};
use crate::shared::process_core::kill_child_process_tree;
use crate::shared::workspaces_core;
use crate::storage::write_workspaces;
use crate::types::{AppSettings, WorkspaceEntry, WorkspaceKind, WorkspaceSettings};
use serde_json::json;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex};

fn run_async_test<F>(future: F)
where
    F: Future<Output = ()>,
{
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future);
}

fn make_temp_dir(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "codex-monitor-{prefix}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn test_state(data_dir: &std::path::Path) -> DaemonState {
    let (tx, _rx) = broadcast::channel::<DaemonEvent>(32);
    let paths = HostPaths::from_data_dir(data_dir.to_path_buf()).expect("test host paths");
    let core = CoreState::load(paths).expect("test core state");
    DaemonState {
        core,
        data_dir: data_dir.to_path_buf(),
        event_sink: DaemonEventSink {
            tx,
            run_store_path: data_dir.join("runs.sqlite3"),
        },
        daemon_binary_path: Some("/tmp/codex-monitor-daemon".to_string()),
    }
}

async fn insert_workspace(state: &DaemonState, workspace_id: &str, workspace_path: &str) {
    let entry = WorkspaceEntry {
        id: workspace_id.to_string(),
        name: "Workspace".to_string(),
        path: workspace_path.to_string(),
        kind: WorkspaceKind::Main,
        parent_id: None,
        worktree: None,
        settings: WorkspaceSettings {
            ..WorkspaceSettings::default()
        },
    };
    state
        .workspaces
        .lock()
        .await
        .insert(workspace_id.to_string(), entry);
}

fn make_workspace_entry(workspace_id: &str, workspace_path: &str) -> WorkspaceEntry {
    WorkspaceEntry {
        id: workspace_id.to_string(),
        name: workspace_id.to_string(),
        path: workspace_path.to_string(),
        kind: WorkspaceKind::Main,
        parent_id: None,
        worktree: None,
        settings: WorkspaceSettings::default(),
    }
}

fn make_session(entry: WorkspaceEntry) -> Arc<WorkspaceSession> {
    let owner_workspace_id = entry.id;
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

    Arc::new(WorkspaceSession {
        codex_args: None,
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
        workspace_ids: Mutex::new(HashSet::from([owner_workspace_id.clone()])),
        workspace_roots: Mutex::new(HashMap::new()),
        owner_workspace_id,
    })
}

#[test]
fn rpc_run_store_matches_shared_contract_and_isolates_backend_identity() {
    run_async_test(async {
        let dir = make_temp_dir("rpc-runs");
        let state = test_state(&dir);
        insert_workspace(&state, "run-workspace", dir.to_str().unwrap()).await;
        let input = json!({"requestId":uuid::Uuid::new_v4().to_string(),"workspaceId":"run-workspace","mode":"local","title":"Durable","prompt":"Initial","images":["data:image/png;base64,AAH/"],"instances":[{"model":"fixture"},{"model":"other","serviceTier":null}]});
        let run = rpc::handle_rpc_request(
            &state,
            "create_run",
            json!({"input":input.clone()}),
            "test".into(),
        )
        .await
        .unwrap();
        let retry =
            rpc::handle_rpc_request(&state, "create_run", json!({"input":input}), "test".into())
                .await
                .unwrap();
        assert_eq!(run, retry);
        let page = rpc::handle_rpc_request(
            &state,
            "list_runs",
            json!({"input":{"workspaceId":"run-workspace"}}),
            "test".into(),
        )
        .await
        .unwrap();
        assert_eq!(page["runs"][0], run);
        assert_eq!(page["storeId"], run["storeId"]);
        let instance = &run["instances"][1]["id"];
        let setup = rpc::handle_rpc_request(
            &state,
            "record_run_setup",
            json!({"input":{"instanceId":instance,"observation":{"status":"dispatched"}}}),
            "test".into(),
        )
        .await
        .unwrap();
        assert_eq!(setup["setup"]["status"], "dispatched");
        assert!(rpc::handle_rpc_request(
            &state,
            "record_run_setup",
            json!({"input":{"instanceId":instance,"observation":{"status":"succeeded"}}}),
            "test".into()
        )
        .await
        .is_err());
        state.workspaces.lock().await.clear();
        let retained = rpc::handle_rpc_request(
            &state,
            "get_run",
            json!({"input":{"runId":run["id"]}}),
            "test".into(),
        )
        .await
        .unwrap();
        assert_eq!(retained["prompt"], "Initial");
        let other = test_state(&make_temp_dir("rpc-other-runs"));
        let page = rpc::handle_rpc_request(&other, "list_runs", json!({"input":{}}), "test".into())
            .await
            .unwrap();
        assert_ne!(page["storeId"], run["storeId"]);
        assert_eq!(page["runs"], json!([]));
    });
}

#[test]
fn rpc_add_clone_uses_workspace_core_validation() {
    run_async_test(async {
        let tmp = make_temp_dir("rpc-add-clone");
        let state = test_state(&tmp);

        let err = rpc::handle_rpc_request(
            &state,
            "add_clone",
            json!({
                "sourceWorkspaceId": "source",
                "copiesFolder": tmp.to_string_lossy().to_string(),
                "copyName": "   "
            }),
            "daemon-test".to_string(),
        )
        .await
        .expect_err("expected validation error");

        assert_eq!(err, "Copy name is required.");
        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[test]
fn rpc_prompts_list_reads_workspace_prompts() {
    run_async_test(async {
        let tmp = make_temp_dir("rpc-prompts-list");
        let workspace_id = "ws-prompts";
        let workspace_dir = tmp.join("workspace");
        std::fs::create_dir_all(&workspace_dir).expect("create workspace dir");

        let state = test_state(&tmp);
        insert_workspace(&state, workspace_id, &workspace_dir.to_string_lossy()).await;

        let prompts_dir = tmp.join("workspaces").join(workspace_id).join("prompts");
        std::fs::create_dir_all(&prompts_dir).expect("create prompts dir");
        std::fs::write(prompts_dir.join("review.md"), "Prompt body").expect("write prompt");

        let result = rpc::handle_rpc_request(
            &state,
            "prompts_list",
            json!({ "workspaceId": workspace_id }),
            "daemon-test".to_string(),
        )
        .await
        .expect("prompts_list should succeed");

        let prompts = result.as_array().expect("array result");
        assert!(
            prompts.iter().any(|entry| {
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name == "review")
            }),
            "expected prompts_list to include workspace prompt"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[test]
fn rpc_local_usage_snapshot_returns_snapshot_shape() {
    run_async_test(async {
        let tmp = make_temp_dir("rpc-local-usage");
        let state = test_state(&tmp);

        let result = rpc::handle_rpc_request(
            &state,
            "local_usage_snapshot",
            json!({ "days": 7 }),
            "daemon-test".to_string(),
        )
        .await
        .expect("local_usage_snapshot should succeed");

        assert!(result.get("days").and_then(Value::as_array).is_some());
        assert!(result.get("totals").is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[test]
fn rpc_daemon_info_reports_identity() {
    run_async_test(async {
        let tmp = make_temp_dir("rpc-daemon-info");
        let state = test_state(&tmp);

        let result =
            rpc::handle_rpc_request(&state, "daemon_info", json!({}), "daemon-test".to_string())
                .await
                .expect("daemon_info should succeed");

        assert_eq!(
            result.get("name").and_then(Value::as_str),
            Some(DAEMON_NAME)
        );
        assert_eq!(result.get("mode").and_then(Value::as_str), Some("tcp"));
        assert_eq!(
            result.get("version").and_then(Value::as_str),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let _ = std::fs::remove_dir_all(&tmp);
    });
}
#[test]
fn list_workspaces_syncs_from_storage_file() {
    run_async_test(async {
        let tmp = make_temp_dir("list-workspaces-sync");
        let state = test_state(&tmp);

        let persisted = vec![WorkspaceEntry {
            id: "ws-sync".to_string(),
            name: "Synced Workspace".to_string(),
            path: tmp.join("workspace").to_string_lossy().to_string(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: WorkspaceSettings::default(),
        }];
        write_workspaces(&state.storage_path, &persisted).expect("write workspaces");

        let listed = state.list_workspaces().await;
        assert!(
            listed.iter().any(|workspace| workspace.id == "ws-sync"),
            "expected daemon list_workspaces to include workspace added on disk"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[test]
fn clone_persistence_failure_retains_checkout_and_shared_session() {
    run_async_test(async {
        let tmp = make_temp_dir("clone-persistence-failure");
        let source = tmp.join("source");
        std::fs::create_dir_all(&source).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init"])
            .current_dir(&source)
            .output()
            .unwrap()
            .status
            .success());
        let entry = make_workspace_entry("source", &source.to_string_lossy());
        let workspaces = Mutex::new(HashMap::from([("source".into(), entry.clone())]));
        let session = make_session(entry);
        let sessions = Mutex::new(HashMap::from([("source".into(), session.clone())]));
        let settings = Mutex::new(AppSettings::default());
        // An existing directory cannot be overwritten as workspaces.json.
        let error = workspaces_core::add_clone_core(
            "source".into(),
            "copy".into(),
            tmp.to_string_lossy().into(),
            &workspaces,
            &sessions,
            &settings,
            &tmp,
            |_, _, _, _| async { panic!("must reuse shared session") },
        )
        .await
        .unwrap_err();
        assert!(error.contains("retained"));
        assert!(tmp.join("copy/.git").is_dir());
        let error = workspaces_core::add_workspace_from_git_url_core(
            source.to_string_lossy().into(),
            tmp.to_string_lossy().into(),
            Some("url-copy".into()),
            &workspaces,
            &sessions,
            &settings,
            &tmp,
            |_, _, _, _| async { panic!("must reuse shared session") },
        )
        .await
        .unwrap_err();
        assert!(error.contains("retained"));
        assert!(tmp.join("url-copy/.git").is_dir());
        assert_eq!(workspaces.lock().await.len(), 1);
        assert_eq!(sessions.lock().await.len(), 1);
        assert!(session.child.lock().await.try_wait().unwrap().is_none());
        kill_child_process_tree(&mut *session.child.lock().await).await;
    });
}

#[test]
fn list_workspaces_prunes_alias_without_killing_shared_process() {
    run_async_test(async {
        let tmp = make_temp_dir("prune-shared-alias");
        let state = test_state(&tmp);
        let keep = make_workspace_entry("keep", &tmp.to_string_lossy());
        write_workspaces(&state.storage_path, &[keep.clone()]).unwrap();
        let session = make_session(keep);
        session.register_workspace("removed").await;
        {
            let mut sessions = state.sessions.lock().await;
            sessions.insert("keep".into(), session.clone());
            sessions.insert("removed".into(), session.clone());
        }
        let listed = state.list_workspaces().await;
        assert_eq!(listed.len(), 1);
        assert!(listed[0].connected);
        assert_eq!(state.sessions.lock().await.len(), 1);
        assert!(!session
            .workspace_ids_snapshot()
            .await
            .contains(&"removed".into()));
        assert!(session.child.lock().await.try_wait().unwrap().is_none());
        kill_child_process_tree(&mut *session.child.lock().await).await;
    });
}

#[test]
fn list_workspaces_sync_prunes_stale_sessions() {
    run_async_test(async {
        let tmp = make_temp_dir("list-workspaces-sync-prune");
        let state = test_state(&tmp);
        let keep_path = tmp.join("workspace-keep");
        let stale_path = tmp.join("workspace-stale");

        let persisted = vec![make_workspace_entry(
            "ws-keep",
            &keep_path.to_string_lossy(),
        )];
        write_workspaces(&state.storage_path, &persisted).expect("write workspaces");

        let keep_session = make_session(make_workspace_entry(
            "ws-keep",
            &keep_path.to_string_lossy(),
        ));
        let stale_session = make_session(make_workspace_entry(
            "ws-stale",
            &stale_path.to_string_lossy(),
        ));
        {
            let mut sessions = state.sessions.lock().await;
            sessions.insert("ws-keep".to_string(), keep_session);
            sessions.insert("ws-stale".to_string(), stale_session.clone());
        }

        let listed = state.list_workspaces().await;
        assert!(
            listed.iter().any(|workspace| workspace.id == "ws-keep"),
            "expected daemon list_workspaces to include persisted workspace"
        );

        {
            let sessions = state.sessions.lock().await;
            assert!(
                sessions.contains_key("ws-keep"),
                "expected connected persisted workspace session to remain"
            );
            assert!(
                !sessions.contains_key("ws-stale"),
                "expected stale session to be removed"
            );
        }

        let stale_session_exited = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let exited = stale_session
                    .child
                    .lock()
                    .await
                    .try_wait()
                    .expect("query stale session child");
                if exited.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .is_ok();
        assert!(
            stale_session_exited,
            "expected stale session child process to terminate"
        );

        if let Some(keep_session) = state.sessions.lock().await.remove("ws-keep") {
            let mut child = keep_session.child.lock().await;
            kill_child_process_tree(&mut child).await;
        }

        if stale_session
            .child
            .lock()
            .await
            .try_wait()
            .expect("query stale session child")
            .is_none()
        {
            let mut child = stale_session.child.lock().await;
            kill_child_process_tree(&mut child).await;
        }

        let _ = std::fs::remove_dir_all(&tmp);
    });
}

#[test]
fn recorded_opencode_worktree_does_not_spawn_codex() {
    run_async_test(async {
        let dir=make_temp_dir("runtime-worktree");
        let repo=dir.join("repo");std::fs::create_dir_all(&repo).unwrap();
        for args in [vec!["init","-q"],vec!["config","user.email","fixture@example.invalid"],vec!["config","user.name","Fixture"]] {
            assert!(std::process::Command::new("git").args(args).current_dir(&repo).status().unwrap().success());
        }
        std::fs::write(repo.join("base"),"base").unwrap();
        for args in [vec!["add","base"],vec!["commit","-qm","base"]] {assert!(std::process::Command::new("git").args(args).current_dir(&repo).status().unwrap().success());}
        let state=test_state(&dir);insert_workspace(&state,"w",repo.to_str().unwrap()).await;
        state.app_settings.lock().await.codex_bin=Some("/nonexistent/codex-must-not-start".into());
        let run=crate::shared::runs_rpc::dispatch("create_run",json!({"requestId":uuid::Uuid::new_v4().to_string(),"workspaceId":"w","title":"OpenCode","prompt":"fixture","mode":"worktree","images":[],"instances":[{"runtime":{"kind":"open_code_acp"}}]}),dir.join("runs.sqlite3"),&state.workspaces,&state.sessions).await.unwrap();
        let info=state.add_worktree("w".into(),"runtime-opencode-fixture".into(),None,false,Some(crate::shared::runs_core::types::RunContext{run_id:run["id"].as_str().unwrap().into(),instance_id:run["instances"][0]["id"].as_str().unwrap().into()}),"test".into()).await.unwrap();
        assert!(!info.connected);assert!(state.sessions.lock().await.is_empty());assert!(std::path::Path::new(&info.path).join("base").exists());
    });
}
