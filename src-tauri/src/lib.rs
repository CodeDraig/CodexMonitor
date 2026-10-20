//! Native desktop and host-independent backend for CodexMonitor.

mod backend;
mod codex;
#[cfg(feature = "native-gui")]
mod daemon_binary;
mod files;
mod git_utils;
mod rules;
mod shared;
mod storage;
mod types;
mod utils;
mod workspaces;
pub(crate) mod client;
#[cfg(feature = "native-gui")]
pub(crate) mod desktop;

/// Start the native eframe application using an optional explicit data directory.
#[cfg(feature = "native-gui")]
pub fn run_native(data_dir: Option<std::path::PathBuf>) -> Result<(), String> {
    let result = desktop::run(data_dir);
    if let Err(error) = &result {
        desktop::platform::show_startup_error_dialog("Codex Monitor could not start", error);
    }
    result
}
