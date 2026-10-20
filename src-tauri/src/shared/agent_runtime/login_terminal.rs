//! Short-lived interactive terminals used by ACP authentication flows.
//!
//! Terminal bytes stay in this process-local buffer. They are deliberately not
//! connected to the conversation journal, run captures, or normal diagnostics.

use serde::Serialize;
use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginTerminalInfo {
    pub operation_id: String,
    pub generation: u64,
    pub terminal_id: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginTerminalOutput {
    pub operation_id: String,
    pub generation: u64,
    /// Absolute byte offset of the first returned byte.
    pub cursor: u64,
    /// Next absolute byte offset callers should use.
    pub next_cursor: u64,
    pub output: String,
    pub truncated: bool,
    pub state: String,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LoginTerminalWriteAck {
    pub operation_id: String,
    pub generation: u64,
    pub sequence: u64,
    pub acknowledged: bool,
}

pub(crate) struct LoginTerminals {
    #[cfg(all(
        feature = "acp-host-services",
        not(any(target_os = "ios", target_os = "android"))
    ))]
    sessions: std::sync::Mutex<HashMap<String, Arc<pty::LoginSession>>>,
    #[cfg(all(
        feature = "acp-host-services",
        not(any(target_os = "ios", target_os = "android"))
    ))]
    output_limit: usize,
}

impl Default for LoginTerminals {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginTerminals {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(all(
                feature = "acp-host-services",
                not(any(target_os = "ios", target_os = "android"))
            ))]
            sessions: std::sync::Mutex::new(HashMap::new()),
            #[cfg(all(
                feature = "acp-host-services",
                not(any(target_os = "ios", target_os = "android"))
            ))]
            output_limit: 1024 * 1024,
        }
    }

    /// Start the exact executable and argument vector on a fresh PTY.
    pub(crate) fn spawn(
        &self,
        operation_id: impl Into<String>,
        generation: u64,
        executable: &str,
        arguments: &[String],
        environment: &std::collections::BTreeMap<String, String>,
        cwd: &Path,
    ) -> Result<LoginTerminalInfo, String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            let operation_id = operation_id.into();
            if operation_id.is_empty() || executable.is_empty() {
                return Err("Authentication terminal requires an operation and executable".into());
            }
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| "Authentication terminal registry unavailable")?;
            if sessions.contains_key(&operation_id) {
                return Err("Authentication terminal already exists for this operation".into());
            }
            let session = pty::LoginSession::spawn(
                operation_id.clone(),
                generation,
                executable,
                arguments,
                environment,
                cwd,
                self.output_limit,
            )?;
            let info = LoginTerminalInfo {
                operation_id: operation_id.clone(),
                generation,
                terminal_id: session.terminal_id.clone(),
                state: "running".into(),
            };
            sessions.insert(operation_id, session);
            Ok(info)
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (
                operation_id.into(),
                generation,
                executable,
                arguments,
                environment,
                cwd,
            );
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    /// Read output since an absolute byte cursor. Output is bounded and lossy
    /// UTF-8 conversion is explicit because terminals may emit arbitrary bytes.
    pub(crate) fn read(
        &self,
        operation_id: &str,
        generation: u64,
        cursor: u64,
        limit: Option<u32>,
    ) -> Result<LoginTerminalOutput, String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            let session = self.session(operation_id, generation)?;
            Ok(session.read(
                cursor,
                limit.unwrap_or(64 * 1024).clamp(1, 256 * 1024) as usize,
            ))
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (operation_id, generation, cursor, limit);
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    /// Input is base64 at the RPC boundary. Sequence retries are acknowledged
    /// once, without writing the bytes to the PTY a second time.
    pub(crate) fn write(
        &self,
        operation_id: &str,
        generation: u64,
        sequence: u64,
        input_base64: &str,
    ) -> Result<LoginTerminalWriteAck, String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            if input_base64.len() > 90 * 1024 {
                return Err("Authentication terminal input exceeds the request limit".into());
            }
            let input =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, input_base64)
                    .map_err(|_| "Authentication terminal input is not valid base64".to_string())?;
            if input.len() > 64 * 1024 {
                return Err("Authentication terminal input exceeds the request limit".into());
            }
            let session = self.session(operation_id, generation)?;
            session.write(sequence, &input)
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (operation_id, generation, sequence, input_base64);
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    pub(crate) fn resize(
        &self,
        operation_id: &str,
        generation: u64,
        columns: u16,
        rows: u16,
    ) -> Result<(), String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            self.session(operation_id, generation)?
                .resize(columns, rows)
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (operation_id, generation, columns, rows);
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    /// Cancel and reap the login process, then forget its transient terminal.
    pub(crate) fn close(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<LoginTerminalOutput, String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            let session = self.session(operation_id, generation)?;
            session.cancel(Duration::from_secs(3))?;
            let result = session.read(0, 256 * 1024);
            self.sessions
                .lock()
                .map_err(|_| "Authentication terminal registry unavailable")?
                .remove(operation_id);
            Ok(result)
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (operation_id, generation);
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    /// Cancel an in-progress login. A completed process keeps its actual exit
    /// result; a live process is marked cancelled before the termination signal.
    pub(crate) fn cancel(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<LoginTerminalOutput, String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            let session = self.session(operation_id, generation)?;
            session.cancel(Duration::from_secs(3))?;
            Ok(session.read(0, 256 * 1024))
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            let _ = (operation_id, generation);
            Err("Interactive ACP authentication terminals are unavailable in this build".into())
        }
    }

    /// Stop every login process owned by this service. Called when its backend
    /// runtime shuts down; command and conversation processes have separate owners.
    pub(crate) fn terminate_all(&self) -> Vec<String> {
        #[cfg(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        ))]
        {
            let sessions = match self.sessions.lock() {
                Ok(mut sessions) => std::mem::take(&mut *sessions),
                Err(poisoned) => std::mem::take(&mut *poisoned.into_inner()),
            };
            let mut failures = Vec::new();
            for (operation_id, session) in sessions {
                if session.terminate(Duration::from_secs(3)).is_err() {
                    failures.push(operation_id);
                }
            }
            failures
        }
        #[cfg(not(all(
            feature = "acp-host-services",
            not(any(target_os = "ios", target_os = "android"))
        )))]
        {
            Vec::new()
        }
    }

    #[cfg(all(
        feature = "acp-host-services",
        not(any(target_os = "ios", target_os = "android"))
    ))]
    fn session(
        &self,
        operation_id: &str,
        generation: u64,
    ) -> Result<Arc<pty::LoginSession>, String> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| "Authentication terminal registry unavailable")?;
        let session = sessions
            .get(operation_id)
            .cloned()
            .ok_or_else(|| "Authentication terminal is no longer available".to_string())?;
        if session.generation != generation {
            return Err("Authentication terminal generation is stale".into());
        }
        Ok(session)
    }
}

#[cfg(all(
    feature = "acp-host-services",
    not(any(target_os = "ios", target_os = "android"))
))]
mod pty {
    use super::*;
    use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
    use sha2::Digest;
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        sync::{Condvar, Mutex, Weak},
        thread,
        time::Instant,
    };

    const MAX_RECORDED_INPUT_SEQUENCES: usize = 4096;

    pub(super) struct LoginSession {
        pub(super) operation_id: String,
        pub(super) generation: u64,
        pub(super) terminal_id: String,
        limit: usize,
        master: Mutex<Box<dyn MasterPty + Send>>,
        writer: Mutex<Box<dyn Write + Send>>,
        child: Mutex<Box<dyn Child + Send>>,
        output: Mutex<Output>,
        output_changed: Condvar,
        status: Mutex<Option<ExitInfo>>,
        input_sequences: Mutex<InputSequence>,
        cancelled: std::sync::atomic::AtomicBool,
    }

    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        start: u64,
        end: u64,
        ended: bool,
    }

    #[derive(Clone, Copy)]
    struct ExitInfo {
        code: Option<i32>,
        signal: Option<i32>,
    }

    #[derive(Default)]
    struct InputSequence {
        last: u64,
        seen: std::collections::HashMap<u64, ([u8; 32], bool)>,
    }

    impl LoginSession {
        pub(super) fn spawn(
            operation_id: String,
            generation: u64,
            executable: &str,
            arguments: &[String],
            environment: &BTreeMap<String, String>,
            cwd: &Path,
            limit: usize,
        ) -> Result<Arc<Self>, String> {
            let system = native_pty_system();
            let pair = system
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|_| "Could not open authentication terminal".to_string())?;
            let mut command = CommandBuilder::new(executable);
            command.cwd(cwd);
            command.env("TERM", "xterm-256color");
            for (key, value) in environment {
                command.env(key, value);
            }
            for argument in arguments {
                command.arg(argument);
            }
            let child = pair
                .slave
                .spawn_command(command)
                .map_err(|_| "Could not start authentication process".to_string())?;
            let reader = pair
                .master
                .try_clone_reader()
                .map_err(|_| "Could not read authentication terminal".to_string())?;
            let writer = pair
                .master
                .take_writer()
                .map_err(|_| "Could not write to authentication terminal".to_string())?;
            let session = Arc::new(Self {
                operation_id,
                generation,
                terminal_id: uuid::Uuid::new_v4().to_string(),
                limit,
                master: Mutex::new(pair.master),
                writer: Mutex::new(writer),
                child: Mutex::new(child),
                output: Mutex::new(Output::default()),
                output_changed: Condvar::new(),
                status: Mutex::new(None),
                input_sequences: Mutex::new(InputSequence::default()),
                cancelled: std::sync::atomic::AtomicBool::new(false),
            });
            if Self::start_reader(Arc::downgrade(&session), reader).is_err()
                || Self::start_reaper(Arc::downgrade(&session)).is_err()
            {
                let _ = session.terminate(Duration::from_secs(3));
                return Err("Could not start authentication terminal monitor".into());
            }
            Ok(session)
        }

        fn start_reader(
            session: Weak<Self>,
            mut reader: Box<dyn Read + Send>,
        ) -> Result<(), std::io::Error> {
            thread::Builder::new()
                .name("acp-auth-terminal-output".into())
                .spawn(move || {
                    let mut chunk = [0u8; 8192];
                    loop {
                        match reader.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(count) => {
                                let Some(session) = session.upgrade() else {
                                    break;
                                };
                                if let Ok(mut output) = session.output.lock() {
                                    output.bytes.extend_from_slice(&chunk[..count]);
                                    output.end = output.end.saturating_add(count as u64);
                                    if output.bytes.len() > session.limit {
                                        let excess = output.bytes.len() - session.limit;
                                        output.bytes.drain(..excess);
                                        output.start = output.start.saturating_add(excess as u64);
                                    }
                                    session.output_changed.notify_all();
                                } else {
                                    break;
                                };
                            }
                        }
                    }
                    if let Some(session) = session.upgrade() {
                        if let Ok(mut output) = session.output.lock() {
                            output.ended = true;
                            session.output_changed.notify_all();
                        };
                    }
                })
                .map(|_| ())
        }

        fn start_reaper(session: Weak<Self>) -> Result<(), std::io::Error> {
            thread::Builder::new()
                .name("acp-auth-terminal-reaper".into())
                .spawn(move || loop {
                    let Some(session) = session.upgrade() else {
                        break;
                    };
                    let result = session
                        .child
                        .lock()
                        .ok()
                        .and_then(|mut child| child.try_wait().ok().flatten());
                    if let Some(status) = result {
                        if let Ok(mut state) = session.status.lock() {
                            *state = Some(exit_info(status));
                        };
                        if let Ok(mut output) = session.output.lock() {
                            output.ended = true;
                            session.output_changed.notify_all();
                        };
                        break;
                    }
                    if session
                        .status
                        .lock()
                        .ok()
                        .and_then(|status| *status)
                        .is_some()
                    {
                        break;
                    }
                    drop(session);
                    thread::sleep(Duration::from_millis(50));
                })
                .map(|_| ())
        }

        pub(super) fn write(
            &self,
            sequence: u64,
            bytes: &[u8],
        ) -> Result<LoginTerminalWriteAck, String> {
            let digest: [u8; 32] = sha2::Sha256::digest(bytes).into();
            let mut sequences = self
                .input_sequences
                .lock()
                .map_err(|_| "Authentication input state unavailable")?;
            if let Some((previous, acknowledged)) = sequences.seen.get(&sequence) {
                if previous != &digest {
                    return Err("Input sequence was reused with different content".into());
                }
                if !acknowledged {
                    return Err("Authentication input delivery is uncertain; do not resend".into());
                }
                return Ok(LoginTerminalWriteAck {
                    operation_id: self.operation_id.clone(),
                    generation: self.generation,
                    sequence,
                    acknowledged: true,
                });
            }
            if sequence == 0 || sequence != sequences.last.saturating_add(1) {
                return Err("Authentication input sequence is stale or out of order".into());
            }
            if sequences.seen.len() >= MAX_RECORDED_INPUT_SEQUENCES {
                return Err(
                    "Authentication input sequence limit reached; restart the login operation"
                        .into(),
                );
            }
            // Reserve before touching the PTY. If the write fails after a
            // partial delivery, an identical retry is rejected as uncertain
            // instead of duplicating credentials or keystrokes.
            sequences.last = sequence;
            sequences.seen.insert(sequence, (digest, false));
            let mut writer = self
                .writer
                .lock()
                .map_err(|_| "Authentication terminal input unavailable")?;
            writer
                .write_all(bytes)
                .and_then(|()| writer.flush())
                .map_err(|_| "Could not deliver authentication input".to_string())?;
            if let Some((_, acknowledged)) = sequences.seen.get_mut(&sequence) {
                *acknowledged = true;
            }
            Ok(LoginTerminalWriteAck {
                operation_id: self.operation_id.clone(),
                generation: self.generation,
                sequence,
                acknowledged: true,
            })
        }

        pub(super) fn resize(&self, columns: u16, rows: u16) -> Result<(), String> {
            self.master
                .lock()
                .map_err(|_| "Authentication terminal unavailable")?
                .resize(PtySize {
                    rows: rows.clamp(2, 300),
                    cols: columns.clamp(2, 500),
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|_| "Could not resize authentication terminal".to_string())
        }

        pub(super) fn read(&self, cursor: u64, limit: usize) -> LoginTerminalOutput {
            let output = self
                .output
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let start = cursor.max(output.start).min(output.end);
            let relative_start = (start - output.start) as usize;
            let count = limit.min(output.bytes.len().saturating_sub(relative_start));
            let end = relative_start + count;
            let status = self.status.lock().ok().and_then(|status| *status);
            let state = if self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                "cancelled"
            } else if let Some(status) = status {
                if status.code == Some(0) {
                    "succeeded"
                } else {
                    "failed"
                }
            } else {
                // PTY EOF can precede child wait/reap by a scheduling tick.
                // Only the child monitor can establish process completion.
                "running"
            };
            LoginTerminalOutput {
                operation_id: self.operation_id.clone(),
                generation: self.generation,
                cursor: start,
                next_cursor: start.saturating_add(count as u64),
                output: String::from_utf8_lossy(&output.bytes[relative_start..end]).into_owned(),
                truncated: cursor < output.start,
                state: state.into(),
                exit_code: status.and_then(|s| s.code),
                exit_signal: status.and_then(|s| s.signal),
            }
        }

        pub(super) fn terminate(&self, timeout: Duration) -> Result<(), String> {
            let deadline = Instant::now() + timeout;
            let mut child = self
                .child
                .lock()
                .map_err(|_| "Authentication process state unavailable")?;
            if let Some(status) = child
                .try_wait()
                .map_err(|_| "Could not inspect authentication process")?
            {
                if let Ok(mut current) = self.status.lock() {
                    *current = Some(exit_info(status));
                }
                return Ok(());
            }
            #[cfg(unix)]
            let process_group = child.process_id().map(|pid| pid as i32);
            #[cfg(unix)]
            {
                // portable-pty creates a session for the PTY child on Unix;
                // signaling its process group also stops login helpers.
                signal_process_group(process_group, libc::SIGTERM);
            }
            let _ = child.kill();
            let hard_kill_at = Instant::now() + Duration::from_millis(300);
            let mut hard_kill_sent = false;
            let mut exit_status = None;
            loop {
                if exit_status.is_none() {
                    exit_status = child
                        .try_wait()
                        .map_err(|_| "Could not reap authentication process")?;
                }
                if !hard_kill_sent && Instant::now() >= hard_kill_at {
                    #[cfg(unix)]
                    signal_process_group(process_group, libc::SIGKILL);
                    #[cfg(not(unix))]
                    if exit_status.is_none() {
                        let _ = child.kill();
                    }
                    hard_kill_sent = true;
                }
                if exit_status.is_some() && hard_kill_sent {
                    if let Ok(mut current) = self.status.lock() {
                        *current = exit_status.map(exit_info);
                    }
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("Authentication process did not stop before deadline".into());
                }
                thread::sleep(Duration::from_millis(20));
            }
        }

        pub(super) fn cancel(&self, timeout: Duration) -> Result<(), String> {
            if self.status.lock().ok().and_then(|status| *status).is_some() {
                return Ok(());
            }
            self.cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            self.terminate(timeout)
        }
    }

    #[cfg(unix)]
    fn signal_process_group(process_group: Option<i32>, signal: i32) {
        if let Some(process_group) = process_group {
            // Negative pid targets the PTY session process group.
            unsafe {
                libc::kill(-process_group, signal);
            }
        }
    }

    fn exit_info(status: portable_pty::ExitStatus) -> ExitInfo {
        #[cfg(unix)]
        {
            ExitInfo {
                code: status.exit_code().try_into().ok(),
                signal: None,
            }
        }
        #[cfg(not(unix))]
        {
            ExitInfo {
                code: status.exit_code().try_into().ok(),
                signal: None,
            }
        }
    }
}

#[cfg(all(
    test,
    feature = "acp-host-services",
    unix,
    not(any(target_os = "ios", target_os = "android"))
))]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, path::Path};

    fn manager() -> LoginTerminals {
        LoginTerminals::new()
    }

    #[test]
    fn interactive_input_is_idempotent_and_output_is_ephemeral_and_bounded() {
        let terminals = manager();
        let mut env = BTreeMap::new();
        env.insert("LOGIN_FIXTURE_SECRET".into(), "do-not-log-me".into());
        let info = terminals.spawn("login-test", 3, "/bin/sh", &[
            "-c".into(),
            "printf 'PROMPT>'; IFS= read -r value; printf 'GOT:%s:%s\\n' \"$value\" \"$LOGIN_FIXTURE_SECRET\"; exit 0".into(),
        ], &env, Path::new("/")).unwrap();
        assert_eq!(info.state, "running");
        let first = terminals
            .write("login-test", 3, 1, "c2VjcmV0LXZhbHVlCg==")
            .unwrap();
        assert!(first.acknowledged);
        let retry = terminals
            .write("login-test", 3, 1, "c2VjcmV0LXZhbHVlCg==")
            .unwrap();
        assert_eq!(first, retry);
        assert!(terminals
            .write("login-test", 3, 1, "ZGlmZmVyZW50Cg==")
            .is_err());
        assert!(terminals.read("login-test", 2, 0, None).is_err());
        let mut read = terminals
            .read("login-test", 3, 0, Some(256 * 1024))
            .unwrap();
        for _ in 0..40 {
            if read.state != "running" && read.output.contains("GOT:") {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
            read = terminals
                .read("login-test", 3, read.next_cursor, Some(256 * 1024))
                .unwrap();
        }
        let all = terminals
            .read("login-test", 3, 0, Some(256 * 1024))
            .unwrap();
        assert!(all.output.contains("GOT:secret-value:do-not-log-me"));
        assert_eq!(all.state, "succeeded");
        assert_eq!(terminals.close("login-test", 3).unwrap().state, "succeeded");
        assert!(terminals.read("login-test", 3, 0, None).is_err());
    }

    #[test]
    fn login_terminal_cancellation_reaps_process_and_resize_is_clamped() {
        let terminals = manager();
        terminals
            .spawn(
                "login-cancel",
                1,
                "/bin/sh",
                &["-c".into(), "printf 'WAITING'; exec sleep 30".into()],
                &BTreeMap::new(),
                Path::new("/"),
            )
            .unwrap();
        terminals.resize("login-cancel", 1, 0, 0).unwrap();
        let stopped = terminals.cancel("login-cancel", 1).unwrap();
        assert_eq!(stopped.state, "cancelled");
        terminals.close("login-cancel", 1).unwrap();
    }

    #[test]
    fn output_buffer_is_bounded_and_reports_lost_prefix() {
        let terminals = LoginTerminals {
            sessions: std::sync::Mutex::new(HashMap::new()),
            output_limit: 32,
        };
        terminals
            .spawn(
                "login-output-bound",
                2,
                "/bin/sh",
                &[
                    "-c".into(),
                    "head -c 200 /dev/zero | tr '\\000' x; exit 0".into(),
                ],
                &std::collections::BTreeMap::new(),
                Path::new("/"),
            )
            .unwrap();
        let mut result = terminals
            .read("login-output-bound", 2, 0, Some(1024))
            .unwrap();
        for _ in 0..60 {
            if result.state != "running" {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
            result = terminals
                .read("login-output-bound", 2, 0, Some(1024))
                .unwrap();
        }
        assert!(result.truncated);
        assert!(result.output.len() <= 32);
        assert_eq!(result.output, "x".repeat(32));
        assert_eq!(result.next_cursor, 200);
        terminals.close("login-output-bound", 2).unwrap();
    }
}

#[cfg(all(
    test,
    not(all(
        feature = "acp-host-services",
        not(any(target_os = "ios", target_os = "android"))
    ))
))]
mod unavailable_tests {
    use super::*;
    #[test]
    fn builds_without_host_services_report_terminal_unavailable() {
        let error = LoginTerminals::new().read("op", 1, 0, None).unwrap_err();
        assert!(error.contains("unavailable"));
    }
}
