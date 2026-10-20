use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::backend::events::{EventSink, TerminalExit, TerminalOutput};
use crate::event_sink::TauriEventSink;
use crate::state::AppState;

pub(crate) use crate::shared::terminal_core::TerminalSession;

#[derive(Debug, Serialize, Clone)]
pub(crate) struct TerminalSessionInfo {
    id: String,
}

fn terminal_key(workspace_id: &str, terminal_id: &str) -> String {
    format!("{workspace_id}:{terminal_id}")
}

fn is_terminal_closed_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("broken pipe")
        || lower.contains("input/output error")
        || lower.contains("os error 5")
        || lower.contains("eio")
        || lower.contains("io error")
        || lower.contains("not connected")
        || lower.contains("closed")
}

async fn get_terminal_session(
    state: &State<'_, AppState>,
    key: &str,
) -> Result<Arc<TerminalSession>, String> {
    state
        .terminal_sessions
        .lock()
        .await
        .get(key)
        .cloned()
        .ok_or_else(|| "Terminal session not found".to_string())
}

#[cfg(target_os = "windows")]
fn shell_path() -> String {
    std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".to_string())
}

#[cfg(not(target_os = "windows"))]
fn shell_path() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string())
}

#[cfg(any(target_os = "windows", test))]
fn windows_shell_args(shell: &str) -> Vec<&'static str> {
    let shell = shell.to_ascii_lowercase();
    if shell.contains("powershell") || shell.ends_with("pwsh.exe") || shell.ends_with("\\pwsh") {
        vec!["-NoLogo", "-NoExit"]
    } else if shell.ends_with("cmd.exe") || shell.ends_with("\\cmd") {
        vec!["/K"]
    } else {
        Vec::new()
    }
}

fn unix_shell_args() -> Vec<&'static str> {
    vec!["-i"]
}

#[cfg(target_os = "windows")]
fn shell_args() -> Vec<&'static str> {
    windows_shell_args(&shell_path())
}

#[cfg(not(target_os = "windows"))]
fn shell_args() -> Vec<&'static str> {
    unix_shell_args()
}

fn resolve_locale() -> String {
    let candidate = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_else(|_| "en_US.UTF-8".to_string());
    let lower = candidate.to_lowercase();
    if lower.contains("utf-8") || lower.contains("utf8") {
        candidate
    } else {
        "en_US.UTF-8".to_string()
    }
}

fn spawn_terminal_reader(
    event_sink: impl EventSink,
    app: AppHandle,
    session: Arc<TerminalSession>,
    workspace_id: String,
    terminal_id: String,
    mut reader: Box<dyn Read + Send>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    pending.extend_from_slice(&buffer[..count]);
                    loop {
                        match std::str::from_utf8(&pending) {
                            Ok(decoded) => {
                                if !decoded.is_empty() {
                                    event_sink.emit_terminal_output(TerminalOutput {
                                        workspace_id: workspace_id.clone(),
                                        terminal_id: terminal_id.clone(),
                                        data: decoded.to_string(),
                                    });
                                }
                                pending.clear();
                                break;
                            }
                            Err(error) => {
                                let valid_up_to = error.valid_up_to();
                                if valid_up_to > 0 {
                                    let chunk = String::from_utf8_lossy(&pending[..valid_up_to])
                                        .to_string();
                                    if !chunk.is_empty() {
                                        event_sink.emit_terminal_output(TerminalOutput {
                                            workspace_id: workspace_id.clone(),
                                            terminal_id: terminal_id.clone(),
                                            data: chunk,
                                        });
                                    }
                                    pending.drain(..valid_up_to);
                                }
                                if error.error_len().is_none() {
                                    break;
                                }
                                let invalid_len = error.error_len().unwrap_or(1).min(pending.len());
                                if invalid_len == 0 {
                                    break;
                                }
                                pending.drain(..invalid_len);
                            }
                        }
                    }
                }
            }
        }

        event_sink.emit_terminal_exit(TerminalExit {
            workspace_id: workspace_id.clone(),
            terminal_id: terminal_id.clone(),
        });
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let key = terminal_key(&workspace_id, &terminal_id);
            let mut sessions = state.terminal_sessions.lock().await;
            if sessions
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            {
                sessions.remove(&key);
            }
        });
    });
}

async fn get_workspace_path(
    workspace_id: &str,
    state: &State<'_, AppState>,
) -> Result<PathBuf, String> {
    let workspaces = state.workspaces.lock().await;
    let entry = workspaces
        .get(workspace_id)
        .ok_or_else(|| "Unknown workspace".to_string())?;
    Ok(PathBuf::from(&entry.path))
}

#[tauri::command]
pub(crate) async fn terminal_open(
    workspace_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<TerminalSessionInfo, String> {
    if terminal_id.is_empty() {
        return Err("Terminal id is required".to_string());
    }
    let key = terminal_key(&workspace_id, &terminal_id);
    if let Some(existing) = state.terminal_sessions.lock().await.get(&key) {
        return Ok(TerminalSessionInfo {
            id: existing.id.clone(),
        });
    }

    let cwd = get_workspace_path(&workspace_id, &state).await?;
    let (session, reader) = TerminalSession::spawn(
        terminal_id.clone(),
        &cwd,
        &shell_path(),
        &shell_args(),
        cols,
        rows,
        &resolve_locale(),
    )?;
    let existing_id = {
        let mut sessions = state.terminal_sessions.lock().await;
        if let Some(existing) = sessions.get(&key) {
            Some(existing.id.clone())
        } else {
            sessions.insert(key, Arc::clone(&session));
            None
        }
    };
    if let Some(id) = existing_id {
        let _ = tokio::task::spawn_blocking(move || session.kill()).await;
        return Ok(TerminalSessionInfo { id });
    }
    spawn_terminal_reader(
        TauriEventSink::new(app.clone()),
        app,
        session,
        workspace_id,
        terminal_id.clone(),
        reader,
    );
    Ok(TerminalSessionInfo { id: terminal_id })
}

#[tauri::command]
pub(crate) async fn terminal_write(
    workspace_id: String,
    terminal_id: String,
    data: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let key = terminal_key(&workspace_id, &terminal_id);
    let session = get_terminal_session(&state, &key).await?;
    let result = tokio::task::spawn_blocking(move || session.write(data.as_bytes()))
        .await
        .map_err(|e| format!("Terminal write task failed: {e}"))?;
    if let Err(error) = result {
        if is_terminal_closed_error(&error) {
            state.terminal_sessions.lock().await.remove(&key);
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn terminal_resize(
    workspace_id: String,
    terminal_id: String,
    cols: u16,
    rows: u16,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let key = terminal_key(&workspace_id, &terminal_id);
    let session = get_terminal_session(&state, &key).await?;
    let result = tokio::task::spawn_blocking(move || session.resize(cols, rows))
        .await
        .map_err(|e| format!("Terminal resize task failed: {e}"))?;
    if let Err(error) = result {
        if is_terminal_closed_error(&error) {
            state.terminal_sessions.lock().await.remove(&key);
        }
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn terminal_close(
    workspace_id: String,
    terminal_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let key = terminal_key(&workspace_id, &terminal_id);
    let session = state
        .terminal_sessions
        .lock()
        .await
        .remove(&key)
        .ok_or_else(|| "Terminal session not found".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _ = session.kill();
    })
    .await
    .map_err(|e| format!("Terminal close task failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{unix_shell_args, windows_shell_args};
    #[test]
    fn windows_shell_args_match_powershell_and_cmd_variants() {
        assert_eq!(
            windows_shell_args(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"),
            vec!["-NoLogo", "-NoExit"]
        );
        assert_eq!(
            windows_shell_args(r"C:\Program Files\PowerShell\7\pwsh.exe"),
            vec!["-NoLogo", "-NoExit"]
        );
        assert_eq!(
            windows_shell_args(r"C:\Program Files\PowerShell\7\PwSh"),
            vec!["-NoLogo", "-NoExit"]
        );
        assert_eq!(
            windows_shell_args(r"C:\Windows\System32\cmd.exe"),
            vec!["/K"]
        );
        assert_eq!(windows_shell_args(r"C:\Windows\System32\CMD"), vec!["/K"]);
    }

    #[test]
    fn windows_shell_args_are_empty_for_other_shells() {
        assert!(windows_shell_args("nu.exe").is_empty());
    }
    #[test]
    fn unix_shell_args_stay_interactive() {
        assert_eq!(unix_shell_args(), vec!["-i"]);
    }
}
