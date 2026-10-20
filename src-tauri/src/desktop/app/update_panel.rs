//! Native updater controls. Network transfer and archive staging are submitted
//! to `PlatformService`; this panel only renders state and confirmation steps.

use crate::desktop::platform::{
    HostAction, HostResult, HostResultValue, InstalledPackage, UpdateConfiguration, UpdateManifest,
};
use egui::{self, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

fn update_target() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}

fn update_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => other,
    }
}

fn valid_staging_root(path: &std::path::Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| component == std::path::Component::ParentDir)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UpdatePanelPurpose {
    LoadConfiguration,
    SaveConfiguration,
    PickStagingRoot,
    Check,
    Download,
    Stage,
    Rollback,
}

#[derive(Clone, Debug)]
pub(crate) struct UpdatePanelEffect {
    pub(crate) purpose: UpdatePanelPurpose,
    pub(crate) action: HostAction,
}

#[derive(Debug)]
pub(crate) struct UpdatePanel {
    metadata_url: String,
    verifying_key_base64: String,
    target: String,
    architecture: String,
    staging_root: String,
    config_loaded: bool,
    config_pending: bool,
    config_dirty: bool,
    configured: bool,
    pending: HashMap<Uuid, UpdatePanelPurpose>,
    automatic_check_started: bool,
    latest: Option<(Uuid, UpdateManifest)>,
    downloaded: Option<(Uuid, UpdateManifest)>,
    staged: Option<InstalledPackage>,
    confirm_stage: bool,
    confirm_rollback: bool,
    status: Option<String>,
    error: Option<String>,
}

impl Default for UpdatePanel {
    fn default() -> Self {
        Self {
            metadata_url: String::new(),
            verifying_key_base64: String::new(),
            target: update_target().to_owned(),
            architecture: update_architecture().to_owned(),
            staging_root: String::new(),
            config_loaded: false,
            config_pending: false,
            config_dirty: false,
            configured: false,
            pending: HashMap::new(),
            automatic_check_started: false,
            latest: None,
            downloaded: None,
            staged: None,
            confirm_stage: false,
            confirm_rollback: false,
            status: None,
            error: None,
        }
    }
}

impl UpdatePanel {
    /// Seed the empty configuration form with an isolated staging directory.
    /// Callers should pass a dedicated data-directory child, never the launcher.
    pub(crate) fn with_staging_root(path: PathBuf) -> Self {
        Self {
            staging_root: path.to_string_lossy().into_owned(),
            ..Self::default()
        }
    }

    /// Request the saved configuration during application startup without
    /// blocking the UI thread. The first `show` call does the same lazily.
    pub(crate) fn load_configuration(&mut self) -> Option<UpdatePanelEffect> {
        if self.config_loaded || self.config_pending {
            return None;
        }
        self.config_pending = true;
        Some(UpdatePanelEffect {
            purpose: UpdatePanelPurpose::LoadConfiguration,
            action: HostAction::LoadUpdateConfiguration,
        })
    }

    /// Start one availability check during this app session when automatic
    /// checks are enabled and an explicit, validated feed is configured.
    /// Loading the configuration and checking the feed are worker actions;
    /// this method never downloads or stages an update.
    pub(crate) fn automatic_check(&mut self, enabled: bool) -> Option<UpdatePanelEffect> {
        if !enabled || self.automatic_check_started {
            return None;
        }
        if !self.config_loaded {
            return self.load_configuration();
        }
        if self.config_pending || !self.configured || self.config_dirty {
            return None;
        }
        if self
            .pending
            .values()
            .any(|purpose| *purpose == UpdatePanelPurpose::Check)
        {
            self.automatic_check_started = true;
            return None;
        }

        self.automatic_check_started = true;
        Some(UpdatePanelEffect {
            purpose: UpdatePanelPurpose::Check,
            action: HostAction::CheckForUpdate {
                current_version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        })
    }

    pub(crate) fn show(&mut self, ui: &mut Ui) -> Vec<UpdatePanelEffect> {
        let mut effects = Vec::new();
        ui.heading("Application updates and About");
        ui.label(format!(
            "CodexMonitor {} · {} · {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
        ui.label(format!(
            "Build profile: {}",
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
        ));
        ui.weak("Updates are verified and staged under an explicit directory. Staging does not replace the running application or desktop launcher.");
        ui.separator();

        if let Some(effect) = self.load_configuration() {
            effects.push(effect);
        }
        ui.horizontal(|ui| {
            ui.heading("Signed update feed");
            if ui
                .add_enabled(
                    !self.config_pending && !self.config_dirty,
                    egui::Button::new("Reload configuration"),
                )
                .clicked()
            {
                self.config_pending = true;
                effects.push(UpdatePanelEffect {
                    purpose: UpdatePanelPurpose::LoadConfiguration,
                    action: HostAction::LoadUpdateConfiguration,
                });
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if let Some(status) = &self.status {
            ui.label(status);
        }
        egui::Grid::new("native-update-config")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("Metadata URL");
                if ui
                    .add_enabled(
                        !self.config_pending,
                        egui::TextEdit::singleline(&mut self.metadata_url),
                    )
                    .changed()
                {
                    self.config_dirty = true;
                }
                ui.end_row();
                ui.label("Ed25519 public key (base64, 32 decoded bytes)");
                if ui
                    .add_enabled(
                        !self.config_pending,
                        egui::TextEdit::singleline(&mut self.verifying_key_base64),
                    )
                    .changed()
                {
                    self.config_dirty = true;
                }
                ui.end_row();
                ui.label("Target");
                if ui
                    .add_enabled(
                        !self.config_pending,
                        egui::TextEdit::singleline(&mut self.target),
                    )
                    .changed()
                {
                    self.config_dirty = true;
                }
                ui.end_row();
                ui.label("Architecture");
                if ui
                    .add_enabled(
                        !self.config_pending,
                        egui::TextEdit::singleline(&mut self.architecture),
                    )
                    .changed()
                {
                    self.config_dirty = true;
                }
                ui.end_row();
                ui.label("Staging root (isolated path)");
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.config_pending,
                            egui::TextEdit::singleline(&mut self.staging_root),
                        )
                        .changed()
                    {
                        self.config_dirty = true;
                    }
                    if ui
                        .add_enabled(!self.config_pending, egui::Button::new("Choose…"))
                        .clicked()
                    {
                        effects.push(UpdatePanelEffect {
                            purpose: UpdatePanelPurpose::PickStagingRoot,
                            action: HostAction::PickDirectory,
                        });
                    }
                });
                ui.end_row();
            });

        let staging_path = PathBuf::from(self.staging_root.trim());
        let staging_root_valid = valid_staging_root(&staging_path);
        if !self.staging_root.trim().is_empty() && !staging_root_valid {
            ui.colored_label(
                egui::Color32::LIGHT_RED,
                "Staging root must be an absolute path and cannot contain `..`.",
            );
        }

        let config_ready = !self.metadata_url.trim().is_empty()
            && !self.verifying_key_base64.trim().is_empty()
            && !self.target.trim().is_empty()
            && !self.architecture.trim().is_empty()
            && staging_root_valid;
        if ui
            .add_enabled(
                config_ready && self.config_dirty && !self.config_pending,
                egui::Button::new("Save and validate feed configuration"),
            )
            .clicked()
        {
            let configuration = UpdateConfiguration {
                metadata_url: self.metadata_url.trim().to_owned(),
                verifying_key_base64: self.verifying_key_base64.trim().to_owned(),
                target: self.target.trim().to_owned(),
                architecture: self.architecture.trim().to_owned(),
                staging_root: PathBuf::from(self.staging_root.trim()),
            };
            self.config_pending = true;
            effects.push(UpdatePanelEffect {
                purpose: UpdatePanelPurpose::SaveConfiguration,
                action: HostAction::SaveUpdateConfiguration { configuration },
            });
        }
        if !self.configured {
            ui.weak("No signed update feed is configured. Enter the exact feed URL and trusted public key to enable checks.");
        }
        ui.separator();

        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.configured
                        && !self.config_dirty
                        && !self
                            .pending
                            .values()
                            .any(|p| *p == UpdatePanelPurpose::Check),
                    egui::Button::new("Check for updates"),
                )
                .clicked()
            {
                effects.push(UpdatePanelEffect {
                    purpose: UpdatePanelPurpose::Check,
                    action: HostAction::CheckForUpdate {
                        current_version: env!("CARGO_PKG_VERSION").to_owned(),
                    },
                });
            }
            if !self.configured {
                ui.weak("Configure a signed feed first.");
            }
        });
        if let Some((ticket, manifest)) = &self.latest {
            ui.group(|ui| {
                ui.label(format!(
                    "Verified update available: {} · {} · {} · {}",
                    manifest.version, manifest.target, manifest.architecture, manifest.format
                ));
                ui.label(format!("SHA-256: {}", manifest.sha256));
                if ui
                    .add_enabled(
                        !self
                            .pending
                            .values()
                            .any(|p| *p == UpdatePanelPurpose::Download),
                        egui::Button::new("Download and verify package"),
                    )
                    .clicked()
                {
                    effects.push(UpdatePanelEffect {
                        purpose: UpdatePanelPurpose::Download,
                        action: HostAction::DownloadUpdate { ticket: *ticket },
                    });
                }
            });
        }
        if let Some((package_id, manifest)) = self.downloaded.clone() {
            ui.group(|ui| {
                ui.label(format!(
                    "Package downloaded and hash-verified: {} ({})",
                    manifest.version, manifest.sha256
                ));
                if !self.confirm_stage {
                    if ui.button("Stage this verified build…").clicked() {
                        self.confirm_stage = true;
                    }
                } else {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        format!(
                            "Stage version {} under {}? The active launcher will not change.",
                            manifest.version, self.staging_root
                        ),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self
                                    .pending
                                    .values()
                                    .any(|p| *p == UpdatePanelPurpose::Stage),
                                egui::Button::new("Confirm stage"),
                            )
                            .clicked()
                        {
                            effects.push(UpdatePanelEffect {
                                purpose: UpdatePanelPurpose::Stage,
                                action: HostAction::StageUpdate { package_id },
                            });
                            self.confirm_stage = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_stage = false;
                        }
                    });
                }
            });
        }
        if let Some(staged) = self.staged.clone() {
            ui.group(|ui| {
                ui.label(format!("Staged version {}", staged.version));
                ui.label(format!("Package: {}", staged.package_root.display()));
                ui.label(format!("Launcher: {}", staged.executable.display()));
                ui.label(format!(
                    "Previous staged version: {}",
                    staged.rollback_version.as_deref().unwrap_or("none")
                ));
                if !self.confirm_rollback {
                    if ui.button("Restore previous staged version…").clicked() {
                        self.confirm_rollback = true;
                    }
                } else {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "Restore the updater's previous staged version?",
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !self
                                    .pending
                                    .values()
                                    .any(|p| *p == UpdatePanelPurpose::Rollback),
                                egui::Button::new("Confirm staged rollback"),
                            )
                            .clicked()
                        {
                            effects.push(UpdatePanelEffect {
                                purpose: UpdatePanelPurpose::Rollback,
                                action: HostAction::RollbackStagedUpdate,
                            });
                            self.confirm_rollback = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_rollback = false;
                        }
                    });
                }
            });
        }
        effects
    }

    pub(crate) fn track_request(&mut self, id: Uuid, purpose: UpdatePanelPurpose) {
        if purpose == UpdatePanelPurpose::Check {
            self.automatic_check_started = true;
        }
        self.pending.insert(id, purpose);
    }

    /// Returns `true` when this panel owned the host result.
    pub(crate) fn handle_result(&mut self, result: &HostResult) -> bool {
        let Some(purpose) = self.pending.remove(&result.request_id) else {
            return false;
        };
        match &result.result {
            Err(error) => {
                if purpose == UpdatePanelPurpose::LoadConfiguration {
                    self.config_pending = false;
                    self.config_loaded = true;
                }
                if purpose == UpdatePanelPurpose::SaveConfiguration {
                    self.config_pending = false;
                }
                self.error = Some(error.clone());
                self.status = None;
            }
            Ok(value) => match (purpose, value) {
                (UpdatePanelPurpose::PickStagingRoot, HostResultValue::Directory(Some(path))) => {
                    self.staging_root = path.to_string_lossy().into_owned();
                    self.config_dirty = true;
                    self.error = None;
                }
                (
                    UpdatePanelPurpose::LoadConfiguration,
                    HostResultValue::UpdateConfiguration(config),
                ) => {
                    self.config_pending = false;
                    self.config_loaded = true;
                    self.apply_configuration(config.clone());
                }
                (
                    UpdatePanelPurpose::SaveConfiguration,
                    HostResultValue::UpdateConfiguration(config),
                ) => {
                    self.config_pending = false;
                    self.config_loaded = true;
                    self.apply_configuration(config.clone());
                    self.status = Some("Signed update feed saved and validated.".into());
                    self.error = None;
                }
                (
                    UpdatePanelPurpose::Check,
                    HostResultValue::UpdateAvailable { ticket, manifest },
                ) => {
                    self.latest = Some((*ticket, manifest.clone()));
                    self.downloaded = None;
                    self.status = Some(format!(
                        "Verified version {} is available.",
                        manifest.version
                    ));
                    self.error = None;
                }
                (UpdatePanelPurpose::Check, HostResultValue::UpdateUnavailable) => {
                    self.latest = None;
                    self.downloaded = None;
                    self.status = Some("No newer signed version is available.".into());
                    self.error = None;
                }
                (
                    UpdatePanelPurpose::Download,
                    HostResultValue::UpdateDownloaded {
                        package_id,
                        manifest,
                    },
                ) => {
                    self.downloaded = Some((*package_id, manifest.clone()));
                    self.latest = None;
                    self.status =
                        Some("Package downloaded and verified against its signed SHA-256.".into());
                    self.error = None;
                }
                (UpdatePanelPurpose::Stage, HostResultValue::UpdateStaged(staged)) => {
                    self.staged = Some(staged.clone());
                    self.downloaded = None;
                    self.status = Some(format!(
                        "Version {} was staged under the configured root.",
                        staged.version
                    ));
                    self.error = None;
                }
                (UpdatePanelPurpose::Rollback, HostResultValue::UpdateRolledBack(staged)) => {
                    self.staged = Some(staged.clone());
                    self.status = Some(format!("Restored staged version {}.", staged.version));
                    self.error = None;
                }
                (UpdatePanelPurpose::PickStagingRoot, HostResultValue::Directory(None)) => {}
                (_, other) => {
                    self.error = Some(format!(
                        "Unexpected updater response for {purpose:?}: {other:?}"
                    ));
                }
            },
        }
        true
    }

    fn apply_configuration(&mut self, configuration: Option<UpdateConfiguration>) {
        if let Some(configuration) = configuration {
            self.metadata_url = configuration.metadata_url;
            self.verifying_key_base64 = configuration.verifying_key_base64;
            self.target = configuration.target;
            self.architecture = configuration.architecture;
            self.staging_root = configuration.staging_root.to_string_lossy().into_owned();
            self.configured = true;
            self.config_dirty = false;
        } else {
            self.configured = false;
            self.config_dirty = false;
        }
        self.latest = None;
        self.downloaded = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_result(request_id: Uuid, result: Result<HostResultValue, String>) -> HostResult {
        HostResult { request_id, result }
    }

    #[test]
    fn update_state_requires_explicit_download_and_stage_confirmations() {
        let mut panel = UpdatePanel::default();
        let check = Uuid::new_v4();
        panel.track_request(check, UpdatePanelPurpose::Check);
        assert!(panel.handle_result(&host_result(
            check,
            Ok(HostResultValue::UpdateAvailable {
                ticket: Uuid::new_v4(),
                manifest: UpdateManifest {
                    version: "2.0.0".into(),
                    target: "linux".into(),
                    architecture: "x86_64".into(),
                    format: "tar.gz".into(),
                    package_url: "https://updates.example.test/pkg".into(),
                    sha256: "abc".into()
                },
            })
        )));
        assert!(panel.latest.is_some());
        assert!(panel.downloaded.is_none());
        assert!(panel.staged.is_none());
    }

    #[test]
    fn config_load_failure_is_visible_and_panel_owns_only_correlated_results() {
        let mut panel = UpdatePanel::default();
        let id = Uuid::new_v4();
        panel.track_request(id, UpdatePanelPurpose::LoadConfiguration);
        assert!(!panel.handle_result(&host_result(
            Uuid::new_v4(),
            Ok(HostResultValue::UpdateUnavailable)
        )));
        assert!(panel.handle_result(&host_result(id, Err("bad configuration".into()))));
        assert_eq!(panel.error.as_deref(), Some("bad configuration"));
        assert!(panel.config_loaded);
        assert!(!panel.config_pending);
    }

    #[test]
    fn staging_root_requires_absolute_normalized_path() {
        assert!(!valid_staging_root(std::path::Path::new(
            "relative/staging"
        )));
        assert!(!valid_staging_root(std::path::Path::new(
            "/tmp/../launcher"
        )));
        assert!(valid_staging_root(std::path::Path::new(
            "/var/lib/app/staging"
        )));
    }

    #[test]
    fn automatic_check_obeys_setting_and_requires_saved_feed() {
        let mut panel = UpdatePanel::default();
        assert!(panel.automatic_check(false).is_none());
        assert!(panel
            .automatic_check(true)
            .is_some_and(|effect| { effect.purpose == UpdatePanelPurpose::LoadConfiguration }));
        assert!(panel.automatic_check(true).is_none());

        panel.config_pending = false;
        panel.config_loaded = true;
        panel.configured = false;
        assert!(panel.automatic_check(true).is_none());
        assert!(!panel.automatic_check_started);
    }

    #[test]
    fn automatic_check_is_queued_once_and_manual_check_counts_for_session() {
        let mut panel = UpdatePanel {
            config_loaded: true,
            configured: true,
            ..UpdatePanel::default()
        };
        let effect = panel.automatic_check(true).unwrap();
        assert_eq!(effect.purpose, UpdatePanelPurpose::Check);
        assert!(matches!(effect.action, HostAction::CheckForUpdate { .. }));
        assert!(panel.automatic_check(true).is_none());

        let mut manual = UpdatePanel {
            config_loaded: true,
            configured: true,
            ..UpdatePanel::default()
        };
        manual.track_request(Uuid::new_v4(), UpdatePanelPurpose::Check);
        assert!(manual.automatic_check(true).is_none());
    }
}
