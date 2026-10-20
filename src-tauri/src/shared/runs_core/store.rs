use super::{id, now, types::*};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(crate) type Result<T> = std::result::Result<T, String>;
const BUSY_PREFIX: &str = "Run store busy: ";
const OPEN_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);
const OPEN_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);
pub(crate) fn error(e: impl std::error::Error + 'static) -> String {
    if (&e as &dyn std::error::Error)
        .downcast_ref::<rusqlite::Error>()
        .and_then(rusqlite::Error::sqlite_error_code)
        == Some(rusqlite::ErrorCode::DatabaseBusy)
    {
        format!("{BUSY_PREFIX}{e}")
    } else {
        format!("Run store: {e}")
    }
}
pub(crate) fn is_busy(error: &str) -> bool {
    error.starts_with(BUSY_PREFIX)
}
fn encode(v: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(v).map_err(error)
}
fn decode<T: serde::de::DeserializeOwned>(v: String) -> Result<T> {
    serde_json::from_str(&v).map_err(error)
}

pub(crate) struct Store {
    pub(crate) conn: Connection,
    pub store_id: String,
}
impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        // Multiple desktop/daemon workers can be the first process to touch a
        // fresh store at once. SQLite cannot always wait through the
        // journal_mode=WAL transition, so retry only SQLITE_BUSY from the
        // open/bootstrap path. Normal schema-3 reads still open without taking
        // a writer lock, and all non-contention failures remain immediate.
        let deadline = std::time::Instant::now() + OPEN_RETRY_WINDOW;
        loop {
            match Self::open_once(path) {
                Err(err) if is_busy(&err) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(OPEN_RETRY_DELAY);
                }
                result => return result,
            }
        }
    }

    fn open_once(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(error)?;
        }
        let mut conn = Connection::open(path).map_err(error)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(error)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")
            .map_err(error)?;
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 3 {
            return Err(format!("Unsupported run schema {version}"));
        }
        // WAL lets the desktop retain a read snapshot while the daemon commits.
        // Keep FULL durability; switching to NORMAL would weaken acknowledged writes.
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(error)?;
        if mode != "wal" {
            let mode: String = conn
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                .map_err(error)?;
            if mode != "wal" {
                return Err(format!(
                    "Run store requires WAL on a local filesystem; got {mode}"
                ));
            }
        }
        // Reads of an existing schema must not reserve SQLite's single writer.
        if version == 3 {
            let store_id = conn
                .query_row("SELECT id FROM metadata", [], |r| r.get(0))
                .map_err(error)?;
            return Ok(Self { conn, store_id });
        }
        // VACUUM INTO includes committed WAL pages and never overwrites an old backup.
        if version == 1 || version == 2 {
            let backup = path.with_file_name(format!("runs.schema{version}-backup-{}.sqlite3", id()));
            conn.execute("VACUUM INTO ?", [backup.to_string_lossy().as_ref()]).map_err(error)?;
        }
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        // Another process may have initialized the database while we waited.
        let version: u32 = tx
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 3 {
            return Err(format!("Unsupported run schema {version}"));
        }
        if version == 0 {
            tx.execute_batch("CREATE TABLE metadata(id TEXT NOT NULL);
                CREATE TABLE runs(id TEXT PRIMARY KEY, request_id TEXT UNIQUE NOT NULL, workspace_id TEXT NOT NULL, created_at INTEGER NOT NULL, payload TEXT NOT NULL);
                CREATE INDEX runs_workspace ON runs(workspace_id,created_at,id);
                CREATE TABLE instances(id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), ordinal INTEGER NOT NULL, payload TEXT NOT NULL,
                    thread_id TEXT GENERATED ALWAYS AS (json_extract(payload,'$.threadId')) VIRTUAL,
                    initial_turn_id TEXT GENERATED ALWAYS AS (json_extract(payload,'$.initialTurnId')) VIRTUAL,
                    submission_outcome TEXT GENERATED ALWAYS AS (json_extract(payload,'$.submissionOutcome')) VIRTUAL,
                    execution_outcome TEXT GENERATED ALWAYS AS (json_extract(payload,'$.executionOutcome')) VIRTUAL);
                CREATE INDEX instances_status ON instances(execution_outcome,submission_outcome);
                CREATE TABLE operations(id TEXT PRIMARY KEY, instance_id TEXT NOT NULL REFERENCES instances(id), phase TEXT NOT NULL, input TEXT NOT NULL, receipt TEXT, UNIQUE(instance_id,phase));
                CREATE TABLE captures(id TEXT PRIMARY KEY, instance_id TEXT NOT NULL REFERENCES instances(id), request_id TEXT UNIQUE NOT NULL, payload TEXT NOT NULL);
                CREATE TABLE attachments(id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), data BLOB NOT NULL);
                CREATE TABLE comparisons(id TEXT PRIMARY KEY, left_id TEXT NOT NULL REFERENCES captures(id), right_id TEXT NOT NULL REFERENCES captures(id), revision INTEGER NOT NULL, payload TEXT NOT NULL, UNIQUE(left_id,right_id));
                CREATE TABLE evidence(instance_id TEXT NOT NULL REFERENCES instances(id), turn_id TEXT NOT NULL, event_key TEXT NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(instance_id,turn_id,event_key));
                PRAGMA user_version=1;").map_err(error)?;
            tx.execute("INSERT INTO metadata VALUES(?)", [id()])
                .map_err(error)?;
        }
        if version < 2 {
            tx.execute_batch(crate::shared::agent_runtime::store::SCHEMA).map_err(error)?;
        }
        if version < 3 {
            tx.execute_batch(crate::shared::agent_runtime::conversations::SCHEMA).map_err(error)?;
            crate::shared::agent_runtime::conversations::backfill(&tx)?;
        }
        let store_id = tx
            .query_row("SELECT id FROM metadata", [], |r| r.get(0))
            .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(Self { conn, store_id })
    }
    pub fn get_run(&self, run_id: &str) -> Result<RunRecord> {
        let mut record: RunRecord = decode(
            self.conn
                .query_row("SELECT payload FROM runs WHERE id=?", [run_id], |r| {
                    r.get(0)
                })
                .map_err(error)?,
        )?;
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM instances WHERE run_id=? ORDER BY ordinal")
            .map_err(error)?;
        let rows = stmt
            .query_map([run_id], |r| r.get::<_, String>(0))
            .map_err(error)?;
        record.instances = rows
            .map(|r| decode(r.map_err(error)?))
            .collect::<Result<_>>()?;
        Ok(record)
    }
    pub fn recoverable_instances(&self) -> Result<Vec<RunInstance>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM instances")
            .map_err(error)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?;
        let mut result = Vec::new();
        for row in rows {
            let instance: RunInstance = decode(row.map_err(error)?)?;
            if instance.thread_id.is_some()
                && (instance.captures.is_empty()
                    || matches!(
                        instance.execution_outcome,
                        ExecutionOutcome::Running | ExecutionOutcome::Unknown
                    ))
            {
                result.push(instance);
            }
        }
        Ok(result)
    }
    pub fn find_request(&self, request_id: &str) -> Result<Option<RunRecord>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM runs WHERE request_id=?",
                [request_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        id.map(|id| self.get_run(&id)).transpose()
    }
    pub fn create_run(
        &mut self,
        input: CreateRun,
        name: String,
        path: String,
        base_oid: Option<String>,
        baseline: Value,
        attachments: Vec<Vec<u8>>,
    ) -> Result<RunRecord> {
        if input.instances.is_empty() || input.instances.len() > 100 {
            return Err("A run requires 1 to 100 instances".into());
        }
        if !matches!(input.mode.as_str(), "local" | "worktree") {
            return Err("Invalid run mode".into());
        }
        uuid::Uuid::parse_str(&input.request_id).map_err(error)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM runs WHERE request_id=?",
                [&input.request_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if let Some(existing) = existing {
            tx.commit().map_err(error)?;
            return self.get_run(&existing);
        }
        let run_id = id();
        let record = RunRecord {
            schema_version: 1,
            store_id: self.store_id.clone(),
            id: run_id.clone(),
            request_id: input.request_id,
            workspace_id: input.workspace_id.clone(),
            workspace_name: name,
            workspace_path: path.clone(),
            title: input.title,
            prompt: input.prompt,
            mode: input.mode.clone(),
            created_at: now(),
            revision: 0,
            base_oid,
            baseline,
            attachments: attachments.iter().map(|_| id()).collect(),
            instances: vec![],
        };
        tx.execute(
            "INSERT INTO runs VALUES(?,?,?,?,?)",
            params![
                record.id,
                record.request_id,
                record.workspace_id,
                record.created_at,
                encode(&record)?
            ],
        )
        .map_err(error)?;
        for (ordinal, requested) in input.instances.into_iter().enumerate() {
            let instance = RunInstance {
                id: id(),
                run_id: run_id.clone(),
                ordinal,
                requested,
                preparation: "planned".into(),
                workspace_id: (input.mode == "local").then(|| input.workspace_id.clone()),
                workspace_path: (input.mode == "local").then(|| path.clone()),
                thread_id: None,
                initial_turn_id: None,
                runtime: None, session_id: None, initial_execution_id: None,
                submission_outcome: SubmissionOutcome::NotSubmitted,
                execution_outcome: ExecutionOutcome::NotStarted,
                submission: None,
                baseline: Value::Null,
                setup: json!({"status":"not_configured"}),
                error: None,
                captures: vec![],
            };
            tx.execute(
                "INSERT INTO instances VALUES(?,?,?,?)",
                params![instance.id, run_id, ordinal as i64, encode(&instance)?],
            )
            .map_err(error)?;
        }
        for (attachment_id, bytes) in record.attachments.iter().zip(attachments) {
            tx.execute(
                "INSERT INTO attachments VALUES(?,?,?)",
                params![attachment_id, run_id, bytes],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        self.get_run(&run_id)
    }
    pub fn list_runs(
        &self,
        workspace: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> Result<Value> {
        // UUID tie-breaker plus timestamp cursor keeps pagination stable when new runs arrive.
        let mut stmt = self.conn.prepare("SELECT id FROM runs WHERE (?1 IS NULL OR workspace_id=?1) AND (?2 IS NULL OR (created_at,id)<(SELECT created_at,id FROM runs WHERE id=?2)) ORDER BY created_at DESC,id DESC LIMIT ?3").map_err(error)?;
        let ids = stmt
            .query_map(params![workspace, cursor, limit.clamp(1, 100) + 1], |r| {
                r.get::<_, String>(0)
            })
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        let next = (ids.len() > limit.clamp(1, 100) as usize)
            .then(|| ids[limit.clamp(1, 100) as usize - 1].clone());
        let runs = ids
            .iter()
            .take(limit.clamp(1, 100) as usize)
            .map(|id| self.get_run(id))
            .collect::<Result<Vec<_>>>()?;
        Ok(json!({"storeId":self.store_id,"runs":runs,"nextCursor":next}))
    }
    pub fn instance(&self, instance_id: &str) -> Result<RunInstance> {
        decode(
            self.conn
                .query_row(
                    "SELECT payload FROM instances WHERE id=?",
                    [instance_id],
                    |r| r.get(0),
                )
                .map_err(error)?,
        )
    }
    pub fn validate_context(&self, context: &RunContext) -> Result<RunInstance> {
        let instance = self.instance(&context.instance_id)?;
        if instance.run_id != context.run_id {
            return Err("Run context does not match instance".into());
        }
        Ok(instance)
    }
    pub fn update_instance(
        &mut self,
        instance_id: &str,
        update: impl FnOnce(&mut RunInstance) -> Result<()>,
    ) -> Result<RunInstance> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let mut instance: RunInstance = decode(
            tx.query_row(
                "SELECT payload FROM instances WHERE id=?",
                [instance_id],
                |r| r.get(0),
            )
            .map_err(error)?,
        )?;
        update(&mut instance)?;
        tx.execute(
            "UPDATE instances SET payload=? WHERE id=?",
            params![encode(&instance)?, instance_id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(instance)
    }
    pub fn begin_operation(
        &mut self,
        context: &RunContext,
        phase: &str,
        input: Value,
    ) -> Result<OperationDisposition> {
        self.validate_context(context)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT id,input,receipt FROM operations WHERE instance_id=? AND phase=?",
                params![context.instance_id, phase],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some((id, previous, receipt)) = existing {
            if decode::<Value>(previous)? != input {
                return Err("Tracked operation input changed; create a new run".into());
            }
            return Ok(match receipt {
                Some(receipt) => {
                    let receipt: Value = decode(receipt)?;
                    let identity = match phase {
                        "turn/start" => "/response/result/turn/id",
                        "thread/start" => "/response/result/thread/id",
                        _ => "",
                    };
                    if !identity.is_empty()
                        && receipt
                            .pointer(identity)
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .is_none()
                        && receipt.pointer("/response/error").is_none()
                    {
                        OperationDisposition::Unresolved { id }
                    } else {
                        OperationDisposition::Completed { receipt }
                    }
                }
                None => OperationDisposition::Unresolved { id },
            });
        }
        let id = id();
        tx.execute(
            "INSERT INTO operations VALUES(?,?,?,?,NULL)",
            params![id, context.instance_id, phase, encode(&input)?],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(OperationDisposition::Execute { id })
    }
    pub fn finish_operation(
        &mut self,
        operation_id: &str,
        mut receipt: Value,
        update: impl FnOnce(&mut RunInstance) -> Result<()>,
    ) -> Result<RunInstance> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let (instance_id, phase, existing): (String, String, Option<String>) = tx
            .query_row(
                "SELECT instance_id,phase,receipt FROM operations WHERE id=?",
                [operation_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(error)?;
        let mut instance: RunInstance = decode(
            tx.query_row(
                "SELECT payload FROM instances WHERE id=?",
                [&instance_id],
                |r| r.get(0),
            )
            .map_err(error)?,
        )?;
        if existing.is_none() {
            update(&mut instance)?;
            if phase == "turn/start" {
                receipt["response"]["runReceipt"] = json!({"runId":instance.run_id,"instanceId":instance.id,"outcome":instance.submission_outcome,"threadId":instance.thread_id,"turnId":instance.initial_turn_id,"message":instance.error});
            }
            tx.execute(
                "UPDATE operations SET receipt=? WHERE id=?",
                params![encode(&receipt)?, operation_id],
            )
            .map_err(error)?;
            tx.execute(
                "UPDATE instances SET payload=? WHERE id=?",
                params![encode(&instance)?, instance_id],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(instance)
    }
    pub fn append_capture(&mut self, capture: RunCapture) -> Result<RunCapture> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT payload FROM captures WHERE request_id=?",
                [&capture.request_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if let Some(existing) = existing {
            return decode(existing);
        }
        let mut instance: RunInstance = decode(
            tx.query_row(
                "SELECT payload FROM instances WHERE id=?",
                [&capture.instance_id],
                |r| r.get(0),
            )
            .map_err(error)?,
        )?;
        instance.captures.push(CaptureSummary {
            id: capture.id.clone(),
            kind: capture.kind.clone(),
            turn_id: capture.turn_id.clone(),
            execution_id: capture.execution_id.clone(),
            label: capture.label.clone(),
            created_at: capture.created_at,
            complete: capture.complete,
        });
        tx.execute(
            "INSERT INTO captures VALUES(?,?,?,?)",
            params![
                capture.id,
                capture.instance_id,
                capture.request_id,
                encode(&capture)?
            ],
        )
        .map_err(error)?;
        tx.execute(
            "UPDATE instances SET payload=? WHERE id=?",
            params![encode(&instance)?, instance.id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(capture)
    }
    pub fn capture_by_request(&self, request_id: &str) -> Result<Option<RunCapture>> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT payload FROM captures WHERE request_id=?",
                [request_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        value.map(decode).transpose()
    }
    pub fn capture(&self, capture_id: &str) -> Result<RunCapture> {
        decode(
            self.conn
                .query_row(
                    "SELECT payload FROM captures WHERE id=?",
                    [capture_id],
                    |r| r.get(0),
                )
                .map_err(error)?,
        )
    }
    pub fn update_title(
        &mut self,
        run_id: &str,
        revision: u32,
        title: String,
    ) -> Result<RunRecord> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let mut run: RunRecord = decode(
            tx.query_row("SELECT payload FROM runs WHERE id=?", [run_id], |r| {
                r.get(0)
            })
            .map_err(error)?,
        )?;
        if run.revision != revision {
            return Err("conflict: run metadata changed".into());
        }
        run.title = title;
        run.revision += 1;
        tx.execute(
            "UPDATE runs SET payload=? WHERE id=?",
            params![encode(&run)?, run_id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        self.get_run(run_id)
    }
    pub fn save_comparison(
        &mut self,
        left: String,
        right: String,
        choice: String,
        notes: String,
        revision: u32,
    ) -> Result<RunComparison> {
        if !["left", "right", "tie", "undecided"].contains(&choice.as_str()) || left == right {
            return Err("Invalid comparison".into());
        }
        let left_run = self.get_run(&self.instance(&self.capture(&left)?.instance_id)?.run_id)?;
        let right_run = self.get_run(&self.instance(&self.capture(&right)?.instance_id)?.run_id)?;
        if left_run.workspace_id != right_run.workspace_id {
            return Err("Compare captures from the same workspace".into());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT payload FROM comparisons WHERE left_id=? AND right_id=?",
                params![left, right],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        let mut record = match existing {
            Some(v) => decode(v)?,
            None => RunComparison {
                id: id(),
                left_capture_id: left,
                right_capture_id: right,
                choice: "undecided".into(),
                notes: String::new(),
                revision: 0,
                updated_at: now(),
            },
        };
        if record.revision != revision {
            return Err("conflict: comparison notes changed; reload before saving".into());
        }
        // Keep acknowledged comparison revisions recoverable alongside the current row.
        if record.revision > 0 {
            tx.execute("INSERT OR IGNORE INTO comparison_revisions VALUES(?,?,?)", params![record.id,record.revision,encode(&record)?]).map_err(error)?;
        }
        record.choice = choice;
        record.notes = notes;
        record.revision += 1;
        record.updated_at = now();
        tx.execute("INSERT INTO comparisons VALUES(?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload", params![record.id,record.left_capture_id,record.right_capture_id,record.revision,encode(&record)?]).map_err(error)?;
        tx.execute("INSERT INTO comparison_revisions VALUES(?,?,?)", params![record.id,record.revision,encode(&record)?]).map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(record)
    }
    pub fn comparisons(
        &self,
        run_id: Option<&str>,
        capture_id: Option<&str>,
    ) -> Result<Vec<RunComparison>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT c.payload FROM comparisons c JOIN captures a ON a.id=c.left_id JOIN captures b ON b.id=c.right_id JOIN instances ia ON ia.id=a.instance_id JOIN instances ib ON ib.id=b.instance_id WHERE (?1 IS NULL OR ia.run_id=?1 OR ib.run_id=?1) AND (?2 IS NULL OR c.left_id=?2 OR c.right_id=?2)").map_err(error)?;
        let result = stmt
            .query_map(params![run_id, capture_id], |r| r.get::<_, String>(0))
            .map_err(error)?
            .map(|r| decode(r.map_err(error)?))
            .collect();
        result
    }
    #[cfg(test)]
    pub fn evidence(&mut self, instance: &str, turn: &str, key: &str, value: &Value) -> Result<()> {
        self.conn.execute("INSERT INTO evidence VALUES(?,?,?,?) ON CONFLICT(instance_id,turn_id,event_key) DO UPDATE SET payload=excluded.payload", params![instance,turn,key,encode(value)?]).map_err(error)?;
        Ok(())
    }
    pub fn read_evidence(&self, instance: &str, turn: &str) -> Result<Vec<Value>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT payload FROM evidence WHERE instance_id=? AND turn_id=? ORDER BY rowid",
            )
            .map_err(error)?;
        let result = stmt
            .query_map(params![instance, turn], |r| r.get::<_, String>(0))
            .map_err(error)?
            .map(|r| decode(r.map_err(error)?))
            .collect();
        result
    }
}

pub(crate) async fn with_store<T: Send + 'static>(
    path: PathBuf,
    f: impl FnOnce(&mut Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(move || f(&mut Store::open(&path)?))
        .await
        .map_err(error)?
}

mod events;

#[cfg(test)]
mod tests;
