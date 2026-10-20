use super::*;
use serde_json::json;
use std::path::PathBuf;

fn path() -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "codex-acp-client-store-{}",
            crate::shared::runs_core::id()
        ))
        .join("runs.sqlite3")
}

#[test]
fn callback_identity_preserves_json_id_type_and_never_restarts_uncertain_work() {
    let mut store = Store::open(&path()).unwrap();
    let (numeric, created) = store
        .begin_acp_callback(
            "connection-a",
            7,
            &json!(1),
            Some("monitor-session"),
            "fs/write_text_file",
            "sha256-a",
        )
        .unwrap();
    assert!(created);
    assert_eq!(numeric.request_id, json!(1));
    assert_eq!(numeric.state, AcpCallbackState::Pending);

    let (string, created) = store
        .begin_acp_callback(
            "connection-a",
            7,
            &json!("1"),
            Some("monitor-session"),
            "fs/write_text_file",
            "sha256-a",
        )
        .unwrap();
    assert!(created);
    assert_ne!(numeric.id, string.id);
    assert_eq!(string.request_id, json!("1"));

    let (duplicate, created) = store
        .begin_acp_callback(
            "connection-a",
            7,
            &json!(1),
            Some("monitor-session"),
            "fs/write_text_file",
            "sha256-a",
        )
        .unwrap();
    assert!(!created);
    assert_eq!(duplicate.id, numeric.id);
    assert_eq!(
        store.begin_acp_callback(
            "connection-a",
            7,
            &json!(1),
            Some("monitor-session"),
            "fs/write_text_file",
            "changed-hash"
        ),
        Err("invalid_input: ACP callback request identity conflicts".into())
    );

    assert_eq!(
        store
            .mark_acp_callbacks_uncertain("connection-a", 7, "connection lost")
            .unwrap(),
        2
    );
    let (after_disconnect, created) = store
        .begin_acp_callback(
            "connection-a",
            7,
            &json!(1),
            Some("monitor-session"),
            "fs/write_text_file",
            "sha256-a",
        )
        .unwrap();
    assert!(!created);
    assert_eq!(after_disconnect.state, AcpCallbackState::Uncertain);
    assert_eq!(after_disconnect.error.as_deref(), Some("connection lost"));
}

#[test]
fn callback_finish_is_idempotent_and_rejects_conflicting_result() {
    let mut store = Store::open(&path()).unwrap();
    let (operation, created) = store
        .begin_acp_callback(
            "connection-b",
            1,
            &json!("request"),
            None,
            "terminal/kill",
            "hash",
        )
        .unwrap();
    assert!(created);

    let result = json!({"acknowledged": true});
    let finished = store
        .finish_acp_callback(&operation.id, Some(&result), None)
        .unwrap();
    assert_eq!(finished.state, AcpCallbackState::Succeeded);
    assert_eq!(finished.result.as_ref(), Some(&result));
    assert_eq!(
        store
            .finish_acp_callback(&operation.id, Some(&result), None)
            .unwrap(),
        finished
    );
    assert!(store
        .finish_acp_callback(&operation.id, Some(&json!({"acknowledged": false})), None)
        .unwrap_err()
        .starts_with("conflict:"));

    let failed = store
        .begin_acp_callback(
            "connection-b",
            1,
            &json!("error"),
            None,
            "fs/read_text_file",
            "hash2",
        )
        .unwrap()
        .0;
    let failed = store
        .finish_acp_callback(&failed.id, None, Some("denied"))
        .unwrap();
    assert_eq!(failed.state, AcpCallbackState::Failed);
    assert_eq!(failed.error.as_deref(), Some("denied"));
}

#[test]
fn command_terminal_snapshot_is_bounded_and_survives_reopen() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let snapshot = AcpTerminalSnapshot {
        session_id: "session".into(),
        generation: 4,
        terminal_id: "terminal".into(),
        payload: json!({
            "output": "last visible output\n",
            "released": true,
            "exitCode": 0,
            "truncated": false
        }),
    };
    store.save_acp_terminal_snapshot(&snapshot).unwrap();
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .read_acp_terminal_snapshot("session", 4, "terminal")
            .unwrap(),
        Some(snapshot.clone())
    );
    assert_eq!(
        store
            .read_latest_acp_terminal_snapshot("session", "terminal")
            .unwrap(),
        Some(snapshot)
    );

    let too_large = AcpTerminalSnapshot {
        session_id: "session".into(),
        generation: 4,
        terminal_id: "large".into(),
        payload: json!({"output": "x".repeat(MAX_TERMINAL_OUTPUT_BYTES + 1)}),
    };
    let mut store = store;
    assert!(store
        .save_acp_terminal_snapshot(&too_large)
        .unwrap_err()
        .contains("exceeds"));
    assert_eq!(
        store
            .read_acp_terminal_snapshot("session", 4, "large")
            .unwrap(),
        None
    );
}

#[test]
fn schema3_upgrade_backs_up_consistent_store_and_preserves_existing_payload_bytes() {
    let path = path();
    let mut store = Store::open(&path).unwrap();
    let payload = "{ \"legacy\" : \"byte-for-byte\", \"number\": 1.00 }";
    store
        .conn
        .execute(
            "INSERT INTO runs(id,request_id,workspace_id,created_at,payload) VALUES('legacy-run','legacy-request','legacy-workspace',1,?)",
            [payload],
        )
        .unwrap();
    let store_id = store.store_id.clone();
    store.conn.execute_batch("PRAGMA user_version=3").unwrap();
    drop(store);

    let migrated = Store::open(&path).unwrap();
    assert_eq!(migrated.store_id, store_id);
    assert_eq!(
        migrated
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        4
    );
    let after: String = migrated
        .conn
        .query_row(
            "SELECT payload FROM runs WHERE id='legacy-run'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(after, payload);
    assert_eq!(
        migrated
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );

    let backup_name = path.file_name().unwrap().to_string_lossy();
    let prefix = backup_name.replace("runs.sqlite3", "runs.schema3-backup-");
    let backup = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .expect("schema3 SQLite backup must exist");
    let backup = rusqlite::Connection::open(backup).unwrap();
    assert_eq!(
        backup
            .query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        backup
            .query_row(
                "SELECT payload FROM runs WHERE id='legacy-run'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        payload
    );
}
