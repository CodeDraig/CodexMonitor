//! Backend-owned command processes for ACP `terminal/*` client requests.
//!
//! Handles are scoped to a monitor session and ACP connection generation. This
//! service deliberately starts the supplied executable directly; it never
//! inserts a shell between an agent and the requested command.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex as StdMutex},
};
use tokio::{
    io::AsyncRead,
    process::{Child, Command},
    sync::{mpsc, Notify},
};
use uuid::Uuid;

const DEFAULT_OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_OUTPUT_LIMIT: usize = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_ARGUMENTS: usize = 4096;
const MAX_ENVIRONMENT: usize = 512;
const MAX_ACTIVE_TERMINALS: usize = 128;
const MAX_RETAINED_TERMINALS: usize = 2048;

type TerminalKey = (String, u64, String);
type GenerationKey = (String, u64);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AcpTerminalSnapshot {
    pub session_id: String,
    pub generation: u64,
    pub terminal_id: String,
    pub output: String,
    pub truncated: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub released: bool,
}

#[derive(Default)]
struct OutputTail {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl OutputTail {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    fn append_valid(&mut self, bytes: &[u8]) {
        self.bytes.extend(bytes.iter().copied());
        while self.bytes.len() > self.limit {
            self.bytes.pop_front();
            self.truncated = true;
        }
        // Trimming can cut through a multi-byte codepoint. Drop its remaining
        // continuation bytes so every returned tail starts on a UTF-8 boundary.
        while self
            .bytes
            .front()
            .is_some_and(|byte| byte & 0b1100_0000 == 0b1000_0000)
        {
            self.bytes.pop_front();
            self.truncated = true;
        }
    }

    fn text(&self, requested_limit: Option<usize>) -> (String, bool) {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        let cap = requested_limit.unwrap_or(bytes.len()).min(bytes.len());
        let mut start = bytes.len().saturating_sub(cap);
        while start < bytes.len() && bytes[start] & 0b1100_0000 == 0b1000_0000 {
            start += 1;
        }
        let clipped = start > 0;
        let output = String::from_utf8(bytes[start..].to_vec())
            .expect("OutputTail stores complete UTF-8 codepoints");
        (output, self.truncated || clipped)
    }
}

#[derive(Default)]
struct TerminalStatus {
    exit_code: Option<i32>,
    signal: Option<i32>,
    finished: bool,
    process_exited: bool,
    readers_remaining: usize,
    released: bool,
    output: OutputTail,
}

struct TerminalEntry {
    session_id: String,
    generation: u64,
    terminal_id: String,
    state: StdMutex<TerminalStatus>,
    changed: Notify,
    kill: mpsc::Sender<()>,
}

impl TerminalEntry {
    fn lock(&self) -> std::sync::MutexGuard<'_, TerminalStatus> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn snapshot(&self) -> AcpTerminalSnapshot {
        let state = self.lock();
        let (output, truncated) = state.output.text(None);
        AcpTerminalSnapshot {
            session_id: self.session_id.clone(),
            generation: self.generation,
            terminal_id: self.terminal_id.clone(),
            output,
            truncated,
            exit_code: state.exit_code,
            signal: state.signal,
            released: state.released,
        }
    }

    fn signal_change(&self) {
        // Wake every waiter already registered, while also retaining one permit
        // for a waiter that raced between checking terminal state and awaiting.
        self.changed.notify_waiters();
        self.changed.notify_one();
    }
}

#[derive(Default)]
struct ServiceState {
    active: HashMap<TerminalKey, Arc<TerminalEntry>>,
    retained: HashMap<TerminalKey, AcpTerminalSnapshot>,
    retained_order: VecDeque<TerminalKey>,
    closed_generations: HashSet<GenerationKey>,
    stopping_generations: HashSet<GenerationKey>,
}

struct ServiceInner {
    state: StdMutex<ServiceState>,
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        for entry in state.active.values() {
            let _ = entry.kill.try_send(());
        }
    }
}

/// ACP terminal handles belong to the backend that created them. Sharing this
/// service between RPC adapters preserves that ownership across requests.
#[derive(Clone)]
pub(crate) struct AcpTerminalService {
    inner: Arc<ServiceInner>,
}

impl Default for AcpTerminalService {
    fn default() -> Self {
        Self {
            inner: Arc::new(ServiceInner {
                state: StdMutex::new(ServiceState::default()),
            }),
        }
    }
}

impl AcpTerminalService {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Spawn `terminal/create` using the exact command, args, env and cwd sent
    /// by the ACP client. Returns `{ "terminalId": ... }` promptly.
    pub(crate) async fn create(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Value, String> {
        if session.is_empty() {
            return Err("ACP terminal requires a session identity".into());
        }
        let command = required_string(params, "command")?;
        if command.trim().is_empty() || command.len() > MAX_COMMAND_BYTES || command.contains('\0')
        {
            return Err("ACP terminal command is empty, too long, or contains NUL".into());
        }
        let args = match params.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(args)) => args.clone(),
            Some(_) => return Err("ACP terminal args must be an array".into()),
        };
        if args.len() > MAX_ARGUMENTS {
            return Err("ACP terminal has too many arguments".into());
        }
        let args = args
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|text| !text.contains('\0'))
                    .map(str::to_owned)
                    .ok_or_else(|| "ACP terminal arguments must be NUL-free strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let env = parse_env(params)?;
        let cwd = params.get("cwd").and_then(Value::as_str);
        if params
            .get("cwd")
            .is_some_and(|value| !value.is_null() && cwd.is_none())
        {
            return Err("ACP terminal cwd must be a string".into());
        }
        let limit = params
            .get("outputByteLimit")
            .filter(|value| !value.is_null())
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    "ACP terminal outputByteLimit must be an unsigned integer".to_string()
                })
            })
            .transpose()?
            .map(|value| value.min(MAX_OUTPUT_LIMIT as u64) as usize)
            .unwrap_or(DEFAULT_OUTPUT_LIMIT)
            .max(1);

        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if state
                .closed_generations
                .contains(&(session.to_string(), generation))
                || state
                    .stopping_generations
                    .contains(&(session.to_string(), generation))
            {
                return Err("ACP terminal generation is closed".into());
            }
            if state.active.len() >= MAX_ACTIVE_TERMINALS {
                return Err("ACP terminal capacity reached".into());
            }
        }

        let mut process = crate::shared::process_core::tokio_command(command);
        process.args(args);
        process.envs(env);
        if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
            process.current_dir(cwd);
        }
        process
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.as_std_mut().process_group(0);
        }

        let mut child = process.spawn().map_err(|error| {
            format!("failed to spawn ACP terminal command {command:?}: {error}")
        })?;
        let pid = child
            .id()
            .ok_or_else(|| "ACP terminal process has no pid".to_string())?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let readers_remaining = (stdout.is_some() as usize) + (stderr.is_some() as usize);
        let terminal_id = Uuid::new_v4().to_string();
        let (kill_tx, kill_rx) = mpsc::channel(1);
        let entry = Arc::new(TerminalEntry {
            session_id: session.to_string(),
            generation,
            terminal_id: terminal_id.clone(),
            state: StdMutex::new(TerminalStatus {
                output: OutputTail::new(limit),
                readers_remaining,
                ..TerminalStatus::default()
            }),
            changed: Notify::new(),
            kill: kill_tx,
        });

        if let Some(stdout) = stdout {
            tokio::spawn(drain_output(stdout, entry.clone()));
        }
        if let Some(stderr) = stderr {
            tokio::spawn(drain_output(stderr, entry.clone()));
        }
        let supervisor_entry = entry.clone();
        tokio::spawn(async move {
            supervise(child, pid, kill_rx, supervisor_entry).await;
        });

        let key = (session.to_string(), generation, terminal_id.clone());
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Check again after spawn so concurrent creates cannot exceed the bound.
        if state
            .closed_generations
            .contains(&(session.to_string(), generation))
            || state
                .stopping_generations
                .contains(&(session.to_string(), generation))
        {
            let _ = entry.kill.try_send(());
            return Err("ACP terminal generation is closed".into());
        }
        if state.active.len() >= MAX_ACTIVE_TERMINALS {
            let _ = entry.kill.try_send(());
            return Err("ACP terminal capacity reached".into());
        }
        state.active.insert(key, entry);
        Ok(json!({ "terminalId": terminal_id }))
    }

    pub(crate) async fn output(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Value, String> {
        let entry = self.active_entry(session, generation, params)?;
        let limit = params
            .get("outputByteLimit")
            .filter(|value| !value.is_null())
            .map(|value| {
                value.as_u64().ok_or_else(|| {
                    "ACP terminal outputByteLimit must be an unsigned integer".to_string()
                })
            })
            .transpose()?
            .map(|value| value.min(MAX_OUTPUT_LIMIT as u64) as usize);
        let state = entry.lock();
        if state.released {
            return Err("ACP terminal has been released".into());
        }
        let (output, truncated) = state.output.text(limit);
        let exit_status = state.finished.then_some(json!({
            "exitCode": state.exit_code,
            "signal": state.signal.map(|signal| signal.to_string()),
        }));
        let mut result = json!({ "output": output, "truncated": truncated });
        if let Some(exit_status) = exit_status {
            result["exitStatus"] = exit_status;
        }
        Ok(result)
    }

    pub(crate) async fn wait_for_exit(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Value, String> {
        let entry = self.active_entry(session, generation, params)?;
        loop {
            let changed = entry.changed.notified();
            {
                let state = entry.lock();
                if state.released {
                    return Err("ACP terminal has been released".into());
                }
                if state.finished {
                    let mut result = json!({ "exitCode": state.exit_code });
                    if let Some(signal) = state.signal {
                        result["signal"] = json!(signal.to_string());
                    }
                    return Ok(result);
                }
            }
            changed.await;
        }
    }

    pub(crate) async fn kill(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Value, String> {
        let entry = self.active_entry(session, generation, params)?;
        if !entry.lock().finished {
            entry
                .kill
                .send(())
                .await
                .map_err(|_| "ACP terminal supervisor has stopped".to_string())?;
        }
        Ok(json!({}))
    }

    pub(crate) async fn release(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Value, String> {
        let terminal_id = required_string(params, "terminalId")?;
        let key = (session.to_string(), generation, terminal_id.to_string());
        let entry = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state
                .active
                .remove(&key)
                .ok_or_else(|| "unknown or released ACP terminal handle".to_string())?
        };
        if !entry.lock().finished {
            let _ = entry.kill.try_send(());
        }
        // Ensure the final exit status is available before the durable owner
        // takes its snapshot. Releasing a handle must not orphan its process.
        loop {
            let changed = entry.changed.notified();
            if entry.lock().finished {
                break;
            }
            changed.await;
        }
        let mut snapshot = entry.snapshot();
        snapshot.released = true;
        {
            let mut state = entry.lock();
            state.released = true;
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.retained.insert(key.clone(), snapshot);
        state.retained_order.push_back(key);
        while state.retained_order.len() > MAX_RETAINED_TERMINALS {
            if let Some(expired) = state.retained_order.pop_front() {
                state.retained.remove(&expired);
            }
        }
        Ok(json!({}))
    }

    pub(crate) fn read_retained(
        &self,
        session: &str,
        generation: u64,
        terminal_id: &str,
    ) -> Option<AcpTerminalSnapshot> {
        let key = (session.to_string(), generation, terminal_id.to_string());
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retained
            .get(&key)
            .cloned()
    }

    /// Returns and forgets a retained snapshot after its durable owner commits
    /// it. Live, unreleased handles cannot be removed through this method.
    pub(crate) fn take_snapshot(
        &self,
        session: &str,
        generation: u64,
        terminal_id: &str,
    ) -> Option<AcpTerminalSnapshot> {
        let key = (session.to_string(), generation, terminal_id.to_string());
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshot = state.retained.remove(&key)?;
        state.retained_order.retain(|candidate| candidate != &key);
        Some(snapshot)
    }

    /// Stop all processes owned by one conversation generation.
    pub(crate) async fn close_generation(
        &self,
        session: &str,
        generation: u64,
    ) -> Vec<AcpTerminalSnapshot> {
        self.stop_generation(session, generation, true).await
    }

    /// Stop and retain active terminals while leaving the generation open for
    /// later turns on the same conversation.
    pub(crate) async fn cancel_generation(
        &self,
        session: &str,
        generation: u64,
    ) -> Vec<AcpTerminalSnapshot> {
        self.stop_generation(session, generation, false).await
    }

    async fn stop_generation(
        &self,
        session: &str,
        generation: u64,
        seal: bool,
    ) -> Vec<AcpTerminalSnapshot> {
        let entries: Vec<_> = {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if seal {
                state
                    .closed_generations
                    .insert((session.to_string(), generation));
            } else {
                state
                    .stopping_generations
                    .insert((session.to_string(), generation));
            }
            let keys: Vec<_> = state
                .active
                .keys()
                .filter(|(id, gen, _)| id == session && *gen == generation)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| state.active.remove(&key))
                .collect()
        };
        for entry in entries {
            let _ = entry.kill.try_send(());
            loop {
                let changed = entry.changed.notified();
                if entry.lock().finished {
                    break;
                }
                changed.await;
            }
            let mut snapshot = entry.snapshot();
            snapshot.released = true;
            let key = (
                session.to_string(),
                generation,
                snapshot.terminal_id.clone(),
            );
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.retained.insert(key.clone(), snapshot);
            state.retained_order.push_back(key);
            while state.retained_order.len() > MAX_RETAINED_TERMINALS {
                if let Some(expired) = state.retained_order.pop_front() {
                    state.retained.remove(&expired);
                }
            }
        }
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshots = state
            .retained
            .iter()
            .filter_map(|((id, gen, _), snapshot)| {
                (id == session && *gen == generation).then(|| snapshot.clone())
            })
            .collect();
        if !seal {
            state
                .stopping_generations
                .remove(&(session.to_string(), generation));
        }
        snapshots
    }

    fn active_entry(
        &self,
        session: &str,
        generation: u64,
        params: &Value,
    ) -> Result<Arc<TerminalEntry>, String> {
        let terminal_id = required_string(params, "terminalId")?;
        let key = (session.to_string(), generation, terminal_id.to_string());
        self.inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .active
            .get(&key)
            .cloned()
            .ok_or_else(|| "unknown, stale, or released ACP terminal handle".to_string())
    }
}

fn required_string<'a>(params: &'a Value, field: &str) -> Result<&'a str, String> {
    params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("ACP terminal requires string field {field}"))
}

fn parse_env(params: &Value) -> Result<Vec<(String, String)>, String> {
    let Some(values) = params.get("env") else {
        return Ok(Vec::new());
    };
    let values = values
        .as_array()
        .ok_or_else(|| "ACP terminal env must be an array".to_string())?;
    if values.len() > MAX_ENVIRONMENT {
        return Err("ACP terminal has too many environment variables".into());
    }
    values
        .iter()
        .map(|value| {
            let name = required_string(value, "name")?;
            let value = required_string(value, "value")?;
            if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0')
            {
                return Err("ACP terminal environment entry is invalid".into());
            }
            Ok((name.to_string(), value.to_string()))
        })
        .collect()
}

async fn drain_output<R>(mut reader: R, entry: Arc<TerminalEntry>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0_u8; 8192];
    // stdout and stderr are independent byte streams. Keep a decoder per pipe
    // so interleaved reads cannot splice UTF-8 codepoints across the streams.
    let mut pending_utf8 = Vec::new();
    loop {
        match tokio::io::AsyncReadExt::read(&mut reader, &mut buffer).await {
            Ok(0) | Err(_) => {
                let tail = decode_utf8(&mut pending_utf8, &[], true);
                if !tail.is_empty() {
                    entry.lock().output.append_valid(&tail);
                }
                reader_finished(&entry);
                break;
            }
            Ok(read) => {
                let decoded = decode_utf8(&mut pending_utf8, &buffer[..read], false);
                if !decoded.is_empty() {
                    entry.lock().output.append_valid(&decoded);
                    entry.signal_change();
                }
            }
        }
    }
}

fn decode_utf8(pending: &mut Vec<u8>, chunk: &[u8], final_chunk: bool) -> Vec<u8> {
    pending.extend_from_slice(chunk);
    let mut output = Vec::with_capacity(pending.len());
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.extend_from_slice(text.as_bytes());
                pending.clear();
                break;
            }
            Err(error) => {
                let valid_end = error.valid_up_to();
                if valid_end > 0 {
                    output.extend_from_slice(&pending[..valid_end]);
                    pending.drain(..valid_end);
                }
                if let Some(invalid_len) = error.error_len() {
                    output.extend_from_slice("�".as_bytes());
                    pending.drain(..invalid_len.min(pending.len()));
                    continue;
                }
                if final_chunk && !pending.is_empty() {
                    output.extend_from_slice("�".as_bytes());
                    pending.clear();
                }
                break;
            }
        }
    }
    output
}

async fn supervise(
    mut child: Child,
    pid: u32,
    mut kill: mpsc::Receiver<()>,
    entry: Arc<TerminalEntry>,
) {
    let status = tokio::select! {
        result = child.wait() => result,
        _ = kill.recv() => {
            kill_process_tree(&mut child, pid).await;
            child.wait().await
        }
    };
    update_exit(&entry, status);
}

async fn kill_process_tree(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    unsafe {
        // Each ACP command receives its own process group at spawn. Negative
        // pid targets only that group, never the daemon or unrelated children.
        let _ = libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        let _ = crate::shared::process_core::kill_child_process_tree(child).await;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill().await;
    }
    let _ = child.start_kill();
}

fn update_exit(entry: &TerminalEntry, result: io::Result<ExitStatus>) {
    let mut state = entry.lock();
    match result {
        Ok(status) => {
            state.exit_code = status.code();
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                state.signal = status.signal();
            }
            state.process_exited = true;
        }
        Err(_) => {
            state.process_exited = true;
        }
    }
    state.finished = state.process_exited && state.readers_remaining == 0;
    drop(state);
    entry.signal_change();
}

fn reader_finished(entry: &TerminalEntry) {
    let mut state = entry.lock();
    state.readers_remaining = state.readers_remaining.saturating_sub(1);
    state.finished = state.process_exited && state.readers_remaining == 0;
    drop(state);
    entry.signal_change();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::OnceLock, time::Duration};
    use tokio::runtime::{Builder, Runtime};

    fn runtime() -> &'static Runtime {
        static RUNTIME: OnceLock<Runtime> = OnceLock::new();
        RUNTIME.get_or_init(|| Builder::new_multi_thread().enable_all().build().unwrap())
    }

    fn create_params(command: &str, args: &[&str]) -> Value {
        json!({ "command": command, "args": args, "env": [], "outputByteLimit": 64 })
    }

    #[test]
    fn utf8_decoder_keeps_codepoints_split_across_reads_and_replaces_bad_bytes() {
        let mut pending = Vec::new();
        assert!(decode_utf8(&mut pending, &[0xe2], false).is_empty());
        assert!(decode_utf8(&mut pending, &[0x98], false).is_empty());
        assert_eq!(decode_utf8(&mut pending, &[0x83], false), "☃".as_bytes());
        assert_eq!(decode_utf8(&mut pending, &[0xff], false), "�".as_bytes());
        assert_eq!(
            decode_utf8(&mut pending, &[0xf0, 0x9f], true),
            "�".as_bytes()
        );
        assert!(pending.is_empty());
    }

    #[cfg(unix)]
    fn shell_command(script: &str) -> Value {
        create_params("/bin/sh", &["-c", script])
    }

    async fn wait_finished(service: &AcpTerminalService, session: &str, id: &str) -> Value {
        tokio::time::timeout(
            Duration::from_secs(5),
            service.wait_for_exit(session, 1, &json!({ "terminalId": id })),
        )
        .await
        .expect("terminal should exit")
        .expect("wait_for_exit should succeed")
    }

    #[cfg(unix)]
    #[test]
    fn combines_split_utf8_and_keeps_a_bounded_valid_tail() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let params = shell_command(
                "printf 'a'; printf '\\342'; sleep 0.03; printf '\\230\\203'; printf 'bcdef'",
            );
            let created = service.create("s", 1, &params).await.unwrap();
            let id = created["terminalId"].as_str().unwrap();
            wait_finished(&service, "s", id).await;
            let result = service
                .output("s", 1, &json!({ "terminalId": id }))
                .await
                .unwrap();
            let output = result["output"].as_str().unwrap();
            assert_eq!(output, "a☃bcdef");
            assert!(output.is_char_boundary(output.len()));
        });
    }

    #[cfg(unix)]
    #[test]
    fn bounds_output_and_marks_truncation() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let params = json!({
                "command": "/bin/sh",
                "args": ["-c", "printf '0123456789'"],
                "outputByteLimit": 8
            });
            let created = service.create("s", 1, &params).await.unwrap();
            let id = created["terminalId"].as_str().unwrap();
            wait_finished(&service, "s", id).await;
            let result = service
                .output("s", 1, &json!({ "terminalId": id }))
                .await
                .unwrap();
            assert_eq!(result["output"], "23456789");
            assert_eq!(result["truncated"], true);
        });
    }

    #[cfg(unix)]
    #[test]
    fn returns_exit_status_and_released_snapshot_but_rejects_wire_operations() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let created = service
                .create("s", 1, &shell_command("printf done; exit 7"))
                .await
                .unwrap();
            let id = created["terminalId"].as_str().unwrap();
            assert_eq!(wait_finished(&service, "s", id).await["exitCode"], 7);
            service
                .release("s", 1, &json!({ "terminalId": id }))
                .await
                .unwrap();
            let snapshot = service.read_retained("s", 1, id).unwrap();
            assert_eq!(snapshot.output, "done");
            assert_eq!(snapshot.exit_code, Some(7));
            assert!(snapshot.released);
            assert!(service
                .output("s", 1, &json!({ "terminalId": id }))
                .await
                .is_err());
            assert_eq!(service.take_snapshot("s", 1, id).unwrap(), snapshot);
            assert!(service.read_retained("s", 1, id).is_none());
        });
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_wait_and_kill_reaps_child_tree() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let created = service
                .create("s", 1, &shell_command("sleep 30 & echo $!; wait"))
                .await
                .unwrap();
            let id = created["terminalId"].as_str().unwrap().to_string();
            let params = json!({ "terminalId": id });
            let waiting = {
                let service = service.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    service
                        .wait_for_exit("s", 1, &json!({ "terminalId": id }))
                        .await
                })
            };
            tokio::time::sleep(Duration::from_millis(40)).await;
            service.kill("s", 1, &params).await.unwrap();
            let result = tokio::time::timeout(Duration::from_secs(3), waiting)
                .await
                .expect("wait must unblock after kill")
                .unwrap()
                .unwrap();
            assert!(result["exitCode"].is_number() || result["signal"].is_string());
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn kill_targets_the_owned_process_group_children() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let created = service
                .create(
                    "tree-session",
                    1,
                    &shell_command("sleep 30 & echo $!; wait"),
                )
                .await
                .unwrap();
            let id = created["terminalId"].as_str().unwrap().to_string();
            tokio::time::sleep(Duration::from_millis(40)).await;
            service
                .kill("tree-session", 1, &json!({ "terminalId": id }))
                .await
                .unwrap();
            wait_finished(&service, "tree-session", &id).await;
            let mut output = String::new();
            for _ in 0..40 {
                output = service
                    .output("tree-session", 1, &json!({ "terminalId": id }))
                    .await
                    .unwrap()["output"]
                    .as_str()
                    .unwrap()
                    .to_string();
                if output
                    .lines()
                    .any(|line| line.trim().parse::<u32>().is_ok())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let child_pid: u32 = output
                .lines()
                .find_map(|line| line.trim().parse().ok())
                .expect("shell reported its child pid");
            let proc_stat = format!("/proc/{child_pid}/stat");
            let mut stopped = false;
            for _ in 0..40 {
                match std::fs::read_to_string(&proc_stat) {
                    Ok(stat) => {
                        // A killed child may briefly remain a zombie until the
                        // host init process reaps it; it must no longer run.
                        let state = stat.split_whitespace().nth(2).unwrap_or("?");
                        if state == "Z" || state == "X" {
                            stopped = true;
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        stopped = true;
                        break;
                    }
                    Err(_) => {}
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert!(stopped, "owned command child {child_pid} remained live");
        });
    }

    #[cfg(unix)]
    #[test]
    fn handles_are_scoped_to_session_and_generation() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let created = service
                .create("session-a", 4, &shell_command("sleep 1"))
                .await
                .unwrap();
            let id = created["terminalId"].as_str().unwrap();
            assert!(service
                .output("session-b", 4, &json!({ "terminalId": id }))
                .await
                .is_err());
            assert!(service
                .output("session-a", 5, &json!({ "terminalId": id }))
                .await
                .is_err());
            service.close_generation("session-a", 4).await;
            assert!(service
                .output("session-a", 4, &json!({ "terminalId": id }))
                .await
                .is_err());
            let snapshot = service.read_retained("session-a", 4, id).unwrap();
            assert!(snapshot.released);
        });
    }

    #[cfg(unix)]
    #[test]
    fn uses_supplied_environment_and_working_directory_without_shell_insertion() {
        runtime().block_on(async {
            let service = AcpTerminalService::new();
            let params = json!({
                "command": "/usr/bin/printenv",
                "args": ["ACP_TERMINAL_MARKER"],
                "cwd": "/tmp",
                "env": [{ "name": "ACP_TERMINAL_MARKER", "value": "exact" }],
                "outputByteLimit": 1024
            });
            let created = service.create("s", 1, &params).await.unwrap();
            let id = created["terminalId"].as_str().unwrap();
            wait_finished(&service, "s", id).await;
            let result = service
                .output("s", 1, &json!({ "terminalId": id }))
                .await
                .unwrap();
            assert_eq!(result["output"], "exact\n");

            // Inspect cwd independently so this test remains bounded even if
            // the host inherits a very large environment from the test runner.
            let cwd_params = json!({
                "command": "/usr/bin/pwd",
                "args": [],
                "cwd": "/tmp",
                "outputByteLimit": 1024
            });
            let cwd_created = service.create("s", 1, &cwd_params).await.unwrap();
            let cwd_id = cwd_created["terminalId"].as_str().unwrap();
            wait_finished(&service, "s", cwd_id).await;
            let cwd = service
                .output("s", 1, &json!({ "terminalId": cwd_id }))
                .await
                .unwrap();
            assert_eq!(cwd["output"], "/tmp\n");
        });
    }
}
