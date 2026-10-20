use super::*;
fn path() -> PathBuf {
    std::env::temp_dir()
        .join(format!("codex-runs-test-{}", id()))
        .join("runs.sqlite3")
}
fn create(store: &mut Store, request: &str) -> RunRecord {
    store
        .create_run(
            CreateRun {
                request_id: request.into(),
                workspace_id: "w".into(),
                title: "run".into(),
                prompt: "original".into(),
                mode: "local".into(),
                images: vec![],
                instances: vec![
                    json!({"model":"a"}),
                    json!({"model":"b","serviceTier":null}),
                ],
            },
            "workspace".into(),
            "/tmp/workspace".into(),
            None,
            Value::Null,
            vec![vec![0, 1, 255]],
        )
        .unwrap()
}
fn capture(instance: &RunInstance, kind: CaptureKind, turn: &str) -> RunCapture {
    super::super::capture::assemble(
        instance,
        turn.into(),
        kind,
        id(),
        "capture".into(),
        Some(
            json!({"status":"completed","items":[{"id":"i","type":"agentMessage","text":"tests passed"}]}),
        ),
        vec![],
        json!({"errors":[]}),
        vec![],
    )
}
#[test]
fn durable_identity_attachments_and_idempotency() {
    let path = path();
    let mut a = Store::open(&path).unwrap();
    let request = id();
    let run = create(&mut a, &request);
    let store_id = a.store_id.clone();
    drop(a);
    let mut b = Store::open(&path).unwrap();
    assert_eq!(b.store_id, store_id);
    let loaded = create(&mut b, &request);
    assert_eq!(run.id, loaded.id);
    assert_eq!(loaded.instances.len(), 2);
    assert_eq!(loaded.instances[0].id, run.instances[0].id);
    assert_eq!(loaded.prompt, "original");
    assert!(!loaded.instances[0]
        .requested
        .as_object()
        .unwrap()
        .contains_key("serviceTier"));
    assert_eq!(loaded.instances[1].requested["serviceTier"], Value::Null);
    let bytes: Vec<u8> = b
        .conn
        .query_row(
            "SELECT data FROM attachments WHERE id=?",
            [&run.attachments[0]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bytes, [0, 1, 255]);
}
#[test]
fn independent_connections_receipts_and_revision_conflicts() {
    let path = path();
    let mut a = Store::open(&path).unwrap();
    let run = create(&mut a, &id());
    let mut b = Store::open(&path).unwrap();
    a.update_instance(&run.instances[0].id, |i| {
        i.error = Some("preparation failed".into());
        Ok(())
    })
    .unwrap();
    b.update_instance(&run.instances[1].id, |i| {
        i.preparation = "ready".into();
        Ok(())
    })
    .unwrap();
    assert_eq!(
        a.get_run(&run.id).unwrap().instances[1].preparation,
        "ready"
    );
    assert_eq!(
        b.get_run(&run.id).unwrap().instances[0].error.as_deref(),
        Some("preparation failed")
    );
    let context = RunContext {
        run_id: run.id,
        instance_id: run.instances[0].id.clone(),
    };
    let OperationDisposition::Execute { id: op } = a
        .begin_operation(&context, "turn", json!({"prompt":"x"}))
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        b.begin_operation(&context, "turn", json!({"prompt":"x"}))
            .unwrap(),
        OperationDisposition::Unresolved { .. }
    ));
    a.finish_operation(&op, json!({"turn":"t1"}), |_| Ok(()))
        .unwrap();
    assert!(matches!(
        b.begin_operation(&context, "turn", json!({"prompt":"x"}))
            .unwrap(),
        OperationDisposition::Completed { .. }
    ));
    assert!(b
        .begin_operation(&context, "turn", json!({"prompt":"different"}))
        .is_err());
    let c1 = a
        .append_capture(capture(&run.instances[0], CaptureKind::InitialTurn, "t1"))
        .unwrap();
    let c2 = b
        .append_capture(capture(&run.instances[1], CaptureKind::InitialTurn, "t2"))
        .unwrap();
    let saved = a
        .save_comparison(
            c1.id.clone(),
            c2.id.clone(),
            "left".into(),
            "evidence".into(),
            0,
        )
        .unwrap();
    assert!(b
        .save_comparison(
            c1.id.clone(),
            c2.id.clone(),
            "right".into(),
            "stale".into(),
            0
        )
        .unwrap_err()
        .starts_with("conflict:"));
    assert_eq!(
        b.comparisons(None, Some(&c1.id)).unwrap()[0].notes,
        saved.notes
    );
    let manual = b
        .append_capture(capture(&run.instances[0], CaptureKind::Manual, "later"))
        .unwrap();
    assert_ne!(manual.id, c1.id);
    assert_eq!(b.capture(&c1.id).unwrap().turn_id, "t1");
    assert_eq!(b.append_capture(c1.clone()).unwrap().id, c1.id);
}
#[test]
fn unsupported_schema_is_not_empty_history() {
    let path = path();
    let s = Store::open(&path).unwrap();
    s.conn.execute_batch("PRAGMA user_version=99").unwrap();
    drop(s);
    assert!(Store::open(&path).err().unwrap().contains("Unsupported"));
}
#[test]
fn competing_connections_grant_exactly_one_execution() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let run = create(&mut store, &id());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let context = RunContext {
        run_id: run.id,
        instance_id: run.instances[0].id.clone(),
    };
    let handles = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = barrier.clone();
            let context = context.clone();
            std::thread::spawn(move || {
                let mut store = Store::open(&path).unwrap();
                barrier.wait();
                store
                    .begin_operation(&context, "thread/start", json!({"cwd":"same"}))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, OperationDisposition::Execute { .. }))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, OperationDisposition::Unresolved { .. }))
            .count(),
        1
    );
}

#[test]
fn active_reader_does_not_block_evidence_commit() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let run = create(&mut store, &id());
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM instances;")
        .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let instance = run.instances[0].id.clone();
    let worker = std::thread::spawn(move || {
        let result = Store::open(&path).and_then(|mut store| {
            store.evidence(
                &instance,
                "t1",
                "item:held-reader",
                &json!({"id":"held-reader"}),
            )
        });
        tx.send(result).unwrap();
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(2));
    reader.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    result
        .expect("A reader must not stall evidence commits")
        .unwrap();
    assert_eq!(
        store
            .read_evidence(&run.instances[0].id, "t1")
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn opening_existing_store_for_reads_does_not_request_writer_lock() {
    let path = path();
    let store = Store::open(&path).unwrap();
    store.conn.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(Store::open(&path).map(|s| s.store_id)).unwrap();
    });
    let result = rx.recv_timeout(std::time::Duration::from_secs(2));
    store.conn.execute_batch("ROLLBACK").unwrap();
    worker.join().unwrap();
    assert_eq!(
        result
            .expect("Reading an initialized store must not reserve a writer lock")
            .unwrap(),
        store.store_id
    );
}

#[test]
fn event_and_instance_transition_commit_atomically() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let run = create(&mut store, &id());
    let context = RunContext {
        run_id: run.id,
        instance_id: run.instances[0].id.clone(),
    };
    store
        .update_instance(&context.instance_id, |i| {
            i.submission_outcome = SubmissionOutcome::Submitting;
            Ok(())
        })
        .unwrap();
    store.conn.execute_batch("CREATE TRIGGER reject_transition BEFORE UPDATE ON instances BEGIN SELECT RAISE(ABORT, 'injected transition failure'); END;").unwrap();
    let event =
        json!({"method":"turn/started","params":{"turn":{"id":"t1","status":"inProgress"}}});
    assert!(store
        .record_event(&context, &event)
        .unwrap_err()
        .contains("injected transition failure"));
    assert!(store
        .read_evidence(&context.instance_id, "t1")
        .unwrap()
        .is_empty());
    assert!(store
        .instance(&context.instance_id)
        .unwrap()
        .initial_turn_id
        .is_none());
}

#[test]
fn rollback_journal_upgrade_preserves_existing_records() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let run = create(&mut store, &id());
    let store_id = store.store_id.clone();
    store
        .conn
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.store_id, store_id);
    assert_eq!(
        serde_json::to_value(store.get_run(&run.id).unwrap()).unwrap(),
        serde_json::to_value(run).unwrap()
    );
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(
        store
            .conn
            .query_row("PRAGMA synchronous", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn synchronized_concurrent_first_opens_initialize_empty_and_migrate_seeded_store() {
    const OPENERS: usize = 16;

    // Repeated cold starts place many independent connections at the first
    // journal-mode/schema transition, as desktop Profiles and Catalog do.
    for _ in 0..4 {
        let empty_path = path();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(OPENERS));
        let handles = (0..OPENERS)
            .map(|_| {
                let path = empty_path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::open(&path).map(|store| {
                        let version = store
                            .conn
                            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                            .unwrap();
                        let integrity = store
                            .conn
                            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                            .unwrap();
                        (store.store_id.clone(), version, integrity)
                    })
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("first-open worker panicked"))
            .collect::<Result<Vec<_>>>();
        let opened = results.expect("all concurrent first opens must succeed");
        assert_eq!(opened.len(), OPENERS);
        assert!(opened
            .iter()
            .all(|(_, version, integrity)| { *version == 3 && integrity == "ok" }));
        assert!(opened
            .iter()
            .all(|(store_id, _, _)| store_id == &opened[0].0));
    }

    // Also exercise concurrent first opens during a populated schema-2
    // upgrade. Existing run/instance/attachment payload bytes must survive
    // competing openers while only one of them performs the migration.
    let migration_path = path();
    let mut seeded = Store::open(&migration_path).unwrap();
    let run = create(&mut seeded, &id());
    let run_payload: String = seeded
        .conn
        .query_row("SELECT payload FROM runs WHERE id=?", [&run.id], |row| {
            row.get(0)
        })
        .unwrap();
    let instance_payloads = run
        .instances
        .iter()
        .map(|instance| {
            seeded
                .conn
                .query_row(
                    "SELECT payload FROM instances WHERE id=?",
                    [&instance.id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let attachment_payloads = run
        .attachments
        .iter()
        .map(|attachment| {
            seeded
                .conn
                .query_row(
                    "SELECT data FROM attachments WHERE id=?",
                    [attachment],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let store_id = seeded.store_id.clone();
    seeded
        .conn
        .execute_batch(
            "DROP TABLE conversation_attachments;
             DROP TABLE conversation_items;
             DROP TABLE conversation_snapshots;
             DROP TABLE history_loads;
             DROP TABLE conversation_aliases;
             DROP TABLE session_catalog;
             PRAGMA user_version=2;",
        )
        .unwrap();
    drop(seeded);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(OPENERS));
    let handles = (0..OPENERS)
        .map(|_| {
            let path = migration_path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Store::open(&path).map(|store| {
                    let version = store
                        .conn
                        .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                        .unwrap();
                    let integrity = store
                        .conn
                        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                        .unwrap();
                    (store.store_id.clone(), version, integrity)
                })
            })
        })
        .collect::<Vec<_>>();
    let opened = handles
        .into_iter()
        .map(|handle| handle.join().expect("migration worker panicked"))
        .collect::<Result<Vec<_>>>()
        .expect("all concurrent schema-2 opens must succeed");
    assert_eq!(opened.len(), OPENERS);
    assert!(opened
        .iter()
        .all(|(_, version, integrity)| { *version == 3 && integrity == "ok" }));
    assert!(opened
        .iter()
        .all(|(opened_id, _, _)| opened_id == &store_id));

    let migrated = Store::open(&migration_path).unwrap();
    assert_eq!(
        migrated
            .conn
            .query_row("SELECT payload FROM runs WHERE id=?", [&run.id], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        run_payload
    );
    let migrated_instances = run
        .instances
        .iter()
        .map(|instance| {
            migrated
                .conn
                .query_row(
                    "SELECT payload FROM instances WHERE id=?",
                    [&instance.id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    let migrated_attachments = run
        .attachments
        .iter()
        .map(|attachment| {
            migrated
                .conn
                .query_row(
                    "SELECT data FROM attachments WHERE id=?",
                    [attachment],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(migrated_instances, instance_payloads);
    assert_eq!(migrated_attachments, attachment_payloads);
}

#[test]
fn only_sqlite_busy_is_classified_as_retryable_contention() {
    assert!(is_busy(&error(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
        None
    ))));
    assert!(!is_busy(&error(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
        None
    ))));
    assert!(!is_busy("Run store: database is locked"));
}
