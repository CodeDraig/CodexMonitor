use crate::state::AppState;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::io::AsyncWriteExt;

pub(super) const DEFAULT_MODEL_ID: &str = "base";

struct DictationModelInfo {
    id: &'static str,
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const MODEL_CATALOG: &[DictationModelInfo] = &[
    DictationModelInfo {
        id: "tiny",
        filename: "ggml-tiny.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    DictationModelInfo {
        id: "base",
        filename: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    DictationModelInfo {
        id: "small",
        filename: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    DictationModelInfo {
        id: "medium",
        filename: "ggml-medium.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    DictationModelInfo {
        id: "large-v3",
        filename: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
];

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DictationModelState {
    Missing,
    Downloading,
    Ready,
    Error,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DictationDownloadProgress {
    #[serde(rename = "downloadedBytes")]
    pub(crate) downloaded_bytes: u64,
    #[serde(rename = "totalBytes")]
    pub(crate) total_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Clone)]
pub(crate) struct DictationModelStatus {
    pub(crate) state: DictationModelState,
    #[serde(rename = "modelId")]
    pub(crate) model_id: String,
    pub(crate) progress: Option<DictationDownloadProgress>,
    pub(crate) error: Option<String>,
    pub(crate) path: Option<String>,
}

fn model_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| ".".into()))
        .join("models")
        .join("whisper")
}

fn model_info(model_id: &str) -> Option<&'static DictationModelInfo> {
    MODEL_CATALOG.iter().find(|info| info.id == model_id)
}

pub(super) fn model_path(app: &AppHandle, model_id: &str) -> Result<PathBuf, String> {
    let info =
        model_info(model_id).ok_or_else(|| format!("Unknown dictation model: {model_id}"))?;
    Ok(model_dir(app).join(info.filename))
}

fn model_temp_path(app: &AppHandle, model_id: &str) -> Result<PathBuf, String> {
    let info =
        model_info(model_id).ok_or_else(|| format!("Unknown dictation model: {model_id}"))?;
    Ok(model_dir(app).join(format!("{}.partial", info.filename)))
}

pub(super) fn missing_status(model_id: &str) -> DictationModelStatus {
    DictationModelStatus {
        state: DictationModelState::Missing,
        model_id: model_id.to_string(),
        progress: None,
        error: None,
        path: None,
    }
}

fn ready_status(model_id: &str, path: &PathBuf) -> DictationModelStatus {
    DictationModelStatus {
        state: DictationModelState::Ready,
        model_id: model_id.to_string(),
        progress: None,
        error: None,
        path: Some(path.to_string_lossy().to_string()),
    }
}

fn emit_status(app: &AppHandle, status: &DictationModelStatus) {
    let _ = app.emit("dictation-download", status);
}

async fn update_status(app: &AppHandle, state: &State<'_, AppState>, status: DictationModelStatus) {
    {
        let mut dictation = state.dictation.lock().await;
        dictation.model_status = status.clone();
    }
    emit_status(app, &status);
}

async fn clear_download_state(state: &State<'_, AppState>) {
    let mut dictation = state.dictation.lock().await;
    dictation.download_cancel = None;
    dictation.download_task = None;
}

pub(super) async fn resolve_model_id(
    state: &State<'_, AppState>,
    model_id: Option<String>,
) -> String {
    let candidate = if let Some(model_id) = model_id {
        model_id
    } else {
        let settings = state.app_settings.lock().await;
        if settings.dictation_model_id.trim().is_empty() {
            DEFAULT_MODEL_ID.to_string()
        } else {
            settings.dictation_model_id.clone()
        }
    };
    if model_info(&candidate).is_some() {
        candidate
    } else {
        DEFAULT_MODEL_ID.to_string()
    }
}

pub(super) async fn refresh_status(
    app: &AppHandle,
    state: &State<'_, AppState>,
    model_id: &str,
) -> DictationModelStatus {
    let mut dictation = state.dictation.lock().await;
    if dictation.model_status.state == DictationModelState::Downloading
        && dictation.model_status.model_id == model_id
    {
        return dictation.model_status.clone();
    }

    let path = match model_path(app, model_id) {
        Ok(path) => path,
        Err(error) => {
            dictation.model_status = DictationModelStatus {
                state: DictationModelState::Error,
                model_id: model_id.to_string(),
                progress: None,
                error: Some(error),
                path: None,
            };
            return dictation.model_status.clone();
        }
    };

    if path.exists() {
        dictation.model_status = ready_status(model_id, &path);
    } else {
        dictation.model_status = missing_status(model_id);
    }
    dictation.model_status.clone()
}

#[tauri::command]
pub(crate) async fn dictation_model_status(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<DictationModelStatus, String> {
    let model_id = resolve_model_id(&state, model_id).await;
    Ok(refresh_status(&app, &state, &model_id).await)
}

#[tauri::command]
pub(crate) async fn dictation_download_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<DictationModelStatus, String> {
    let model_id = resolve_model_id(&state, model_id).await;
    let current = refresh_status(&app, &state, &model_id).await;
    if current.state == DictationModelState::Ready {
        return Ok(current);
    }
    if current.state == DictationModelState::Downloading && current.model_id == model_id {
        return Ok(current);
    }

    let cancel_flag = Arc::new(AtomicBool::new(false));
    {
        let mut dictation = state.dictation.lock().await;
        if dictation.model_status.state == DictationModelState::Downloading
            && dictation.model_status.model_id != model_id
        {
            if let Some(flag) = dictation.download_cancel.take() {
                flag.store(true, Ordering::SeqCst);
            }
            if let Some(task) = dictation.download_task.take() {
                task.abort();
            }
        }
        dictation.download_cancel = Some(cancel_flag.clone());
        dictation.model_status = DictationModelStatus {
            state: DictationModelState::Downloading,
            model_id: model_id.clone(),
            progress: Some(DictationDownloadProgress {
                downloaded_bytes: 0,
                total_bytes: None,
            }),
            error: None,
            path: None,
        };
    }
    emit_status(&app, &refresh_status(&app, &state, &model_id).await);

    let app_handle = app.clone();
    let model_id_clone = model_id.clone();
    let task = tokio::spawn(async move {
        let state = app_handle.state::<AppState>();
        let model_dir = model_dir(&app_handle);
        let model_path = match model_path(&app_handle, &model_id_clone) {
            Ok(path) => path,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(error),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };
        let temp_path = match model_temp_path(&app_handle, &model_id_clone) {
            Ok(path) => path,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(error),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };

        if let Err(error) = tokio::fs::create_dir_all(&model_dir).await {
            let status = DictationModelStatus {
                state: DictationModelState::Error,
                model_id: model_id_clone.clone(),
                progress: None,
                error: Some(format!("Failed to create model directory: {error}")),
                path: None,
            };
            update_status(&app_handle, &state, status).await;
            clear_download_state(&state).await;
            return;
        }

        let (url, expected_sha) = match model_info(&model_id_clone) {
            Some(info) => (info.url, info.sha256),
            None => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some("Unknown dictation model.".to_string()),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };
        let client = match reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30 * 60))
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(format!("Failed to configure download client: {error}")),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };
        let response = match client.get(url).send().await {
            Ok(response) => response,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(format!("Failed to download model: {error}")),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };
        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(format!("Model download failed: {error}")),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };

        let total = response.content_length();
        let mut downloaded = 0u64;
        let mut file = match tokio::fs::File::create(&temp_path).await {
            Ok(file) => file,
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(format!("Failed to write model: {error}")),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
        };

        let mut response = response;
        let mut hasher = Sha256::new();
        let mut last_progress = Instant::now();
        loop {
            let cancel = {
                let dictation = state.dictation.lock().await;
                dictation
                    .download_cancel
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Relaxed))
                    .unwrap_or(false)
            };
            if cancel {
                let _ = tokio::fs::remove_file(&temp_path).await;
                let status = missing_status(&model_id_clone);
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }

            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(error) => {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                    let status = DictationModelStatus {
                        state: DictationModelState::Error,
                        model_id: model_id_clone.clone(),
                        progress: None,
                        error: Some(format!("Model download failed: {error}")),
                        path: None,
                    };
                    update_status(&app_handle, &state, status).await;
                    clear_download_state(&state).await;
                    return;
                }
            };

            if let Err(error) = file.write_all(&chunk).await {
                let _ = tokio::fs::remove_file(&temp_path).await;
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: model_id_clone.clone(),
                    progress: None,
                    error: Some(format!("Failed to write model: {error}")),
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
                clear_download_state(&state).await;
                return;
            }
            downloaded += chunk.len() as u64;
            hasher.update(&chunk);

            if last_progress.elapsed() >= Duration::from_millis(150) {
                last_progress = Instant::now();
                let status = DictationModelStatus {
                    state: DictationModelState::Downloading,
                    model_id: model_id_clone.clone(),
                    progress: Some(DictationDownloadProgress {
                        downloaded_bytes: downloaded,
                        total_bytes: total,
                    }),
                    error: None,
                    path: None,
                };
                update_status(&app_handle, &state, status).await;
            }
        }

        let hash = hasher.finalize();
        let mut hash_hex = String::with_capacity(64);
        for byte in hash {
            use std::fmt::Write;
            let _ = write!(&mut hash_hex, "{:02x}", byte);
        }
        if hash_hex != expected_sha {
            let _ = tokio::fs::remove_file(&temp_path).await;
            let status = DictationModelStatus {
                state: DictationModelState::Error,
                model_id: model_id_clone.clone(),
                progress: None,
                error: Some("Model hash mismatch; download canceled.".to_string()),
                path: None,
            };
            update_status(&app_handle, &state, status).await;
            clear_download_state(&state).await;
            return;
        }

        if let Err(error) = file.flush().await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            let status = DictationModelStatus {
                state: DictationModelState::Error,
                model_id: model_id_clone.clone(),
                progress: None,
                error: Some(format!("Failed to finalize model: {error}")),
                path: None,
            };
            update_status(&app_handle, &state, status).await;
            clear_download_state(&state).await;
            return;
        }

        if let Err(error) = tokio::fs::rename(&temp_path, &model_path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            let status = DictationModelStatus {
                state: DictationModelState::Error,
                model_id: model_id_clone.clone(),
                progress: None,
                error: Some(format!("Failed to move model into place: {error}")),
                path: None,
            };
            update_status(&app_handle, &state, status).await;
            clear_download_state(&state).await;
            return;
        }

        let status = ready_status(&model_id_clone, &model_path);
        update_status(&app_handle, &state, status).await;
        clear_download_state(&state).await;
    });

    {
        let mut dictation = state.dictation.lock().await;
        dictation.download_task = Some(task);
    }

    Ok(refresh_status(&app, &state, &model_id).await)
}

#[tauri::command]
pub(crate) async fn dictation_cancel_download(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<DictationModelStatus, String> {
    let model_id = resolve_model_id(&state, model_id).await;
    {
        let mut dictation = state.dictation.lock().await;
        if let Some(flag) = dictation.download_cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
        if let Some(task) = dictation.download_task.take() {
            task.abort();
        }
        dictation.model_status = missing_status(&model_id);
    }
    if let Ok(temp_path) = model_temp_path(&app, &model_id) {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    let status = refresh_status(&app, &state, &model_id).await;
    emit_status(&app, &status);
    Ok(status)
}

#[tauri::command]
pub(crate) async fn dictation_remove_model(
    app: AppHandle,
    state: State<'_, AppState>,
    model_id: Option<String>,
) -> Result<DictationModelStatus, String> {
    let model_id = resolve_model_id(&state, model_id).await;
    let model_path = model_path(&app, &model_id)?;
    if model_path.exists() {
        tokio::fs::remove_file(&model_path)
            .await
            .map_err(|error| format!("Failed to remove model: {error}"))?;
    }
    {
        let mut dictation = state.dictation.lock().await;
        if dictation
            .cached_context
            .as_ref()
            .map(|cached| cached.model_id.as_str() == model_id)
            .unwrap_or(false)
        {
            dictation.cached_context = None;
        }
        dictation.model_status = missing_status(&model_id);
    }
    let status = refresh_status(&app, &state, &model_id).await;
    emit_status(&app, &status);
    Ok(status)
}
