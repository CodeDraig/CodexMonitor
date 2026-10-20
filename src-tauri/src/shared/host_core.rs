use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::backend::app_server::session::WorkspaceSession;
use crate::shared::agent_runtime::RuntimeRegistry;
use crate::shared::codex_core::login::CodexLoginCancelState;
use crate::storage::{read_settings, read_workspaces};
use crate::types::{AppSettings, WorkspaceEntry};

/// Files used by both the desktop host and the managed daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostPaths {
    pub(crate) data_dir: PathBuf,
    pub(crate) settings_path: PathBuf,
    pub(crate) workspaces_path: PathBuf,
    pub(crate) run_store_path: PathBuf,
}

impl HostPaths {
    /// Resolve an explicit data directory or the existing desktop application data location.
    ///
    /// On Linux/macOS/Windows this matches Tauri's app-data convention: the platform's data
    /// directory followed by the existing application identifier. An invalid or
    /// uncreatable directory is reported instead of falling back to a different store.
    pub(crate) fn resolve(explicit_data_dir: Option<PathBuf>) -> Result<Self, String> {
        let data_dir = match explicit_data_dir {
            Some(path) => path,
            None => dirs::data_dir()
                .ok_or_else(|| {
                    "could not resolve the platform application data directory".to_string()
                })?
                .join("com.dimillian.codexmonitor"),
        };
        Self::from_data_dir(data_dir)
    }

    pub(crate) fn from_data_dir(data_dir: PathBuf) -> Result<Self, String> {
        std::fs::create_dir_all(&data_dir).map_err(|error| {
            format!(
                "could not create data directory {}: {error}",
                data_dir.display()
            )
        })?;
        if !data_dir.is_dir() {
            return Err(format!(
                "data path is not a directory: {}",
                data_dir.display()
            ));
        }
        let data_dir = normalize_existing_parent(&data_dir)?;
        Ok(Self {
            settings_path: data_dir.join("settings.json"),
            workspaces_path: data_dir.join("workspaces.json"),
            run_store_path: data_dir.join("runs.sqlite3"),
            data_dir,
        })
    }
}

fn normalize_existing_parent(path: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| {
        format!(
            "could not resolve data directory {}: {error}",
            path.display()
        )
    })
}

/// Process state shared by the native desktop host and daemon host.
pub(crate) struct CoreState {
    pub(crate) runtime_registry: RuntimeRegistry,
    pub(crate) workspaces: Mutex<HashMap<String, WorkspaceEntry>>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<WorkspaceSession>>>,
    /// Compatibility alias for the historical workspaces.json path.
    pub(crate) storage_path: PathBuf,
    pub(crate) settings_path: PathBuf,
    pub(crate) app_settings: Mutex<AppSettings>,
    pub(crate) codex_login_cancels: Mutex<HashMap<String, CodexLoginCancelState>>,
}

impl CoreState {
    pub(crate) fn load(paths: HostPaths) -> Result<Self, String> {
        let workspaces = read_workspaces(&paths.workspaces_path)?;
        let app_settings = read_settings(&paths.settings_path)?;
        Ok(Self {
            runtime_registry: RuntimeRegistry::default(),
            workspaces: Mutex::new(workspaces),
            sessions: Mutex::new(HashMap::new()),
            storage_path: paths.workspaces_path,
            settings_path: paths.settings_path,
            app_settings: Mutex::new(app_settings),
            codex_login_cancels: Mutex::new(HashMap::new()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codex-monitor-host-core-{label}-{}",
            uuid::Uuid::new_v4()
        ))
    }

    #[test]
    fn explicit_paths_are_used_for_all_store_files() {
        let data_dir = temp_path("explicit");
        let paths = HostPaths::resolve(Some(data_dir.clone())).unwrap();
        assert_eq!(paths.data_dir, data_dir.canonicalize().unwrap());
        assert_eq!(paths.settings_path, paths.data_dir.join("settings.json"));
        assert_eq!(
            paths.workspaces_path,
            paths.data_dir.join("workspaces.json")
        );
        assert_eq!(paths.run_store_path, paths.data_dir.join("runs.sqlite3"));
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[test]
    fn a_file_cannot_be_used_as_the_data_directory() {
        let file = temp_path("not-a-directory");
        std::fs::write(&file, b"keep").unwrap();
        assert!(HostPaths::resolve(Some(file.clone())).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"keep");
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn core_loads_the_existing_json_store_paths() {
        let paths = HostPaths::resolve(Some(temp_path("core-load"))).unwrap();
        let expected_workspaces = paths.workspaces_path.clone();
        let expected_settings = paths.settings_path.clone();
        let core = CoreState::load(paths).unwrap();
        assert_eq!(core.storage_path, expected_workspaces);
        assert_eq!(core.settings_path, expected_settings);
        assert!(core.workspaces.try_lock().unwrap().is_empty());
    }
}
