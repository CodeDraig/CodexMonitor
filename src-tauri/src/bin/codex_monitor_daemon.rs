use crate::backend::app_server::session::WorkspaceSession;
use crate::events::DaemonEvent;
use crate::events::DaemonEventSink;
use crate::shared::codex_core::login::CodexLoginCancelState;
use crate::storage::read_settings;
use crate::storage::read_workspaces;
use crate::types::AppSettings;
use crate::types::WorkspaceEntry;
use config::{parse_args, usage, DaemonConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

#[allow(dead_code)]
#[path = "../backend/mod.rs"]
mod backend;
#[path = "../codex/args.rs"]
mod codex_args;
#[path = "../codex/config.rs"]
mod codex_config;
#[path = "../codex/home.rs"]
mod codex_home;
#[path = "codex_monitor_daemon/config.rs"]
mod config;
#[path = "codex_monitor_daemon/events.rs"]
mod events;
#[path = "../files/io.rs"]
mod file_io;
#[path = "../files/ops.rs"]
mod file_ops;
#[path = "../files/policy.rs"]
mod file_policy;
#[path = "../git_utils.rs"]
mod git_utils;
#[path = "codex_monitor_daemon/rpc.rs"]
mod rpc;
#[path = "../rules.rs"]
mod rules;
#[path = "../shared/mod.rs"]
mod shared;
#[path = "../storage.rs"]
mod storage;
#[path = "codex_monitor_daemon/transport.rs"]
mod transport;
#[allow(dead_code)]
#[path = "../types.rs"]
mod types;
#[path = "../utils.rs"]
mod utils;
#[path = "../workspaces/macos.rs"]
mod workspace_macos;
#[path = "../workspaces/settings.rs"]
mod workspace_settings;

// Provide feature-style module paths for shared cores when compiled in the daemon.
mod codex {
    pub(crate) mod args {
        pub(crate) use crate::codex_args::*;
    }
    pub(crate) mod config {
        pub(crate) use crate::codex_config::*;
    }
    pub(crate) mod home {
        pub(crate) use crate::codex_home::*;
    }
}

mod files {
    pub(crate) mod io {
        pub(crate) use crate::file_io::*;
    }
    pub(crate) mod ops {
        pub(crate) use crate::file_ops::*;
    }
    pub(crate) mod policy {
        pub(crate) use crate::file_policy::*;
    }
}

const MAX_IN_FLIGHT_RPC_PER_CONNECTION: usize = 32;
const DAEMON_NAME: &str = "codex-monitor-daemon";

struct DaemonState {
    runtime_registry: shared::agent_runtime::RuntimeRegistry,
    data_dir: PathBuf,
    workspaces: Mutex<HashMap<String, WorkspaceEntry>>,
    sessions: Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    storage_path: PathBuf,
    settings_path: PathBuf,
    app_settings: Mutex<AppSettings>,
    event_sink: DaemonEventSink,
    codex_login_cancels: Mutex<HashMap<String, CodexLoginCancelState>>,
    daemon_binary_path: Option<String>,
}

impl DaemonState {
    fn load(config: &DaemonConfig, event_sink: DaemonEventSink) -> Self {
        let storage_path = config.data_dir.join("workspaces.json");
        let settings_path = config.data_dir.join("settings.json");
        let workspaces = read_workspaces(&storage_path).unwrap_or_default();
        let app_settings = read_settings(&settings_path).unwrap_or_default();
        let daemon_binary_path = std::env::current_exe()
            .ok()
            .and_then(|path| path.to_str().map(str::to_string));
        Self {
            runtime_registry: Default::default(),
            data_dir: config.data_dir.clone(),
            workspaces: Mutex::new(workspaces),
            sessions: Mutex::new(HashMap::new()),
            storage_path,
            settings_path,
            app_settings: Mutex::new(app_settings),
            event_sink,
            codex_login_cancels: Mutex::new(HashMap::new()),
            daemon_binary_path,
        }
    }
}

#[cfg(test)]
#[path = "codex_monitor_daemon/tests.rs"]
mod tests;

fn main() {
    let config = match parse_args() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}\n\n{}", usage());
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    runtime.block_on(async move {
        let (events_tx, _events_rx) = broadcast::channel::<DaemonEvent>(2048);
        let event_sink = DaemonEventSink {
            run_store_path: config.data_dir.join("runs.sqlite3"),
            tx: events_tx.clone(),
        };
        let state = Arc::new(DaemonState::load(&config, event_sink));
        let config = Arc::new(config);

        let listener = match TcpListener::bind(config.listen).await {
            Ok(listener) => listener,
            Err(err) => {
                eprintln!("failed to bind {}: {err}", config.listen);
                std::process::exit(2);
            }
        };
        eprintln!(
            "codex-monitor-daemon listening on {} (data dir: {})",
            config.listen,
            state
                .storage_path
                .parent()
                .unwrap_or(&state.storage_path)
                .display()
        );

        loop {
            match listener.accept().await {
                Ok((socket, _addr)) => {
                    let config = Arc::clone(&config);
                    let state = Arc::clone(&state);
                    let events = events_tx.clone();
                    tokio::spawn(async move {
                        transport::handle_client(socket, config, state, events).await;
                    });
                }
                Err(_) => continue,
            }
        }
    });
}
