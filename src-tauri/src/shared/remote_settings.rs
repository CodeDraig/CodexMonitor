use crate::types::{AppSettings, RemoteBackendTarget};

pub(crate) fn active_remote_backend(settings: &AppSettings) -> RemoteBackendTarget {
    settings
        .remote_backends
        .iter()
        .find(|entry| Some(&entry.id) == settings.active_remote_backend_id.as_ref())
        .or_else(|| settings.remote_backends.first())
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_entry_owns_the_connection_details() {
        let mut settings = AppSettings::default();
        settings.remote_backends.push(RemoteBackendTarget {
            id: "second".into(),
            host: "second:5732".into(),
            token: Some("second-token".into()),
            ..RemoteBackendTarget::default()
        });
        settings.active_remote_backend_id = Some("second".into());
        let selected = active_remote_backend(&settings);
        assert_eq!(selected.host, "second:5732");
        assert_eq!(selected.token.as_deref(), Some("second-token"));
        settings.active_remote_backend_id = Some("missing".into());
        assert_eq!(active_remote_backend(&settings).id, "remote-default");
        settings.remote_backends.clear();
        assert_eq!(active_remote_backend(&settings).host, "127.0.0.1:4732");
    }

    #[test]
    fn serialized_settings_have_one_connection_contract() {
        let settings = AppSettings::default();
        let value = serde_json::to_value(&settings).unwrap();
        assert!(value.get("remoteBackendHost").is_none());
        assert!(value.get("remoteBackendToken").is_none());
        assert!(value.get("remoteBackendProvider").is_none());
        assert!(value.get("steerEnabled").is_none());
        assert!(value.get("collaborationModesEnabled").is_none());
        assert!(value.get("personality").is_none());
        let restored: AppSettings = serde_json::from_value(value).unwrap();
        assert_eq!(active_remote_backend(&restored).host, "127.0.0.1:4732");
    }
}
