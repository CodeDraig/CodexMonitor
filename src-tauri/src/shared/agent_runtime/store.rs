use super::types::*;
use crate::shared::runs_core::{
    id,
    store::{Result, Store},
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS runtime_profiles(id TEXT PRIMARY KEY, revision INTEGER NOT NULL, payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runtime_sessions(id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, profile_id TEXT NOT NULL, native_id TEXT NOT NULL, owner TEXT, active_execution TEXT, payload TEXT NOT NULL, UNIQUE(profile_id,native_id));
CREATE TABLE IF NOT EXISTS runtime_executions(id TEXT PRIMARY KEY, request_id TEXT UNIQUE NOT NULL, session_id TEXT NOT NULL REFERENCES runtime_sessions(id), payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runtime_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL REFERENCES runtime_sessions(id), execution_id TEXT REFERENCES runtime_executions(id), payload TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS comparison_revisions(comparison_id TEXT NOT NULL REFERENCES comparisons(id), revision INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(comparison_id,revision));
CREATE INDEX IF NOT EXISTS runtime_events_session ON runtime_events(session_id,sequence);
PRAGMA user_version=2;";
fn error(e: rusqlite::Error) -> String {
    crate::shared::runs_core::store::error(e)
}
fn decode<T: serde::de::DeserializeOwned>(s: String) -> Result<T> {
    serde_json::from_str(&s).map_err(|e| e.to_string())
}
fn encode(v: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}
impl Store {
    pub fn runtime_profiles(&self) -> Result<Vec<RuntimeProfile>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM runtime_profiles ORDER BY id")
            .map_err(error)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?)).collect()
    }
    pub fn save_runtime_profile(
        &mut self,
        mut p: RuntimeProfile,
        expected: u64,
    ) -> Result<RuntimeProfile> {
        if p.id == "codex"
            || p.id.trim().is_empty()
            || p.executable.trim().is_empty()
            || p.name.trim().is_empty()
        {
            return Err(
                "invalid_input: profile needs a name, executable and non-reserved ID".into(),
            );
        }
        if p.working_directory.as_deref().is_some_and(|d|!std::path::Path::new(d).is_absolute()) {return Err("invalid_input: launch directory must be absolute".into());}
        if p.kind == RuntimeKind::Codex {
            return Err(
                "unsupported: configure the default Codex profile in Codex settings".into(),
            );
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT revision FROM runtime_profiles WHERE id=?",
                [&p.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if current.unwrap_or(0)
            != i64::try_from(expected).map_err(|_| "invalid_input: revision overflow")?
        {
            return Err("revision_conflict: runtime profile changed".into());
        }
        p.revision = expected + 1;
        tx.execute("INSERT INTO runtime_profiles VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,payload=excluded.payload", params![p.id,i64::try_from(p.revision).map_err(|_|"invalid_input: revision overflow")?,encode(&p)?]).map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(p)
    }
    pub fn runtime_session(&self, id: &str) -> Result<RuntimeSession> {
        decode(
            self.conn
                .query_row(
                    "SELECT payload FROM runtime_sessions WHERE id=?",
                    [id],
                    |r| r.get(0),
                )
                .map_err(error)?,
        )
    }
    pub fn runtime_sessions(&self, workspace: &str) -> Result<Vec<RuntimeSession>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT payload FROM runtime_sessions WHERE workspace_id=? ORDER BY rowid DESC",
            )
            .map_err(error)?;
        let rows = stmt
            .query_map([workspace], |r| r.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?)).collect()
    }
    pub fn insert_runtime_session(&mut self, s: &RuntimeSession, owner: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO runtime_sessions VALUES(?,?,?,?,?,NULL,?)",
                params![
                    s.session_id,
                    s.workspace_id,
                    s.profile.connection_scope(),
                    s.native_session_id,
                    owner,
                    encode(s)?
                ],
            )
            .map_err(error)?;
        self.catalog_binding(s)?;
        Ok(())
    }
    pub fn claim_runtime_session(&mut self, session: &str, owner: &str) -> Result<()> {
        // Reclaim completed sessions only after proving the old backend process is gone.
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT owner FROM runtime_sessions WHERE id=?",
                [session],
                |row| row.get(0),
            )
            .map_err(error)?;
        if let Some(previous) =
            previous.filter(|previous| previous != owner && owner_is_dead(previous))
        {
            self.conn.execute("UPDATE runtime_sessions SET owner=NULL WHERE id=? AND owner=? AND active_execution IS NULL",params![session,previous]).map_err(error)?;
        }
        let n = self.conn.execute("UPDATE runtime_sessions SET owner=?2 WHERE id=?1 AND (owner IS NULL OR owner=?2) AND active_execution IS NULL", params![session,owner]).map_err(error)?;
        if n != 1 {
            return Err("runtime_busy: session is active or owned by another backend; reconnect to its owner".into());
        }
        Ok(())
    }
    pub fn save_runtime_session(&mut self, s: &RuntimeSession) -> Result<()> {
        self.conn
            .execute(
                "UPDATE runtime_sessions SET payload=? WHERE id=?",
                params![encode(s)?, s.session_id],
            )
            .map_err(error)?;
        Ok(())
    }
    pub fn runtime_execution(&self, execution: &str) -> Result<ExecutionTicket> {
        decode(
            self.conn
                .query_row(
                    "SELECT payload FROM runtime_executions WHERE id=?",
                    [execution],
                    |r| r.get(0),
                )
                .map_err(error)?,
        )
    }
    pub fn find_runtime_request(&self, request: &str) -> Result<Option<ExecutionTicket>> {
        self.conn
            .query_row(
                "SELECT payload FROM runtime_executions WHERE request_id=?",
                [request],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
            .map(decode)
            .transpose()
    }
    pub fn begin_runtime_execution(
        &mut self,
        input: SubmitExecution,
        owner: &str,
    ) -> Result<(ExecutionTicket, bool)> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        if let Some(v) = tx
            .query_row(
                "SELECT payload FROM runtime_executions WHERE request_id=?",
                [&input.request_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
        {
            let ticket: ExecutionTicket = decode(v)?;
            if serde_json::to_value(&ticket.input).unwrap() != serde_json::to_value(&input).unwrap()
            {
                return Err("invalid_input: submission request identity conflicts".into());
            }
            return Ok((ticket, false));
        }
        let session: RuntimeSession = decode(
            tx.query_row(
                "SELECT payload FROM runtime_sessions WHERE id=?",
                [&input.session_id],
                |r| r.get(0),
            )
            .map_err(error)?,
        )?;
        let configuration = session.configuration;
        let ticket = ExecutionTicket {
            execution_id: id(),
            request_id: input.request_id.clone(),
            session_id: input.session_id.clone(),
            submission: crate::shared::runs_core::types::SubmissionOutcome::Submitting,
            execution: crate::shared::runs_core::types::ExecutionOutcome::NotStarted,
            native_turn_id: None,
            input,
            configuration,
            runtime: Some(session.runtime),
            agent_info: session.agent_info,
            error: None,
        };
        if let Some(context) = &ticket.input.run_context {
            let mut instance: crate::shared::runs_core::types::RunInstance = decode(
                tx.query_row(
                    "SELECT payload FROM instances WHERE id=?",
                    [&context.instance_id],
                    |r| r.get(0),
                )
                .map_err(error)?,
            )?;
            if instance.run_id != context.run_id
                || instance.initial_execution_id.is_some()
                || instance.initial_turn_id.is_some()
            {
                return Err("invalid_input: run context already bound or mismatched".into());
            }
            let session: RuntimeSession = decode(
                tx.query_row(
                    "SELECT payload FROM runtime_sessions WHERE id=?",
                    [&ticket.session_id],
                    |r| r.get(0),
                )
                .map_err(error)?,
            )?;
            if instance
                .workspace_id
                .as_deref()
                .is_some_and(|w| w != session.workspace_id)
            {
                return Err("invalid_input: run workspace mismatch".into());
            }
            instance.runtime = Some(session.runtime);
            instance.session_id = Some(session.session_id);
            instance.initial_execution_id = Some(ticket.execution_id.clone());
            instance.workspace_id = Some(session.workspace_id);
            instance.workspace_path = Some(session.cwd);
            instance.preparation = "ready".into();
            instance.submission_outcome =
                crate::shared::runs_core::types::SubmissionOutcome::Submitting;
            tx.execute(
                "UPDATE instances SET payload=? WHERE id=?",
                params![encode(&instance)?, instance.id],
            )
            .map_err(error)?;
        }
        let replaying: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM history_loads WHERE session_id=? AND state='loading')",[&ticket.session_id],|r|r.get(0)).map_err(error)?;
        if replaying { return Err("runtime_busy: conversation history is loading".into()); }
        let n = tx.execute("UPDATE runtime_sessions SET active_execution=? WHERE id=? AND owner=? AND active_execution IS NULL", params![ticket.execution_id,ticket.session_id,owner]).map_err(error)?;
        if n != 1 {
            return Err(
                "runtime_busy: session already has an execution or belongs to another backend"
                    .into(),
            );
        }
        tx.execute(
            "INSERT INTO runtime_executions VALUES(?,?,?,?)",
            params![
                ticket.execution_id,
                ticket.request_id,
                ticket.session_id,
                encode(&ticket)?
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok((ticket, true))
    }
    pub fn update_runtime_execution(
        &mut self,
        ticket: &ExecutionTicket,
        terminal: bool,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: ExecutionTicket = decode(
            tx.query_row(
                "SELECT payload FROM runtime_executions WHERE id=?",
                [&ticket.execution_id],
                |row| row.get(0),
            )
            .map_err(error)?,
        )?;
        use crate::shared::runs_core::types::ExecutionOutcome;
        let known_terminal = matches!(
            existing.execution,
            ExecutionOutcome::Completed | ExecutionOutcome::Failed | ExecutionOutcome::Interrupted
        );
        if known_terminal
            && matches!(
                ticket.execution,
                ExecutionOutcome::Unknown
                    | ExecutionOutcome::Running
                    | ExecutionOutcome::NotStarted
            )
        {
            return Ok(());
        }
        if let Some(context) = &ticket.input.run_context {
            let mut instance: crate::shared::runs_core::types::RunInstance = decode(
                tx.query_row(
                    "SELECT payload FROM instances WHERE id=?",
                    [&context.instance_id],
                    |row| row.get(0),
                )
                .map_err(error)?,
            )?;
            if instance.initial_execution_id.as_deref() == Some(&ticket.execution_id) {
                instance.submission_outcome = ticket.submission.clone();
                instance.execution_outcome = ticket.execution.clone();
                instance.initial_turn_id = ticket.native_turn_id.clone();
                instance.error = ticket.error.clone();
                tx.execute(
                    "UPDATE instances SET payload=? WHERE id=?",
                    params![encode(&instance)?, instance.id],
                )
                .map_err(error)?;
            }
        }
        tx.execute(
            "UPDATE runtime_executions SET payload=? WHERE id=?",
            params![encode(ticket)?, ticket.execution_id],
        )
        .map_err(error)?;
        if terminal {
            tx.execute("UPDATE runtime_sessions SET active_execution=NULL WHERE id=? AND active_execution=?", params![ticket.session_id,ticket.execution_id]).map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(())
    }
    pub fn append_runtime_event(&mut self, mut event: RuntimeEvent) -> Result<RuntimeEvent> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        tx.execute(
            "INSERT INTO runtime_events(session_id,execution_id,payload) VALUES(?,?,?)",
            params![event.session_id, event.execution_id, encode(&event)?],
        )
        .map_err(error)?;
        event.sequence = tx.last_insert_rowid() as u64;
        tx.execute(
            "UPDATE runtime_events SET payload=? WHERE sequence=?",
            params![encode(&event)?, event.sequence as i64],
        )
        .map_err(error)?;
        super::conversations::materialize(&tx, &event)?;
        tx.commit().map_err(error)?;
        Ok(event)
    }
    pub fn runtime_events(&self, session: &str, after: u64) -> Result<Vec<RuntimeEvent>> {
        let mut stmt = self.conn.prepare("SELECT payload FROM runtime_events WHERE session_id=? AND sequence>? ORDER BY sequence LIMIT 1000").map_err(error)?;
        let rows = stmt
            .query_map(
                params![
                    session,
                    i64::try_from(after).map_err(|_| "invalid_input: event cursor overflow")?
                ],
                |r| r.get::<_, String>(0),
            )
            .map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?)).collect()
    }
    pub fn runtime_history(&self, session: &str) -> Result<Vec<ExecutionTicket>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM runtime_executions WHERE session_id=? ORDER BY rowid")
            .map_err(error)?;
        let rows = stmt
            .query_map([session], |r| r.get::<_, String>(0))
            .map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?)).collect()
    }
    /// An owner releases only its own bindings after its transport has stopped.
    pub fn release_runtime_owner(&mut self, session: &str, owner: &str) -> Result<()> {
        self.conn.execute("UPDATE runtime_sessions SET owner=NULL WHERE id=? AND owner=? AND active_execution IS NULL",params![session,owner]).map_err(error)?;
        Ok(())
    }
    pub fn runtime_store_info(&self) -> Value {
        json!({"storeId":self.store_id,"schemaVersion":3})
    }
}

impl Store {
    /// Recover a committed primary capture, otherwise retain uncertainty after owner death.
    pub fn reconcile_runtime_owner(&mut self, session_id: &str) -> Result<()> {
        let (owner, active): (Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT owner,active_execution FROM runtime_sessions WHERE id=?",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(error)?;
        if let (Some(owner), Some(active)) = (owner, active) {
            if owner_is_dead(&owner) {
                let mut ticket = self.runtime_execution(&active)?;
                if ticket.execution == crate::shared::runs_core::types::ExecutionOutcome::Unknown {
                    return Ok(());
                }
                let capture = if let Some(context) = &ticket.input.run_context {
                    self.capture_by_request(&format!("initial:{}", context.instance_id))?
                } else {
                    None
                };
                let recovered =
                    capture.filter(|capture| capture.execution_id.as_deref() == Some(&active));
                let terminal = recovered.is_some();
                if let Some(capture) = recovered {
                    ticket.execution = capture.status;
                    ticket.submission =
                        crate::shared::runs_core::types::SubmissionOutcome::Accepted;
                } else {
                    ticket.execution = crate::shared::runs_core::types::ExecutionOutcome::Unknown;
                    if !matches!(
                        ticket.submission,
                        crate::shared::runs_core::types::SubmissionOutcome::Accepted
                    ) {
                        ticket.submission =
                            crate::shared::runs_core::types::SubmissionOutcome::Unknown;
                    }
                    ticket.error=Some("Owning backend exited during execution; result is uncertain and will not be resubmitted".into());
                }
                self.update_runtime_execution(&ticket, terminal)?;
            }
        }
        Ok(())
    }
}
pub(crate) fn owner_is_dead(owner: &str) -> bool {
    let Some(pid) = owner
        .split(':')
        .next()
        .and_then(|pid| pid.parse::<u32>().ok())
        .filter(|pid| *pid > 0)
    else {
        return false;
    };
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        (unsafe { libc::kill(pid, 0) }) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn WaitForSingleObject(handle: *mut std::ffi::c_void, millis: u32) -> u32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            fn GetLastError() -> u32;
        }
        let handle = OpenProcess(0x00101000, 0, pid);
        if handle.is_null() {
            GetLastError() == 87
        } else {
            let dead = WaitForSingleObject(handle, 0) == 0;
            CloseHandle(handle);
            dead
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}
