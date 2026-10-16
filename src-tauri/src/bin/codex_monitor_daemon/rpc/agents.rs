use super::parse_string;
use crate::events::emit_background_thread_hide;
use crate::shared::agents_config_core;
use crate::shared::codex_aux_core;
use crate::DaemonState;
use serde::de::DeserializeOwned;
use serde_json::json;
use serde_json::Value;

fn parse_input<T: DeserializeOwned>(params: &Value) -> Result<T, String> {
    let input_value = params
        .as_object()
        .and_then(|map| map.get("input"))
        .cloned()
        .ok_or_else(|| "missing `input`".to_string())?;
    serde_json::from_value(input_value).map_err(|err| err.to_string())
}

pub(super) async fn try_handle(
    state: &DaemonState,
    method: &str,
    params: &Value,
) -> Option<Result<Value, String>> {
    match method {
        "get_agents_settings" => Some(
            state
                .get_agents_settings()
                .await
                .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
        ),
        "set_agents_core_settings" => {
            let input = match parse_input::<agents_config_core::SetAgentsCoreInput>(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .set_agents_core_settings(input)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        "create_agent" => {
            let input = match parse_input::<agents_config_core::CreateAgentInput>(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .create_agent(input)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        "update_agent" => {
            let input = match parse_input::<agents_config_core::UpdateAgentInput>(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .update_agent(input)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        "delete_agent" => {
            let input = match parse_input::<agents_config_core::DeleteAgentInput>(params) {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .delete_agent(input)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        "read_agent_config_toml" => {
            let agent_name = match parse_string(params, "agentName") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .read_agent_config_toml(agent_name)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        "write_agent_config_toml" => {
            let agent_name = match parse_string(params, "agentName") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let content = match parse_string(params, "content") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .write_agent_config_toml(agent_name, content)
                    .await
                    .map(|_| json!({ "ok": true })),
            )
        }
        "generate_agent_description" => {
            let workspace_id = match parse_string(params, "workspaceId") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            let description = match parse_string(params, "description") {
                Ok(value) => value,
                Err(err) => return Some(Err(err)),
            };
            Some(
                state
                    .generate_agent_description(workspace_id, description)
                    .await
                    .and_then(|value| serde_json::to_value(value).map_err(|err| err.to_string())),
            )
        }
        _ => None,
    }
}

impl DaemonState {
    pub(crate) async fn get_agents_settings(
        &self,
    ) -> Result<agents_config_core::AgentsSettingsDto, String> {
        agents_config_core::get_agents_settings_core()
    }

    pub(crate) async fn set_agents_core_settings(
        &self,
        input: agents_config_core::SetAgentsCoreInput,
    ) -> Result<agents_config_core::AgentsSettingsDto, String> {
        agents_config_core::set_agents_core_settings_core(input)
    }

    pub(crate) async fn create_agent(
        &self,
        input: agents_config_core::CreateAgentInput,
    ) -> Result<agents_config_core::AgentsSettingsDto, String> {
        agents_config_core::create_agent_core(input)
    }

    pub(crate) async fn update_agent(
        &self,
        input: agents_config_core::UpdateAgentInput,
    ) -> Result<agents_config_core::AgentsSettingsDto, String> {
        agents_config_core::update_agent_core(input)
    }

    pub(crate) async fn delete_agent(
        &self,
        input: agents_config_core::DeleteAgentInput,
    ) -> Result<agents_config_core::AgentsSettingsDto, String> {
        agents_config_core::delete_agent_core(input)
    }

    pub(crate) async fn read_agent_config_toml(
        &self,
        agent_name: String,
    ) -> Result<String, String> {
        agents_config_core::read_agent_config_toml_core(agent_name.as_str())
    }

    pub(crate) async fn write_agent_config_toml(
        &self,
        agent_name: String,
        content: String,
    ) -> Result<(), String> {
        agents_config_core::write_agent_config_toml_core(agent_name.as_str(), content.as_str())
    }

    pub(crate) async fn generate_agent_description(
        &self,
        workspace_id: String,
        description: String,
    ) -> Result<codex_aux_core::GeneratedAgentConfiguration, String> {
        codex_aux_core::generate_agent_description_core(
            &self.sessions,
            &self.workspaces,
            workspace_id,
            &description,
            |workspace_id, thread_id| {
                emit_background_thread_hide(&self.event_sink, workspace_id, thread_id);
            },
        )
        .await
    }
}
