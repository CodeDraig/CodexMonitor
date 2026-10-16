use crate::state::AppState;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::oneshot;
use whisper_rs::{WhisperContext, WhisperContextParameters};

#[path = "desktop/capture.rs"]
mod capture;
#[path = "desktop/models.rs"]
mod models;
#[path = "desktop/permissions.rs"]
mod permissions;
#[path = "desktop/transcription.rs"]
mod transcription;

use capture::start_capture_thread;
use models::{missing_status, model_path, refresh_status, resolve_model_id, DEFAULT_MODEL_ID};
use permissions::request_microphone_permission;
use transcription::transcribe_audio;

pub(crate) use models::*;

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DictationSessionState {
    Idle,
    Listening,
    Processing,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DictationEvent {
    State { state: DictationSessionState },
    Level { value: f32 },
    Transcript { text: String },
    Error { message: String },
    Canceled { message: String },
}

pub(crate) struct DictationSessionHandle {
    pub(crate) stop: mpsc::Sender<()>,
    pub(crate) stopped: oneshot::Receiver<()>,
    pub(crate) audio: Arc<Mutex<Vec<f32>>>,
    pub(crate) sample_rate: u32,
    pub(crate) model_id: String,
    pub(crate) preferred_language: Option<String>,
}

pub(crate) struct DictationState {
    pub(crate) model_status: DictationModelStatus,
    pub(crate) download_cancel: Option<Arc<AtomicBool>>,
    pub(crate) download_task: Option<tokio::task::JoinHandle<()>>,
    pub(crate) session_state: DictationSessionState,
    pub(crate) session: Option<DictationSessionHandle>,
    pub(crate) processing_cancel: Option<Arc<AtomicBool>>,
    pub(crate) cached_context: Option<CachedWhisperContext>,
}

pub(crate) struct CachedWhisperContext {
    pub(crate) model_id: String,
    pub(crate) context: Arc<WhisperContext>,
}

impl Default for DictationState {
    fn default() -> Self {
        Self {
            model_status: missing_status(DEFAULT_MODEL_ID),
            download_cancel: None,
            download_task: None,
            session_state: DictationSessionState::Idle,
            session: None,
            processing_cancel: None,
            cached_context: None,
        }
    }
}

fn emit_event(app: &AppHandle, event: DictationEvent) {
    let _ = app.emit("dictation-event", event);
}

async fn clear_processing_cancel(app: &AppHandle, cancel_flag: &Arc<AtomicBool>) -> bool {
    let state_handle = app.state::<AppState>();
    let mut dictation = state_handle.dictation.lock().await;
    if dictation
        .processing_cancel
        .as_ref()
        .map_or(false, |flag| Arc::ptr_eq(flag, cancel_flag))
    {
        dictation.processing_cancel = None;
        return true;
    }
    false
}

#[tauri::command]
pub(crate) async fn dictation_start(
    preferred_language: Option<String>,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DictationSessionState, String> {
    let model_id = resolve_model_id(&state, None).await;
    let model_status = refresh_status(&app, &state, &model_id).await;
    if model_status.state != DictationModelState::Ready {
        let message = "Dictation model is not downloaded yet.".to_string();
        emit_event(
            &app,
            DictationEvent::Error {
                message: message.clone(),
            },
        );
        return Err(message);
    }
    {
        let dictation = state.dictation.lock().await;
        if dictation.session_state != DictationSessionState::Idle {
            let message = "Dictation is already active.".to_string();
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
    }

    // Request microphone permission before attempting to capture audio
    match request_microphone_permission(&app).await {
        Ok(true) => {
            // Permission granted, continue
        }
        Ok(false) => {
            let message = "Microphone access was denied. Please grant microphone permission in System Settings > Privacy & Security > Microphone.".to_string();
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
        Err(error) => {
            emit_event(
                &app,
                DictationEvent::Error {
                    message: error.clone(),
                },
            );
            return Err(error);
        }
    }

    let audio = Arc::new(Mutex::new(Vec::new()));
    let (stop_tx, stop_rx) = mpsc::channel();
    let stop_tx_thread = stop_tx.clone();
    let (ready_tx, ready_rx) = oneshot::channel();
    let (stopped_tx, stopped_rx) = oneshot::channel();
    let app_handle = app.clone();
    let preferred_clone = preferred_language.clone();
    let audio_capture = audio.clone();

    std::thread::spawn(move || {
        start_capture_thread(
            app_handle,
            audio_capture,
            stop_rx,
            stop_tx_thread,
            stopped_tx,
            ready_tx,
        );
    });

    let sample_rate = match ready_rx.await {
        Ok(Ok(rate)) => rate,
        Ok(Err(message)) => {
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
        Err(_) => {
            let message = "Failed to start microphone capture.".to_string();
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
    };

    {
        let mut dictation = state.dictation.lock().await;
        dictation.session_state = DictationSessionState::Listening;
        dictation.session = Some(DictationSessionHandle {
            stop: stop_tx,
            stopped: stopped_rx,
            audio,
            sample_rate,
            model_id: model_id.clone(),
            preferred_language: preferred_clone,
        });
    }

    emit_event(
        &app,
        DictationEvent::State {
            state: DictationSessionState::Listening,
        },
    );

    Ok(DictationSessionState::Listening)
}

#[tauri::command]
pub(crate) async fn dictation_request_permission(app: AppHandle) -> Result<bool, String> {
    request_microphone_permission(&app).await
}

#[tauri::command]
pub(crate) async fn dictation_stop(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DictationSessionState, String> {
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let (audio, sample_rate, model_id, preferred_language, stopped, stop_tx) = {
        let mut dictation = state.dictation.lock().await;
        if dictation.session_state != DictationSessionState::Listening {
            let message = "Dictation is not currently listening.".to_string();
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
        dictation.session_state = DictationSessionState::Processing;
        dictation.processing_cancel = Some(Arc::clone(&cancel_flag));
        let session = dictation
            .session
            .take()
            .ok_or_else(|| "Dictation session is unavailable.".to_string())?;
        (
            session.audio,
            session.sample_rate,
            session.model_id,
            session.preferred_language,
            session.stopped,
            session.stop,
        )
    };

    emit_event(
        &app,
        DictationEvent::State {
            state: DictationSessionState::Processing,
        },
    );

    let app_handle = app.clone();
    let _ = stop_tx.send(());
    let _ = stopped.await;
    tokio::spawn(async move {
        let samples = {
            let mut guard = audio.lock().unwrap();
            let captured = guard.clone();
            guard.clear();
            captured
        };
        if cancel_flag.load(Ordering::Relaxed) {
            clear_processing_cancel(&app_handle, &cancel_flag).await;
            return;
        }

        let state_handle = app_handle.state::<AppState>();
        let cached_context = {
            let dictation = state_handle.dictation.lock().await;
            dictation
                .cached_context
                .as_ref()
                .filter(|cached| cached.model_id == model_id)
                .map(|cached| Arc::clone(&cached.context))
        };

        let context = if let Some(context) = cached_context {
            context
        } else {
            let model_path = match model_path(&app_handle, &model_id) {
                Ok(path) => path,
                Err(error) => {
                    emit_event(&app_handle, DictationEvent::Error { message: error });
                    let mut dictation = state_handle.dictation.lock().await;
                    dictation.session_state = DictationSessionState::Idle;
                    emit_event(
                        &app_handle,
                        DictationEvent::State {
                            state: DictationSessionState::Idle,
                        },
                    );
                    return;
                }
            };
            let path = model_path.to_string_lossy().into_owned();
            let created = tokio::task::spawn_blocking(move || {
                WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            })
            .await;
            let context = match created {
                Ok(Ok(context)) => context,
                Ok(Err(error)) => {
                    emit_event(
                        &app_handle,
                        DictationEvent::Error {
                            message: format!("Failed to load Whisper model: {error}"),
                        },
                    );
                    let mut dictation = state_handle.dictation.lock().await;
                    dictation.session_state = DictationSessionState::Idle;
                    emit_event(
                        &app_handle,
                        DictationEvent::State {
                            state: DictationSessionState::Idle,
                        },
                    );
                    return;
                }
                Err(error) => {
                    emit_event(
                        &app_handle,
                        DictationEvent::Error {
                            message: format!("Failed to load Whisper model: {error}"),
                        },
                    );
                    let mut dictation = state_handle.dictation.lock().await;
                    dictation.session_state = DictationSessionState::Idle;
                    emit_event(
                        &app_handle,
                        DictationEvent::State {
                            state: DictationSessionState::Idle,
                        },
                    );
                    return;
                }
            };
            let context = Arc::new(context);
            let mut dictation = state_handle.dictation.lock().await;
            dictation.cached_context = Some(CachedWhisperContext {
                model_id: model_id.clone(),
                context: Arc::clone(&context),
            });
            context
        };

        let preferred = preferred_language.clone();

        let result = tokio::task::spawn_blocking(move || {
            transcribe_audio(samples, sample_rate, &context, preferred)
        })
        .await;

        let outcome = match result {
            Ok(result) => result,
            Err(error) => Err(format!("Transcription task failed: {error}")),
        };

        if cancel_flag.load(Ordering::Relaxed) {
            clear_processing_cancel(&app_handle, &cancel_flag).await;
            return;
        }

        match outcome {
            Ok(text) => {
                if !text.trim().is_empty() {
                    emit_event(&app_handle, DictationEvent::Transcript { text });
                }
            }
            Err(message) => {
                emit_event(&app_handle, DictationEvent::Error { message });
            }
        }

        clear_processing_cancel(&app_handle, &cancel_flag).await;
        let state_handle = app_handle.state::<AppState>();
        let mut dictation = state_handle.dictation.lock().await;
        dictation.session_state = DictationSessionState::Idle;
        emit_event(
            &app_handle,
            DictationEvent::State {
                state: DictationSessionState::Idle,
            },
        );
    });

    Ok(DictationSessionState::Processing)
}

#[tauri::command]
pub(crate) async fn dictation_cancel(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<DictationSessionState, String> {
    {
        let mut dictation = state.dictation.lock().await;
        if dictation.session_state == DictationSessionState::Processing {
            if let Some(flag) = dictation.processing_cancel.take() {
                flag.store(true, Ordering::Relaxed);
            }
            dictation.session_state = DictationSessionState::Idle;
            emit_event(
                &app,
                DictationEvent::State {
                    state: DictationSessionState::Idle,
                },
            );
            emit_event(
                &app,
                DictationEvent::Canceled {
                    message: "Canceled".to_string(),
                },
            );
            return Ok(DictationSessionState::Idle);
        }
    }
    let (audio, stopped, stop_tx) = {
        let mut dictation = state.dictation.lock().await;
        if dictation.session_state != DictationSessionState::Listening {
            let message = "Dictation is not currently listening.".to_string();
            emit_event(
                &app,
                DictationEvent::Error {
                    message: message.clone(),
                },
            );
            return Err(message);
        }
        dictation.session_state = DictationSessionState::Idle;
        let session = dictation
            .session
            .take()
            .ok_or_else(|| "Dictation session is unavailable.".to_string())?;
        (session.audio, session.stopped, session.stop)
    };

    let _ = stop_tx.send(());
    let _ = stopped.await;
    {
        let mut guard = audio.lock().unwrap();
        guard.clear();
    }

    emit_event(
        &app,
        DictationEvent::State {
            state: DictationSessionState::Idle,
        },
    );
    emit_event(
        &app,
        DictationEvent::Canceled {
            message: "Canceled".to_string(),
        },
    );

    Ok(DictationSessionState::Idle)
}
