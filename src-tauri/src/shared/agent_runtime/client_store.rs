//! Durable evidence for ACP client callbacks and command terminals.
//!
//! Authentication terminal input/output is intentionally not represented by
//! these records. Only ACP command-terminal snapshots may be stored here.

use crate::shared::runs_core::store::{decode, encode, error, Result, Store};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const MAX_TERMINAL_OUTPUT_BYTES: usize = 1024 * 1024;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS acp_callback_operations(
    id TEXT PRIMARY KEY,
    connection_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation >= 0),
    request_id_type TEXT NOT NULL,
    request_id_json TEXT NOT NULL,
    session_id TEXT,
    method TEXT NOT NULL,
    params_hash TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending','succeeded','failed','uncertain')),
    intent_at INTEGER NOT NULL,
    completed_at INTEGER,
    result_json TEXT,
    error_text TEXT,
    UNIQUE(connection_id,generation,request_id_type,request_id_json)
);
CREATE INDEX IF NOT EXISTS acp_callback_operations_state
    ON acp_callback_operations(state,connection_id,generation);
CREATE TABLE IF NOT EXISTS acp_terminal_snapshots(
    session_id TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation >= 0),
    terminal_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(session_id,generation,terminal_id)
);
PRAGMA user_version=4;";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpCallbackOperation {
    pub id: String,
    pub connection_id: String,
    pub generation: u64,
    /// Stored as a JSON value so JSON-RPC numeric and string IDs remain distinct.
    pub request_id: Value,
    pub session_id: Option<String>,
    pub method: String,
    pub params_hash: String,
    pub state: AcpCallbackState,
    pub intent_at: i64,
    pub completed_at: Option<i64>,
    pub result: Option<Value>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AcpCallbackState {
    Pending,
    Succeeded,
    Failed,
    Uncertain,
}

impl AcpCallbackState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpTerminalSnapshot {
    pub session_id: String,
    pub generation: u64,
    pub terminal_id: String,
    /// Bounded terminal output and lifecycle metadata (including `released`).
    pub payload: Value,
}

impl Store {
    /// Record an operation intent before dispatching its external side effect.
    /// A duplicate wire identity returns the existing record only when its
    /// method, params hash, and session match; callers must never rerun it.
    pub(crate) fn begin_acp_callback(
        &mut self,
        connection_id: &str,
        generation: u64,
        request_id: &Value,
        session_id: Option<&str>,
        method: &str,
        params_hash: &str,
    ) -> Result<(AcpCallbackOperation, bool)> {
        if connection_id.is_empty() || method.is_empty() || params_hash.is_empty() {
            return Err("invalid_input: ACP callback identity is incomplete".into());
        }
        let generation_sql = i64::try_from(generation)
            .map_err(|_| "invalid_input: ACP connection generation overflow")?;
        let request_id_type = json_type(request_id);
        let request_id_json = encode(request_id)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM acp_callback_operations
                 WHERE connection_id=? AND generation=? AND request_id_type=? AND request_id_json=?",
                params![connection_id,generation_sql,request_id_type,request_id_json],
                |row| row.get(0),
            )
            .optional()
            .map_err(error)?;
        if let Some(id) = existing {
            let operation = read_callback(&tx, &id)?;
            if operation.method != method
                || operation.params_hash != params_hash
                || operation.session_id.as_deref() != session_id
            {
                return Err("invalid_input: ACP callback request identity conflicts".into());
            }
            tx.commit().map_err(error)?;
            return Ok((operation, false));
        }
        let operation = AcpCallbackOperation {
            id: crate::shared::runs_core::id(),
            connection_id: connection_id.into(),
            generation,
            request_id: request_id.clone(),
            session_id: session_id.map(str::to_owned),
            method: method.into(),
            params_hash: params_hash.into(),
            state: AcpCallbackState::Pending,
            intent_at: crate::shared::runs_core::now(),
            completed_at: None,
            result: None,
            error: None,
        };
        tx.execute(
            "INSERT INTO acp_callback_operations
             (id,connection_id,generation,request_id_type,request_id_json,session_id,method,params_hash,state,intent_at)
             VALUES(?,?,?,?,?,?,?,?,?,?)",
            params![
                operation.id,
                operation.connection_id,
                generation_sql,
                request_id_type,
                request_id_json,
                operation.session_id,
                operation.method,
                operation.params_hash,
                operation.state.as_str(),
                operation.intent_at
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok((operation, true))
    }

    /// Complete a previously recorded callback. Repeated completion with the
    /// same outcome is idempotent; conflicting completion is surfaced.
    pub(crate) fn finish_acp_callback(
        &mut self,
        operation_id: &str,
        result: Option<&Value>,
        error_text: Option<&str>,
    ) -> Result<AcpCallbackOperation> {
        if result.is_some() == error_text.is_some() {
            return Err(
                "invalid_input: callback completion requires exactly one result or error".into(),
            );
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let current = read_callback(&tx, operation_id)?;
        let target_state = if error_text.is_some() {
            AcpCallbackState::Failed
        } else {
            AcpCallbackState::Succeeded
        };
        if current.state != AcpCallbackState::Pending {
            if current.state == target_state
                && current.result.as_ref() == result
                && current.error.as_deref() == error_text
            {
                tx.commit().map_err(error)?;
                return Ok(current);
            }
            return Err("conflict: ACP callback already reached a different terminal state".into());
        }
        let result_json = result.map(encode).transpose()?;
        tx.execute(
            "UPDATE acp_callback_operations SET state=?,completed_at=?,result_json=?,error_text=? WHERE id=? AND state='pending'",
            params![target_state.as_str(), crate::shared::runs_core::now(), result_json, error_text, operation_id],
        ).map_err(error)?;
        let completed = read_callback(&tx, operation_id)?;
        tx.commit().map_err(error)?;
        Ok(completed)
    }

    /// Mark unfinished work uncertain when its owning connection disappears.
    /// It is an evidence transition only: no callback is retried here.
    pub(crate) fn mark_acp_callbacks_uncertain(
        &mut self,
        connection_id: &str,
        generation: u64,
        reason: &str,
    ) -> Result<u64> {
        let generation_sql = i64::try_from(generation)
            .map_err(|_| "invalid_input: ACP connection generation overflow")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let changed = tx
            .execute(
                "UPDATE acp_callback_operations SET state='uncertain',completed_at=?,error_text=?
             WHERE connection_id=? AND generation=? AND state='pending'",
                params![
                    crate::shared::runs_core::now(),
                    reason,
                    connection_id,
                    generation_sql
                ],
            )
            .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(changed as u64)
    }

    pub(crate) fn acp_callback_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<AcpCallbackOperation>> {
        let json = self
            .conn
            .query_row(
                "SELECT id FROM acp_callback_operations WHERE id=?",
                [operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?;
        json.map(|id| read_callback(&self.conn, &id)).transpose()
    }

    pub(crate) fn save_acp_terminal_snapshot(
        &mut self,
        snapshot: &AcpTerminalSnapshot,
    ) -> Result<()> {
        if snapshot.session_id.is_empty() || snapshot.terminal_id.is_empty() {
            return Err("invalid_input: terminal snapshot identity is incomplete".into());
        }
        let generation = i64::try_from(snapshot.generation)
            .map_err(|_| "invalid_input: terminal generation overflow")?;
        if let Some(output) = snapshot.payload.get("output") {
            let output = output
                .as_str()
                .ok_or_else(|| "invalid_input: terminal output must be UTF-8 text".to_owned())?;
            if output.len() > MAX_TERMINAL_OUTPUT_BYTES {
                return Err(format!(
                    "invalid_input: terminal output exceeds {} bytes",
                    MAX_TERMINAL_OUTPUT_BYTES
                ));
            }
        }
        let payload = encode(&snapshot.payload)?;
        self.conn.execute(
            "INSERT INTO acp_terminal_snapshots(session_id,generation,terminal_id,payload,updated_at)
             VALUES(?,?,?,?,?) ON CONFLICT(session_id,generation,terminal_id)
             DO UPDATE SET payload=excluded.payload,updated_at=excluded.updated_at",
            params![snapshot.session_id,generation,snapshot.terminal_id,payload,crate::shared::runs_core::now()],
        ).map_err(error)?;
        Ok(())
    }

    pub(crate) fn read_acp_terminal_snapshot(
        &self,
        session_id: &str,
        generation: u64,
        terminal_id: &str,
    ) -> Result<Option<AcpTerminalSnapshot>> {
        let generation =
            i64::try_from(generation).map_err(|_| "invalid_input: terminal generation overflow")?;
        let payload: Option<String> = self.conn.query_row(
            "SELECT payload FROM acp_terminal_snapshots WHERE session_id=? AND generation=? AND terminal_id=?",
            params![session_id,generation,terminal_id],
            |row| row.get(0),
        ).optional().map_err(error)?;
        payload
            .map(|payload| {
                Ok(AcpTerminalSnapshot {
                    session_id: session_id.into(),
                    generation: u64::try_from(generation).map_err(error)?,
                    terminal_id: terminal_id.into(),
                    payload: decode(payload)?,
                })
            })
            .transpose()
    }

    /// Read the newest generation for a terminal identifier when the caller
    /// only has the stable session and terminal handles available.
    pub(crate) fn read_latest_acp_terminal_snapshot(
        &self,
        session_id: &str,
        terminal_id: &str,
    ) -> Result<Option<AcpTerminalSnapshot>> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT generation,payload FROM acp_terminal_snapshots
                 WHERE session_id=? AND terminal_id=?
                 ORDER BY generation DESC,updated_at DESC LIMIT 1",
                params![session_id, terminal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        row.map(|(generation, payload)| {
            Ok(AcpTerminalSnapshot {
                session_id: session_id.into(),
                generation: u64::try_from(generation).map_err(error)?,
                terminal_id: terminal_id.into(),
                payload: decode(payload)?,
            })
        })
        .transpose()
    }
}

fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn read_callback(
    connection: &rusqlite::Connection,
    operation_id: &str,
) -> Result<AcpCallbackOperation> {
    let (id, connection_id, generation, request_id_json, session_id, method, params_hash, state,
        intent_at, completed_at, result_json, error_text):
        (String,String,i64,String,Option<String>,String,String,String,i64,Option<i64>,Option<String>,Option<String>) = connection.query_row(
        "SELECT id,connection_id,generation,request_id_json,session_id,method,params_hash,state,intent_at,completed_at,result_json,error_text
         FROM acp_callback_operations WHERE id=?",
        [operation_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?)),
    ).map_err(error)?;
    let request_id: Value = decode(request_id_json)?;
    let generation = u64::try_from(generation).map_err(error)?;
    let state = match state.as_str() {
        "pending" => AcpCallbackState::Pending,
        "succeeded" => AcpCallbackState::Succeeded,
        "failed" => AcpCallbackState::Failed,
        "uncertain" => AcpCallbackState::Uncertain,
        _ => return Err("Run store: invalid ACP callback state".into()),
    };
    Ok(AcpCallbackOperation {
        id,
        connection_id,
        generation,
        request_id,
        session_id,
        method,
        params_hash,
        state,
        intent_at,
        completed_at,
        result: result_json.map(decode).transpose()?,
        error: error_text,
    })
}

#[cfg(test)]
mod tests;
