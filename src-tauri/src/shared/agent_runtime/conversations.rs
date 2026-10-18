//! Durable catalog and replay snapshots. Reads never acquire the SQLite writer.
use super::types::*;
use crate::shared::runs_core::{
    id, now,
    store::{error, Result, Store},
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};

pub(crate) const SCHEMA: &str = "
CREATE TABLE session_catalog(id TEXT PRIMARY KEY, scope TEXT NOT NULL, native_id TEXT NOT NULL,
 session_id TEXT REFERENCES runtime_sessions(id), profile TEXT NOT NULL, cwd TEXT NOT NULL,
 title TEXT, updated_at TEXT, metadata TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0,
 presentation TEXT NOT NULL DEFAULT '{}', UNIQUE(scope,native_id));
CREATE TABLE conversation_aliases(session_id TEXT PRIMARY KEY REFERENCES runtime_sessions(id), catalog_id TEXT NOT NULL REFERENCES session_catalog(id));
CREATE TABLE history_loads(id TEXT PRIMARY KEY, request_id TEXT UNIQUE NOT NULL, session_id TEXT NOT NULL REFERENCES runtime_sessions(id),
 state TEXT NOT NULL, started_at INTEGER NOT NULL, completed_at INTEGER, error TEXT);
CREATE INDEX history_loads_session ON history_loads(session_id,started_at);
CREATE UNIQUE INDEX history_loads_active ON history_loads(session_id) WHERE state='loading';
CREATE TABLE conversation_snapshots(session_id TEXT PRIMARY KEY REFERENCES runtime_sessions(id), generation TEXT NOT NULL, through_sequence INTEGER NOT NULL, loaded_at INTEGER NOT NULL);
CREATE TABLE conversation_items(session_id TEXT NOT NULL REFERENCES runtime_sessions(id), generation TEXT NOT NULL, ordinal INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(session_id,generation,ordinal));
CREATE INDEX conversation_items_identity ON conversation_items(session_id,generation,json_extract(payload,'$.kind'),json_extract(payload,'$.id'));
CREATE TABLE conversation_attachments(id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES runtime_sessions(id), mime_type TEXT NOT NULL, data TEXT NOT NULL);
PRAGMA user_version=3;";
fn decode<T: serde::de::DeserializeOwned>(s: String) -> Result<T> {
    serde_json::from_str(&s).map_err(|e| e.to_string())
}
fn encode(v: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(v).map_err(|e| e.to_string())
}
impl Store {
    pub fn backfill_catalog(&mut self) -> Result<()> {
        let sessions: Vec<RuntimeSession> = {
            let mut q = self
                .conn
                .prepare("SELECT payload FROM runtime_sessions ORDER BY rowid")
                .map_err(error)?;
            let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(error)?;
            rows.map(|r| decode(r.map_err(error)?))
                .collect::<Result<_>>()?
        };
        for session in sessions {
            self.catalog_binding(&session)?;
        }
        Ok(())
    }
    pub fn catalog_binding(&self, session: &RuntimeSession) -> Result<()> {
        self.conn.execute("INSERT INTO session_catalog(id,scope,native_id,session_id,profile,cwd,metadata) VALUES(?,?,?,?,?,?,'{}') ON CONFLICT(scope,native_id) DO UPDATE SET session_id=COALESCE(session_catalog.session_id,excluded.session_id)",
            params![id(),session.profile.connection_scope(),session.native_session_id,session.session_id,encode(&session.profile)?,session.cwd]).map_err(error)?;
        self.conn.execute("INSERT OR IGNORE INTO conversation_aliases SELECT ?1,id FROM session_catalog WHERE scope=?2 AND native_id=?3",params![session.session_id,session.profile.connection_scope(),session.native_session_id]).map_err(error)?;
        Ok(())
    }
    pub fn catalog_upsert(
        &mut self,
        profile: &RuntimeProfile,
        info: &Value,
    ) -> Result<CatalogSession> {
        let native = info["sessionId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("invalid_input: session/list missing sessionId")?;
        let cwd = info["cwd"]
            .as_str()
            .ok_or("invalid_input: session/list missing cwd")?;
        if !std::path::Path::new(cwd).is_absolute() {
            return Err("invalid_input: agent session directory must be absolute".into());
        }
        // A partial scan never removes rows. Monitor presentation metadata is separate.
        self.conn.execute("INSERT INTO session_catalog(id,scope,native_id,profile,cwd,title,updated_at,metadata) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(scope,native_id) DO UPDATE SET profile=excluded.profile,cwd=excluded.cwd,title=CASE WHEN json_type(excluded.metadata,'$.title') IS NULL THEN session_catalog.title ELSE excluded.title END,updated_at=CASE WHEN json_type(excluded.metadata,'$.updatedAt') IS NULL THEN session_catalog.updated_at ELSE excluded.updated_at END,metadata=json_patch(session_catalog.metadata,excluded.metadata)",
            params![id(),profile.connection_scope(),native,encode(profile)?,cwd,info["title"].as_str(),info["updatedAt"].as_str(),encode(info)?]).map_err(error)?;
        let cid: String = self
            .conn
            .query_row(
                "SELECT id FROM session_catalog WHERE scope=? AND native_id=?",
                params![profile.connection_scope(), native],
                |r| r.get(0),
            )
            .map_err(error)?;
        self.catalog_session(&cid)
    }
    pub fn catalog_session(&self, cid: &str) -> Result<CatalogSession> {
        self.conn.query_row("SELECT id,scope,native_id,session_id,profile,cwd,title,updated_at,metadata,revision,presentation,(SELECT workspace_id FROM runtime_sessions WHERE id=session_catalog.session_id) FROM session_catalog WHERE id=?",[cid],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,String>(8)?,r.get::<_,i64>(9)? as u64,r.get::<_,String>(10)?,r.get::<_,Option<String>>(11)?))).map_err(error).and_then(|(catalog_id,connection_scope,native_session_id,session_id,p,cwd,title,updated_at,m,revision,v,workspace_id)|Ok(CatalogSession{workspace_id,catalog_id,store_id:self.store_id.clone(),connection_scope,native_session_id,session_id,profile:decode(p)?,cwd,title,updated_at,metadata:decode(m)?,revision,presentation:decode(v)?}))
    }
    pub fn catalog(&self) -> Result<Vec<CatalogSession>> {
        let mut q = self
            .conn
            .prepare(
                "SELECT id FROM session_catalog ORDER BY COALESCE(updated_at,'') DESC,rowid DESC",
            )
            .map_err(error)?;
        let ids = q.query_map([], |r| r.get::<_, String>(0)).map_err(error)?;
        ids.map(|r| self.catalog_session(&r.map_err(error)?))
            .collect()
    }
    pub fn conversation_metadata(
        &mut self,
        sid: &str,
        expected: u64,
        patch: &Value,
    ) -> Result<CatalogSession> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let (cid,revision,p):(String,i64,String)=tx.query_row("SELECT c.id,c.revision,c.presentation FROM session_catalog c JOIN conversation_aliases a ON a.catalog_id=c.id WHERE a.session_id=?",[sid],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(error)?;
        if expected != revision as u64 {
            return Err("revision_conflict: conversation metadata changed".into());
        }
        let mut v: Value = decode(p)?;
        for key in ["titleOverride", "pinned", "hidden"] {
            if let Some(value) = patch.get(key) {
                if key == "titleOverride" && !(value.is_null() || value.is_string())
                    || key != "titleOverride" && !value.is_boolean()
                {
                    return Err("invalid_input: conversation metadata type".into());
                }
                v[key] = value.clone();
            }
        }
        tx.execute(
            "UPDATE session_catalog SET revision=revision+1,presentation=? WHERE id=?",
            params![encode(&v)?, cid],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        self.catalog_session(&cid)
    }
    pub fn history_load(&self, request: &str) -> Result<Option<HistoryLoad>> {
        self.conn.query_row("SELECT id,request_id,session_id,state,started_at,completed_at,error FROM history_loads WHERE request_id=?",[request],|r|Ok(HistoryLoad{id:r.get(0)?,request_id:r.get(1)?,session_id:r.get(2)?,state:r.get(3)?,started_at:r.get(4)?,completed_at:r.get(5)?,error:r.get(6)?})).optional().map_err(error)
    }
    pub fn begin_history_load(
        &mut self,
        sid: &str,
        request: &str,
        owner: &str,
    ) -> Result<(HistoryLoad, bool)> {
        if let Some(ticket) = self.history_load(request)? {
            if ticket.session_id != sid {
                return Err("invalid_input: history request identity conflicts".into());
            }
            return Ok((ticket, false));
        }
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT owner FROM runtime_sessions WHERE id=?",
                [sid],
                |r| r.get(0),
            )
            .map_err(error)?;
        if previous.as_deref().is_some_and(super::store::owner_is_dead) {
            self.conn.execute("UPDATE history_loads SET state='failed',completed_at=?,error='Owning backend stopped before replay committed' WHERE session_id=? AND state='loading'",params![now(),sid]).map_err(error)?;
        }
        self.claim_runtime_session(sid, owner)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        if let Some(ticket)=tx.query_row("SELECT id,request_id,session_id,state,started_at,completed_at,error FROM history_loads WHERE request_id=?",[request],|r|Ok(HistoryLoad{id:r.get(0)?,request_id:r.get(1)?,session_id:r.get(2)?,state:r.get(3)?,started_at:r.get(4)?,completed_at:r.get(5)?,error:r.get(6)?})).optional().map_err(error)? {if ticket.session_id!=sid{return Err("invalid_input: history request identity conflicts".into());}return Ok((ticket,false));}
        let busy:bool=tx.query_row("SELECT active_execution IS NOT NULL OR EXISTS(SELECT 1 FROM history_loads WHERE session_id=?1 AND state='loading') FROM runtime_sessions WHERE id=?1",[sid],|r|r.get(0)).map_err(error)?;
        if busy {
            return Err("runtime_busy: conversation is executing or replaying".into());
        }
        let ticket = HistoryLoad {
            id: id(),
            request_id: request.into(),
            session_id: sid.into(),
            state: "loading".into(),
            started_at: now(),
            completed_at: None,
            error: None,
        };
        tx.execute(
            "INSERT INTO history_loads VALUES(?,?,?,'loading',?,NULL,NULL)",
            params![ticket.id, request, sid, ticket.started_at],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok((ticket, true))
    }
    pub fn finish_history_load(
        &mut self,
        load: &str,
        failure: Option<&str>,
        sequence: u64,
    ) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let sid: String = tx
            .query_row(
                "SELECT session_id FROM history_loads WHERE id=? AND state='loading'",
                [load],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?
            .ok_or("invalid_input: history load is not active")?;
        if failure.is_none() {
            tx.execute("INSERT INTO conversation_snapshots VALUES(?,?,?,?) ON CONFLICT(session_id) DO UPDATE SET generation=excluded.generation,through_sequence=excluded.through_sequence,loaded_at=excluded.loaded_at",params![sid,load,sequence as i64,now()]).map_err(error)?;
        }
        tx.execute(
            "UPDATE history_loads SET state=?,completed_at=?,error=? WHERE id=?",
            params![
                if failure.is_some() {
                    "failed"
                } else {
                    "complete"
                },
                now(),
                failure,
                load
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(())
    }
    pub fn conversation_read(
        &self,
        sid: &str,
        generation: Option<&str>,
        cursor: u64,
        limit: u64,
        owner: &str,
    ) -> Result<Value> {
        // A deferred WAL read transaction keeps the published generation, its
        // items and load status coherent without reserving the writer lock.
        let read = self.conn.unchecked_transaction().map_err(error)?;
        self.runtime_session(sid)?;
        let snapshot: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT generation,loaded_at FROM conversation_snapshots WHERE session_id=?",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        let current = snapshot.as_ref().map(|s| s.0.as_str()).unwrap_or("live");
        let selected = generation.unwrap_or(current);
        if generation.is_some() && selected != current {
            return Err(
                "revision_conflict: conversation generation changed; restart pagination".into(),
            );
        }
        let limit = limit.clamp(1, 500);
        let mut q=self.conn.prepare("SELECT ordinal,payload FROM conversation_items WHERE session_id=? AND generation=? AND ordinal>? ORDER BY ordinal LIMIT ?").map_err(error)?;
        let rows = q
            .query_map(
                params![
                    sid,
                    selected,
                    i64::try_from(cursor).map_err(|_| "invalid_input: cursor overflow")?,
                    (limit + 1) as i64
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(error)?;
        let mut items = rows
            .map(|r| {
                let (n, p) = r.map_err(error)?;
                Ok((n, decode::<Value>(p)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let more = items.len() > limit as usize;
        if more {
            items.pop();
        }
        let next = if more {
            items.last().map(|v| v.0)
        } else {
            None
        };
        let mut state:Option<Value>=self.conn.query_row("SELECT json_object('id',id,'state',state,'error',error) FROM history_loads WHERE session_id=? ORDER BY started_at DESC,rowid DESC LIMIT 1",[sid],|r|r.get::<_,String>(0)).optional().map_err(error)?.map(decode).transpose()?;
        let (stored_owner, active): (Option<String>, Option<String>) = self
            .conn
            .query_row(
                "SELECT owner,active_execution FROM runtime_sessions WHERE id=?",
                [sid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(error)?;
        if stored_owner
            .as_deref()
            .is_some_and(super::store::owner_is_dead)
        {
            if let Some(load) = state.as_mut().filter(|s| s["state"] == "loading") {
                load["state"] = json!("failed");
                load["error"] = json!("Owning backend stopped before replay committed");
            }
        }
        let read_only = stored_owner
            .as_ref()
            .is_some_and(|o| o != owner && !super::store::owner_is_dead(o));
        let result = json!({"storeId":self.store_id,"sessionId":sid,"generation":current,"items":items.into_iter().map(|v|v.1).collect::<Vec<_>>(),"nextCursor":next,"load":state,"coverage":if snapshot.is_some(){"agent_replay"}else{"monitor_observed_only"},"freshness":{"loadedAt":snapshot.as_ref().map(|v|v.1),"stale":state.as_ref().is_some_and(|v|v["state"]=="failed")||snapshot.as_ref().is_some_and(|v|now()-v.1>30000)},"readOnly":read_only,"activeExecutionId":active});
        drop(q);
        read.commit().map_err(error)?;
        Ok(result)
    }
}

/// The same reducer processes replay and live content. No text-based deduplication.
pub(crate) fn materialize(tx: &rusqlite::Transaction<'_>, event: &RuntimeEvent) -> Result<()> {
    if event.kind == "unattributed_session_update" {
        return Ok(());
    }
    if event.kind == "session_info_update" {
        tx.execute("UPDATE session_catalog SET title=CASE WHEN json_type(?1,'$.title') IS NULL THEN title ELSE json_extract(?1,'$.title') END,updated_at=CASE WHEN json_type(?1,'$.updatedAt') IS NULL THEN updated_at ELSE json_extract(?1,'$.updatedAt') END,metadata=json_patch(metadata,?1) WHERE id=(SELECT catalog_id FROM conversation_aliases WHERE session_id=?2)",params![encode(&event.payload)?,event.session_id]).map_err(error)?;
    }
    let generation = if let Some(load) = &event.history_load_id {
        load.clone()
    } else {
        tx.query_row(
            "SELECT generation FROM conversation_snapshots WHERE session_id=?",
            [&event.session_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(error)?
        .unwrap_or_else(|| "live".into())
    };
    let p = &event.payload;
    let mut item = match event.kind.as_str() {
        "message_delta" => {
            json!({"id":p.get("itemId").filter(|v|v.is_string()).cloned().unwrap_or_else(||json!(format!("{generation}:{}",event.sequence))),"kind":if p["role"]=="reasoning"{"reasoning"}else{"message"},"role":p["role"],"text":p["text"].as_str().unwrap_or(""),"content":p.get("content").map(|c|vec![c.clone()]).unwrap_or_default(),"annotations":p["annotations"],"native":p["native"]})
        }
        "tool_patch" => {
            let mut v = p.clone();
            v["id"] = p["toolCallId"].clone();
            if let Some(kind) = p.get("kind") {
                v["nativeKind"] = kind.clone();
            }
            v["kind"] = json!("tool");
            v
        }
        "execution_submitted" => {
            json!({"id":format!("input:{}",event.execution_id.as_deref().unwrap_or("")),"kind":"message","role":"user","text":p.pointer("/input/prompt").and_then(Value::as_array).map(|a|a.iter().filter_map(|c|c["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default(),"content":p.pointer("/input/prompt").cloned().unwrap_or(json!([]))})
        }
        "permission" if event.source == "history" => {
            json!({"id":format!("history-permission:{}",event.sequence),"kind":"protocol","type":"Historical permission","data":p})
        }
        "compaction_update" | "compaction_summary_chunk" => {
            json!({"id":format!("compaction:{}",p["compactionId"].as_str().unwrap_or("unknown")),"kind":"protocol","type":"compaction","data":p})
        }
        "plan" | "configuration" | "compaction" | "available_commands_update" => {
            json!({"id":format!("{generation}:{}",event.sequence),"kind":"protocol","type":event.kind,"data":p})
        }
        _ if event.raw["method"] == "session/update" => {
            json!({"id":format!("{generation}:{}",event.sequence),"kind":"protocol","type":event.kind,"data":p})
        }
        _ => return Ok(()),
    };
    // Extract binary blocks from materialized content; the journal retains original protocol evidence.
    fn extract(tx: &rusqlite::Transaction<'_>, sid: &str, v: &mut Value) -> Result<()> {
        if let Some(o) = v.as_object_mut() {
            let binary = o.get("type").is_some_and(|t| t == "image" || t == "audio")
                && o.get("data").is_some_and(Value::is_string);
            let blob = o.get("blob").is_some_and(Value::is_string);
            if binary || blob {
                let key = if blob { "blob" } else { "data" };
                let data = o.remove(key).unwrap();
                use sha2::{Digest, Sha256};
                let aid = format!(
                    "{sid}:{:x}",
                    Sha256::digest(data.as_str().unwrap().as_bytes())
                );
                tx.execute(
                    "INSERT OR IGNORE INTO conversation_attachments VALUES(?,?,?,?)",
                    params![
                        aid,
                        sid,
                        o.get("mimeType")
                            .and_then(Value::as_str)
                            .unwrap_or("application/octet-stream"),
                        data.as_str().unwrap()
                    ],
                )
                .map_err(error)?;
                o.insert("attachmentId".into(), json!(aid));
            }
            for child in o.values_mut() {
                extract(tx, sid, child)?;
            }
        } else if let Some(a) = v.as_array_mut() {
            for c in a {
                extract(tx, sid, c)?;
            }
        }
        Ok(())
    }
    if event.kind == "compaction_summary_chunk" {
        item["data"] = json!({"compactionId":p["compactionId"],"status":"in_progress","summary":[p["content"]]});
    }
    extract(tx, &event.session_id, &mut item)?;
    let user_echo = event.kind == "message_delta"
        && p["role"] == "user"
        && event.execution_id.is_some()
        && event.history_load_id.is_none();
    if user_echo {
        let submitted = format!("input:{}", event.execution_id.as_deref().unwrap());
        let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM conversation_items WHERE session_id=? AND generation=? AND json_extract(payload,'$.id')=?)",params![event.session_id,generation,submitted],|r|r.get(0)).map_err(error)?;
        if existing {
            item["id"] = json!(submitted);
        }
    }
    if event.kind == "message_delta" && !p.get("itemId").is_some_and(Value::is_string) && !user_echo
    {
        let last:Option<String>=tx.query_row("SELECT payload FROM conversation_items WHERE session_id=? AND generation=? ORDER BY ordinal DESC LIMIT 1",params![event.session_id,generation],|r|r.get(0)).optional().map_err(error)?;
        if let Some(last) = last {
            let last: Value = decode(last)?;
            if last["kind"] == item["kind"]
                && last["role"] == item["role"]
                && last["chunkGroup"] == true
            {
                item["id"] = last["id"].clone();
            }
        }
        item["chunkGroup"] = json!(true);
    }
    let previous:Option<(i64,String)>=tx.query_row("SELECT ordinal,payload FROM conversation_items WHERE session_id=? AND generation=? AND json_extract(payload,'$.kind')=? AND json_extract(payload,'$.id')=? ORDER BY ordinal LIMIT 1",params![event.session_id,generation,item["kind"].as_str(),item["id"].as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(error)?;
    if let Some((ordinal, old)) = previous {
        let mut old: Value = decode(old)?;
        if event.kind == "message_delta" {
            if user_echo && old["echoStarted"] != true {
                old["text"] = json!("");
                old["content"] = json!([]);
                old["echoStarted"] = json!(true);
            }
            old["text"] = json!(format!(
                "{}{}",
                old["text"].as_str().unwrap_or(""),
                item["text"].as_str().unwrap_or("")
            ));
            if let (Some(a), Some(b)) = (old["content"].as_array_mut(), item["content"].as_array())
            {
                a.extend(b.iter().cloned());
            }
        } else if event.kind == "compaction_summary_chunk" {
            let summary = old["data"]
                .as_object_mut()
                .ok_or("invalid compaction data")?
                .entry("summary")
                .or_insert(json!([]));
            if let Some(chunks) = summary.as_array_mut() {
                chunks.extend(
                    item["data"]["summary"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .cloned(),
                );
            }
        } else if event.kind == "compaction_update" {
            super::projection::patch(&mut old["data"], &item["data"]);
        } else {
            super::projection::patch(&mut old, &item);
        }
        tx.execute("UPDATE conversation_items SET payload=? WHERE session_id=? AND generation=? AND ordinal=?",params![encode(&old)?,event.session_id,generation,ordinal]).map_err(error)?;
    } else {
        tx.execute(
            "INSERT INTO conversation_items VALUES(?,?,?,?)",
            params![
                event.session_id,
                generation,
                event.sequence as i64,
                encode(&item)?
            ],
        )
        .map_err(error)?;
    }
    Ok(())
}

pub(crate) fn backfill(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let sessions: Vec<RuntimeSession> = {
        let mut q = tx
            .prepare("SELECT payload FROM runtime_sessions ORDER BY rowid")
            .map_err(error)?;
        let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?))
            .collect::<Result<_>>()?
    };
    for s in sessions {
        tx.execute("INSERT INTO session_catalog(id,scope,native_id,session_id,profile,cwd,metadata) VALUES(?,?,?,?,?,?,'{}') ON CONFLICT(scope,native_id) DO NOTHING",params![id(),s.profile.connection_scope(),s.native_session_id,s.session_id,encode(&s.profile)?,s.cwd]).map_err(error)?;
        tx.execute("INSERT INTO conversation_aliases SELECT ?1,id FROM session_catalog WHERE scope=?2 AND native_id=?3",params![s.session_id,s.profile.connection_scope(),s.native_session_id]).map_err(error)?;
    }
    let events: Vec<RuntimeEvent> = {
        let mut q = tx
            .prepare("SELECT payload FROM runtime_events ORDER BY sequence")
            .map_err(error)?;
        let rows = q.query_map([], |r| r.get::<_, String>(0)).map_err(error)?;
        rows.map(|r| decode(r.map_err(error)?))
            .collect::<Result<_>>()?
    };
    for event in events {
        if event.source == "live" {
            materialize(tx, &event)?;
        }
    }
    Ok(())
}
impl Store {
    pub fn attach_catalog(
        &mut self,
        cid: &str,
        session: &RuntimeSession,
    ) -> Result<RuntimeSession> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT session_id FROM session_catalog WHERE id=?",
                [cid],
                |r| r.get(0),
            )
            .map_err(error)?;
        if let Some(sid) = existing {
            let mut saved: RuntimeSession = decode(
                tx.query_row(
                    "SELECT payload FROM runtime_sessions WHERE id=?",
                    [&sid],
                    |r| r.get(0),
                )
                .map_err(error)?,
            )?;
            // Explicit workspace association can restore a missing registration.
            // Runtime, native identity, execution references and evidence remain intact.
            if saved.workspace_id != session.workspace_id {
                saved.workspace_id = session.workspace_id.clone();
                tx.execute(
                    "UPDATE runtime_sessions SET workspace_id=?,payload=? WHERE id=?",
                    params![saved.workspace_id, encode(&saved)?, sid],
                )
                .map_err(error)?;
            }
            tx.commit().map_err(error)?;
            return Ok(saved);
        }
        tx.execute(
            "INSERT INTO runtime_sessions VALUES(?,?,?,?,NULL,NULL,?)",
            params![
                session.session_id,
                session.workspace_id,
                session.profile.connection_scope(),
                session.native_session_id,
                encode(session)?
            ],
        )
        .map_err(error)?;
        tx.execute(
            "UPDATE session_catalog SET session_id=? WHERE id=?",
            params![session.session_id, cid],
        )
        .map_err(error)?;
        tx.execute(
            "INSERT INTO conversation_aliases VALUES(?,?)",
            params![session.session_id, cid],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(session.clone())
    }
    pub fn conversation_attachment(&self, sid: &str, aid: &str) -> Result<Value> {
        self.conn
            .query_row(
                "SELECT mime_type,data FROM conversation_attachments WHERE session_id=? AND id=?",
                params![sid, aid],
                |r| Ok(json!({"mimeType":r.get::<_,String>(0)?,"data":r.get::<_,String>(1)?})),
            )
            .map_err(error)
    }
}
impl Store {
    /// Route another native session to its attached conversation when its identity is known.
    /// Unknown directories are never inferred from the parent; their raw evidence stays unattributed.
    pub fn route_foreign_conversation_event(
        &mut self,
        scope: &str,
        native: &str,
        mut event: RuntimeEvent,
    ) -> Result<Option<RuntimeEvent>> {
        let sid: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT session_id FROM session_catalog WHERE scope=? AND native_id=?",
                params![scope, native],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        let Some(sid) = sid.flatten() else {
            return Ok(None);
        };
        let session = self.runtime_session(&sid)?;
        event.session_id = sid;
        event.runtime = session.runtime;
        event.execution_id = None;
        event.history_load_id = None;
        let (kind, payload) = super::projection::normalize(&session.profile.kind, &event.raw);
        event.kind = kind;
        event.payload = payload;
        if event.kind == "permission" {
            event.kind = "unattributed_permission".into();
        }
        self.append_runtime_event(event).map(Some)
    }
}
