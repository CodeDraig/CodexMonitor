use crate::{
    remote_backend,
    shared::{runs_core, runs_rpc},
    state::AppState,
};
use serde_json::Value;
use tauri::{AppHandle, State};

macro_rules! command {
    ($name:ident) => {
        #[tauri::command]
        pub(crate) async fn $name(input: Value, state: State<'_, AppState>, app: AppHandle) -> Result<Value,String> {
            if remote_backend::is_remote_mode(&state).await {
                return remote_backend::call_remote(&state,app,stringify!($name),serde_json::json!({"input":input})).await;
            }
            runs_rpc::dispatch(stringify!($name),input,runs_core::db_path(&state.storage_path),&state.workspaces,&state.sessions).await
        }
    }
}
command!(create_run);
command!(list_runs);
command!(get_run);
command!(update_run_metadata);
command!(record_run_setup);
command!(record_run_failure);
command!(reconcile_run);
command!(capture_run_instance);
command!(get_run_capture);
command!(save_run_comparison);
command!(list_run_comparisons);

command!(list_run_turns);
