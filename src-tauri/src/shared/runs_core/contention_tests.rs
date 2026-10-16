//! Cross-process acceptance of the shared production recorder and RPC dispatcher.
use super::{
    capture, id, recording,
    store::{self, Store},
    types::*,
};
use crate::shared::runs_rpc;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const WORKER: &str = "shared::runs_core::contention_tests::shared_store_process_worker";
fn read(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
async fn rpc(path: &Path, method: &str, input: Value) -> store::Result<Value> {
    runs_rpc::dispatch(
        method,
        input,
        path.into(),
        &Mutex::new(Default::default()),
        &Mutex::new(Default::default()),
    )
    .await
}
fn event(role: &str, index: usize) -> Value {
    json!({"method":"item/completed","params":{"turnId":"initial","item":{
        "id":format!("{role}:{index}"),"type":"agentMessage","text":format!("{role}:{index}:{}", "e".repeat(2048))
    }}})
}
fn snapshot(instance: &RunInstance, request: String, events: Vec<Value>) -> RunCapture {
    let items: Vec<_> = events
        .iter()
        .filter_map(|e| e.pointer("/params/item").cloned())
        .collect();
    capture::assemble(
        instance,
        "initial".into(),
        CaptureKind::Manual,
        request,
        "stress evidence".into(),
        Some(json!({"status":"completed","items":items})),
        events,
        json!({"errors":[]}),
        vec![],
    )
}

// Invoked by the parent in separate OS processes. A normal test run is a no-op.
#[test]
fn shared_store_process_worker() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(worker());
}
async fn worker() {
    let Ok(dir) = std::env::var("CODEX_RUN_CONTENTION_WORKER_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let role = std::env::var("CODEX_RUN_CONTENTION_ROLE").unwrap();
    let fixture = read(&dir.join("fixture.json"));
    let path = dir.join("runs.sqlite3");
    let n = fixture["iterations"].as_u64().unwrap() as usize;
    let pace = Duration::from_millis(fixture["durationMs"].as_u64().unwrap() / n as u64);
    let context: RunContext = serde_json::from_value(fixture[&role].clone()).unwrap();
    let tracking = recording::Tracking {
        path: path.clone(),
        context: context.clone(),
    };
    write(&dir.join(format!("{role}.ready")), &json!(true));
    let deadline = Instant::now() + Duration::from_secs(120);
    while !dir.join("go").exists() {
        assert!(Instant::now() < deadline, "parent did not release worker");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let started = Instant::now();
    let evidence = async {
        for index in 0..n {
            assert!(recording::persist_event(&tracking, &event(&role, index))
                .await
                .unwrap());
            tokio::time::sleep(pace).await;
        }
        let terminal = json!({"method":"turn/completed","params":{"turn":{"id":"initial","status":"completed"}}});
        assert!(recording::persist_event(&tracking, &terminal)
            .await
            .unwrap());
        let mut store = Store::open(&path).unwrap();
        let instance = store.instance(&context.instance_id).unwrap();
        let events = store.read_evidence(&instance.id, "initial").unwrap();
        let capture = snapshot(&instance, format!("{role}-final"), events);
        // Retry the same immutable, idempotent capture, never regenerate evidence on a retry.
        loop {
            match store.append_capture(capture.clone()) {
                Ok(saved) => break saved.id,
                Err(error) if store::is_busy(&error) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    };
    let comparisons = async {
        let mut receipts = vec![];
        let mut conflicts = 0;
        let mut busy = 0;
        let mut polls = 0;
        for index in 0..n {
            let token = format!("{role}:{index}");
            loop {
                let saved = rpc(
                    &path,
                    "list_run_comparisons",
                    json!({"runId":context.run_id}),
                )
                .await
                .unwrap();
                let (revision, mut notes): (u64, Vec<String>) =
                    if saved.as_array().unwrap().is_empty() {
                        (0, vec![])
                    } else {
                        (
                            saved[0]["revision"].as_u64().unwrap(),
                            serde_json::from_str(saved[0]["notes"].as_str().unwrap()).unwrap(),
                        )
                    };
                assert!(!notes.contains(&token));
                notes.push(token.clone());
                // Ensure both processes initially submit revision zero under the held writer lock.
                let result = rpc(&path, "save_run_comparison", json!({
                    "leftCaptureId":fixture["left"], "rightCaptureId":fixture["right"],
                    "choice":"tie", "notes":serde_json::to_string(&notes).unwrap(), "expectedRevision":revision
                })).await;
                match result {
                    Ok(value) => {
                        receipts.push(json!({"revision":value["revision"],"token":token}));
                        break;
                    }
                    Err(error) if error.starts_with("conflict:") => conflicts += 1,
                    Err(error) if store::is_busy(&error) => busy += 1,
                    Err(error) => panic!("{error}"),
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let run = rpc(&path, "get_run", json!({"runId":context.run_id}))
                .await
                .unwrap();
            assert_eq!(run["storeId"], fixture["storeId"]);
            rpc(&path, "list_runs", json!({"workspaceId":"stress"}))
                .await
                .unwrap();
            polls += 2;
            tokio::time::sleep(pace).await;
        }
        json!({"receipts":receipts,"conflicts":conflicts,"busy":busy,"polls":polls})
    };
    let (capture, mut result) = tokio::time::timeout(Duration::from_secs(110), async {
        futures_util::future::join(evidence, comparisons).await
    })
    .await
    .unwrap();
    result["capture"] = json!(capture);
    result["elapsedMs"] = json!(started.elapsed().as_millis());
    result["executable"] = json!(std::env::current_exe().unwrap());
    write(&dir.join(format!("{role}.result.json")), &result);
}

struct Children(Vec<Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
#[test]
fn sustained_desktop_daemon_writers_share_store() {
    let dir = std::env::temp_dir().join(format!("codex-run-contention-{}", id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("runs.sqlite3");
    let n: usize = std::env::var("CODEX_RUN_CONTENTION_ITERATIONS")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(120);
    let duration_ms: u64 = std::env::var("CODEX_RUN_CONTENTION_DURATION_MS")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(10_000);
    assert!(n > 0);
    let mut store = Store::open(&path).unwrap();
    let run = store
        .create_run(
            CreateRun {
                request_id: id(),
                workspace_id: "stress".into(),
                title: "concurrent writers".into(),
                prompt: "preserve evidence".into(),
                mode: "local".into(),
                images: vec![],
                instances: vec![json!({"model":"desktop"}), json!({"model":"daemon"})],
            },
            "stress".into(),
            dir.to_string_lossy().into(),
            None,
            Value::Null,
            vec![],
        )
        .unwrap();
    for instance in &run.instances {
        store
            .update_instance(&instance.id, |i| {
                i.initial_turn_id = Some("initial".into());
                i.submission_outcome = SubmissionOutcome::Accepted;
                Ok(())
            })
            .unwrap();
    }
    let left = store
        .append_capture(snapshot(&run.instances[0], id(), vec![]))
        .unwrap();
    let right = store
        .append_capture(snapshot(&run.instances[1], id(), vec![]))
        .unwrap();
    let fixture = json!({"iterations":n,"durationMs":duration_ms,"storeId":store.store_id,"left":left.id,"right":right.id,
        "desktop":{"runId":run.id,"instanceId":run.instances[0].id},"daemon":{"runId":run.id,"instanceId":run.instances[1].id}});
    write(&dir.join("fixture.json"), &fixture);
    // Hold an actual read snapshot for the whole workload, including >1000 WAL pages.
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM instances;")
        .unwrap();
    let mut children = Children(vec![]);
    let desktop = std::env::current_exe().unwrap();
    let daemon = std::env::var_os("CODEX_RUN_CONTENTION_DAEMON_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| desktop.clone());
    for (role, exe) in [("desktop", &desktop), ("daemon", &daemon)] {
        let log = std::fs::File::create(dir.join(format!("{role}.log"))).unwrap();
        children.0.push(
            Command::new(exe)
                .args(["--exact", WORKER, "--nocapture"])
                .env("CODEX_RUN_CONTENTION_WORKER_DIR", &dir)
                .env("CODEX_RUN_CONTENTION_ROLE", role)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
    }
    eprintln!("Contention evidence: {}", dir.display());
    let deadline = Instant::now() + Duration::from_secs(120);
    while !dir.join("desktop.ready").exists() || !dir.join("daemon.ready").exists() {
        assert!(
            Instant::now() < deadline,
            "workers not ready; inspect {}",
            dir.display()
        );
        for child in &mut children.0 {
            assert!(
                child.try_wait().unwrap().is_none(),
                "worker exited early: {}",
                dir.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let blocker = Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    write(&dir.join("go"), &json!(true));
    std::thread::sleep(Duration::from_millis(6200)); // Deliberately exceeds the production busy timeout.
    blocker.execute_batch("ROLLBACK").unwrap();
    loop {
        let done = children
            .0
            .iter_mut()
            .map(|child| child.try_wait().unwrap())
            .collect::<Vec<_>>();
        for status in done.iter().flatten() {
            assert!(status.success(), "worker failed; inspect {}", dir.display());
        }
        if done.iter().all(Option::is_some) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "workers timed out; inspect {}",
            dir.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let wal_bytes = std::fs::metadata(dir.join("runs.sqlite3-wal"))
        .unwrap()
        .len();
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    drop(blocker);
    drop(store);
    let store = Store::open(&path).unwrap(); // Verify committed state after both processes exit.
    assert_eq!(store.store_id, fixture["storeId"].as_str().unwrap());
    let mut receipts = vec![];
    let mut stats = json!({});
    for (role, instance) in [
        ("desktop", &run.instances[0]),
        ("daemon", &run.instances[1]),
    ] {
        let result = read(&dir.join(format!("{role}.result.json")));
        assert!(
            result["busy"].as_u64().unwrap() > 0,
            "busy-timeout path was not exercised"
        );
        receipts.extend(result["receipts"].as_array().unwrap().clone());
        let events = store.read_evidence(&instance.id, "initial").unwrap();
        assert_eq!(events.len(), n + 1);
        for index in 0..n {
            assert!(
                events.contains(&event(role, index)),
                "missing or altered {role}:{index}"
            );
        }
        let current = store.instance(&instance.id).unwrap();
        assert_eq!(current.execution_outcome, ExecutionOutcome::Completed);
        assert_eq!(current.initial_turn_id.as_deref(), Some("initial"));
        assert_eq!(current.captures.len(), 2);
        let saved = store.capture(result["capture"].as_str().unwrap()).unwrap();
        assert_eq!(saved.items.len(), n);
        for index in 0..n {
            assert!(saved.items.contains(&event(role, index)["params"]["item"]));
        }
        assert!(saved.complete);
        stats[role] = json!({"events":events.len(),"captureItems":saved.items.len(),"busy":result["busy"],"conflicts":result["conflicts"],"polls":result["polls"],"elapsedMs":result["elapsedMs"],"executable":result["executable"]});
    }
    assert_eq!(
        serde_json::to_value(store.capture(&left.id).unwrap()).unwrap(),
        serde_json::to_value(&left).unwrap()
    );
    assert_eq!(
        serde_json::to_value(store.capture(&right.id).unwrap()).unwrap(),
        serde_json::to_value(&right).unwrap()
    );
    receipts.sort_by_key(|r| r["revision"].as_u64().unwrap());
    let saved = store.comparisons(Some(&run.id), None).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].revision as usize, 2 * n);
    let notes: Vec<String> = serde_json::from_str(&saved[0].notes).unwrap();
    assert_eq!(notes.len(), 2 * n);
    assert_eq!(notes.iter().collect::<HashSet<_>>().len(), 2 * n);
    for (index, receipt) in receipts.iter().enumerate() {
        assert_eq!(receipt["revision"], json!(index + 1));
        assert_eq!(receipt["token"], notes[index]);
    }
    assert_eq!(receipts.len(), 2 * n);
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    stats["comparisonRevisions"] = json!(2 * n);
    stats["walBytesWithReader"] = json!(wal_bytes);
    stats["mixedExecutables"] = json!(desktop != daemon);
    stats["integrity"] = json!("ok");
    write(&dir.join("result.json"), &stats);
    eprintln!("{}", serde_json::to_string_pretty(&stats).unwrap());
}
