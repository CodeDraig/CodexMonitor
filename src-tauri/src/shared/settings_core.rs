use std::path::PathBuf;

use tokio::sync::Mutex;

use crate::codex::config as codex_config;
use crate::storage::write_settings;
use crate::types::AppSettings;
use crate::utils::normalize_windows_namespace_path;

pub(crate) async fn get_app_settings_core(app_settings: &Mutex<AppSettings>) -> AppSettings {
    let mut settings = app_settings.lock().await.clone();
    if let Ok(Some(unified_exec_enabled)) = codex_config::read_unified_exec_enabled() {
        settings.unified_exec_enabled = unified_exec_enabled;
    }
    if let Ok(Some(apps_enabled)) = codex_config::read_apps_enabled() {
        settings.experimental_apps_enabled = apps_enabled;
    }
    settings
}

pub(crate) async fn update_app_settings_core(
    settings: AppSettings,
    app_settings: &Mutex<AppSettings>,
    settings_path: &PathBuf,
) -> Result<AppSettings, String> {
    let codex_home = crate::codex::home::resolve_default_codex_home()
        .ok_or_else(|| "Unable to resolve CODEX_HOME".to_string())?;
    update_app_settings_at(settings, app_settings, settings_path, &codex_home).await
}

async fn update_app_settings_at(
    mut settings: AppSettings,
    app_settings: &Mutex<AppSettings>,
    settings_path: &PathBuf,
    codex_home: &std::path::Path,
) -> Result<AppSettings, String> {
    // Serialize the config/settings pair, including the in-memory commit.
    let mut current = app_settings.lock().await;
    settings.global_worktrees_folder = settings
        .global_worktrees_folder
        .map(|path| normalize_windows_namespace_path(&path));
    codex_config::write_app_settings(codex_home, &settings)?;
    write_settings(settings_path, &settings).map_err(|error| {
        format!("Codex config saved, but app settings could not be saved: {error}")
    })?;
    *current = settings.clone();
    Ok(settings)
}

pub(crate) fn get_codex_config_path_core() -> Result<String, String> {
    codex_config::config_toml_path()
        .ok_or_else(|| "Unable to resolve CODEX_HOME".to_string())
        .and_then(|path| {
            path.to_str()
                .map(|value| value.to_string())
                .ok_or_else(|| "Unable to resolve CODEX_HOME".to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("settings-save-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn invalid_config_leaves_app_settings_and_config_unchanged() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            for contents in ["[invalid", "features = false\n"] {
                let root = root();
                let config = root.join("config.toml");
                std::fs::write(&config, contents).unwrap();
                let path = root.join("settings.json");
                let initial = AppSettings::default();
                write_settings(&path, &initial).unwrap();
                let before = std::fs::read(&path).unwrap();
                let current = Mutex::new(initial.clone());
                let mut changed = initial;
                changed.theme = "dark".into();
                assert!(update_app_settings_at(changed, &current, &path, &root)
                    .await
                    .is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                assert_eq!(std::fs::read_to_string(&config).unwrap(), contents);
                assert_ne!(current.lock().await.theme, "dark");
            }
        });
    }

    #[test]
    fn config_write_failure_does_not_commit_app_settings() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = root();
            // A file cannot become the CODEX_HOME directory, independent of privileges.
            let home = root.join("not-a-directory");
            std::fs::write(&home, "preserve").unwrap();
            let path = root.join("settings.json");
            let current = Mutex::new(AppSettings::default());
            assert!(
                update_app_settings_at(AppSettings::default(), &current, &path, &home)
                    .await
                    .is_err()
            );
            assert!(!path.exists());
            assert_eq!(std::fs::read_to_string(home).unwrap(), "preserve");
        });
    }

    #[test]
    fn saves_all_config_fields_and_reports_partial_app_save_failure() {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let root = root();
            std::fs::write(root.join("config.toml"), "model = \"preserve-me\"\n").unwrap();
            let path = root.join("settings.json");
            let current = Mutex::new(AppSettings::default());
            let mut settings = AppSettings::default();
            settings.experimental_apps_enabled = true;
            update_app_settings_at(settings.clone(), &current, &path, &root)
                .await
                .unwrap();
            let (_, document) =
                crate::shared::config_toml_core::load_global_config_document(&root).unwrap();
            assert_eq!(document["model"].as_str(), Some("preserve-me"));
            assert!(document.get("personality").is_none());
            assert_eq!(document["features"]["apps"].as_bool(), Some(true));
            assert!(document["features"].get("steer").is_none());
            assert_eq!(
                document["features"]["unified_exec"].as_bool(),
                Some(settings.unified_exec_enabled)
            );
            assert!(document["features"].get("collaboration_modes").is_none());
            let error = update_app_settings_at(settings, &current, &root, &root)
                .await
                .unwrap_err();
            assert!(error.contains("Codex config saved, but app settings could not be saved"));
        });
    }
}
