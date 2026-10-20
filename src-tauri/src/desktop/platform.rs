//! Native operating-system services for the egui host.
//!
//! UI code submits actions and drains results; dialogs, clipboard access,
//! notifications, and process launching run outside `eframe::App::ui`.

mod tray;
mod update;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::shared::dictation_core::{
    DictationEngine, DictationEvent, DictationModelStatus, DictationSessionState,
};
use crate::shared::host_core::HostPaths;
use crate::shared::managed_daemon_core::{self, ManagedDaemonRuntime};
use crate::types::{AppSettings, OpenAppTarget};

pub(crate) use tray::{
    TrayAvailability, TrayConversation, TrayEvent, TrayUsageSummary, WindowCloseBehavior,
};
pub(crate) use update::{
    InstalledPackage, UpdateConfiguration, UpdateError, UpdateManifest, UpdateService,
    VerifiedPackage,
};

#[derive(Clone, Debug)]
pub(crate) struct DialogFilter {
    pub name: String,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum HostAction {
    PickFiles {
        multiple: bool,
        filters: Vec<DialogFilter>,
    },
    PickDirectory,
    ReadClipboardText,
    WriteClipboardText {
        text: String,
    },
    ReadClipboardImage,
    WriteClipboardImage {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    OpenPath {
        path: PathBuf,
    },
    /// Open a local path using one configured AppSettings.openAppTargets entry.
    OpenInTarget {
        target: OpenAppTarget,
        path: PathBuf,
    },
    OpenUrl {
        url: String,
    },
    Notify {
        title: String,
        body: String,
    },
    PlayNotificationSound {
        sound: NotificationSound,
    },
    /// Decode and play an audio attachment using a host-owned playback worker.
    PlayAudioBytes {
        playback_id: Uuid,
        bytes: Vec<u8>,
        mime_type: String,
    },
    AudioControl {
        playback_id: Uuid,
        action: AudioPlaybackAction,
    },
    CheckForUpdate {
        current_version: String,
    },
    DownloadUpdate {
        ticket: Uuid,
    },
    StageUpdate {
        package_id: Uuid,
    },
    RollbackStagedUpdate,
    LoadUpdateConfiguration,
    SaveUpdateConfiguration {
        configuration: UpdateConfiguration,
    },
    TailscaleStatus,
    ManagedDaemonCommandPreview {
        settings: AppSettings,
    },
    ManagedDaemonStatus {
        settings: AppSettings,
    },
    ManagedDaemonStart {
        settings: AppSettings,
    },
    ManagedDaemonStop {
        settings: AppSettings,
    },
    ManagedDaemonShutdownForExit {
        settings: AppSettings,
    },
    DictationModelStatus {
        model_id: Option<String>,
    },
    DictationDownloadModel {
        model_id: Option<String>,
    },
    DictationCancelDownload,
    DictationStart {
        model_id: Option<String>,
        preferred_language: Option<String>,
    },
    DictationStop {
        preferred_language: Option<String>,
    },
    DictationCancel,
    /// Resolves to HideToTray only when native tray creation succeeded.
    CloseWindow,
    QuitApplication,
    PersistWindowState {
        state: WindowState,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HostResultValue {
    Files(Option<Vec<PathBuf>>),
    Directory(Option<PathBuf>),
    ClipboardText(String),
    ClipboardImage {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    Completed,
    WindowAction(WindowAction),
    DictationModel(DictationModelStatus),
    DictationState(DictationSessionState),
    DictationEvent(DictationEvent),
    AudioPlayback(AudioPlaybackState),
    UpdateAvailable {
        ticket: Uuid,
        manifest: UpdateManifest,
    },
    UpdateUnavailable,
    UpdateDownloaded {
        package_id: Uuid,
        manifest: UpdateManifest,
    },
    UpdateStaged(InstalledPackage),
    UpdateRolledBack(InstalledPackage),
    UpdateConfiguration(Option<UpdateConfiguration>),
    TailscaleStatus(Value),
    ManagedDaemonCommandPreview(Value),
    ManagedDaemonStatus(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowAction {
    HideToTray,
    Close,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotificationSound {
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum AudioPlaybackAction {
    Pause,
    Resume,
    Seek { position_seconds: f64 },
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioPlaybackStatus {
    Playing,
    Paused,
    Finished,
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AudioPlaybackState {
    pub playback_id: Uuid,
    pub state: AudioPlaybackStatus,
    pub position_seconds: f64,
    pub duration_seconds: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HostResult {
    pub request_id: Uuid,
    pub result: Result<HostResultValue, String>,
}

/// Show a startup failure when a native display session exists. Returns false
/// in headless Linux environments so a CLI/automation launch cannot hang.
pub(crate) fn show_startup_error_dialog(title: &str, message: &str) -> bool {
    #[cfg(target_os = "linux")]
    let has_display = ["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let has_display = true;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let has_display = false;

    if !has_display {
        return false;
    }
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(title)
        .set_description(message)
        .show();
    true
}

/// Persisted native window geometry. The eframe host applies it to the root viewport.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowState {
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub maximized: bool,
}

/// A thread-safe action bridge. Call `enqueue` from UI callbacks, process the
/// work on Tokio, then consume `drain` from the eframe update loop.
pub(crate) struct PlatformService {
    runtime: Handle,
    paths: HostPaths,
    wake: Arc<dyn Fn() + Send + Sync>,
    results_tx: mpsc::UnboundedSender<HostResult>,
    results_rx: Mutex<mpsc::UnboundedReceiver<HostResult>>,
    dictation: Arc<DictationEngine>,
    audio: AudioPlaybackService,
    updates: Arc<Mutex<Option<UpdateRuntime>>>,
    managed_daemon: Arc<tokio::sync::Mutex<ManagedDaemonRuntime>>,
    tray: tray::TrayService,
}

#[derive(Clone)]
struct UpdateRuntime {
    service: UpdateService,
    manifests: HashMap<Uuid, update::VerifiedManifest>,
    packages: HashMap<Uuid, VerifiedPackage>,
}

#[derive(Clone)]
struct AudioPlaybackService {
    sessions: Arc<Mutex<HashMap<Uuid, std_mpsc::Sender<AudioControlMessage>>>>,
    results: mpsc::UnboundedSender<HostResult>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

struct AudioControlMessage {
    action: AudioPlaybackAction,
    reply: std_mpsc::Sender<Result<AudioPlaybackState, String>>,
}

impl AudioPlaybackService {
    fn new(results: mpsc::UnboundedSender<HostResult>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            results,
            wake,
        }
    }

    fn play(
        &self,
        playback_id: Uuid,
        bytes: Vec<u8>,
        mime_type: String,
    ) -> Result<AudioPlaybackState, String> {
        let mime_type = mime_type
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if !matches!(
            mime_type.as_str(),
            "audio/mpeg" | "audio/mp3" | "audio/wav" | "audio/x-wav" | "audio/ogg" | "audio/vorbis"
        ) {
            return Err(format!("unsupported audio attachment type: {mime_type}"));
        }
        if bytes.is_empty() {
            return Err("audio attachment is empty".into());
        }
        let (control_tx, control_rx) = std_mpsc::channel();
        {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|_| "audio playback registry lock poisoned".to_string())?;
            if sessions.contains_key(&playback_id) {
                return Err("audio playback ID is already active".into());
            }
            for sender in sessions.values() {
                let (reply, _ignored) = std_mpsc::channel();
                let _ = sender.send(AudioControlMessage {
                    action: AudioPlaybackAction::Stop,
                    reply,
                });
            }
            sessions.clear();
            sessions.insert(playback_id, control_tx);
        }
        let sessions = Arc::clone(&self.sessions);
        let results = self.results.clone();
        let wake = Arc::clone(&self.wake);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        std::thread::spawn(move || {
            audio_worker(
                playback_id,
                bytes,
                control_rx,
                ready_tx,
                sessions,
                results,
                wake,
            );
        });
        match ready_rx.recv_timeout(Duration::from_secs(15)) {
            Ok(result) => result,
            Err(error) => {
                if let Ok(mut sessions) = self.sessions.lock() {
                    sessions.remove(&playback_id);
                }
                Err(format!("audio playback did not initialize: {error}"))
            }
        }
    }

    fn control(
        &self,
        playback_id: Uuid,
        action: AudioPlaybackAction,
    ) -> Result<AudioPlaybackState, String> {
        if let AudioPlaybackAction::Seek { position_seconds } = action {
            if !position_seconds.is_finite()
                || position_seconds < 0.0
                || position_seconds > u64::MAX as f64
            {
                return Err("audio seek position must be finite and nonnegative".into());
            }
        }
        let sender = self
            .sessions
            .lock()
            .map_err(|_| "audio playback registry lock poisoned".to_string())?
            .get(&playback_id)
            .cloned()
            .ok_or_else(|| "audio playback is no longer active".to_string())?;
        let (reply_tx, reply_rx) = std_mpsc::channel();
        sender
            .send(AudioControlMessage {
                action,
                reply: reply_tx,
            })
            .map_err(|_| "audio playback worker has stopped".to_string())?;
        reply_rx
            .recv_timeout(Duration::from_secs(3))
            .map_err(|error| format!("audio playback control timed out: {error}"))?
    }
}

impl PlatformService {
    pub(crate) fn new(
        paths: HostPaths,
        runtime: Handle,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let (results_tx, results_rx) = mpsc::unbounded_channel();
        let dictation = Arc::new(DictationEngine::new(paths.data_dir.join("models/whisper")));
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        let audio = AudioPlaybackService::new(results_tx.clone(), Arc::clone(&wake));
        let tray = tray::TrayService::new(Arc::clone(&wake));
        Self {
            runtime,
            paths,
            wake,
            results_tx,
            results_rx: Mutex::new(results_rx),
            dictation,
            audio,
            updates: Arc::new(Mutex::new(None)),
            managed_daemon: Arc::new(tokio::sync::Mutex::new(ManagedDaemonRuntime::default())),
            tray,
        }
    }

    pub(crate) fn enqueue(&self, action: HostAction) -> Result<Uuid, String> {
        let id = match &action {
            HostAction::PlayAudioBytes { playback_id, .. }
            | HostAction::AudioControl { playback_id, .. } => *playback_id,
            _ => Uuid::new_v4(),
        };
        let tx = self.results_tx.clone();
        let wake = Arc::clone(&self.wake);
        let paths = self.paths.clone();
        let dictation = Arc::clone(&self.dictation);
        let audio = self.audio.clone();
        let updates = Arc::clone(&self.updates);
        let managed_daemon = Arc::clone(&self.managed_daemon);
        let tray_available = self.tray.availability().is_available();
        self.runtime.spawn(async move {
            let result = execute(
                action,
                paths,
                dictation,
                audio,
                updates,
                managed_daemon,
                tray_available,
            )
            .await;
            let _ = tx.send(HostResult {
                request_id: id,
                result,
            });
            wake();
        });
        Ok(id)
    }

    /// Drain all completed host operations without waiting for pending work.
    pub(crate) fn drain(&self) -> Vec<HostResult> {
        let Ok(mut receiver) = self.results_rx.lock() else {
            return Vec::new();
        };
        let mut results = Vec::new();
        while let Ok(result) = receiver.try_recv() {
            results.push(result);
        }
        results.extend(
            self.dictation
                .drain_events()
                .into_iter()
                .map(|event| HostResult {
                    request_id: Uuid::nil(),
                    result: Ok(HostResultValue::DictationEvent(event)),
                }),
        );
        results
    }

    pub(crate) fn load_window_state(&self) -> Result<WindowState, String> {
        let path = window_state_path(&self.paths);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| {
                format!("invalid native window state {}: {error}", path.display())
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(WindowState::default())
            }
            Err(error) => Err(format!(
                "could not read native window state {}: {error}",
                path.display()
            )),
        }
    }

    pub(crate) fn save_window_state(&self, state: WindowState) -> Result<Uuid, String> {
        self.enqueue(HostAction::PersistWindowState { state })
    }

    pub(crate) fn tray_availability(&self) -> TrayAvailability {
        self.tray.availability()
    }

    pub(crate) fn update_tray_menu(
        &mut self,
        recent: &[TrayConversation],
        usage: &TrayUsageSummary,
    ) -> Result<(), String> {
        self.tray.update_menu(recent, usage)
    }

    pub(crate) fn drain_tray_events(&self) -> Vec<TrayEvent> {
        self.tray.drain_events()
    }

    pub(crate) fn window_close_behavior(&self) -> WindowCloseBehavior {
        tray::close_behavior(self.tray.availability().is_available())
    }
}

fn valid_window_state(state: WindowState) -> bool {
    [state.x, state.y, state.width, state.height]
        .into_iter()
        .flatten()
        .all(f32::is_finite)
        && state.width.map_or(true, |width| width > 0.0)
        && state.height.map_or(true, |height| height > 0.0)
}

fn window_state_path(paths: &HostPaths) -> PathBuf {
    paths.data_dir.join("native-window.json")
}

fn persist_window_state(paths: &HostPaths, state: WindowState) -> Result<(), String> {
    if !valid_window_state(state) {
        return Err("window geometry must be finite and have positive dimensions".into());
    }
    let path = window_state_path(paths);
    let temp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&state).map_err(|error| error.to_string())?;
    std::fs::write(&temp, bytes)
        .map_err(|error| format!("could not write {}: {error}", temp.display()))?;
    atomic_replace(&temp, &path)
}

fn atomic_replace(temp: &PathBuf, destination: &PathBuf) -> Result<(), String> {
    match std::fs::rename(temp, destination) {
        Ok(()) => Ok(()),
        Err(first_error) if destination.exists() => {
            let backup = destination.with_extension(format!("backup-{}.json", Uuid::new_v4()));
            std::fs::rename(destination, &backup).map_err(|error| {
                format!(
                    "could not preserve previous window state ({}): {error}",
                    first_error
                )
            })?;
            match std::fs::rename(temp, destination) {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = std::fs::rename(backup, destination);
                    let _ = std::fs::remove_file(temp);
                    Err(format!("could not save window state: {error}"))
                }
            }
        }
        Err(error) => {
            let _ = std::fs::remove_file(temp);
            Err(format!("could not save window state: {error}"))
        }
    }
}

async fn execute(
    action: HostAction,
    paths: HostPaths,
    dictation: Arc<DictationEngine>,
    audio: AudioPlaybackService,
    updates: Arc<Mutex<Option<UpdateRuntime>>>,
    managed_daemon: Arc<tokio::sync::Mutex<ManagedDaemonRuntime>>,
    tray_available: bool,
) -> Result<HostResultValue, String> {
    match action {
        HostAction::PickFiles { multiple, filters } => {
            let mut dialog = rfd::AsyncFileDialog::new();
            for filter in filters {
                dialog = dialog.add_filter(filter.name, &filter.extensions);
            }
            let selected = if multiple {
                dialog.pick_files().await.map(|files| {
                    files
                        .into_iter()
                        .map(|file| file.path().to_path_buf())
                        .collect()
                })
            } else {
                dialog
                    .pick_file()
                    .await
                    .map(|file| vec![file.path().to_path_buf()])
            };
            Ok(HostResultValue::Files(selected))
        }
        HostAction::PickDirectory => {
            let selected = rfd::AsyncFileDialog::new()
                .pick_folder()
                .await
                .map(|directory| directory.path().to_path_buf());
            Ok(HostResultValue::Directory(selected))
        }
        HostAction::ReadClipboardText => clipboard_text(None)
            .await
            .map(HostResultValue::ClipboardText),
        HostAction::WriteClipboardText { text } => clipboard_text(Some(text))
            .await
            .map(|_| HostResultValue::Completed),
        HostAction::ReadClipboardImage => {
            clipboard_image()
                .await
                .map(|image| HostResultValue::ClipboardImage {
                    width: image.width,
                    height: image.height,
                    rgba: image.bytes.into_owned(),
                })
        }
        HostAction::WriteClipboardImage {
            width,
            height,
            rgba,
        } => {
            let expected = width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| "clipboard image dimensions overflow".to_string())?;
            if width == 0 || height == 0 || rgba.len() != expected {
                return Err("clipboard image dimensions do not match the RGBA buffer".into());
            }
            tokio::task::spawn_blocking(move || {
                let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
                clipboard
                    .set_image(arboard::ImageData {
                        width,
                        height,
                        bytes: rgba.into(),
                    })
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| format!("clipboard task failed: {error}"))??;
            Ok(HostResultValue::Completed)
        }
        HostAction::OpenPath { path } => open_target(path.to_string_lossy().into_owned()).await,
        HostAction::OpenInTarget { target, path } => open_in_target(target, path).await,
        HostAction::OpenUrl { url } => {
            let scheme = url
                .split_once(':')
                .map(|(scheme, _)| scheme.to_ascii_lowercase())
                .ok_or_else(|| "invalid URL: missing scheme".to_string())?;
            if !matches!(scheme.as_str(), "http" | "https" | "mailto") {
                return Err(format!("unsupported external URL scheme: {scheme}"));
            }
            open_target(url).await
        }
        HostAction::Notify { title, body } => notify(title, body).await,
        HostAction::PlayNotificationSound { sound } => play_notification_sound(sound).await,
        HostAction::PlayAudioBytes {
            playback_id,
            bytes,
            mime_type,
        } => audio
            .play(playback_id, bytes, mime_type)
            .map(HostResultValue::AudioPlayback),
        HostAction::AudioControl {
            playback_id,
            action,
        } => audio
            .control(playback_id, action)
            .map(HostResultValue::AudioPlayback),
        HostAction::CheckForUpdate { current_version } => {
            check_for_update(&updates, &current_version).await
        }
        HostAction::DownloadUpdate { ticket } => download_update(&updates, ticket).await,
        HostAction::StageUpdate { package_id } => stage_update(&updates, package_id).await,
        HostAction::RollbackStagedUpdate => rollback_staged_update(&updates).await,
        HostAction::LoadUpdateConfiguration => load_update_configuration(&paths, &updates),
        HostAction::SaveUpdateConfiguration { configuration } => {
            save_update_configuration(&paths, &updates, configuration)
        }
        HostAction::TailscaleStatus => {
            serde_json::to_value(managed_daemon_core::tailscale_status().await?)
                .map(HostResultValue::TailscaleStatus)
                .map_err(|error| error.to_string())
        }
        HostAction::ManagedDaemonCommandPreview { settings } => {
            let daemon_path = crate::daemon_binary::resolve_daemon_binary_path()?;
            let preview = managed_daemon_core::daemon_command_preview(
                &settings,
                &daemon_path,
                &paths.data_dir,
            );
            serde_json::to_value(preview)
                .map(HostResultValue::ManagedDaemonCommandPreview)
                .map_err(|error| error.to_string())
        }
        HostAction::ManagedDaemonStatus { settings } => {
            let mut runtime = managed_daemon.lock().await;
            let status = managed_daemon_core::daemon_status(&settings, &mut runtime).await?;
            serde_json::to_value(status)
                .map(HostResultValue::ManagedDaemonStatus)
                .map_err(|error| error.to_string())
        }
        HostAction::ManagedDaemonStart { settings } => {
            let daemon_path = crate::daemon_binary::resolve_daemon_binary_path()?;
            let mut runtime = managed_daemon.lock().await;
            let status = managed_daemon_core::daemon_start(
                &settings,
                &paths.data_dir,
                &daemon_path,
                &mut runtime,
            )
            .await?;
            serde_json::to_value(status)
                .map(HostResultValue::ManagedDaemonStatus)
                .map_err(|error| error.to_string())
        }
        HostAction::ManagedDaemonStop { settings } => {
            let mut runtime = managed_daemon.lock().await;
            let status = managed_daemon_core::daemon_stop(&settings, &mut runtime).await?;
            serde_json::to_value(status)
                .map(HostResultValue::ManagedDaemonStatus)
                .map_err(|error| error.to_string())
        }
        HostAction::ManagedDaemonShutdownForExit { settings } => {
            let mut runtime = managed_daemon.lock().await;
            let status = managed_daemon_core::shutdown_for_exit(&settings, &mut runtime).await?;
            serde_json::to_value(status)
                .map(HostResultValue::ManagedDaemonStatus)
                .map_err(|error| error.to_string())
        }
        HostAction::PersistWindowState { state } => {
            persist_window_state(&paths, state)?;
            Ok(HostResultValue::Completed)
        }
        HostAction::DictationModelStatus { model_id } => Ok(HostResultValue::DictationModel(
            dictation.model_status(model_id.as_deref()).await,
        )),
        HostAction::DictationDownloadModel { model_id } => dictation
            .download_model(model_id.as_deref())
            .await
            .map(HostResultValue::DictationModel),
        HostAction::DictationCancelDownload => {
            dictation.cancel_download().await?;
            Ok(HostResultValue::Completed)
        }
        HostAction::DictationStart {
            model_id,
            preferred_language,
        } => dictation
            .start(model_id.as_deref(), preferred_language)
            .await
            .map(HostResultValue::DictationState),
        HostAction::DictationStop { preferred_language } => dictation
            .stop(preferred_language)
            .await
            .map(HostResultValue::DictationState),
        HostAction::DictationCancel => dictation
            .cancel()
            .await
            .map(HostResultValue::DictationState),
        HostAction::CloseWindow => Ok(HostResultValue::WindowAction(
            match tray::close_behavior(tray_available) {
                WindowCloseBehavior::HideToTray => WindowAction::HideToTray,
                WindowCloseBehavior::CloseWindow => WindowAction::Close,
            },
        )),
        HostAction::QuitApplication => Ok(HostResultValue::WindowAction(WindowAction::Quit)),
    }
}

async fn check_for_update(
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
    current_version: &str,
) -> Result<HostResultValue, String> {
    let service = update_runtime(updates)?.service.clone();
    match service
        .check(current_version)
        .await
        .map_err(|error| error.to_string())?
    {
        Some(verified) => {
            let ticket = Uuid::new_v4();
            let manifest = verified.manifest().clone();
            updates
                .lock()
                .map_err(|_| "native updater configuration lock poisoned".to_string())?
                .as_mut()
                .ok_or_else(|| "native updater is not configured".to_string())?
                .manifests
                .insert(ticket, verified);
            Ok(HostResultValue::UpdateAvailable { ticket, manifest })
        }
        None => Ok(HostResultValue::UpdateUnavailable),
    }
}

async fn download_update(
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
    ticket: Uuid,
) -> Result<HostResultValue, String> {
    let (service, manifest) = {
        let runtime = update_runtime(updates)?;
        let manifest = runtime
            .manifests
            .get(&ticket)
            .cloned()
            .ok_or_else(|| "update check ticket is missing or expired".to_string())?;
        (runtime.service.clone(), manifest)
    };
    let manifest_info = manifest.manifest().clone();
    let package = service
        .download_verified(&manifest)
        .await
        .map_err(|error| error.to_string())?;
    let package_id = Uuid::new_v4();
    updates
        .lock()
        .map_err(|_| "native updater configuration lock poisoned".to_string())?
        .as_mut()
        .ok_or_else(|| "native updater is not configured".to_string())?
        .packages
        .insert(package_id, package);
    Ok(HostResultValue::UpdateDownloaded {
        package_id,
        manifest: manifest_info,
    })
}

async fn stage_update(
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
    package_id: Uuid,
) -> Result<HostResultValue, String> {
    let (service, package) = {
        let runtime = update_runtime(updates)?;
        let package = runtime
            .packages
            .get(&package_id)
            .cloned()
            .ok_or_else(|| "verified package token is missing or expired".to_string())?;
        (runtime.service.clone(), package)
    };
    let package_id_for_cleanup = package_id;
    let installed = tokio::task::spawn_blocking(move || service.install(package))
        .await
        .map_err(|error| format!("update staging task failed: {error}"))?
        .map_err(|error| error.to_string())?;
    if let Ok(mut guard) = updates.lock() {
        if let Some(runtime) = guard.as_mut() {
            runtime.packages.remove(&package_id_for_cleanup);
        }
    }
    Ok(HostResultValue::UpdateStaged(installed))
}

async fn rollback_staged_update(
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
) -> Result<HostResultValue, String> {
    let service = update_runtime(updates)?.service.clone();
    let installed = tokio::task::spawn_blocking(move || service.rollback())
        .await
        .map_err(|error| format!("update rollback task failed: {error}"))?
        .map_err(|error| error.to_string())?;
    Ok(HostResultValue::UpdateRolledBack(installed))
}

fn update_runtime(updates: &Arc<Mutex<Option<UpdateRuntime>>>) -> Result<UpdateRuntime, String> {
    updates
        .lock()
        .map_err(|_| "native updater configuration lock poisoned".to_string())?
        .as_ref()
        .cloned()
        .ok_or_else(|| "native updater is not configured".to_string())
}

fn update_configuration_path(paths: &HostPaths) -> PathBuf {
    paths.data_dir.join("native-updater.json")
}

fn load_update_configuration(
    paths: &HostPaths,
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
) -> Result<HostResultValue, String> {
    let path = update_configuration_path(paths);
    let configuration = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<UpdateConfiguration>(&bytes).map_err(|error| {
            format!("invalid updater configuration {}: {error}", path.display())
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostResultValue::UpdateConfiguration(None));
        }
        Err(error) => return Err(format!("could not read updater configuration: {error}")),
    };
    let service = validate_update_configuration(&configuration)?;
    install_update_configuration(updates, service)?;
    Ok(HostResultValue::UpdateConfiguration(Some(configuration)))
}

fn save_update_configuration(
    paths: &HostPaths,
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
    configuration: UpdateConfiguration,
) -> Result<HostResultValue, String> {
    let service = validate_update_configuration(&configuration)?;
    let path = update_configuration_path(paths);
    std::fs::create_dir_all(&paths.data_dir)
        .map_err(|error| format!("could not create application data directory: {error}"))?;
    let temp = path.with_extension(format!("json.{}.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&configuration).map_err(|error| error.to_string())?;
    std::fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    atomic_replace(&temp, &path)?;
    install_update_configuration(updates, service)?;
    Ok(HostResultValue::UpdateConfiguration(Some(configuration)))
}

fn validate_update_configuration(
    configuration: &UpdateConfiguration,
) -> Result<UpdateService, String> {
    if configuration.staging_root.as_os_str().is_empty() {
        return Err("an explicit updater staging root is required".into());
    }
    let service = configuration
        .build_service()
        .map_err(|error| error.to_string())?;
    let current_exe = std::env::current_exe()
        .map_err(|error| format!("could not locate current application executable: {error}"))?;
    let current_dir = current_exe
        .parent()
        .ok_or_else(|| "current executable has no parent directory".to_string())?;
    let installation_dir = std::fs::canonicalize(current_dir)
        .map_err(|error| format!("could not resolve current executable directory: {error}"))?;
    let prospective_stage = prospective_canonical_path(&configuration.staging_root)?;
    if prospective_stage.starts_with(&installation_dir)
        || installation_dir.starts_with(&prospective_stage)
    {
        return Err(
            "updater staging root must be separate from the running application directory".into(),
        );
    }
    std::fs::create_dir_all(&configuration.staging_root)
        .map_err(|error| format!("could not create updater staging root: {error}"))?;
    let staging_root = std::fs::canonicalize(&configuration.staging_root)
        .map_err(|error| format!("could not resolve staging root: {error}"))?;
    if staging_root.starts_with(&installation_dir) || installation_dir.starts_with(&staging_root) {
        return Err(
            "updater staging root must be separate from the running application directory".into(),
        );
    }
    Ok(service)
}

/// Resolve a possibly not-yet-created absolute path through its nearest
/// existing ancestor. This lets validation reject an install-overlapping path
/// before `create_dir_all` can touch the installation tree.
fn prospective_canonical_path(path: &std::path::Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("updater staging root must be an absolute path".into());
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("updater staging root must not contain parent-directory components".into());
    }
    let mut cursor = path;
    let mut missing = Vec::new();
    let mut base = loop {
        if cursor.exists() {
            break std::fs::canonicalize(cursor)
                .map_err(|error| format!("could not resolve staging path ancestor: {error}"))?;
        }
        let name = cursor
            .file_name()
            .ok_or_else(|| "updater staging root has no existing ancestor".to_string())?;
        missing.push(name.to_os_string());
        cursor = cursor
            .parent()
            .ok_or_else(|| "updater staging root has no existing ancestor".to_string())?;
    };
    for component in missing.iter().rev() {
        base.push(component);
    }
    Ok(base)
}

fn install_update_configuration(
    updates: &Arc<Mutex<Option<UpdateRuntime>>>,
    service: UpdateService,
) -> Result<(), String> {
    let mut updates = updates
        .lock()
        .map_err(|_| "native updater configuration lock poisoned".to_string())?;
    *updates = Some(UpdateRuntime {
        service,
        manifests: HashMap::new(),
        packages: HashMap::new(),
    });
    Ok(())
}

async fn clipboard_text(write: Option<String>) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
        match write {
            Some(text) => {
                clipboard
                    .set_text(text)
                    .map_err(|error| error.to_string())?;
                Ok(String::new())
            }
            None => clipboard.get_text().map_err(|error| error.to_string()),
        }
    })
    .await
    .map_err(|error| format!("clipboard task failed: {error}"))?
}

async fn clipboard_image() -> Result<arboard::ImageData<'static>, String> {
    tokio::task::spawn_blocking(move || {
        let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
        clipboard.get_image().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("clipboard task failed: {error}"))?
}

#[cfg(target_os = "linux")]
async fn open_target(target: String) -> Result<HostResultValue, String> {
    run_command("xdg-open", &[target.as_str()]).await?;
    Ok(HostResultValue::Completed)
}

#[cfg(target_os = "macos")]
async fn open_target(target: String) -> Result<HostResultValue, String> {
    run_command("open", &[target.as_str()]).await?;
    Ok(HostResultValue::Completed)
}

#[cfg(target_os = "windows")]
async fn open_target(target: String) -> Result<HostResultValue, String> {
    let output = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Process -FilePath $env:CODEX_MONITOR_OPEN_TARGET",
        ])
        .env("CODEX_MONITOR_OPEN_TARGET", target)
        .output()
        .await
        .map_err(|error| format!("could not start Windows shell: {error}"))?;
    command_result("Windows shell", output)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
async fn open_target(_target: String) -> Result<HostResultValue, String> {
    Err("opening files and URLs is unsupported on this platform".into())
}

async fn notify(title: String, body: String) -> Result<HostResultValue, String> {
    tokio::task::spawn_blocking(move || {
        notify_rust::Notification::new()
            .summary(&title)
            .body(&body)
            .show()
            .map(|_| HostResultValue::Completed)
            .map_err(|error| format!("native notification failed: {error}"))
    })
    .await
    .map_err(|error| format!("notification task failed: {error}"))?
}

async fn open_in_target(target: OpenAppTarget, path: PathBuf) -> Result<HostResultValue, String> {
    match target.kind.as_str() {
        "app" => {
            let app = target
                .app_name
                .as_deref()
                .map(str::trim)
                .filter(|app| !app.is_empty())
                .ok_or_else(|| format!("Open-in target `{}` has no app name", target.label))?;
            crate::shared::workspaces_core::open_workspace_in_core(
                path.to_string_lossy().into_owned(),
                Some(app.to_owned()),
                target.args,
                None,
                None,
                None,
            )
            .await?;
            Ok(HostResultValue::Completed)
        }
        "command" => {
            let command = target
                .command
                .as_deref()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .ok_or_else(|| format!("Open-in target `{}` has no command", target.label))?;
            crate::shared::workspaces_core::open_workspace_in_core(
                path.to_string_lossy().into_owned(),
                None,
                target.args,
                Some(command.to_owned()),
                None,
                None,
            )
            .await?;
            Ok(HostResultValue::Completed)
        }
        "finder" => reveal_path(path).await,
        kind => Err(format!("unsupported Open-in target kind: {kind}")),
    }
}

#[cfg(target_os = "linux")]
async fn reveal_path(path: PathBuf) -> Result<HostResultValue, String> {
    let reveal = if path.is_dir() {
        path
    } else {
        path.parent()
            .map(PathBuf::from)
            .ok_or_else(|| format!("cannot reveal path without a parent: {}", path.display()))?
    };
    let value = reveal.to_string_lossy().into_owned();
    run_command("xdg-open", &[&value]).await?;
    Ok(HostResultValue::Completed)
}

#[cfg(target_os = "macos")]
async fn reveal_path(path: PathBuf) -> Result<HostResultValue, String> {
    let value = path.to_string_lossy().into_owned();
    run_command("open", &["-R", &value]).await?;
    Ok(HostResultValue::Completed)
}

#[cfg(target_os = "windows")]
async fn reveal_path(path: PathBuf) -> Result<HostResultValue, String> {
    let mut command = tokio::process::Command::new("explorer.exe");
    if path.is_dir() {
        command.arg(&path);
    } else {
        command.arg("/select,").arg(&path);
    }
    let output = command
        .output()
        .await
        .map_err(|error| format!("could not start File Explorer: {error}"))?;
    command_result("File Explorer", output)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
async fn reveal_path(_path: PathBuf) -> Result<HostResultValue, String> {
    Err("revealing paths in a file manager is unsupported on this platform".into())
}

async fn play_notification_sound(sound: NotificationSound) -> Result<HostResultValue, String> {
    tokio::task::spawn_blocking(move || {
        use rodio::{Decoder, OutputStream, Sink};
        use std::io::Cursor;
        let bytes: &'static [u8] = match sound {
            NotificationSound::Success => {
                include_bytes!("../../assets/success-notification.mp3")
            }
            NotificationSound::Error => {
                include_bytes!("../../assets/error-notification.mp3")
            }
        };
        let (_stream, stream_handle) = OutputStream::try_default()
            .map_err(|error| format!("could not open native audio output: {error}"))?;
        let sink = Sink::try_new(&stream_handle)
            .map_err(|error| format!("could not create audio player: {error}"))?;
        let decoder = Decoder::new(Cursor::new(bytes))
            .map_err(|error| format!("could not decode notification sound: {error}"))?;
        sink.set_volume(0.05);
        sink.append(decoder);
        sink.sleep_until_end();
        Ok(HostResultValue::Completed)
    })
    .await
    .map_err(|error| format!("notification sound task failed: {error}"))?
}

fn audio_worker(
    playback_id: Uuid,
    bytes: Vec<u8>,
    controls: std_mpsc::Receiver<AudioControlMessage>,
    ready: std_mpsc::SyncSender<Result<AudioPlaybackState, String>>,
    sessions: Arc<Mutex<HashMap<Uuid, std_mpsc::Sender<AudioControlMessage>>>>,
    results: mpsc::UnboundedSender<HostResult>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    use rodio::{Decoder, OutputStream, Sink, Source};
    use std::io::Cursor;

    let setup = (|| {
        let (stream, handle) = OutputStream::try_default()
            .map_err(|error| format!("could not open native audio output: {error}"))?;
        let sink = Sink::try_new(&handle)
            .map_err(|error| format!("could not create audio player: {error}"))?;
        let decoder = Decoder::new(Cursor::new(bytes))
            .map_err(|error| format!("could not decode audio attachment: {error}"))?;
        let duration = decoder.total_duration();
        sink.append(decoder);
        sink.play();
        Ok::<_, String>((stream, sink, duration))
    })();
    let (_stream, sink, duration) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            send_audio_result(&results, &wake, playback_id, Err(error));
            if let Ok(mut sessions) = sessions.lock() {
                sessions.remove(&playback_id);
            }
            return;
        }
    };
    let duration_seconds = duration.map(|value| value.as_secs_f64());
    let mut playback_status = AudioPlaybackStatus::Playing;
    let initial = audio_playback_state(playback_id, playback_status, &sink, duration_seconds);
    let _ = ready.send(Ok(initial.clone()));
    send_audio_result(
        &results,
        &wake,
        playback_id,
        Ok(HostResultValue::AudioPlayback(initial)),
    );

    loop {
        match controls.recv_timeout(Duration::from_millis(150)) {
            Ok(message) => {
                let result = match message.action {
                    AudioPlaybackAction::Pause => {
                        sink.pause();
                        playback_status = AudioPlaybackStatus::Paused;
                        Ok(audio_playback_state(
                            playback_id,
                            playback_status,
                            &sink,
                            duration_seconds,
                        ))
                    }
                    AudioPlaybackAction::Resume => {
                        sink.play();
                        playback_status = AudioPlaybackStatus::Playing;
                        Ok(audio_playback_state(
                            playback_id,
                            playback_status,
                            &sink,
                            duration_seconds,
                        ))
                    }
                    AudioPlaybackAction::Seek { position_seconds } => {
                        if duration_seconds.map_or(false, |duration| position_seconds > duration) {
                            Err("audio seek position exceeds media duration".into())
                        } else {
                            sink.try_seek(Duration::from_secs_f64(position_seconds))
                                .map(|_| {
                                    audio_playback_state(
                                        playback_id,
                                        playback_status,
                                        &sink,
                                        duration_seconds,
                                    )
                                })
                                .map_err(|error| format!("audio seek failed: {error}"))
                        }
                    }
                    AudioPlaybackAction::Stop => {
                        sink.stop();
                        playback_status = AudioPlaybackStatus::Stopped;
                        Ok(audio_playback_state(
                            playback_id,
                            playback_status,
                            &sink,
                            duration_seconds,
                        ))
                    }
                };
                if let Ok(state) = &result {
                    send_audio_result(
                        &results,
                        &wake,
                        playback_id,
                        Ok(HostResultValue::AudioPlayback(state.clone())),
                    );
                }
                let should_stop = matches!(message.action, AudioPlaybackAction::Stop);
                let _ = message.reply.send(result);
                if should_stop {
                    break;
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                if sink.empty() {
                    playback_status = AudioPlaybackStatus::Finished;
                    let state =
                        audio_playback_state(playback_id, playback_status, &sink, duration_seconds);
                    send_audio_result(
                        &results,
                        &wake,
                        playback_id,
                        Ok(HostResultValue::AudioPlayback(state)),
                    );
                    break;
                }
                let state =
                    audio_playback_state(playback_id, playback_status, &sink, duration_seconds);
                send_audio_result(
                    &results,
                    &wake,
                    playback_id,
                    Ok(HostResultValue::AudioPlayback(state)),
                );
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if let Ok(mut sessions) = sessions.lock() {
        sessions.remove(&playback_id);
    }
}

fn audio_playback_state(
    playback_id: Uuid,
    state: AudioPlaybackStatus,
    sink: &rodio::Sink,
    duration_seconds: Option<f64>,
) -> AudioPlaybackState {
    AudioPlaybackState {
        playback_id,
        state,
        position_seconds: sink.get_pos().as_secs_f64(),
        duration_seconds,
    }
}

fn send_audio_result(
    results: &mpsc::UnboundedSender<HostResult>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    playback_id: Uuid,
    result: Result<HostResultValue, String>,
) {
    let _ = results.send(HostResult {
        request_id: playback_id,
        result,
    });
    wake();
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
async fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|error| format!("could not start {program}: {error}"))?;
    command_result(program, output).map(|_| ())
}

fn command_result(program: &str, output: std::process::Output) -> Result<HostResultValue, String> {
    if output.status.success() {
        return Ok(HostResultValue::Completed);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("{program} exited with {}", output.status)
    } else {
        format!("{program} failed: {stderr}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_window_geometry_is_rejected() {
        assert!(!valid_window_state(WindowState {
            width: Some(0.0),
            ..WindowState::default()
        }));
        assert!(!valid_window_state(WindowState {
            x: Some(f32::NAN),
            ..WindowState::default()
        }));
        assert!(valid_window_state(WindowState {
            x: Some(-20.0),
            width: Some(700.0),
            height: Some(500.0),
            ..WindowState::default()
        }));
    }

    #[test]
    fn url_opening_rejects_non_external_schemes() {
        let paths = test_paths();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(execute(
                HostAction::OpenUrl {
                    url: "javascript:alert(1)".into(),
                },
                paths.clone(),
                Arc::new(DictationEngine::new(std::env::temp_dir())),
                test_audio(),
                test_updates(),
                test_managed_daemon(),
                false,
            ));
        assert!(result
            .unwrap_err()
            .contains("unsupported external URL scheme"));
        std::fs::remove_dir_all(paths.data_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn open_in_command_passes_configured_arguments_as_distinct_argv_entries() {
        let root = std::env::temp_dir().join(format!("cm-open-in-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let output_path = root.join("argv.txt");
        let command_path = root.join("capture-argv");
        let quoted_output = format!("'{}'", output_path.to_string_lossy().replace('\'', "'\\''"));
        std::fs::write(
            &command_path,
            format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {quoted_output}\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&command_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let input_path = root.join("file with spaces.txt");
        std::fs::write(&input_path, "fixture").unwrap();
        let target = OpenAppTarget {
            id: "test-command".into(),
            label: "Test command".into(),
            kind: "command".into(),
            app_name: None,
            command: Some(command_path.to_string_lossy().into_owned()),
            args: vec!["--format=json".into(), "value with spaces".into()],
        };
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(open_in_target(target, input_path.clone()))
            .unwrap();

        assert_eq!(result, HostResultValue::Completed);
        assert_eq!(
            std::fs::read_to_string(&output_path).unwrap(),
            format!(
                "--format=json\nvalue with spaces\n{}\n",
                input_path.display()
            )
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn startup_error_dialog_is_skipped_when_linux_has_no_display() {
        #[cfg(target_os = "linux")]
        {
            let display = std::env::var_os("DISPLAY");
            let wayland = std::env::var_os("WAYLAND_DISPLAY");
            if display.as_ref().is_some_and(|value| !value.is_empty())
                || wayland.as_ref().is_some_and(|value| !value.is_empty())
            {
                return;
            }
            assert!(!show_startup_error_dialog("startup failure", "test"));
        }
    }

    #[test]
    fn close_window_uses_tray_policy_while_quit_remains_explicit() {
        let paths = test_paths();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let dictation = Arc::new(DictationEngine::new(std::env::temp_dir()));
        let hidden = runtime.block_on(execute(
            HostAction::CloseWindow,
            paths.clone(),
            Arc::clone(&dictation),
            test_audio(),
            test_updates(),
            test_managed_daemon(),
            true,
        ));
        assert_eq!(
            hidden.unwrap(),
            HostResultValue::WindowAction(WindowAction::HideToTray)
        );
        let closed = runtime.block_on(execute(
            HostAction::CloseWindow,
            paths.clone(),
            dictation.clone(),
            test_audio(),
            test_updates(),
            test_managed_daemon(),
            false,
        ));
        assert_eq!(
            closed.unwrap(),
            HostResultValue::WindowAction(WindowAction::Close)
        );
        let quit = runtime.block_on(execute(
            HostAction::QuitApplication,
            paths.clone(),
            dictation,
            test_audio(),
            test_updates(),
            test_managed_daemon(),
            true,
        ));
        assert_eq!(
            quit.unwrap(),
            HostResultValue::WindowAction(WindowAction::Quit)
        );
        std::fs::remove_dir_all(paths.data_dir).unwrap();
    }

    #[test]
    fn attachment_playback_rejects_unsupported_mime_before_opening_audio_device() {
        let paths = test_paths();
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(execute(
                HostAction::PlayAudioBytes {
                    playback_id: Uuid::new_v4(),
                    bytes: vec![1, 2, 3],
                    mime_type: "application/octet-stream".into(),
                },
                paths.clone(),
                Arc::new(DictationEngine::new(std::env::temp_dir())),
                test_audio(),
                test_updates(),
                test_managed_daemon(),
                false,
            ));
        assert!(result
            .unwrap_err()
            .contains("unsupported audio attachment type"));
        std::fs::remove_dir_all(paths.data_dir).unwrap();
    }

    #[test]
    fn audio_seek_rejects_non_finite_and_negative_positions() {
        let audio = test_audio();
        assert!(audio
            .control(
                Uuid::new_v4(),
                AudioPlaybackAction::Seek {
                    position_seconds: f64::NAN
                }
            )
            .unwrap_err()
            .contains("finite and nonnegative"));
        assert!(audio
            .control(
                Uuid::new_v4(),
                AudioPlaybackAction::Seek {
                    position_seconds: -1.0
                }
            )
            .unwrap_err()
            .contains("finite and nonnegative"));
    }

    #[test]
    fn window_state_persistence_roundtrips_and_rejects_bad_geometry() {
        let paths = test_paths();
        let state = WindowState {
            x: Some(-20.0),
            y: Some(40.0),
            width: Some(1200.0),
            height: Some(800.0),
            maximized: true,
        };
        persist_window_state(&paths, state).unwrap();
        let stored: WindowState =
            serde_json::from_slice(&std::fs::read(window_state_path(&paths)).unwrap()).unwrap();
        assert_eq!(stored, state);
        assert!(persist_window_state(
            &paths,
            WindowState {
                width: Some(f32::NAN),
                ..state
            }
        )
        .is_err());
        std::fs::remove_dir_all(paths.data_dir).unwrap();
    }

    #[test]
    fn prospective_staging_path_resolves_missing_descendants_without_creating_them() {
        let install_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let missing = install_dir.join(format!("cm-stage-must-not-exist-{}", Uuid::new_v4()));
        assert!(!missing.exists());
        let prospective = prospective_canonical_path(&missing).unwrap();
        assert!(prospective.starts_with(std::fs::canonicalize(install_dir).unwrap()));
        assert!(!missing.exists());
    }

    #[test]
    fn bundled_notification_sounds_are_decodable_without_an_audio_device() {
        use rodio::{Decoder, Source};
        use std::io::Cursor;
        for bytes in [
            include_bytes!("../../assets/success-notification.mp3").as_slice(),
            include_bytes!("../../assets/error-notification.mp3").as_slice(),
        ] {
            // These bundled MPEG streams do not always expose a duration
            // estimate. Decode their complete sample stream instead.
            let decoded_samples = Decoder::new(Cursor::new(bytes)).unwrap().count();
            assert!(decoded_samples > 0);
        }
    }

    fn test_paths() -> HostPaths {
        HostPaths::from_data_dir(
            std::env::temp_dir().join(format!("cm-platform-{}", Uuid::new_v4())),
        )
        .unwrap()
    }

    fn test_audio() -> AudioPlaybackService {
        let (sender, _receiver) = mpsc::unbounded_channel();
        AudioPlaybackService::new(sender, Arc::new(|| {}))
    }

    fn test_updates() -> Arc<Mutex<Option<UpdateRuntime>>> {
        Arc::new(Mutex::new(None))
    }

    fn test_managed_daemon() -> Arc<tokio::sync::Mutex<ManagedDaemonRuntime>> {
        Arc::new(tokio::sync::Mutex::new(ManagedDaemonRuntime::default()))
    }
}
