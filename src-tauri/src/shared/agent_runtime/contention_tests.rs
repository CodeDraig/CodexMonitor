//! Two independent registries and protocol readers contend with comparison updates.
#![cfg(unix)]
use super::{
    registry::persist,
    tests::{dir, runtime, Fixture},
    types::*,
};
use crate::shared::runs_core::{self, capture, store::Store, types::*};
use serde_json::{json, Value};
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const WORKER: &str = "shared::agent_runtime::contention_tests::mixed_runtime_worker";
#[test]
fn mixed_runtime_worker() {
    let Some(root) = std::env::var_os("MONITOR_RUNTIME_STRESS_DIR").map(PathBuf::from) else {
        return;
    };
    let role = std::env::var("MONITOR_RUNTIME_STRESS_ROLE").unwrap();
    runtime().block_on(async{
 let fixture:Value=serde_json::from_slice(&std::fs::read(root.join("fixture.json")).unwrap()).unwrap();
 let mut f=Fixture::new();f.path=root.join("runs.sqlite3");let cwd=root.join(&role);std::fs::create_dir_all(&cwd).unwrap();f.workspaces.lock().await.get_mut("w").unwrap().path=cwd.to_string_lossy().into();
 let profile=if role=="desktop"{let script=root.join("fake-codex");std::fs::write(&script,include_str!("fixtures/fake_codex.py")).unwrap();std::fs::set_permissions(&script,std::fs::Permissions::from_mode(0o700)).unwrap();f.settings.lock().await.codex_bin=Some(script.to_string_lossy().into());"codex"}else{"fixture"};
 let session=f.rpc("create_session",json!({"workspaceId":"w","profileId":profile})).await.unwrap();
 let start=Instant::now();let first=f.submit(&session,"slow initial evidence",Some(RunContext{run_id:fixture["runId"].as_str().unwrap().into(),instance_id:fixture[&role].as_str().unwrap().into()})).await;
 std::fs::write(root.join(format!("{role}.ready")),serde_json::to_vec(&session).unwrap()).unwrap();
 while !root.join("go").exists(){tokio::time::sleep(Duration::from_millis(20)).await;}
 let replay=f.rpc("create_session",json!({"workspaceId":"w","profileId":"fixture"})).await.unwrap();
 let imported=cwd.join("imported-agent-directory");std::fs::create_dir_all(&imported).unwrap();
 let registered=crate::shared::workspaces_core::register_workspace_core(&imported.to_string_lossy(),None,&f.workspaces,&root.join("workspaces.json")).await.unwrap();
 let mut replay_tickets=Vec::new();
 let mut tickets=vec![first.clone()];f.terminal(&first).await;
 let mut receipts=Vec::new();let mut n=0;
 while start.elapsed()<Duration::from_secs(62){
  let marker=format!("{role}:{n}");
  let ticket=f.submit(&session,&marker,None).await;f.terminal(&ticket).await;tickets.push(ticket);
  loop{
   let path=f.path.clone();let left=fixture["left"].as_str().unwrap().to_string();let right=fixture["right"].as_str().unwrap().to_string();let saved_marker=marker.clone();let marker=marker.clone();
   let result=persist(path,move|s|{let old=s.comparisons(None,Some(&left))?.into_iter().find(|c|c.left_capture_id==left&&c.right_capture_id==right);let revision=old.as_ref().map(|c|c.revision).unwrap_or(0);let notes=format!("{}{}\n",old.map(|c|c.notes).unwrap_or_default(),marker);s.save_comparison(left.clone(),right.clone(),"tie".into(),notes,revision)}).await;
   match result{Ok(saved)=>{receipts.push(json!({"revision":saved.revision,"marker":saved_marker}));break},Err(e) if e.starts_with("conflict:")=>continue,Err(e)=>panic!("{e}")}
  }
  let request=runs_core::id();let load=f.rpc("load_conversation_history",json!({"sessionId":replay["sessionId"],"requestId":request})).await.unwrap();
  let duplicate=f.rpc("load_conversation_history",json!({"sessionId":replay["sessionId"],"requestId":request})).await.unwrap();assert_eq!(load["id"],duplicate["id"]);
  loop {let view=f.rpc("read_conversation",json!({"sessionId":replay["sessionId"]})).await.unwrap();if view["load"]["state"]=="complete"{assert_eq!(view["generation"],load["id"]);assert_eq!(view["items"].as_array().unwrap().len(),1);break;}tokio::time::sleep(Duration::from_millis(10)).await;}
  replay_tickets.push(load);
  n+=1;tokio::time::sleep(Duration::from_millis(80)).await;
 }
 let sid=session["sessionId"].as_str().unwrap().to_string();
 let mut all=Vec::new();let mut cursor=0;loop{let page=f.rpc("events",json!({"sessionId":sid,"after":cursor})).await.unwrap();let page=page.as_array().unwrap();if page.is_empty(){break}cursor=page.last().unwrap()["sequence"].as_u64().unwrap();all.extend(page.clone());}
 let result=json!({"session":session,"tickets":tickets,"receipts":receipts,"events":all,"registeredWorkspace":registered,"replaySession":replay,"replayTickets":replay_tickets,"elapsedMs":start.elapsed().as_millis()});std::fs::write(root.join(format!("{role}.result.json")),serde_json::to_vec_pretty(&result).unwrap()).unwrap();
 f.rpc("disconnect",json!({"sessionId":sid})).await.unwrap();f.rpc("disconnect",json!({"sessionId":replay["sessionId"]})).await.unwrap();
 for c in f.sessions.lock().await.values(){crate::shared::process_core::kill_child_process_tree(&mut *c.child.lock().await).await;}
 });
}
#[test]
#[ignore = "62-second cross-process acceptance; run explicitly with --ignored"]
fn sustained_mixed_runtime_writers_preserve_events_captures_and_notes() {
    let root = dir();
    let mut f = Fixture::new();
    f.path = root.join("runs.sqlite3");
    let mut store = Store::open(&f.path).unwrap();
    let profile = RuntimeProfile { working_directory: None,
        id: "fixture".into(),
        revision: 0,
        name: "Fixture".into(),
        kind: RuntimeKind::OpenCodeAcp,
        executable: "python3".into(),
        args: vec![format!(
            "{}/src/shared/agent_runtime/fixtures/fake_acp.py",
            env!("CARGO_MANIFEST_DIR")
        )],
    };
    store.save_runtime_profile(profile, 0).unwrap();
    let run = store
        .create_run(
            CreateRun {
                request_id: runs_core::id(),
                workspace_id: "w".into(),
                title: "Mixed runtime contention".into(),
                prompt: "Evidence".into(),
                mode: "local".into(),
                images: vec![],
                instances: vec![json!({}), json!({})],
            },
            "Fixture".into(),
            root.to_string_lossy().into(),
            None,
            json!({}),
            vec![],
        )
        .unwrap();
    let captures: Vec<_> = run
        .instances
        .iter()
        .map(|i| {
            store
                .append_capture(capture::assemble(
                    i,
                    "old".into(),
                    CaptureKind::Manual,
                    runs_core::id(),
                    "Baseline".into(),
                    Some(json!({"status":"completed","items":[]})),
                    vec![],
                    json!({"errors":[]}),
                    vec![],
                ))
                .unwrap()
        })
        .collect();
    let fixture = json!({"runId":run.id,"desktop":run.instances[0].id,"daemon":run.instances[1].id,"left":captures[0].id,"right":captures[1].id});
    std::fs::write(
        root.join("fixture.json"),
        serde_json::to_vec(&fixture).unwrap(),
    )
    .unwrap();
    let reader = rusqlite::Connection::open(&f.path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM instances;")
        .unwrap();
    let mut children = Vec::new();
    for role in ["desktop", "daemon"] {
        let log = std::fs::File::create(root.join(format!("{role}.log"))).unwrap();
        let executable = if role == "daemon" {
            std::env::var_os("MONITOR_RUNTIME_STRESS_DAEMON_EXE")
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_exe().unwrap())
        } else {
            std::env::current_exe().unwrap()
        };
        children.push(
            Command::new(executable)
                .args(["--exact", WORKER, "--nocapture"])
                .env("MONITOR_RUNTIME_STRESS_DIR", &root)
                .env("MONITOR_RUNTIME_STRESS_ROLE", role)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
    }
    eprintln!("Mixed runtime acceptance evidence: {}", root.display());
    let deadline = Instant::now() + Duration::from_secs(120);
    while !root.join("desktop.ready").exists() || !root.join("daemon.ready").exists() {
        assert!(
            Instant::now() < deadline,
            "workers not ready: {}",
            root.display()
        );
        for child in &mut children {
            assert!(
                child.try_wait().unwrap().is_none(),
                "worker failed: {}",
                root.display()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let blocker = rusqlite::Connection::open(&f.path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    std::fs::write(root.join("go"), b"go").unwrap();
    std::thread::sleep(Duration::from_millis(6500));
    blocker.execute_batch("ROLLBACK").unwrap();
    loop {
        let done: Vec<_> = children.iter_mut().map(|c| c.try_wait().unwrap()).collect();
        for status in done.iter().flatten() {
            assert!(status.success(), "worker failed: {}", root.display());
        }
        if done.iter().all(Option::is_some) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "workers timed out: {}",
            root.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    reader.execute_batch("ROLLBACK").unwrap();
    drop(store);
    let store = Store::open(&f.path).unwrap();
    let comparison = &store.comparisons(None, Some(&captures[0].id)).unwrap()[0];
    let mut receipt_count = 0;
    for role in ["desktop", "daemon"] {
        let result: Value = serde_json::from_slice(
            &std::fs::read(root.join(format!("{role}.result.json"))).unwrap(),
        )
        .unwrap();
        assert!(result["elapsedMs"].as_u64().unwrap() >= 60_000);
        let receipts = result["receipts"].as_array().unwrap();
        assert!(receipts.len() > 100);
        for receipt in receipts {
            assert_eq!(
                comparison
                    .notes
                    .lines()
                    .filter(|line| *line == receipt["marker"].as_str().unwrap())
                    .count(),
                1
            );
            receipt_count += 1;
        }
        let registered=crate::storage::read_workspaces(&root.join("workspaces.json")).unwrap();
        let imported=result["registeredWorkspace"]["id"].as_str().unwrap();assert!(registered.contains_key(imported));assert_eq!(registered[imported].settings.codex_auto_connect,Some(false));
        let replay_sid=result["replaySession"]["sessionId"].as_str().unwrap();
        let loads=result["replayTickets"].as_array().unwrap();assert!(loads.len()>100);
        for load in loads {
            let saved=store.history_load(load["requestId"].as_str().unwrap()).unwrap().unwrap();assert_eq!(saved.state,"complete");assert_eq!(saved.id,load["id"]);
            let count:i64=store.conn.query_row("SELECT COUNT(*) FROM conversation_items WHERE session_id=? AND generation=?",rusqlite::params![replay_sid,saved.id],|r|r.get(0)).unwrap();assert_eq!(count,1);
        }
        let cached=store.conversation_read(replay_sid,None,0,200,"unowned-reader").unwrap();assert_eq!(cached["generation"],loads.last().unwrap()["id"]);assert_eq!(cached["items"].as_array().unwrap().len(),1);
        let sid = result["session"]["sessionId"].as_str().unwrap();
        let history = store.runtime_history(sid).unwrap();
        assert_eq!(history.len(), result["tickets"].as_array().unwrap().len());
        for t in &history {
            assert_eq!(t.execution, ExecutionOutcome::Completed);
            let count: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM runtime_events WHERE execution_id=?",
                    [&t.execution_id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(count >= 3);
        }
        let instance = store.instance(fixture[role].as_str().unwrap()).unwrap();
        let initial = instance
            .captures
            .iter()
            .find(|c| c.kind == CaptureKind::InitialTurn)
            .unwrap();
        let capture = store.capture(&initial.id).unwrap();
        assert_eq!(
            capture.execution_id.as_deref(),
            history.first().map(|t| t.execution_id.as_str())
        );
        assert!(capture
            .items
            .iter()
            .any(|v| v["text"] == "slow initial evidence"));
    }
    assert_eq!(comparison.revision, receipt_count);
    let integrity: String = store
        .conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let foreign: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(foreign, 0);
    std::fs::write(root.join("acceptance.json"),serde_json::to_vec_pretty(&json!({"passed":true,"comparisonRevision":comparison.revision,"notes":comparison.notes,"storeId":store.store_id})).unwrap()).unwrap();
}
