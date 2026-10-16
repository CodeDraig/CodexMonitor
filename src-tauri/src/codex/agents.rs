use crate::backend::events::AppServerEvent;
use crate::remote_backend;
use crate::shared::agents_config_core;
use crate::state::AppState;
use serde_json::json;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::State;

#[tauri::command]
pub(crate) async fn get_agents_settings(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<agents_config_core::AgentsSettingsDto, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response =
            remote_backend::call_remote(&*state, app, "get_agents_settings", json!({})).await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::get_agents_settings_core()
}

#[tauri::command]
pub(crate) async fn set_agents_core_settings(
    input: agents_config_core::SetAgentsCoreInput,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<agents_config_core::AgentsSettingsDto, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response = remote_backend::call_remote(
            &*state,
            app,
            "set_agents_core_settings",
            json!({ "input": input }),
        )
        .await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::set_agents_core_settings_core(input)
}

#[tauri::command]
pub(crate) async fn create_agent(
    input: agents_config_core::CreateAgentInput,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<agents_config_core::AgentsSettingsDto, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response =
            remote_backend::call_remote(&*state, app, "create_agent", json!({ "input": input }))
                .await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::create_agent_core(input)
}

#[tauri::command]
pub(crate) async fn update_agent(
    input: agents_config_core::UpdateAgentInput,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<agents_config_core::AgentsSettingsDto, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response =
            remote_backend::call_remote(&*state, app, "update_agent", json!({ "input": input }))
                .await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::update_agent_core(input)
}

#[tauri::command]
pub(crate) async fn delete_agent(
    input: agents_config_core::DeleteAgentInput,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<agents_config_core::AgentsSettingsDto, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response =
            remote_backend::call_remote(&*state, app, "delete_agent", json!({ "input": input }))
                .await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::delete_agent_core(input)
}

#[tauri::command]
pub(crate) async fn read_agent_config_toml(
    agent_name: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let response = remote_backend::call_remote(
            &*state,
            app,
            "read_agent_config_toml",
            json!({ "agentName": agent_name }),
        )
        .await?;
        return serde_json::from_value(response).map_err(|err| err.to_string());
    }

    agents_config_core::read_agent_config_toml_core(agent_name.as_str())
}

#[tauri::command]
pub(crate) async fn write_agent_config_toml(
    agent_name: String,
    content: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<(), String> {
    if remote_backend::is_remote_mode(&*state).await {
        remote_backend::call_remote(
            &*state,
            app,
            "write_agent_config_toml",
            json!({
                "agentName": agent_name,
                "content": content,
            }),
        )
        .await?;
        return Ok(());
    }

    agents_config_core::write_agent_config_toml_core(agent_name.as_str(), content.as_str())
}

#[tauri::command]
pub(crate) async fn generate_agent_description(
    workspace_id: String,
    description: String,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<crate::shared::codex_aux_core::GeneratedAgentConfiguration, String> {
    if remote_backend::is_remote_mode(&*state).await {
        let value = remote_backend::call_remote(
            &*state,
            app,
            "generate_agent_description",
            json!({ "workspaceId": workspace_id, "description": description }),
        )
        .await?;
        return serde_json::from_value(value).map_err(|err| err.to_string());
    }

    crate::shared::codex_aux_core::generate_agent_description_core(
        &state.sessions,
        &state.workspaces,
        workspace_id,
        &description,
        |workspace_id, thread_id| {
            let _ = app.emit(
                "app-server-event",
                AppServerEvent {
                    workspace_id: workspace_id.to_string(),
                    message: json!({
                        "method": "codex/backgroundThread",
                        "params": {
                            "threadId": thread_id,
                            "action": "hide"
                        }
                    }),
                },
            );
        },
    )
    .await
}
