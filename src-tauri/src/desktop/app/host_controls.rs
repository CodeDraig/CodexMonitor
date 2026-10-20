//! Native controls for microphone dictation and cached audio playback.
//!
//! The view emits host actions and returns draft effects. It deliberately does
//! not know about `NativeApp`; callers decide when an effect is still current.

use crate::{
    client::contracts::ConversationKey,
    desktop::platform::{
        AudioPlaybackAction, AudioPlaybackState as HostAudioState, AudioPlaybackStatus, HostAction,
        HostResult, HostResultValue,
    },
    shared::dictation_core::{
        DictationEvent, DictationModelState, DictationModelStatus, DictationSessionState,
    },
    types::AppSettings,
};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DictationScope {
    pub conversation: ConversationKey,
    /// Changes whenever the selected backend connection is replaced.
    pub connection_epoch: u64,
    /// Incremented by the caller whenever the target draft changes.
    pub draft_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum HostControlsEffect {
    InsertTranscript { scope: DictationScope, text: String },
    Notice(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AudioPlaybackState {
    Idle,
    Starting {
        playback_id: Uuid,
    },
    Playing {
        playback_id: Uuid,
        position_seconds: f64,
        duration_seconds: Option<f64>,
    },
    Paused {
        playback_id: Uuid,
        position_seconds: f64,
        duration_seconds: Option<f64>,
    },
    Finished,
    Stopped,
    Error(String),
}

pub(crate) struct HostControls {
    model: Option<DictationModelStatus>,
    session: DictationSessionState,
    waveform_level: f32,
    error: Option<String>,
    notice: Option<String>,
    pending_scope: Option<DictationScope>,
    hold_was_down: bool,
    hold_release_pending: bool,
    audio: AudioPlaybackState,
    audio_seek_target: Option<(Uuid, f64)>,
}

impl Default for HostControls {
    fn default() -> Self {
        Self {
            model: None,
            session: DictationSessionState::Idle,
            waveform_level: 0.0,
            error: None,
            notice: None,
            pending_scope: None,
            hold_was_down: false,
            hold_release_pending: false,
            audio: AudioPlaybackState::Idle,
            audio_seek_target: None,
        }
    }
}

impl Default for AudioPlaybackState {
    fn default() -> Self {
        Self::Idle
    }
}

impl HostControls {
    pub(crate) fn needs_frequent_repaint(&self) -> bool {
        self.session != DictationSessionState::Idle
            || matches!(
                self.audio,
                AudioPlaybackState::Starting { .. } | AudioPlaybackState::Playing { .. }
            )
    }

    /// Preferences editor; the caller persists `settings` when this returns
    /// true. Keep names and values aligned with the previous dictation panel.
    pub(crate) fn show_preferences(ui: &mut egui::Ui, settings: &mut AppSettings) -> bool {
        let before = (
            settings.dictation_enabled,
            settings.dictation_model_id.clone(),
            settings.dictation_preferred_language.clone(),
            settings.dictation_hold_key.clone(),
        );
        ui.checkbox(
            &mut settings.dictation_enabled,
            "Enable microphone dictation",
        );
        egui::ComboBox::from_label("Dictation model")
            .selected_text(model_label(&settings.dictation_model_id))
            .show_ui(ui, |ui| {
                for (id, label) in [
                    ("tiny", "Tiny · 75 MB · fastest"),
                    ("base", "Base · 142 MB · balanced"),
                    ("small", "Small · 466 MB · better accuracy"),
                    ("medium", "Medium · 1.5 GB · high accuracy"),
                    ("large-v3", "Large V3 · 3.0 GB · best accuracy"),
                ] {
                    ui.selectable_value(&mut settings.dictation_model_id, id.into(), label);
                }
            });
        egui::ComboBox::from_label("Preferred language")
            .selected_text(language_label(
                settings.dictation_preferred_language.as_deref(),
            ))
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut settings.dictation_preferred_language,
                    None,
                    "Auto-detect",
                );
                for (id, name) in [
                    ("en", "English"),
                    ("es", "Spanish"),
                    ("fr", "French"),
                    ("de", "German"),
                    ("it", "Italian"),
                    ("pt", "Portuguese"),
                    ("nl", "Dutch"),
                    ("sv", "Swedish"),
                    ("no", "Norwegian"),
                    ("da", "Danish"),
                    ("fi", "Finnish"),
                    ("pl", "Polish"),
                    ("tr", "Turkish"),
                    ("ru", "Russian"),
                    ("uk", "Ukrainian"),
                    ("ja", "Japanese"),
                    ("ko", "Korean"),
                    ("zh", "Chinese"),
                ] {
                    ui.selectable_value(
                        &mut settings.dictation_preferred_language,
                        Some(id.into()),
                        name,
                    );
                }
            });
        egui::ComboBox::from_label("Hold-to-dictate key")
            .selected_text(hold_key_label(&settings.dictation_hold_key))
            .show_ui(ui, |ui| {
                for (id, label) in [
                    ("", "Off"),
                    ("alt", "Alt / Option"),
                    ("shift", "Shift"),
                    ("control", "Control"),
                    ("meta", "Meta / Command"),
                ] {
                    ui.selectable_value(&mut settings.dictation_hold_key, id.into(), label);
                }
            });
        before
            != (
                settings.dictation_enabled,
                settings.dictation_model_id.clone(),
                settings.dictation_preferred_language.clone(),
                settings.dictation_hold_key.clone(),
            )
    }

    pub(crate) fn model_status_action(settings: &AppSettings) -> HostAction {
        HostAction::DictationModelStatus {
            model_id: Some(settings.dictation_model_id.clone()),
        }
    }

    /// Render dictation controls in or beside the composer. Returned actions
    /// must be submitted to the host service by the caller.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        settings: &AppSettings,
        scope: Option<DictationScope>,
        permission_pending: bool,
    ) -> Vec<HostAction> {
        let mut actions = Vec::new();
        ui.horizontal_wrapped(|ui| {
            let status = self.model.as_ref();
            if settings.dictation_enabled {
                if let Some(status) = status {
                    match status.state {
                        DictationModelState::Ready => {
                            ui.label(format!("Dictation ready · {}", status.model_id));
                        }
                        DictationModelState::Missing => {
                            ui.label(format!(
                                "Dictation model {} is not installed",
                                status.model_id
                            ));
                            if ui.button("Download model").clicked() {
                                actions.push(HostAction::DictationDownloadModel {
                                    model_id: Some(settings.dictation_model_id.clone()),
                                });
                            }
                        }
                        DictationModelState::Downloading => {
                            let progress = status
                                .progress
                                .as_ref()
                                .map(|progress| match progress.total_bytes {
                                    Some(total) if total > 0 => format!(
                                        "{}%",
                                        progress.downloaded_bytes.saturating_mul(100) / total
                                    ),
                                    _ => format!("{} bytes", progress.downloaded_bytes),
                                })
                                .unwrap_or_else(|| "starting".into());
                            ui.label(format!("Downloading dictation model · {progress}"));
                            if ui.button("Cancel download").clicked() {
                                actions.push(HostAction::DictationCancelDownload);
                            }
                        }
                        DictationModelState::Error => {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                status.error.as_deref().unwrap_or("Dictation model error"),
                            );
                            if ui.button("Retry model status").clicked() {
                                actions.push(Self::model_status_action(settings));
                            }
                        }
                    }
                } else {
                    ui.label("Checking dictation model…");
                    if ui.small_button("Refresh").clicked() {
                        actions.push(Self::model_status_action(settings));
                    }
                }

                let model_ready = self
                    .model
                    .as_ref()
                    .is_some_and(|m| m.state == DictationModelState::Ready);
                let can_start = model_ready && scope.is_some() && !permission_pending;
                match self.session {
                    DictationSessionState::Idle => {
                        if ui
                            .add_enabled(can_start, egui::Button::new("Dictate by voice"))
                            .clicked()
                        {
                            if let Some(scope) = scope.clone() {
                                self.pending_scope = Some(scope);
                                self.error = None;
                                actions.push(HostAction::DictationStart {
                                    model_id: Some(settings.dictation_model_id.clone()),
                                    preferred_language: settings
                                        .dictation_preferred_language
                                        .clone(),
                                });
                            }
                        }
                    }
                    DictationSessionState::Listening => {
                        ui.colored_label(
                            egui::Color32::LIGHT_RED,
                            format!("Listening {}", waveform(self.waveform_level)),
                        );
                        if ui.button("Finish dictation").clicked() {
                            actions.push(HostAction::DictationStop {
                                preferred_language: settings.dictation_preferred_language.clone(),
                            });
                        }
                        if ui.button("Cancel dictation").clicked() {
                            self.pending_scope = None;
                            actions.push(HostAction::DictationCancel);
                        }
                    }
                    DictationSessionState::Processing => {
                        ui.spinner();
                        ui.label("Transcribing…");
                        if ui.button("Cancel dictation").clicked() {
                            self.pending_scope = None;
                            actions.push(HostAction::DictationCancel);
                        }
                    }
                }
            } else {
                ui.label("Dictation disabled in preferences");
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        if let Some(notice) = &self.notice {
            ui.weak(notice);
        }
        actions
    }

    /// Track the configured modifier key as a press/release pair. Call once
    /// each visible frame. Releasing while listening stops capture; losing the
    /// target or encountering a permission request cancels it safely.
    pub(crate) fn hold_to_talk(
        &mut self,
        ctx: &egui::Context,
        settings: &AppSettings,
        scope: Option<DictationScope>,
        permission_pending: bool,
    ) -> Vec<HostAction> {
        let down = ctx.input(|input| hold_key_down(input.modifiers, &settings.dictation_hold_key));
        let ready = self
            .model
            .as_ref()
            .is_some_and(|m| m.state == DictationModelState::Ready);
        let eligible =
            settings.dictation_enabled && ready && scope.is_some() && !permission_pending;
        let mut actions = Vec::new();
        if self.hold_was_down && scope.as_ref() != self.pending_scope.as_ref() {
            self.hold_was_down = false;
            self.pending_scope = None;
            self.hold_release_pending = false;
            actions.push(HostAction::DictationCancel);
        } else if !eligible && self.hold_was_down {
            self.hold_was_down = false;
            self.hold_release_pending = false;
            if self.pending_scope.is_some() || self.session == DictationSessionState::Listening {
                self.pending_scope = None;
                actions.push(HostAction::DictationCancel);
            }
        } else if eligible
            && down
            && !self.hold_was_down
            && self.session == DictationSessionState::Idle
        {
            if let Some(scope) = scope {
                self.pending_scope = Some(scope);
                self.error = None;
                self.hold_was_down = true;
                actions.push(HostAction::DictationStart {
                    model_id: Some(settings.dictation_model_id.clone()),
                    preferred_language: settings.dictation_preferred_language.clone(),
                });
            }
        } else if self.hold_was_down && !down {
            self.hold_was_down = false;
            if self.session == DictationSessionState::Listening {
                actions.push(HostAction::DictationStop {
                    preferred_language: settings.dictation_preferred_language.clone(),
                });
            } else {
                // Start is still crossing the host boundary. Defer stop until
                // the resulting listening state is observed.
                self.hold_release_pending = true;
            }
        } else if self.hold_release_pending && self.session == DictationSessionState::Listening {
            self.hold_release_pending = false;
            actions.push(HostAction::DictationStop {
                preferred_language: settings.dictation_preferred_language.clone(),
            });
        }
        actions
    }

    /// Process host replies/events. A transcript is returned only for the
    /// exact conversation, connection generation, and draft revision that
    /// initiated capture, and only when no permission UI is pending.
    pub(crate) fn handle_host_result(
        &mut self,
        result: &HostResult,
        current_scope: Option<&DictationScope>,
        permission_pending: bool,
    ) -> Vec<HostControlsEffect> {
        let mut effects = Vec::new();
        match &result.result {
            Err(error) => {
                self.error = Some(error.clone());
                if audio_playback_id(&self.audio) == Some(result.request_id) {
                    self.audio = AudioPlaybackState::Error(error.clone());
                    self.audio_seek_target = None;
                }
                self.session = DictationSessionState::Idle;
                self.pending_scope = None;
                self.hold_release_pending = false;
                effects.push(HostControlsEffect::Notice(error.clone()));
            }
            Ok(HostResultValue::DictationModel(status)) => {
                self.model = Some(status.clone());
            }
            Ok(HostResultValue::DictationState(state)) => {
                self.session = *state;
                if *state == DictationSessionState::Idle {
                    self.waveform_level = 0.0;
                }
            }
            Ok(HostResultValue::DictationEvent(event)) => match event {
                DictationEvent::State { state } => {
                    self.session = *state;
                    if *state == DictationSessionState::Idle {
                        self.waveform_level = 0.0;
                    }
                }
                DictationEvent::Level { value } => self.waveform_level = value.clamp(0.0, 1.0),
                DictationEvent::Model { status } => self.model = Some(status.clone()),
                DictationEvent::Error { message } => {
                    self.error = Some(message.clone());
                    self.session = DictationSessionState::Idle;
                    self.pending_scope = None;
                    self.hold_release_pending = false;
                    effects.push(HostControlsEffect::Notice(message.clone()));
                }
                DictationEvent::Canceled { message } => {
                    self.notice = Some(message.clone());
                    self.session = DictationSessionState::Idle;
                    self.pending_scope = None;
                    self.hold_release_pending = false;
                }
                DictationEvent::Transcript { text } => {
                    self.session = DictationSessionState::Idle;
                    self.waveform_level = 0.0;
                    self.hold_release_pending = false;
                    let target = self.pending_scope.take();
                    if let Some(target) = target {
                        if !permission_pending
                            && current_scope == Some(&target)
                            && !text.trim().is_empty()
                        {
                            effects.push(HostControlsEffect::InsertTranscript {
                                scope: target,
                                text: text.clone(),
                            });
                        } else if permission_pending {
                            self.notice = Some(
                                "Transcript discarded while a permission request is pending".into(),
                            );
                        } else if current_scope != Some(&target) {
                            self.notice = Some("Transcript discarded because its original draft is no longer active".into());
                        }
                    }
                }
            },
            Ok(HostResultValue::Completed) => {
                // Audio transport has its own state payload; unrelated
                // completed host operations do not affect playback.
            }
            Ok(HostResultValue::AudioPlayback(state)) => {
                if result.request_id == state.playback_id
                    && audio_playback_id(&self.audio) == Some(state.playback_id)
                {
                    self.audio = audio_view_state(state);
                    if matches!(
                        self.audio,
                        AudioPlaybackState::Finished | AudioPlaybackState::Stopped
                    ) {
                        self.audio_seek_target = None;
                    }
                }
            }
            _ => {}
        }
        effects
    }

    pub(crate) fn begin_audio(&mut self, bytes: Vec<u8>, mime_type: String) -> HostAction {
        let playback_id = Uuid::new_v4();
        self.audio = AudioPlaybackState::Starting { playback_id };
        self.audio_seek_target = None;
        HostAction::PlayAudioBytes {
            playback_id,
            bytes,
            mime_type,
        }
    }

    pub(crate) fn audio_control_action(
        &mut self,
        action: AudioPlaybackAction,
    ) -> Option<HostAction> {
        let playback_id = audio_playback_id(&self.audio)?;
        let action = match action {
            AudioPlaybackAction::Seek { position_seconds } => {
                if !position_seconds.is_finite() {
                    return None;
                }
                let duration = audio_duration(&self.audio)?;
                AudioPlaybackAction::Seek {
                    position_seconds: position_seconds.clamp(0.0, duration),
                }
            }
            other => other,
        };
        Some(HostAction::AudioControl {
            playback_id,
            action,
        })
    }

    pub(crate) fn audio_status(&self) -> &AudioPlaybackState {
        &self.audio
    }

    /// Transport controls are normal egui buttons and a labeled slider, so
    /// they remain operable by keyboard navigation and screen-reader tooling.
    pub(crate) fn show_audio_status(&mut self, ui: &mut egui::Ui) -> Vec<HostAction> {
        let mut actions = Vec::new();
        match self.audio.clone() {
            AudioPlaybackState::Idle => {}
            AudioPlaybackState::Starting { .. } => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Loading audio attachment…");
                });
            }
            AudioPlaybackState::Playing {
                playback_id,
                position_seconds,
                duration_seconds,
            } => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Pause audio").clicked() {
                        if let Some(action) = self.audio_control_action(AudioPlaybackAction::Pause)
                        {
                            actions.push(action);
                        }
                    }
                    if ui.button("Stop audio").clicked() {
                        if let Some(action) = self.audio_control_action(AudioPlaybackAction::Stop) {
                            actions.push(action);
                        }
                    }
                    if let Some(duration) = duration_seconds.filter(|d| d.is_finite() && *d > 0.0) {
                        let initial = self
                            .audio_seek_target
                            .filter(|(id, _)| *id == playback_id)
                            .map(|(_, target)| target)
                            .unwrap_or(position_seconds)
                            .clamp(0.0, duration);
                        let mut seek = initial;
                        let response = ui.add(
                            egui::Slider::new(&mut seek, 0.0..=duration)
                                .text("Audio position")
                                .show_value(true),
                        );
                        if response.changed() {
                            self.audio_seek_target = Some((playback_id, seek));
                        }
                        if response.drag_stopped() || (response.changed() && !response.dragged()) {
                            if let Some(action) =
                                self.audio_control_action(AudioPlaybackAction::Seek {
                                    position_seconds: seek,
                                })
                            {
                                actions.push(action);
                                self.audio_seek_target = None;
                            }
                        }
                    } else {
                        ui.label(format!(
                            "{} · seek unavailable",
                            format_time(position_seconds)
                        ));
                    }
                });
            }
            AudioPlaybackState::Paused {
                playback_id,
                position_seconds,
                duration_seconds,
            } => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Resume audio").clicked() {
                        if let Some(action) = self.audio_control_action(AudioPlaybackAction::Resume)
                        {
                            actions.push(action);
                        }
                    }
                    if ui.button("Stop audio").clicked() {
                        if let Some(action) = self.audio_control_action(AudioPlaybackAction::Stop) {
                            actions.push(action);
                        }
                    }
                    if let Some(duration) = duration_seconds.filter(|d| d.is_finite() && *d > 0.0) {
                        let initial = self
                            .audio_seek_target
                            .filter(|(id, _)| *id == playback_id)
                            .map(|(_, target)| target)
                            .unwrap_or(position_seconds)
                            .clamp(0.0, duration);
                        let mut seek = initial;
                        let response = ui.add(
                            egui::Slider::new(&mut seek, 0.0..=duration)
                                .text("Audio position")
                                .show_value(true),
                        );
                        if response.changed() {
                            self.audio_seek_target = Some((playback_id, seek));
                        }
                        if response.drag_stopped() || (response.changed() && !response.dragged()) {
                            if let Some(action) =
                                self.audio_control_action(AudioPlaybackAction::Seek {
                                    position_seconds: seek,
                                })
                            {
                                actions.push(action);
                                self.audio_seek_target = None;
                            }
                        }
                    } else {
                        ui.label(format!(
                            "{} · seek unavailable",
                            format_time(position_seconds)
                        ));
                    }
                });
            }
            AudioPlaybackState::Finished => {
                ui.label("Audio playback finished");
            }
            AudioPlaybackState::Stopped => {
                ui.label("Audio playback stopped");
            }
            AudioPlaybackState::Error(message) => {
                ui.colored_label(egui::Color32::LIGHT_RED, message);
            }
        }
        actions
    }
}

fn audio_playback_id(state: &AudioPlaybackState) -> Option<Uuid> {
    match state {
        AudioPlaybackState::Starting { playback_id }
        | AudioPlaybackState::Playing { playback_id, .. }
        | AudioPlaybackState::Paused { playback_id, .. } => Some(*playback_id),
        AudioPlaybackState::Idle
        | AudioPlaybackState::Finished
        | AudioPlaybackState::Stopped
        | AudioPlaybackState::Error(_) => None,
    }
}

fn audio_duration(state: &AudioPlaybackState) -> Option<f64> {
    match state {
        AudioPlaybackState::Playing {
            duration_seconds, ..
        }
        | AudioPlaybackState::Paused {
            duration_seconds, ..
        } => duration_seconds.filter(|duration| duration.is_finite() && *duration > 0.0),
        _ => None,
    }
}

fn audio_view_state(state: &HostAudioState) -> AudioPlaybackState {
    match state.state {
        AudioPlaybackStatus::Playing => AudioPlaybackState::Playing {
            playback_id: state.playback_id,
            position_seconds: state.position_seconds.max(0.0),
            duration_seconds: state.duration_seconds,
        },
        AudioPlaybackStatus::Paused => AudioPlaybackState::Paused {
            playback_id: state.playback_id,
            position_seconds: state.position_seconds.max(0.0),
            duration_seconds: state.duration_seconds,
        },
        AudioPlaybackStatus::Finished => AudioPlaybackState::Finished,
        AudioPlaybackStatus::Stopped => AudioPlaybackState::Stopped,
    }
}

fn format_time(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn model_label(id: &str) -> &'static str {
    match id {
        "tiny" => "Tiny · 75 MB",
        "base" => "Base · 142 MB",
        "small" => "Small · 466 MB",
        "medium" => "Medium · 1.5 GB",
        "large-v3" => "Large V3 · 3.0 GB",
        _ => "Unknown model",
    }
}

fn language_label(language: Option<&str>) -> &'static str {
    match language {
        None => "Auto-detect",
        Some("en") => "English",
        Some("es") => "Spanish",
        Some("fr") => "French",
        Some("de") => "German",
        Some("it") => "Italian",
        Some("pt") => "Portuguese",
        Some("nl") => "Dutch",
        Some("sv") => "Swedish",
        Some("no") => "Norwegian",
        Some("da") => "Danish",
        Some("fi") => "Finnish",
        Some("pl") => "Polish",
        Some("tr") => "Turkish",
        Some("ru") => "Russian",
        Some("uk") => "Ukrainian",
        Some("ja") => "Japanese",
        Some("ko") => "Korean",
        Some("zh") => "Chinese",
        _ => "Other",
    }
}

fn hold_key_label(key: &str) -> &'static str {
    match key {
        "alt" => "Alt / Option",
        "shift" => "Shift",
        "control" => "Control",
        "meta" => "Meta / Command",
        _ => "Off",
    }
}

fn waveform(value: f32) -> String {
    const BARS: usize = 8;
    let active = (value.clamp(0.0, 1.0) * BARS as f32).round() as usize;
    format!("{}{}", "▮".repeat(active), "▯".repeat(BARS - active))
}

fn hold_key_down(modifiers: egui::Modifiers, hold_key: &str) -> bool {
    match hold_key.to_ascii_lowercase().as_str() {
        "alt" => modifiers.alt,
        "shift" => modifiers.shift,
        "control" => modifiers.ctrl,
        "meta" => modifiers.mac_cmd,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::contracts::ConversationRef;
    use uuid::Uuid;

    fn scope(thread: &str, epoch: u64, draft_revision: u64) -> DictationScope {
        DictationScope {
            conversation: ConversationKey {
                endpoint_id: "local".into(),
                conversation: ConversationRef::Codex {
                    workspace_id: "workspace".into(),
                    thread_id: thread.into(),
                },
            },
            connection_epoch: epoch,
            draft_revision,
        }
    }

    fn transcript(
        controls: &mut HostControls,
        current: Option<&DictationScope>,
        pending: bool,
    ) -> Vec<HostControlsEffect> {
        let result = HostResult {
            request_id: Uuid::nil(),
            result: Ok(HostResultValue::DictationEvent(
                DictationEvent::Transcript {
                    text: "spoken words".into(),
                },
            )),
        };
        controls.handle_host_result(&result, current, pending)
    }

    #[test]
    fn transcript_only_targets_the_original_conversation_epoch_and_draft() {
        let original = scope("thread-a", 7, 12);
        let mut controls = HostControls {
            pending_scope: Some(original.clone()),
            ..Default::default()
        };
        assert!(transcript(&mut controls, Some(&scope("thread-b", 7, 12)), false).is_empty());

        controls.pending_scope = Some(original.clone());
        assert!(transcript(&mut controls, Some(&scope("thread-a", 8, 12)), false).is_empty());

        controls.pending_scope = Some(original.clone());
        assert!(transcript(&mut controls, Some(&scope("thread-a", 7, 13)), false).is_empty());

        controls.pending_scope = Some(original.clone());
        assert_eq!(
            transcript(&mut controls, Some(&original), false),
            vec![HostControlsEffect::InsertTranscript {
                scope: original,
                text: "spoken words".into(),
            }]
        );
    }

    #[test]
    fn transcript_does_not_arrive_during_pending_permission() {
        let original = scope("thread-a", 7, 12);
        let mut controls = HostControls {
            pending_scope: Some(original.clone()),
            ..Default::default()
        };
        assert!(transcript(&mut controls, Some(&original), true).is_empty());
        assert!(controls.pending_scope.is_none());
    }

    #[test]
    fn configured_hold_modifier_is_recognized() {
        assert!(hold_key_down(
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
            "alt"
        ));
        assert!(!hold_key_down(
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
            "control"
        ));
    }

    #[test]
    fn playback_controls_use_the_current_playback_and_clamp_seek() {
        let mut controls = HostControls::default();
        let action = controls.begin_audio(vec![1, 2, 3], "audio/wav".into());
        let playback_id = match action {
            HostAction::PlayAudioBytes { playback_id, .. } => playback_id,
            _ => panic!("expected audio playback action"),
        };
        controls.audio = AudioPlaybackState::Playing {
            playback_id,
            position_seconds: 2.0,
            duration_seconds: Some(8.0),
        };
        assert!(matches!(
            controls.audio_control_action(AudioPlaybackAction::Pause),
            Some(HostAction::AudioControl {
                playback_id: id,
                action: AudioPlaybackAction::Pause,
            }) if id == playback_id
        ));
        let seek = controls
            .audio_control_action(AudioPlaybackAction::Seek {
                position_seconds: 99.0,
            })
            .expect("seek action");
        let HostAction::AudioControl {
            playback_id: id,
            action: AudioPlaybackAction::Seek { position_seconds },
        } = seek
        else {
            panic!("expected seek action");
        };
        assert_eq!(id, playback_id);
        assert_eq!(position_seconds, 8.0);
    }

    #[test]
    fn stale_playback_updates_cannot_replace_the_active_audio() {
        let mut controls = HostControls::default();
        let HostAction::PlayAudioBytes { playback_id, .. } =
            controls.begin_audio(vec![1], "audio/wav".into())
        else {
            panic!("expected audio playback action");
        };
        let result = HostResult {
            request_id: Uuid::new_v4(),
            result: Ok(HostResultValue::AudioPlayback(HostAudioState {
                playback_id: Uuid::new_v4(),
                state: AudioPlaybackStatus::Paused,
                position_seconds: 3.0,
                duration_seconds: Some(10.0),
            })),
        };
        controls.handle_host_result(&result, None, false);
        assert_eq!(
            controls.audio_status(),
            &AudioPlaybackState::Starting { playback_id }
        );
    }
}
