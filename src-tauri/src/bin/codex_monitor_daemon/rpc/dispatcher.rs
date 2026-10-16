use super::*;

pub(super) async fn dispatch_rpc_request(
    state: &DaemonState,
    method: &str,
    params: &Value,
    client_version: &str,
) -> Result<Value, String> {
    if crate::shared::runs_rpc::METHODS.contains(&method) {
        return crate::shared::runs_rpc::dispatch(
            method,
            params.get("input").cloned().unwrap_or(Value::Null),
            crate::shared::runs_core::db_path(&state.storage_path),
            &state.workspaces,
            &state.sessions,
        )
        .await;
    }
    if let Some(result) = daemon::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = workspace::try_handle(state, method, params, client_version).await {
        return result;
    }

    if let Some(result) = files::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = settings::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = usage::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = catalog::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = threads::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = turns::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = agents::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = account::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = approvals::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = metadata::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = git::try_handle(state, method, params).await {
        return result;
    }

    if let Some(result) = prompts::try_handle(state, method, params).await {
        return result;
    }

    Err(format!("unknown method: {method}"))
}
