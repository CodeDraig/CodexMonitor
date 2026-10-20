#[cfg(feature = "tauri-host")]
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
#[cfg(feature = "tauri-host")]
use std::sync::Arc;
#[cfg(feature = "tauri-host")]
use tokio::sync::Mutex;

#[cfg(feature = "tauri-host")]
use crate::dictation::DictationState;
use crate::shared::host_core::{CoreState, HostPaths};
#[cfg(feature = "tauri-host")]
use tauri::{AppHandle, Manager};

#[cfg(feature = "tauri-host")]
pub(crate) use crate::shared::managed_daemon_core::ManagedDaemonRuntime as TcpDaemonRuntime;

#[cfg(feature = "tauri-host")]
pub(crate) struct AppState {
    core: CoreState,
    #[cfg(feature = "tauri-host")]
    pub(crate) terminal_sessions: Mutex<HashMap<String, Arc<crate::terminal::TerminalSession>>>,
    #[cfg(feature = "tauri-host")]
    pub(crate) remote_backend: Mutex<Option<crate::remote_backend::RemoteBackend>>,
    #[cfg(feature = "tauri-host")]
    pub(crate) dictation: Mutex<DictationState>,
    #[cfg(feature = "tauri-host")]
    pub(crate) tcp_daemon: Mutex<TcpDaemonRuntime>,
}

#[cfg(feature = "tauri-host")]
impl Deref for AppState {
    type Target = CoreState;

    fn deref(&self) -> &Self::Target {
        &self.core
    }
}

#[cfg(feature = "tauri-host")]
impl DerefMut for AppState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.core
    }
}

#[cfg(feature = "tauri-host")]
impl AppState {
    #[cfg(feature = "tauri-host")]
    pub(crate) fn load(app: &AppHandle) -> Result<Self, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| format!("could not resolve CodexMonitor data directory: {error}"))?;
        let paths = HostPaths::from_data_dir(data_dir)?;
        let core = CoreState::load(paths)?;
        Ok(Self {
            core,
            terminal_sessions: Mutex::new(HashMap::new()),
            remote_backend: Mutex::new(None),
            dictation: Mutex::new(DictationState::default()),
            tcp_daemon: Mutex::new(TcpDaemonRuntime::default()),
        })
    }
}
