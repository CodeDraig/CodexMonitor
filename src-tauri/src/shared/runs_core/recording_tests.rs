//! Exercises the real app-server reader and shared recorder without a model service.
#![cfg(unix)]
use super::*;
use crate::backend::{
    app_server::spawn_workspace_session,
    events::{AppServerEvent, EventSink, TerminalExit, TerminalOutput},
};
use crate::shared::runs_core::{id, store::Store};
use crate::types::{WorkspaceEntry, WorkspaceKind, WorkspaceSettings};
use std::os::unix::fs::PermissionsExt;

#[derive(Clone)]
struct Sink;
impl EventSink for Sink {
    fn emit_app_server_event(&self, _: AppServerEvent) {}
    fn emit_terminal_output(&self, _: TerminalOutput) {}
    fn emit_terminal_exit(&self, _: TerminalExit) {}
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
async fn fixture() -> (Arc<WorkspaceSession>, Tracking, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("run-recorder-{}", id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("fake-codex");
    std::fs::write(&script, include_str!("test_app_server.py")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entry = WorkspaceEntry {
        id: "workspace".into(),
        name: "Fixture".into(),
        path: dir.to_string_lossy().into(),
        kind: WorkspaceKind::Main,
        parent_id: None,
        worktree: None,
        settings: WorkspaceSettings::default(),
    };
    let path = dir.join("runs.sqlite3");
    let mut store = Store::open(&path).unwrap();
    let run = store
        .create_run(
            CreateRun {
                request_id: id(),
                workspace_id: entry.id.clone(),
                title: "Fixture".into(),
                prompt: "Original".into(),
                mode: "local".into(),
                images: vec![],
                instances: vec![json!({"model":"fixture"})],
            },
            entry.name.clone(),
            entry.path.clone(),
            None,
            Value::Null,
            vec![],
        )
        .unwrap();
    let tracking = Tracking {
        path,
        context: RunContext {
            run_id: run.id,
            instance_id: run.instances[0].id.clone(),
        },
    };
    let session = spawn_workspace_session(
        entry,
        Some(script.to_string_lossy().into()),
        None,
        None,
        "test".into(),
        Sink,
    )
    .await
    .unwrap();
    (session, tracking, dir)
}
async fn thread(session: &Arc<WorkspaceSession>, tracking: &Tracking, dir: &std::path::Path) {
    let response = tracked_request(
        session,
        "workspace",
        "thread/start",
        json!({"cwd":dir}),
        Some(tracking.clone()),
    )
    .await
    .unwrap();
    assert_eq!(response["result"]["thread"]["id"], "thread-1");
}
#[test]
fn fast_events_record_without_frontend_and_followups_do_not_rewrite() {
    runtime().block_on(async {
    let (session,tracking,dir)=fixture().await; thread(&session,&tracking,&dir).await;
    let params=json!({"threadId":"thread-1","cwd":dir,"input":[{"type":"text","text":"Original"}],"model":"accepted"});
    let result=tracked_request(&session,"workspace","turn/start",params.clone(),Some(tracking.clone())).await.unwrap();
    assert_eq!(result["runReceipt"]["outcome"],"accepted");
    let capture=tokio::time::timeout(std::time::Duration::from_secs(5),async {
        loop {
            let store=Store::open(&tracking.path).unwrap();
            let instance=store.instance(&tracking.context.instance_id).unwrap();
            if let Some(capture)=instance.captures.first() {break store.capture(&capture.id).unwrap();}
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    assert_eq!(capture.turn_id,"t1"); assert_eq!(capture.items[0]["text"],"first result");
    assert_eq!(capture.usage.as_ref().unwrap()["totalTokens"],12);
    let repeat=tracked_request(&session,"workspace","turn/start",params,Some(tracking.clone())).await.unwrap();
    assert_eq!(repeat["result"]["turn"]["id"],"t1");
    session.send_request_for_workspace("workspace","turn/start",json!({"threadId":"thread-1","model":"followup"})).await.unwrap();
    let manual=capture_turn(&session,tracking.clone(),"t2".into(),CaptureKind::Manual,Some(id()),"Later".into()).await.unwrap();
    assert_eq!(manual.items[0]["text"],"later result");
    let store=Store::open(&tracking.path).unwrap(); assert_eq!(store.capture(&capture.id).unwrap().items,capture.items);
    assert_eq!(store.instance(&tracking.context.instance_id).unwrap().initial_turn_id.as_deref(),Some("t1"));
    session.child.lock().await.kill().await.unwrap();
});
}
#[test]
fn rejection_malformed_reply_and_lost_reply_have_distinct_outcomes() {
    runtime().block_on(async {
        for (model, expected) in [
            ("rejected", "rejected"),
            ("malformed", "unknown"),
            ("lost", "unknown"),
        ] {
            let (session, tracking, dir) = fixture().await;
            thread(&session, &tracking, &dir).await;
            let params = json!({"threadId":"thread-1","cwd":dir,"model":model});
            if model == "lost" {
                let s = session.clone();
                let count = dir.join("turn-count");
                tokio::spawn(async move {
                    loop {
                        if count.exists() && !s.pending.lock().await.is_empty() {
                            s.pending.lock().await.clear();
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                });
            }
            let _ = tracked_request(
                &session,
                "workspace",
                "turn/start",
                params.clone(),
                Some(tracking.clone()),
            )
            .await;
            let store = Store::open(&tracking.path).unwrap();
            let instance = store.instance(&tracking.context.instance_id).unwrap();
            assert_eq!(
                serde_json::to_value(instance.submission_outcome).unwrap(),
                json!(expected)
            );
            assert!(instance.initial_turn_id.is_none());
            let _ = tracked_request(
                &session,
                "workspace",
                "turn/start",
                params,
                Some(tracking.clone()),
            )
            .await;
            let count = std::fs::read_to_string(dir.join("turn-count")).unwrap();
            assert_eq!(count.lines().count(), 1);
            session.child.lock().await.kill().await.unwrap();
        }
    });
}
#[test]
fn recorded_worktrees_pin_base_and_refuse_branch_adoption() {
    runtime().block_on(async {
        use crate::shared::workspaces_core::add_worktree_core;
        use crate::types::AppSettings;
        use std::collections::HashMap;
        use tokio::sync::Mutex;
        let (session, _, dir) = fixture().await;
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "Fixture"]);
        std::fs::write(dir.join("base"), "a\n").unwrap();
        git(&["add", "base"]);
        git(&["commit", "-m", "A"]);
        let repo = git2::Repository::open(&dir).unwrap();
        let base = repo.head().unwrap().target().unwrap().to_string();
        let entry = WorkspaceEntry {
            id: "workspace".into(),
            name: "Fixture".into(),
            path: dir.to_string_lossy().into(),
            kind: WorkspaceKind::Main,
            parent_id: None,
            worktree: None,
            settings: WorkspaceSettings::default(),
        };
        let mut store = Store::open(&dir.join("runs.sqlite3")).unwrap();
        let run = store
            .create_run(
                CreateRun {
                    request_id: id(),
                    workspace_id: entry.id.clone(),
                    title: "batch".into(),
                    prompt: "test".into(),
                    mode: "worktree".into(),
                    images: vec![],
                    instances: vec![json!({}), json!({}), json!({})],
                },
                entry.name.clone(),
                entry.path.clone(),
                Some(base.clone()),
                Value::Null,
                vec![],
            )
            .unwrap();
        let workspaces = Mutex::new(HashMap::from([(entry.id.clone(), entry)]));
        let sessions = Mutex::new(HashMap::from([("workspace".into(), session.clone())]));
        let settings = Mutex::new(AppSettings::default());
        let storage = dir.join("workspaces.json");
        for (index, branch) in ["fixture-one", "fixture-two", "fixture-one"]
            .iter()
            .enumerate()
        {
            let result = add_worktree_core(
                "workspace".into(),
                branch.to_string(),
                None,
                false,
                Some(RunContext {
                    run_id: run.id.clone(),
                    instance_id: run.instances[index].id.clone(),
                }),
                &dir,
                &workspaces,
                &sessions,
                &settings,
                &storage,
                |s| s.into(),
                |root, name| Ok(root.join(name)),
                |root, branch| {
                    std::future::ready(Ok(git2::Repository::open(root)
                        .unwrap()
                        .find_branch(branch, git2::BranchType::Local)
                        .is_ok()))
                },
                None::<fn(&std::path::PathBuf, &str) -> std::future::Ready<Result<Option<String>>>>,
                |root, args| {
                    let output = std::process::Command::new("git")
                        .current_dir(root)
                        .args(args)
                        .output()
                        .unwrap();
                    std::future::ready(if output.status.success() {
                        Ok(())
                    } else {
                        Err(String::from_utf8_lossy(&output.stderr).into())
                    })
                },
                |_, _, _, _| std::future::ready(Ok(session.clone())),
            )
            .await;
            if index == 2 {
                assert!(result.unwrap_err().contains("already exists"));
                break;
            }
            let workspace = result.unwrap();
            assert_eq!(
                git2::Repository::open(&workspace.path)
                    .unwrap()
                    .head()
                    .unwrap()
                    .target()
                    .unwrap()
                    .to_string(),
                base
            );
            if index == 0 {
                std::fs::write(dir.join("base"), "b\n").unwrap();
                git(&["add", "base"]);
                git(&["commit", "-m", "B"]);
            }
        }
        assert_ne!(repo.head().unwrap().target().unwrap().to_string(), base);
        session.child.lock().await.kill().await.unwrap();
    });
}
#[test]
fn restart_recovers_known_turn_without_resubmission_and_page_failure_is_incomplete() {
    runtime().block_on(async {
        let (session, tracking, dir) = fixture().await;
        let mut store = Store::open(&tracking.path).unwrap();
        store
            .update_instance(&tracking.context.instance_id, |i| {
                i.thread_id = Some("thread-1".into());
                i.initial_turn_id = Some("t1".into());
                i.submission_outcome = SubmissionOutcome::Accepted;
                i.execution_outcome = ExecutionOutcome::Running;
                Ok(())
            })
            .unwrap();
        restore(session.clone(), tracking.path.clone());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if !Store::open(&tracking.path)
                    .unwrap()
                    .instance(&tracking.context.instance_id)
                    .unwrap()
                    .captures
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            !dir.join("turn-count").exists(),
            "Recovery must not launch a turn"
        );
        std::fs::write(dir.join("fail-history"), "fail second page").unwrap();
        let capture = capture_turn(
            &session,
            tracking.clone(),
            "t1".into(),
            CaptureKind::Recovered,
            Some(id()),
            "Recovery failure".into(),
        )
        .await
        .unwrap();
        assert!(!capture.complete);
        assert!(capture
            .errors
            .iter()
            .any(|e| e.contains("required history page failed")));
        session.child.lock().await.kill().await.unwrap();
    });
}
#[test]
fn uncertain_initial_submission_cannot_capture_a_later_followup_as_primary() {
    runtime().block_on(async {
        let (session, tracking, _) = fixture().await;
        let mut store = Store::open(&tracking.path).unwrap();
        store
            .update_instance(&tracking.context.instance_id, |i| {
                i.thread_id = Some("thread-1".into());
                i.submission_outcome = SubmissionOutcome::Unknown;
                Ok(())
            })
            .unwrap();
        register(&session, "thread-1", tracking.clone()).await;
        session
            .send_request_for_workspace(
                "workspace",
                "turn/start",
                json!({"threadId":"thread-1","model":"followup"}),
            )
            .await
            .unwrap();
        // Let queued notifications finish before inspecting the durable record.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let instance = store.instance(&tracking.context.instance_id).unwrap();
        assert!(instance.initial_turn_id.is_none());
        assert!(instance.captures.is_empty());
        session.child.lock().await.kill().await.unwrap();
    });
}

#[test]
fn reconciliation_never_freezes_active_turn_on_history_failure_and_supports_full_read() {
    runtime().block_on(async {
        use std::collections::HashMap;
        use tokio::sync::Mutex;
        let (session, tracking, dir) = fixture().await;
        git2::Repository::init(&dir).unwrap();
        let mut store = Store::open(&tracking.path).unwrap();
        store.update_instance(&tracking.context.instance_id, |i| {
            i.thread_id = Some("thread-1".into());
            i.initial_turn_id = Some("t1".into());
            i.submission_outcome = SubmissionOutcome::Accepted;
            i.execution_outcome = ExecutionOutcome::Running;
            Ok(())
        }).unwrap();
        let workspaces = Mutex::new(HashMap::new());
        let sessions = Mutex::new(HashMap::from([("workspace".into(), session.clone())]));
        std::fs::write(dir.join("fail-history"), "required page unavailable").unwrap();
        for _ in 0..2 {
            let result = crate::shared::runs_rpc::dispatch("reconcile_run",
                json!({"runId": tracking.context.run_id}), tracking.path.clone(), &workspaces, &sessions).await.unwrap();
            assert!(!result["errors"].as_array().unwrap().is_empty());
            assert!(store.instance(&tracking.context.instance_id).unwrap().captures.is_empty());
        }
        std::fs::write(dir.join("unsupported-history"), "legacy server").unwrap();
        std::fs::write(dir.join("running"), "still running").unwrap();
        let result = crate::shared::runs_rpc::dispatch("reconcile_run",
            json!({"runId": tracking.context.run_id}), tracking.path.clone(), &workspaces, &sessions).await.unwrap();
        assert!(result["errors"].as_array().unwrap().is_empty());
        assert!(store.instance(&tracking.context.instance_id).unwrap().captures.is_empty());
        std::fs::remove_file(dir.join("running")).unwrap();
        crate::shared::runs_rpc::dispatch("reconcile_run",
            json!({"runId": tracking.context.run_id}), tracking.path.clone(), &workspaces, &sessions).await.unwrap();
        let instance = store.instance(&tracking.context.instance_id).unwrap();
        assert_eq!(instance.captures.len(), 1);
        let capture = store.capture(&instance.captures[0].id).unwrap();
        assert!(capture.complete);
        assert_eq!(capture.items[0]["text"], "legacy full result");
        assert_eq!(capture.kind, CaptureKind::InitialTurn);
        assert!(!dir.join("turn-count").exists());
        session.child.lock().await.kill().await.unwrap();
    });
}

#[test]
fn queued_recorder_retains_every_event_past_sqlite_busy_timeout() {
    runtime().block_on(async {
        let (session, tracking, dir) = fixture().await;
        git2::Repository::init(&dir).unwrap();
        let mut store = Store::open(&tracking.path).unwrap();
        store
            .update_instance(&tracking.context.instance_id, |i| {
                i.thread_id = Some("thread-1".into());
                i.submission_outcome = SubmissionOutcome::Submitting;
                Ok(())
            })
            .unwrap();
        register(&session, "thread-1", tracking.clone()).await;
        let blocker = rusqlite::Connection::open(&tracking.path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        // The stdout reader must still deliver the RPC response while its recorder waits.
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.send_request_for_workspace(
                "workspace",
                "turn/start",
                json!({"threadId":"thread-1","model":"accepted"}),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(6200)).await;
        blocker.execute_batch("ROLLBACK").unwrap();
        let capture = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let instance = store.instance(&tracking.context.instance_id).unwrap();
                if let Some(capture) = instance.captures.first() {
                    break store.capture(&capture.id).unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let evidence = store
            .read_evidence(&tracking.context.instance_id, "t1")
            .unwrap();
        assert_eq!(
            evidence.len(),
            4,
            "No queued event may be dropped after BUSY: {evidence:?}"
        );
        assert!(evidence.iter().any(|e| e["method"] == "turn/started"));
        assert!(evidence.iter().any(|e| e["method"] == "turn/completed"));
        assert_eq!(capture.items[0]["text"], "first result");
        assert_eq!(capture.usage.unwrap()["totalTokens"], 12);
        session.child.lock().await.kill().await.unwrap();
    });
}

#[test]
fn accepted_response_survives_busy_timeout_without_resubmission() {
    runtime().block_on(async {
        let (session, tracking, dir) = fixture().await;
        git2::Repository::init(&dir).unwrap();
        thread(&session, &tracking, &dir).await;
        let params = json!({"threadId":"thread-1","cwd":dir,"model":"receipt-lock"});
        let task = {
            let session = session.clone();
            let tracking = tracking.clone();
            let params = params.clone();
            tokio::spawn(async move {
                tracked_request(&session, "workspace", "turn/start", params, Some(tracking)).await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !dir.join("response-ready").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let blocker = rusqlite::Connection::open(&tracking.path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        std::fs::write(dir.join("release-response"), "go").unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(6200)).await;
        assert!(
            !task.is_finished(),
            "Receipt must stay pending while SQLite is busy"
        );
        blocker.execute_batch("ROLLBACK").unwrap();
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response["result"]["turn"]["id"], "t1");
        let repeat = tracked_request(
            &session,
            "workspace",
            "turn/start",
            params,
            Some(tracking.clone()),
        )
        .await
        .unwrap();
        assert_eq!(repeat["result"], response["result"]);
        assert_eq!(
            std::fs::read_to_string(dir.join("turn-count")).unwrap(),
            "turn\n"
        );
        let store = Store::open(&tracking.path).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while store
                .instance(&tracking.context.instance_id)
                .unwrap()
                .captures
                .is_empty()
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            store
                .read_evidence(&tracking.context.instance_id, "t1")
                .unwrap()
                .len(),
            4
        );
        session.child.lock().await.kill().await.unwrap();
    });
}

#[test]
fn capture_waits_out_busy_timeout_and_keeps_request_identity() {
    runtime().block_on(async {
        let (session, tracking, dir) = fixture().await;
        git2::Repository::init(&dir).unwrap();
        let mut store = Store::open(&tracking.path).unwrap();
        store.update_instance(&tracking.context.instance_id, |i| {
            i.thread_id = Some("thread-1".into());
            i.initial_turn_id = Some("t1".into());
            i.submission_outcome = SubmissionOutcome::Accepted;
            Ok(())
        }).unwrap();
        let blocker = rusqlite::Connection::open(&tracking.path).unwrap();
        blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let task = {
            let session = session.clone();
            let tracking = tracking.clone();
            tokio::spawn(async move { capture_turn(&session, tracking, "t1".into(), CaptureKind::Manual,
                Some("busy-capture".into()), "Retained capture".into()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(6200)).await;
        assert!(!task.is_finished(), "Capture must stay pending while SQLite is busy");
        blocker.execute_batch("ROLLBACK").unwrap();
        let saved = tokio::time::timeout(std::time::Duration::from_secs(10), task).await.unwrap().unwrap().unwrap();
        assert!(saved.complete);
        assert_eq!(saved.items[0]["text"], "first result");
        assert_eq!(saved.request_id, "busy-capture");
        assert_eq!(store.instance(&tracking.context.instance_id).unwrap().captures.len(), 1);
        assert_eq!(store.capture_by_request("busy-capture").unwrap().unwrap().id, saved.id);
        session.child.lock().await.kill().await.unwrap();
    });
}
