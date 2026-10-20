//! Host-independent managed TCP daemon and Tailscale operations shared by the
//! native desktop and daemon clients. Process paths and settings are supplied by
//! each host; this module owns the protocol, identity checks, and lifecycle.

use std::{
    ffi::{OsStr, OsString},
    io::ErrorKind,
    path::Path,
    process::Output,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpListener, TcpStream,
    },
    process::Child,
    time::{sleep, timeout, Instant},
};

use crate::{
    shared::{
        process_core::{kill_child_process_tree, tokio_command},
        remote_settings::active_remote_backend,
    },
    types::{
        AppSettings, TailscaleDaemonCommandPreview, TailscaleStatus, TcpDaemonState,
        TcpDaemonStatus,
    },
};

#[path = "../tailscale/rpc_client.rs"]
mod rpc_client;
#[path = "../tailscale/core.rs"]
mod tailscale_projection;

const EXPECTED_DAEMON_NAME: &str = "codex-monitor-daemon";
const EXPECTED_DAEMON_MODE: &str = "tcp";
const CURRENT_APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_DAEMON_LISTEN_ADDR: &str = "0.0.0.0:4732";
const REMOTE_TOKEN_PLACEHOLDER: &str = "<remote-backend-token>";
const DAEMON_RPC_TIMEOUT: Duration = Duration::from_millis(700);

pub(crate) struct ManagedDaemonRuntime {
    pub(crate) child: Option<Child>,
    pub(crate) status: TcpDaemonStatus,
}

impl Default for ManagedDaemonRuntime {
    fn default() -> Self {
        Self {
            child: None,
            status: TcpDaemonStatus {
                state: TcpDaemonState::Stopped,
                pid: None,
                started_at_ms: None,
                last_error: None,
                listen_addr: None,
            },
        }
    }
}

fn is_managed_daemon(info: &rpc_client::DaemonInfo) -> bool {
    info.name == EXPECTED_DAEMON_NAME
}

fn can_force_stop_daemon(auth_ok: bool, info: Option<&rpc_client::DaemonInfo>) -> bool {
    auth_ok && info.is_some_and(is_managed_daemon)
}

fn should_restart_daemon(info: Option<&rpc_client::DaemonInfo>) -> bool {
    let Some(info) = info else {
        return true;
    };
    !is_managed_daemon(info)
        || info.version != CURRENT_APP_VERSION
        || info.mode != EXPECTED_DAEMON_MODE
}

fn daemon_restart_reason(info: Option<&rpc_client::DaemonInfo>) -> String {
    let Some(info) = info else {
        return "Daemon is running but did not report identity/version metadata".to_string();
    };
    if !is_managed_daemon(info) {
        return format!("Daemon identity mismatch (`{}`)", info.name);
    }
    if info.version != CURRENT_APP_VERSION {
        return format!(
            "Daemon version {} is different from app version {}",
            info.version, CURRENT_APP_VERSION
        );
    }
    if info.mode != EXPECTED_DAEMON_MODE {
        return format!(
            "Daemon mode `{}` does not match expected `{}`",
            info.mode, EXPECTED_DAEMON_MODE
        );
    }
    "Daemon restart required".to_string()
}

async fn resolve_daemon_pid(
    listen_port: u16,
    info: Option<&rpc_client::DaemonInfo>,
) -> Option<u32> {
    match info.and_then(|entry| entry.pid) {
        Some(pid) => Some(pid),
        None => find_listener_pid(listen_port).await,
    }
}

pub(crate) fn daemon_command_preview(
    settings: &AppSettings,
    daemon_path: &Path,
    data_dir: &Path,
) -> TailscaleDaemonCommandPreview {
    let token_configured = active_remote_backend(settings)
        .token
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let daemon_path_str = daemon_path.to_string_lossy().to_string();
    let data_dir_str = data_dir.to_string_lossy().to_string();
    let args = vec![
        "--listen".to_string(),
        DEFAULT_DAEMON_LISTEN_ADDR.to_string(),
        "--data-dir".to_string(),
        data_dir_str.clone(),
        "--token".to_string(),
        REMOTE_TOKEN_PLACEHOLDER.to_string(),
    ];
    let mut rendered = Vec::with_capacity(args.len() + 1);
    rendered.push(shell_quote(&daemon_path_str));
    rendered.extend(args.iter().map(|value| shell_quote(value)));
    TailscaleDaemonCommandPreview {
        command: rendered.join(" "),
        daemon_path: daemon_path_str,
        args,
        token_configured,
    }
}

pub(crate) async fn daemon_start(
    settings: &AppSettings,
    data_dir: &Path,
    daemon_binary: &Path,
    runtime: &mut ManagedDaemonRuntime,
) -> Result<TcpDaemonStatus, String> {
    let remote = active_remote_backend(settings);
    let token = remote
        .token
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "Set a Remote backend token before starting remote access daemon.".to_string()
        })?;
    let listen_addr = configured_daemon_listen_addr(settings);
    let listen_port = parse_port_from_remote_host(&listen_addr)
        .ok_or_else(|| format!("Invalid daemon listen address: {listen_addr}"))?;

    refresh_tcp_daemon_runtime(runtime).await;
    match rpc_client::probe_daemon(&listen_addr, Some(token)).await {
        rpc_client::DaemonProbe::Running {
            auth_ok,
            auth_error,
            info,
        } => {
            let pid = resolve_daemon_pid(listen_port, info.as_ref()).await;
            let restart_required = should_restart_daemon(info.as_ref());
            let restart_reason = restart_required.then(|| daemon_restart_reason(info.as_ref()));
            runtime.child = None;
            runtime.status = TcpDaemonStatus {
                state: TcpDaemonState::Running,
                pid,
                started_at_ms: runtime.status.started_at_ms,
                last_error: auth_error.clone(),
                listen_addr: Some(listen_addr.clone()),
            };
            if !auth_ok {
                return Err(auth_error.unwrap_or_else(|| {
                    "Daemon is already running but authentication failed.".to_string()
                }));
            }
            if !restart_required {
                return Ok(runtime.status.clone());
            }

            let force_kill_allowed = can_force_stop_daemon(auth_ok, info.as_ref());
            let pid_for_control = pid;
            if let Err(shutdown_error) =
                rpc_client::request_daemon_shutdown(&listen_addr, Some(token)).await
            {
                if !force_kill_allowed {
                    return Err(format!(
                        "{}; automatic restart aborted because daemon ownership could not be verified: {}",
                        restart_reason.clone().unwrap_or_else(|| "Daemon restart required".to_string()),
                        shutdown_error
                    ));
                }
                if let Some(pid) = pid_for_control {
                    kill_pid_gracefully(pid).await.map_err(|err| format!(
                        "{}; graceful shutdown failed ({shutdown_error}) and forced stop failed: {err}",
                        restart_reason.clone().unwrap_or_else(|| "Daemon restart required".to_string())
                    ))?;
                } else {
                    return Err(format!(
                        "{}; daemon did not stop and no PID could be resolved for safe forced stop ({shutdown_error})",
                        restart_reason.unwrap_or_else(|| "Daemon restart required".to_string())
                    ));
                }
            }

            if !rpc_client::wait_for_daemon_shutdown(&listen_addr, Some(token)).await {
                if !force_kill_allowed {
                    return Err(format!(
                        "{}; daemon acknowledged shutdown but is still reachable",
                        restart_reason
                            .clone()
                            .unwrap_or_else(|| "Daemon restart required".to_string())
                    ));
                }
                if let Some(pid) = resolve_daemon_pid(listen_port, info.as_ref()).await {
                    kill_pid_gracefully(pid).await.map_err(|err| {
                        format!(
                            "{}; daemon remained reachable and forced stop failed: {err}",
                            restart_reason
                                .clone()
                                .unwrap_or_else(|| "Daemon restart required".to_string())
                        )
                    })?;
                } else {
                    return Err(format!(
                        "{}; daemon remained reachable and no PID could be resolved for safe forced stop",
                        restart_reason.unwrap_or_else(|| "Daemon restart required".to_string())
                    ));
                }
            }
            runtime.status = TcpDaemonStatus {
                state: TcpDaemonState::Stopped,
                pid: None,
                started_at_ms: None,
                last_error: None,
                listen_addr: Some(listen_addr.clone()),
            };
        }
        rpc_client::DaemonProbe::NotDaemon => {
            return Err(format!(
                "Cannot start remote access daemon because {listen_addr} is already in use by another process."
            ));
        }
        rpc_client::DaemonProbe::NotReachable => {}
    }

    ensure_listen_addr_available(&listen_addr).await?;
    let child = tokio_command(daemon_binary)
        .arg("--listen")
        .arg(&listen_addr)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--token")
        .arg(token)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|err| format!("Failed to start remote access daemon: {err}"))?;

    runtime.status = TcpDaemonStatus {
        state: TcpDaemonState::Running,
        pid: child.id(),
        started_at_ms: Some(now_unix_ms()),
        last_error: None,
        listen_addr: Some(listen_addr),
    };
    runtime.child = Some(child);
    Ok(runtime.status.clone())
}

pub(crate) async fn daemon_stop(
    settings: &AppSettings,
    runtime: &mut ManagedDaemonRuntime,
) -> Result<TcpDaemonStatus, String> {
    let remote = active_remote_backend(settings);
    let configured_listen_addr = configured_daemon_listen_addr(settings);
    let listen_port = parse_port_from_remote_host(&configured_listen_addr);
    let mut stop_error: Option<String> = None;
    if let Some(mut child) = runtime.child.take() {
        kill_child_process_tree(&mut child).await;
        let _ = child.wait().await;
    } else if let Some(port) = listen_port {
        match rpc_client::probe_daemon(&configured_listen_addr, remote.token.as_deref()).await {
            rpc_client::DaemonProbe::Running { auth_ok, info, .. } => {
                let force_kill_allowed = can_force_stop_daemon(auth_ok, info.as_ref());
                if let Err(shutdown_error) = rpc_client::request_daemon_shutdown(
                    &configured_listen_addr,
                    remote.token.as_deref(),
                )
                .await
                {
                    let pid = resolve_daemon_pid(port, info.as_ref()).await;
                    if let Some(pid) = pid {
                        if force_kill_allowed {
                            if let Err(err) = kill_pid_gracefully(pid).await {
                                stop_error = Some(format!("{shutdown_error}; {err}"));
                            }
                        } else {
                            stop_error = Some(format!(
                                "{shutdown_error}; refusing forced stop because daemon ownership could not be verified"
                            ));
                        }
                    } else {
                        stop_error = Some(shutdown_error);
                    }
                } else if !rpc_client::wait_for_daemon_shutdown(
                    &configured_listen_addr,
                    remote.token.as_deref(),
                )
                .await
                {
                    if force_kill_allowed {
                        if let Some(pid) = resolve_daemon_pid(port, info.as_ref()).await {
                            if let Err(err) = kill_pid_gracefully(pid).await {
                                stop_error = Some(format!(
                                    "Daemon acknowledged shutdown but remained reachable; {err}"
                                ));
                            }
                        } else {
                            stop_error = Some(
                                "Daemon acknowledged shutdown but remained reachable and PID could not be resolved."
                                    .to_string(),
                            );
                        }
                    } else {
                        stop_error = Some(
                            "Daemon acknowledged shutdown but is still reachable; refusing forced stop because daemon ownership could not be verified."
                                .to_string(),
                        );
                    }
                }
            }
            rpc_client::DaemonProbe::NotDaemon => {
                stop_error = Some(format!(
                    "Port {port} is in use by a non-daemon process; refusing to stop it."
                ));
            }
            rpc_client::DaemonProbe::NotReachable => {}
        }
    }

    let probe_after_stop =
        rpc_client::probe_daemon(&configured_listen_addr, remote.token.as_deref()).await;
    let pid_after_stop = match listen_port {
        Some(port) => find_listener_pid(port).await,
        None => None,
    };
    runtime.status = match probe_after_stop {
        rpc_client::DaemonProbe::Running { auth_error, .. } => TcpDaemonStatus {
            state: TcpDaemonState::Error,
            pid: pid_after_stop,
            started_at_ms: runtime.status.started_at_ms,
            last_error: Some(
                stop_error
                    .or(auth_error)
                    .unwrap_or_else(|| "Daemon is still running after stop attempt.".to_string()),
            ),
            listen_addr: runtime.status.listen_addr.clone(),
        },
        rpc_client::DaemonProbe::NotDaemon => TcpDaemonStatus {
            state: TcpDaemonState::Error,
            pid: pid_after_stop,
            started_at_ms: runtime.status.started_at_ms,
            last_error: Some(stop_error.unwrap_or_else(|| {
                "Configured port is now occupied by a non-daemon process.".to_string()
            })),
            listen_addr: runtime.status.listen_addr.clone(),
        },
        rpc_client::DaemonProbe::NotReachable => TcpDaemonStatus {
            state: TcpDaemonState::Stopped,
            pid: None,
            started_at_ms: None,
            last_error: stop_error,
            listen_addr: runtime.status.listen_addr.clone(),
        },
    };
    sync_tcp_daemon_listen_addr(&mut runtime.status, &configured_listen_addr);
    Ok(runtime.status.clone())
}

pub(crate) async fn daemon_status(
    settings: &AppSettings,
    runtime: &mut ManagedDaemonRuntime,
) -> Result<TcpDaemonStatus, String> {
    let remote = active_remote_backend(settings);
    let configured_listen_addr = configured_daemon_listen_addr(settings);
    let listen_port = parse_port_from_remote_host(&configured_listen_addr);
    refresh_tcp_daemon_runtime(runtime).await;
    if !matches!(runtime.status.state, TcpDaemonState::Running) {
        let pid = match listen_port {
            Some(port) => find_listener_pid(port).await,
            None => None,
        };
        runtime.status = match rpc_client::probe_daemon(
            &configured_listen_addr,
            remote.token.as_deref(),
        )
        .await
        {
            rpc_client::DaemonProbe::Running { auth_error, .. } => TcpDaemonStatus {
                state: TcpDaemonState::Running,
                pid,
                started_at_ms: runtime.status.started_at_ms,
                last_error: auth_error,
                listen_addr: runtime.status.listen_addr.clone(),
            },
            rpc_client::DaemonProbe::NotDaemon => TcpDaemonStatus {
                state: TcpDaemonState::Error,
                pid,
                started_at_ms: runtime.status.started_at_ms,
                last_error: Some(format!(
                    "Configured daemon port {configured_listen_addr} is occupied by a non-daemon process."
                )),
                listen_addr: runtime.status.listen_addr.clone(),
            },
            rpc_client::DaemonProbe::NotReachable => TcpDaemonStatus {
                state: runtime.status.state.clone(),
                pid: runtime.status.pid,
                started_at_ms: runtime.status.started_at_ms,
                last_error: runtime.status.last_error.clone(),
                listen_addr: runtime.status.listen_addr.clone(),
            },
        };
    }
    sync_tcp_daemon_listen_addr(&mut runtime.status, &configured_listen_addr);
    Ok(runtime.status.clone())
}

/// Stop only this host manager's child process during application exit.
/// Explicit user-triggered `daemon_stop` retains the legacy ability to stop a
/// verified daemon managed by another process.
pub(crate) async fn shutdown_for_exit(
    settings: &AppSettings,
    runtime: &mut ManagedDaemonRuntime,
) -> Result<TcpDaemonStatus, String> {
    if !should_stop_owned_daemon_for_exit(settings, runtime.child.is_some()) {
        // Exit cleanup must never probe the configured port: it may belong to
        // an unrelated daemon that this process does not own.
        return Ok(runtime.status.clone());
    }
    if let Some(mut child) = runtime.child.take() {
        kill_child_process_tree(&mut child).await;
        let _status = child
            .wait()
            .await
            .map_err(|error| format!("Failed waiting for managed daemon process: {error}"))?;
        runtime.status = TcpDaemonStatus {
            // A signal exit is expected because cleanup deliberately killed
            // this child. A successful wait means ownership has ended.
            state: TcpDaemonState::Stopped,
            pid: None,
            started_at_ms: None,
            last_error: None,
            listen_addr: runtime.status.listen_addr.clone(),
        };
    }
    Ok(runtime.status.clone())
}

fn should_stop_owned_daemon_for_exit(settings: &AppSettings, owns_child: bool) -> bool {
    owns_child && !settings.keep_daemon_running_after_app_close
}

async fn ensure_listen_addr_available(listen_addr: &str) -> Result<(), String> {
    match TcpListener::bind(listen_addr).await {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(err) => Err(format!(
            "Cannot start remote access daemon because {listen_addr} is unavailable: {err}"
        )),
    }
}

async fn refresh_tcp_daemon_runtime(runtime: &mut ManagedDaemonRuntime) {
    let Some(child) = runtime.child.as_mut() else {
        runtime.status.state = TcpDaemonState::Stopped;
        runtime.status.pid = None;
        return;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            let pid = child.id();
            runtime.child = None;
            runtime.status = if status.success() {
                TcpDaemonStatus {
                    state: TcpDaemonState::Stopped,
                    pid,
                    started_at_ms: None,
                    last_error: None,
                    listen_addr: runtime.status.listen_addr.clone(),
                }
            } else {
                let failure_hint = if status.code() == Some(101) {
                    " This usually indicates a startup panic (often due to an unavailable listen port)."
                } else {
                    ""
                };
                TcpDaemonStatus {
                    state: TcpDaemonState::Error,
                    pid,
                    started_at_ms: runtime.status.started_at_ms,
                    last_error: Some(format!(
                        "Daemon exited with status: {status}.{failure_hint}"
                    )),
                    listen_addr: runtime.status.listen_addr.clone(),
                }
            };
        }
        Ok(None) => {
            runtime.status.state = TcpDaemonState::Running;
            runtime.status.pid = child.id();
            runtime.status.last_error = None;
        }
        Err(err) => {
            runtime.status = TcpDaemonStatus {
                state: TcpDaemonState::Error,
                pid: child.id(),
                started_at_ms: runtime.status.started_at_ms,
                last_error: Some(format!("Failed to inspect daemon process: {err}")),
                listen_addr: runtime.status.listen_addr.clone(),
            };
        }
    }
}

pub(crate) fn parse_port_from_remote_host(remote_host: &str) -> Option<u16> {
    if remote_host.trim().is_empty() {
        return None;
    }
    if let Ok(addr) = remote_host.trim().parse::<std::net::SocketAddr>() {
        return Some(addr.port());
    }
    remote_host
        .trim()
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

pub(crate) fn daemon_listen_addr(remote_host: &str) -> String {
    let port = parse_port_from_remote_host(remote_host).unwrap_or(4732);
    format!("0.0.0.0:{port}")
}

fn daemon_connect_addr(listen_addr: &str) -> Option<String> {
    let port = parse_port_from_remote_host(listen_addr)?;
    Some(format!("127.0.0.1:{port}"))
}

fn configured_daemon_listen_addr(settings: &AppSettings) -> String {
    daemon_listen_addr(&active_remote_backend(settings).host)
}

pub(crate) fn sync_tcp_daemon_listen_addr(status: &mut TcpDaemonStatus, configured: &str) {
    if matches!(status.state, TcpDaemonState::Running) && status.listen_addr.is_some() {
        return;
    }
    status.listen_addr = Some(configured.to_string());
}

pub(crate) async fn find_listener_pid(port: u16) -> Option<u32> {
    #[cfg(unix)]
    {
        let target = format!(":{port}");
        let output = match tokio_command("lsof")
            .args(["-nP", "-iTCP"])
            .arg(&target)
            .args(["-sTCP:LISTEN", "-t"])
            .output()
            .await
        {
            Ok(output) => output,
            Err(error) if error.kind() == ErrorKind::NotFound => return None,
            Err(_) => return None,
        };
        if !output.status.success() {
            return None;
        }
        return String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|line| line.trim().parse::<u32>().ok());
    }
    #[cfg(not(unix))]
    {
        let _ = port;
        None
    }
}

#[cfg(unix)]
fn is_pid_running(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(unix)]
async fn kill_pid_gracefully(pid: u32) -> Result<(), String> {
    if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!("Failed to stop daemon process {pid}: {error}"));
        }
        return Ok(());
    }
    for _ in 0..12 {
        if !is_pid_running(pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    if unsafe { libc::kill(pid as i32, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(format!(
                "Failed to force-stop daemon process {pid}: {error}"
            ));
        }
    }
    for _ in 0..8 {
        if !is_pid_running(pid) {
            return Ok(());
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(format!("Daemon process {pid} is still running."))
}

#[cfg(not(unix))]
async fn kill_pid_gracefully(_pid: u32) -> Result<(), String> {
    Err("Stopping external daemon by pid is not supported on this platform.".to_string())
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0)
}

fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    if cfg!(windows) {
        format!("\"{}\"", value.replace('"', "\\\""))
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn trim_to_non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
}

fn truncate_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn tailscale_binary_candidates() -> Vec<OsString> {
    let mut candidates = vec![OsString::from("tailscale")];
    #[cfg(target_os = "macos")]
    candidates.extend([
        OsString::from("/opt/homebrew/bin/tailscale"),
        OsString::from("/usr/local/bin/tailscale"),
        OsString::from("/usr/local/bin/Tailscale"),
        OsString::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
        OsString::from("/Applications/Tailscale.app/Contents/MacOS/tailscale"),
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend([
        OsString::from("/usr/bin/tailscale"),
        OsString::from("/usr/sbin/tailscale"),
        OsString::from("/usr/local/bin/tailscale"),
        OsString::from("/run/current-system/sw/bin/tailscale"),
        OsString::from("/snap/bin/tailscale"),
    ]);
    #[cfg(target_os = "windows")]
    candidates.extend([
        OsString::from("C:\\Program Files\\Tailscale\\tailscale.exe"),
        OsString::from("C:\\Program Files (x86)\\Tailscale\\tailscale.exe"),
    ]);
    candidates
}

fn missing_tailscale_message() -> String {
    #[cfg(target_os = "macos")]
    {
        "Tailscale CLI not found on PATH or standard install paths (including /Applications/Tailscale.app/Contents/MacOS/Tailscale).".to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        "Tailscale CLI not found on PATH or standard install paths.".to_string()
    }
}

fn looks_like_tailscale_version(stdout: &str) -> bool {
    stdout
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | '(' | ')' | ';'))
        .any(|token| {
            let trimmed = token.trim().trim_start_matches('v');
            let core = trimmed
                .split_once('-')
                .map(|(value, _)| value)
                .unwrap_or(trimmed);
            let mut count = 0usize;
            for part in core.split('.') {
                if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
                    return false;
                }
                count += 1;
            }
            count >= 2
        })
}

#[cfg(target_os = "macos")]
fn apply_tailscale_command_env(command: &mut tokio::process::Command) {
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".to_string());
    command.env("TERM", term);
}
#[cfg(not(target_os = "macos"))]
fn apply_tailscale_command_env(_command: &mut tokio::process::Command) {}

fn direct_tailscale_command(binary: &OsStr) -> tokio::process::Command {
    let mut command = tokio_command(binary);
    apply_tailscale_command_env(&mut command);
    command
}

#[cfg(target_os = "macos")]
fn tailscale_command(binary: &OsStr) -> tokio::process::Command {
    let mut command = tokio_command("/bin/launchctl");
    let uid = unsafe { libc::geteuid() };
    command.arg("asuser").arg(uid.to_string()).arg(binary);
    apply_tailscale_command_env(&mut command);
    command
}
#[cfg(not(target_os = "macos"))]
fn tailscale_command(binary: &OsStr) -> tokio::process::Command {
    direct_tailscale_command(binary)
}

#[cfg(target_os = "macos")]
async fn tailscale_output(binary: &OsStr, args: &[&str]) -> std::io::Result<Output> {
    let primary = tailscale_command(binary).args(args).output().await;
    match primary {
        Ok(output) if output.status.success() => Ok(output),
        Ok(output) => match direct_tailscale_command(binary).args(args).output().await {
            Ok(fallback) if fallback.status.success() => Ok(fallback),
            _ => Ok(output),
        },
        Err(primary_error) => match direct_tailscale_command(binary).args(args).output().await {
            Ok(fallback) => Ok(fallback),
            Err(_) => Err(primary_error),
        },
    }
}
#[cfg(not(target_os = "macos"))]
async fn tailscale_output(binary: &OsStr, args: &[&str]) -> std::io::Result<Output> {
    tailscale_command(binary).args(args).output().await
}

async fn resolve_tailscale_binary() -> Result<Option<(OsString, Output)>, String> {
    let mut failures = Vec::new();
    for binary in tailscale_binary_candidates() {
        match tailscale_output(binary.as_os_str(), &["version"]).await {
            Ok(version_output) => {
                let stdout = trim_to_non_empty(std::str::from_utf8(&version_output.stdout).ok());
                let stderr = trim_to_non_empty(std::str::from_utf8(&version_output.stderr).ok());
                if version_output.status.success()
                    && stdout.as_deref().is_some_and(looks_like_tailscale_version)
                {
                    return Ok(Some((binary, version_output)));
                }
                let detail = match (stdout, stderr) {
                    (Some(out), Some(err)) => format!("stdout: {out}; stderr: {err}"),
                    (Some(out), None) => format!("stdout: {out}"),
                    (None, Some(err)) => format!("stderr: {err}"),
                    (None, None) => "no output".to_string(),
                };
                failures.push(format!(
                    "{}: tailscale version failed or returned unexpected output ({detail})",
                    binary.to_string_lossy()
                ));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => failures.push(format!("{}: {error}", binary.to_string_lossy())),
        }
    }
    if failures.is_empty() {
        Ok(None)
    } else {
        Err(format!(
            "Failed to run tailscale version from candidate paths: {}",
            failures.join(" | ")
        ))
    }
}

fn degraded_tailscale_status(version: Option<String>, message: String) -> TailscaleStatus {
    TailscaleStatus {
        installed: true,
        running: false,
        version,
        dns_name: None,
        host_name: None,
        tailnet_name: None,
        ipv4: Vec::new(),
        ipv6: Vec::new(),
        suggested_remote_host: None,
        message,
    }
}

pub(crate) async fn tailscale_status() -> Result<TailscaleStatus, String> {
    let resolved = match resolve_tailscale_binary().await {
        Ok(result) => result,
        Err(error) => return Ok(degraded_tailscale_status(None, error)),
    };
    let Some((binary, version_output)) = resolved else {
        return Ok(tailscale_projection::unavailable_status(
            None,
            missing_tailscale_message(),
        ));
    };
    let version = trim_to_non_empty(std::str::from_utf8(&version_output.stdout).ok())
        .and_then(|raw| raw.lines().next().map(str::trim).map(str::to_string));
    let status_output = match tailscale_output(binary.as_os_str(), &["status", "--json"]).await {
        Ok(output) => output,
        Err(error) => {
            return Ok(degraded_tailscale_status(
                version,
                format!("Failed to run tailscale status --json: {error}"),
            ))
        }
    };
    if !status_output.status.success() {
        let message = trim_to_non_empty(std::str::from_utf8(&status_output.stderr).ok())
            .unwrap_or_else(|| "tailscale status returned a non-zero exit code.".to_string());
        return Ok(TailscaleStatus {
            installed: true,
            running: false,
            version,
            dns_name: None,
            host_name: None,
            tailnet_name: None,
            ipv4: Vec::new(),
            ipv6: Vec::new(),
            suggested_remote_host: None,
            message,
        });
    }
    let payload = match std::str::from_utf8(&status_output.stdout) {
        Ok(value) => value,
        Err(error) => {
            return Ok(degraded_tailscale_status(
                version,
                format!("Invalid UTF-8 from tailscale status: {error}"),
            ))
        }
    };
    let stderr = trim_to_non_empty(std::str::from_utf8(&status_output.stderr).ok());
    if payload.trim().is_empty() {
        let suffix = stderr
            .as_deref()
            .map(|value| format!(" stderr: {value}"))
            .unwrap_or_default();
        return Ok(degraded_tailscale_status(
            version,
            format!("tailscale status --json returned empty output.{suffix}"),
        ));
    }
    match tailscale_projection::status_from_json(version.clone(), payload) {
        Ok(status) => Ok(status),
        Err(error) => {
            let preview =
                (!payload.trim().is_empty()).then(|| truncate_preview(payload.trim(), 200));
            let mut details = Vec::new();
            if let Some(stderr) = stderr {
                details.push(format!("stderr: {stderr}"));
            }
            if let Some(preview) = preview {
                details.push(format!("stdout: {preview}"));
            }
            if details.is_empty() {
                Ok(degraded_tailscale_status(version, error))
            } else {
                Ok(degraded_tailscale_status(
                    version,
                    format!("{error} ({})", details.join("; ")),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon_info(version: &str) -> rpc_client::DaemonInfo {
        rpc_client::DaemonInfo {
            name: EXPECTED_DAEMON_NAME.to_string(),
            version: version.to_string(),
            pid: Some(42),
            mode: EXPECTED_DAEMON_MODE.to_string(),
            binary_path: Some("/tmp/codex-monitor-daemon".to_string()),
        }
    }

    #[test]
    fn restart_rules_require_exact_identity_version_and_tcp_mode() {
        let current = daemon_info(CURRENT_APP_VERSION);
        assert!(!should_restart_daemon(Some(&current)));
        assert!(should_restart_daemon(Some(&daemon_info("0.1.0"))));
        let mut wrong_name = current.clone();
        wrong_name.name = "other".into();
        assert!(should_restart_daemon(Some(&wrong_name)));
        let mut wrong_mode = current;
        wrong_mode.mode = "stdio".into();
        assert!(should_restart_daemon(Some(&wrong_mode)));
    }

    #[test]
    fn force_stop_requires_authenticated_managed_identity() {
        let mut info = daemon_info(CURRENT_APP_VERSION);
        assert!(can_force_stop_daemon(true, Some(&info)));
        assert!(!can_force_stop_daemon(false, Some(&info)));
        info.name = "unknown-daemon".into();
        assert!(!can_force_stop_daemon(true, Some(&info)));
        assert!(!can_force_stop_daemon(true, None));
    }

    #[test]
    fn exit_shutdown_only_stops_an_owned_child_when_keep_is_disabled() {
        let mut settings = AppSettings::default();
        assert!(should_stop_owned_daemon_for_exit(&settings, true));
        assert!(!should_stop_owned_daemon_for_exit(&settings, false));
        settings.keep_daemon_running_after_app_close = true;
        assert!(!should_stop_owned_daemon_for_exit(&settings, true));
    }

    #[cfg(unix)]
    #[test]
    fn exit_shutdown_obeys_ownership_keep_setting_and_reaps_owned_children() {
        use tokio::process::Command;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
            let address = listener.local_addr().expect("listener address").to_string();
            let mut settings = AppSettings::default();
            settings.remote_backends[0].host = address.clone();
            let mut state = ManagedDaemonRuntime::default();
            state.status.listen_addr = Some(address);

            let status = shutdown_for_exit(&settings, &mut state)
                .await
                .expect("exit with no owned process");
            assert_eq!(status.state, TcpDaemonState::Stopped);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), listener.accept())
                    .await
                    .is_err(),
                "exit cleanup must not probe an unrelated listener"
            );

            let mut owned = Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .expect("owned test child");
            state.child = Some(owned);
            state.status.state = TcpDaemonState::Running;
            settings.keep_daemon_running_after_app_close = true;
            shutdown_for_exit(&settings, &mut state)
                .await
                .expect("keep preference preserves owned daemon");
            assert!(state
                .child
                .as_mut()
                .expect("child retained")
                .try_wait()
                .unwrap()
                .is_none());
            let mut child = state.child.take().expect("cleanup child");
            kill_child_process_tree(&mut child).await;
            let _ = child.wait().await;

            settings.keep_daemon_running_after_app_close = false;
            let child = Command::new("sh")
                .args(["-c", "exec sleep 30"])
                .spawn()
                .expect("owned test child");
            state.child = Some(child);
            state.status.state = TcpDaemonState::Running;
            let status = shutdown_for_exit(&settings, &mut state)
                .await
                .expect("exit stops owned daemon");
            assert!(
                state.child.is_none(),
                "owned child must be waited and released"
            );
            assert_eq!(status.state, TcpDaemonState::Stopped);
        });
    }

    #[test]
    fn tailscale_candidates_start_with_path_lookup() {
        let candidates = tailscale_binary_candidates();
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].to_string_lossy(), "tailscale");
        #[cfg(target_os = "macos")]
        {
            let usr_local = candidates
                .iter()
                .position(|candidate| candidate == "/usr/local/bin/tailscale")
                .unwrap();
            let app_bundle = candidates
                .iter()
                .position(|candidate| {
                    candidate == "/Applications/Tailscale.app/Contents/MacOS/Tailscale"
                })
                .unwrap();
            assert!(usr_local < app_bundle);
        }
    }

    #[test]
    fn tailscale_version_output_is_validated_as_a_version() {
        assert!(looks_like_tailscale_version(
            "1.94.2\n  tailscale commit: 0a29cf18\n"
        ));
        assert!(!looks_like_tailscale_version(
            "The Tailscale GUI failed to start: Tailscale.CLIError error 3."
        ));
        assert!(!looks_like_tailscale_version(" \n\t "));
    }

    #[test]
    fn truncates_tailscale_error_preview_without_utf8_boundary_panics() {
        let sample = "é".repeat(300);
        let preview = truncate_preview(&sample, 200);
        assert_eq!(preview.chars().count(), 201);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn daemon_address_helpers_keep_legacy_fallback_and_ipv6_behavior() {
        assert_eq!(
            parse_port_from_remote_host("100.100.100.1:4732"),
            Some(4732)
        );
        assert_eq!(
            parse_port_from_remote_host("[fd7a:115c:a1e0::1]:4545"),
            Some(4545)
        );
        assert_eq!(parse_port_from_remote_host("example.ts.net"), None);
        assert_eq!(
            daemon_listen_addr("mac.example.ts.net:8888"),
            "0.0.0.0:8888"
        );
        assert_eq!(daemon_listen_addr("mac.example.ts.net"), "0.0.0.0:4732");
    }

    #[test]
    fn daemon_listen_address_sync_preserves_a_running_address() {
        let mut status = TcpDaemonStatus {
            state: TcpDaemonState::Stopped,
            pid: None,
            started_at_ms: None,
            last_error: None,
            listen_addr: Some("0.0.0.0:4732".to_string()),
        };
        sync_tcp_daemon_listen_addr(&mut status, "0.0.0.0:7777");
        assert_eq!(status.listen_addr.as_deref(), Some("0.0.0.0:7777"));
        status.state = TcpDaemonState::Running;
        status.listen_addr = Some("0.0.0.0:4732".to_string());
        sync_tcp_daemon_listen_addr(&mut status, "0.0.0.0:7777");
        assert_eq!(status.listen_addr.as_deref(), Some("0.0.0.0:4732"));
    }

    #[test]
    fn listen_address_preflight_rejects_an_occupied_port() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let occupied = listener.local_addr().expect("local addr").to_string();
            let error = ensure_listen_addr_available(&occupied)
                .await
                .expect_err("occupied port must fail preflight");
            assert!(error.contains("unavailable"));
        });
    }

    #[test]
    fn shell_quote_keeps_spaces_and_quotes() {
        assert_eq!(shell_quote("/path/with spaces/x"), "'/path/with spaces/x'");
        assert_eq!(shell_quote("a'b"), "'a'\"'\"'b'");
    }
}
