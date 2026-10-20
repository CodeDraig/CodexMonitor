//! Two independent registries and protocol readers contend with comparison updates.
#![cfg(unix)]
use super::{
    client_store::{AcpCallbackState, AcpTerminalSnapshot},
    registry::persist,
    tests::{dir, runtime, Fixture},
    types::*,
};
use crate::shared::runs_core::{self, capture, store::Store, types::*};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
 let mut receipts=Vec::new();let mut callback_receipts=Vec::new();let mut terminal_receipts=Vec::new();let mut n=0;
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
  // Exercise schema-4 callback journaling and terminal snapshots through the
  // same desktop/daemon-shared SQLite file while the existing runtime writes
  // and pinned-reader lock are active.
  let callback_connection=format!("stress-{role}");let callback_generation=1u64;let callback_request=json!(n);let callback_session=session["sessionId"].as_str().unwrap().to_string();let callback_method="terminal/kill".to_string();let callback_params_hash=format!("{:x}",Sha256::digest(marker.as_bytes()));
  let connection_for_begin=callback_connection.clone();let session_for_begin=callback_session.clone();let method_for_begin=callback_method.clone();let hash_for_begin=callback_params_hash.clone();let request_for_begin=callback_request.clone();
  let (operation,created)=persist(f.path.clone(),move|s|s.begin_acp_callback(&connection_for_begin,callback_generation,&request_for_begin,Some(&session_for_begin),&method_for_begin,&hash_for_begin)).await.unwrap();assert!(created,"each stress callback must create one durable intent");
  let callback_result=json!({"acknowledgedMarker":marker});let result_for_finish=callback_result.clone();let operation_id=operation.id.clone();
  let terminal_id=format!("stress-terminal-{role}-{n}");let terminal_snapshot=AcpTerminalSnapshot{session_id:callback_session.clone(),generation:callback_generation,terminal_id:terminal_id.clone(),payload:json!({"output":format!("{marker}\n"),"released":true,"exitCode":0,"truncated":false})};let snapshot_for_save=terminal_snapshot.clone();
  let finished=persist(f.path.clone(),move|s|{let finished=s.finish_acp_callback(&operation_id,Some(&result_for_finish),None)?;s.save_acp_terminal_snapshot(&snapshot_for_save)?;Ok(finished)}).await.unwrap();assert_eq!(finished.state,AcpCallbackState::Succeeded);assert_eq!(finished.result,Some(callback_result.clone()));
  callback_receipts.push(json!({"operationId":operation.id,"requestId":callback_request,"marker":marker,"state":"succeeded","result":callback_result}));
  terminal_receipts.push(json!({"sessionId":terminal_snapshot.session_id,"generation":terminal_snapshot.generation,"terminalId":terminal_snapshot.terminal_id,"payload":terminal_snapshot.payload,"marker":marker}));
  n+=1;tokio::time::sleep(Duration::from_millis(80)).await;
 }
 let sid=session["sessionId"].as_str().unwrap().to_string();
 let mut all=Vec::new();let mut cursor=0;loop{let page=f.rpc("events",json!({"sessionId":sid,"after":cursor})).await.unwrap();let page=page.as_array().unwrap();if page.is_empty(){break}cursor=page.last().unwrap()["sequence"].as_u64().unwrap();all.extend(page.clone());}
 let result=json!({"session":session,"tickets":tickets,"receipts":receipts,"callbackReceipts":callback_receipts,"terminalReceipts":terminal_receipts,"events":all,"registeredWorkspace":registered,"replaySession":replay,"replayTickets":replay_tickets,"elapsedMs":start.elapsed().as_millis()});std::fs::write(root.join(format!("{role}.result.json")),serde_json::to_vec_pretty(&result).unwrap()).unwrap();
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
    let profile = RuntimeProfile {
        working_directory: None,
        preset_id: None,
        environment: Default::default(),
        client_policy: Default::default(),
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
    let mut callback_count = 0;
    let mut terminal_receipt_count = 0;
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
        let callbacks = result["callbackReceipts"].as_array().unwrap();
        assert!(
            callbacks.len() > 100,
            "{} callback intents acknowledged",
            role
        );
        for receipt in callbacks {
            assert_eq!(receipt["state"], "succeeded");
            let marker = receipt["marker"].as_str().unwrap();
            assert_eq!(receipt["result"]["acknowledgedMarker"], marker);
            let saved = store
                .acp_callback_operation(receipt["operationId"].as_str().unwrap())
                .unwrap()
                .expect("acknowledged callback must survive reopening the shared store");
            assert_eq!(saved.state, AcpCallbackState::Succeeded);
            assert_eq!(saved.request_id, receipt["requestId"]);
            assert_eq!(saved.result.as_ref().unwrap()["acknowledgedMarker"], marker);
            assert_eq!(
                saved.session_id.as_deref(),
                result["session"]["sessionId"].as_str()
            );
            callback_count += 1;
        }
        let terminals = result["terminalReceipts"].as_array().unwrap();
        assert!(
            terminals.len() > 100,
            "{} terminal snapshots acknowledged",
            role
        );
        for receipt in terminals {
            let marker = receipt["marker"].as_str().unwrap();
            assert_eq!(receipt["payload"]["output"], format!("{marker}\n"));
            let saved = store
                .read_acp_terminal_snapshot(
                    receipt["sessionId"].as_str().unwrap(),
                    receipt["generation"].as_u64().unwrap(),
                    receipt["terminalId"].as_str().unwrap(),
                )
                .unwrap()
                .expect("acknowledged terminal snapshot must survive reopening the shared store");
            assert_eq!(saved.payload, receipt["payload"]);
            terminal_receipt_count += 1;
        }
        let latest_terminal = terminals.last().unwrap();
        let saved_terminal = store
            .read_latest_acp_terminal_snapshot(
                latest_terminal["sessionId"].as_str().unwrap(),
                latest_terminal["terminalId"].as_str().unwrap(),
            )
            .unwrap()
            .expect("latest terminal snapshot must survive reopening the shared store");
        assert_eq!(
            saved_terminal.generation,
            latest_terminal["generation"].as_u64().unwrap()
        );
        assert_eq!(saved_terminal.payload, latest_terminal["payload"]);
        let registered = crate::storage::read_workspaces(&root.join("workspaces.json")).unwrap();
        let imported = result["registeredWorkspace"]["id"].as_str().unwrap();
        assert!(registered.contains_key(imported));
        assert_eq!(
            registered[imported].settings.codex_auto_connect,
            Some(false)
        );
        let replay_sid = result["replaySession"]["sessionId"].as_str().unwrap();
        let loads = result["replayTickets"].as_array().unwrap();
        assert!(loads.len() > 100);
        for load in loads {
            let saved = store
                .history_load(load["requestId"].as_str().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(saved.state, "complete");
            assert_eq!(saved.id, load["id"]);
            let count: i64 = store
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM conversation_items WHERE session_id=? AND generation=?",
                    rusqlite::params![replay_sid, saved.id],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1);
        }
        let cached = store
            .conversation_read(replay_sid, None, 0, 200, "unowned-reader")
            .unwrap();
        assert_eq!(cached["generation"], loads.last().unwrap()["id"]);
        assert_eq!(cached["items"].as_array().unwrap().len(), 1);
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
    let saved_callback_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM acp_callback_operations", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(saved_callback_count as usize, callback_count);
    let successful_callback_count: i64 = store
        .conn
        .query_row(
            "SELECT COUNT(*) FROM acp_callback_operations WHERE state='succeeded'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(successful_callback_count as usize, callback_count);
    let saved_terminal_count: i64 = store
        .conn
        .query_row("SELECT COUNT(*) FROM acp_terminal_snapshots", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        saved_terminal_count as usize, terminal_receipt_count,
        "every acknowledged terminal snapshot remains available"
    );
    assert_eq!(terminal_receipt_count, callback_count);
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
