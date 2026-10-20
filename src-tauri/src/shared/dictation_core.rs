//! Host-independent microphone capture, Whisper model, and transcription core.
//! The host receives events through a channel and supplies the model directory.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc as std_mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};
use whisper_rs::{
    get_lang_id, FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

const DEFAULT_MODEL: &str = "base";
const MAX_CAPTURE_SECONDS: usize = 120;

struct ModelInfo {
    id: &'static str,
    filename: &'static str,
    url: &'static str,
    sha256: &'static str,
}
const MODELS: &[ModelInfo] = &[
    ModelInfo {
        id: "tiny",
        filename: "ggml-tiny.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    ModelInfo {
        id: "base",
        filename: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    ModelInfo {
        id: "small",
        filename: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    ModelInfo {
        id: "medium",
        filename: "ggml-medium.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
        sha256: "6c14d5adee5f86394037b4e4e8b59f1673b6cee10e3cf0b11bbdbee79c156208",
    },
    ModelInfo {
        id: "large-v3",
        filename: "ggml-large-v3.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        sha256: "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DictationSessionState {
    Idle,
    Listening,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DictationModelState {
    Missing,
    Downloading,
    Ready,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DictationModelProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DictationModelStatus {
    pub state: DictationModelState,
    pub model_id: String,
    pub progress: Option<DictationModelProgress>,
    pub error: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum DictationEvent {
    State { state: DictationSessionState },
    Level { value: f32 },
    Transcript { text: String },
    Error { message: String },
    Canceled { message: String },
    Model { status: DictationModelStatus },
}

struct CaptureSession {
    stop: std_mpsc::Sender<()>,
    stopped: oneshot::Receiver<()>,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    model_id: String,
}

struct State {
    session_state: DictationSessionState,
    starting: bool,
    capture: Option<CaptureSession>,
    download_cancel: Option<Arc<AtomicBool>>,
    download_model_id: Option<String>,
    cached_model: Option<(String, Arc<WhisperContext>)>,
    processing_cancel: Option<Arc<AtomicBool>>,
}

pub(crate) struct DictationEngine {
    model_dir: PathBuf,
    events_tx: mpsc::UnboundedSender<DictationEvent>,
    events_rx: std::sync::Mutex<mpsc::UnboundedReceiver<DictationEvent>>,
    state: AsyncMutex<State>,
}

impl DictationEngine {
    pub(crate) fn new(model_dir: PathBuf) -> Self {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        Self {
            model_dir,
            events_tx,
            events_rx: std::sync::Mutex::new(events_rx),
            state: AsyncMutex::new(State {
                session_state: DictationSessionState::Idle,
                starting: false,
                capture: None,
                download_cancel: None,
                download_model_id: None,
                cached_model: None,
                processing_cancel: None,
            }),
        }
    }

    pub(crate) fn drain_events(&self) -> Vec<DictationEvent> {
        let Ok(mut receiver) = self.events_rx.lock() else {
            return Vec::new();
        };
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        events
    }

    pub(crate) async fn model_status(&self, model_id: Option<&str>) -> DictationModelStatus {
        let id = valid_model_id(model_id.unwrap_or(DEFAULT_MODEL)).unwrap_or(DEFAULT_MODEL);
        let path = self.model_dir.join(model_info(id).unwrap().filename);
        let downloading = self.state.lock().await.download_model_id.as_deref() == Some(id);
        let status = if downloading {
            DictationModelStatus {
                state: DictationModelState::Downloading,
                model_id: id.into(),
                progress: None,
                error: None,
                path: None,
            }
        } else if path.is_file() {
            DictationModelStatus {
                state: DictationModelState::Ready,
                model_id: id.into(),
                progress: None,
                error: None,
                path: Some(path.to_string_lossy().into_owned()),
            }
        } else {
            missing_model(id)
        };
        status
    }

    pub(crate) async fn download_model(
        &self,
        model_id: Option<&str>,
    ) -> Result<DictationModelStatus, String> {
        let id =
            valid_model_id(model_id.unwrap_or(DEFAULT_MODEL)).ok_or("unknown dictation model")?;
        let info = model_info(id).unwrap();
        let path = self.model_dir.join(info.filename);
        if path.is_file() {
            return Ok(self.model_status(Some(id)).await);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut state = self.state.lock().await;
            if state.download_cancel.is_some() {
                return Err("a dictation model download is already active".into());
            }
            state.download_cancel = Some(cancel.clone());
            state.download_model_id = Some(id.to_string());
        }
        let _ = self.events_tx.send(DictationEvent::Model {
            status: DictationModelStatus {
                state: DictationModelState::Downloading,
                model_id: id.into(),
                progress: Some(DictationModelProgress {
                    downloaded_bytes: 0,
                    total_bytes: None,
                }),
                error: None,
                path: None,
            },
        });
        let result = self.download_model_inner(id, &cancel).await;
        {
            let mut state = self.state.lock().await;
            state.download_cancel = None;
            state.download_model_id = None;
        }
        match result {
            Ok(()) => {
                let status = self.model_status(Some(id)).await;
                let _ = self.events_tx.send(DictationEvent::Model {
                    status: status.clone(),
                });
                Ok(status)
            }
            Err(error) => {
                let status = DictationModelStatus {
                    state: DictationModelState::Error,
                    model_id: id.into(),
                    progress: None,
                    error: Some(error.clone()),
                    path: None,
                };
                let _ = self.events_tx.send(DictationEvent::Model { status });
                Err(error)
            }
        }
    }

    async fn download_model_inner(
        &self,
        model_id: &str,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        use futures_util::StreamExt;
        let info = model_info(model_id).ok_or("unknown dictation model")?;
        tokio::fs::create_dir_all(&self.model_dir)
            .await
            .map_err(|error| format!("could not create model directory: {error}"))?;
        let final_path = self.model_dir.join(info.filename);
        let partial_path = self.model_dir.join(format!("{}.partial", info.filename));
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30 * 60))
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(info.url)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let total = response.content_length();
        let mut stream = response.bytes_stream();
        let mut file = tokio::fs::File::create(&partial_path)
            .await
            .map_err(|error| error.to_string())?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) {
                let _ = tokio::fs::remove_file(&partial_path).await;
                return Err("download canceled".into());
            }
            let chunk = chunk.map_err(|error| error.to_string())?;
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|error| error.to_string())?;
            hasher.update(&chunk);
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            let _ = self.events_tx.send(DictationEvent::Model {
                status: DictationModelStatus {
                    state: DictationModelState::Downloading,
                    model_id: model_id.into(),
                    progress: Some(DictationModelProgress {
                        downloaded_bytes: downloaded,
                        total_bytes: total,
                    }),
                    error: None,
                    path: None,
                },
            });
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|error| error.to_string())?;
        let digest = format!("{:x}", hasher.finalize());
        if digest != info.sha256 {
            let _ = tokio::fs::remove_file(&partial_path).await;
            return Err("model hash mismatch; download canceled".into());
        }
        tokio::fs::rename(partial_path, final_path)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn cancel_download(&self) -> Result<(), String> {
        let cancel = self.state.lock().await.download_cancel.clone();
        match cancel {
            Some(cancel) => {
                cancel.store(true, Ordering::SeqCst);
                Ok(())
            }
            None => Err("no dictation model download is active".into()),
        }
    }

    pub(crate) async fn start(
        &self,
        model_id: Option<&str>,
        preferred_language: Option<String>,
    ) -> Result<DictationSessionState, String> {
        let model_id = valid_model_id(model_id.unwrap_or(DEFAULT_MODEL))
            .ok_or("unknown dictation model")?
            .to_string();
        if self.model_status(Some(&model_id)).await.state != DictationModelState::Ready {
            return Err("Dictation model is not downloaded yet.".into());
        }
        {
            let mut state = self.state.lock().await;
            if state.session_state != DictationSessionState::Idle || state.starting {
                return Err("Dictation is already active.".into());
            }
            state.starting = true;
        }
        let samples = Arc::new(Mutex::new(Vec::new()));
        let (stop_tx, stop_rx) = std_mpsc::channel();
        let (stopped_tx, stopped_rx) = oneshot::channel();
        let (ready_tx, ready_rx) = oneshot::channel();
        let event_tx = self.events_tx.clone();
        let capture_samples = samples.clone();
        std::thread::spawn(move || {
            capture_thread(capture_samples, stop_rx, stopped_tx, ready_tx, event_tx)
        });
        let sample_rate = match ready_rx.await {
            Ok(Ok(sample_rate)) => sample_rate,
            Ok(Err(error)) => {
                self.state.lock().await.starting = false;
                return Err(error);
            }
            Err(_) => {
                self.state.lock().await.starting = false;
                return Err("microphone capture failed to initialize".into());
            }
        };
        let mut state = self.state.lock().await;
        state.starting = false;
        state.capture = Some(CaptureSession {
            stop: stop_tx,
            stopped: stopped_rx,
            samples,
            sample_rate,
            model_id,
        });
        state.session_state = DictationSessionState::Listening;
        let _ = self.events_tx.send(DictationEvent::State {
            state: state.session_state,
        });
        let _ = preferred_language;
        Ok(state.session_state)
    }

    pub(crate) async fn stop(
        &self,
        preferred_language: Option<String>,
    ) -> Result<DictationSessionState, String> {
        let capture = {
            let mut state = self.state.lock().await;
            if state.session_state != DictationSessionState::Listening {
                return Err("Dictation is not currently listening.".into());
            }
            state.session_state = DictationSessionState::Processing;
            state
                .capture
                .take()
                .ok_or("Dictation session is unavailable")?
        };
        let processing_cancel = Arc::new(AtomicBool::new(false));
        self.state.lock().await.processing_cancel = Some(processing_cancel.clone());
        let _ = self.events_tx.send(DictationEvent::State {
            state: DictationSessionState::Processing,
        });
        let _ = capture.stop.send(());
        let _ = capture.stopped.await;
        let samples = capture
            .samples
            .lock()
            .map_err(|_| "microphone buffer lock poisoned")?
            .clone();
        let model_id = capture.model_id.clone();
        let model_path = self.model_dir.join(model_info(&model_id).unwrap().filename);
        let cached = self
            .state
            .lock()
            .await
            .cached_model
            .as_ref()
            .filter(|(id, _)| id == &model_id)
            .map(|(_, model)| model.clone());
        let model = if let Some(model) = cached {
            Ok(model)
        } else {
            let path = model_path.to_string_lossy().into_owned();
            match tokio::task::spawn_blocking(move || {
                WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            })
            .await
            {
                Ok(Ok(model)) => {
                    let model = Arc::new(model);
                    self.state.lock().await.cached_model = Some((model_id, model.clone()));
                    Ok(model)
                }
                Ok(Err(error)) => Err(format!("failed to load Whisper model: {error}")),
                Err(error) => Err(format!("failed to load Whisper model: {error}")),
            }
        };
        let sample_rate = capture.sample_rate;
        let result = match model {
            Ok(model) => {
                let preferred = preferred_language;
                match tokio::task::spawn_blocking(move || {
                    transcribe(samples, sample_rate, &model, preferred)
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                }
            }
            Err(error) => Err(error),
        };
        let canceled = {
            let mut state = self.state.lock().await;
            if processing_cancel.load(Ordering::Relaxed) {
                state.processing_cancel = None;
                true
            } else {
                state.session_state = DictationSessionState::Idle;
                state.processing_cancel = None;
                false
            }
        };
        if canceled {
            return Ok(DictationSessionState::Idle);
        }
        match result {
            Ok(text) if !text.trim().is_empty() => {
                let _ = self.events_tx.send(DictationEvent::Transcript { text });
            }
            Ok(_) => {}
            Err(message) => {
                let _ = self.events_tx.send(DictationEvent::Error { message });
            }
        }
        let _ = self.events_tx.send(DictationEvent::State {
            state: DictationSessionState::Idle,
        });
        Ok(DictationSessionState::Idle)
    }

    pub(crate) async fn cancel(&self) -> Result<DictationSessionState, String> {
        let capture = {
            let mut state = self.state.lock().await;
            if state.session_state == DictationSessionState::Processing {
                if let Some(cancel) = state.processing_cancel.take() {
                    cancel.store(true, Ordering::Relaxed);
                }
                state.session_state = DictationSessionState::Idle;
                let _ = self.events_tx.send(DictationEvent::State {
                    state: DictationSessionState::Idle,
                });
                let _ = self.events_tx.send(DictationEvent::Canceled {
                    message: "Canceled".into(),
                });
                return Ok(DictationSessionState::Idle);
            }
            if state.session_state != DictationSessionState::Listening {
                return Err("Dictation is not currently listening.".into());
            }
            state.session_state = DictationSessionState::Idle;
            state
                .capture
                .take()
                .ok_or("Dictation session is unavailable")?
        };
        let _ = capture.stop.send(());
        let _ = capture.stopped.await;
        if let Ok(mut samples) = capture.samples.lock() {
            samples.clear();
        }
        let _ = self.events_tx.send(DictationEvent::State {
            state: DictationSessionState::Idle,
        });
        let _ = self.events_tx.send(DictationEvent::Canceled {
            message: "Canceled".into(),
        });
        Ok(DictationSessionState::Idle)
    }
}

fn valid_model_id(value: &str) -> Option<&'static str> {
    model_info(value).map(|model| model.id)
}
fn model_info(value: &str) -> Option<&'static ModelInfo> {
    MODELS.iter().find(|model| model.id == value)
}
fn missing_model(id: &str) -> DictationModelStatus {
    DictationModelStatus {
        state: DictationModelState::Missing,
        model_id: id.into(),
        progress: None,
        error: None,
        path: None,
    }
}

fn capture_thread(
    samples: Arc<Mutex<Vec<f32>>>,
    stop_rx: std_mpsc::Receiver<()>,
    stopped: oneshot::Sender<()>,
    ready: oneshot::Sender<Result<u32, String>>,
    events: mpsc::UnboundedSender<DictationEvent>,
) {
    let host = cpal::default_host();
    let Some(device) = host.default_input_device() else {
        let _ = ready.send(Err("No microphone input device available.".into()));
        let _ = stopped.send(());
        return;
    };
    let config = match device.default_input_config() {
        Ok(config) => config,
        Err(error) => {
            let _ = ready.send(Err(format!("Failed to read microphone config: {error}")));
            let _ = stopped.send(());
            return;
        }
    };
    let rate = config.sample_rate().0;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let channels = stream_config.channels as usize;
    let max = rate as usize * MAX_CAPTURE_SECONDS;
    let level = Arc::new(AtomicU32::new(0));
    let errors = events.clone();
    let error_callback = move |error| {
        let _ = errors.send(DictationEvent::Error {
            message: format!("Microphone error: {error}"),
        });
    };
    let stream = match format {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &stream_config,
            channels,
            max,
            samples.clone(),
            level.clone(),
            error_callback,
        ),
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &stream_config,
            channels,
            max,
            samples.clone(),
            level.clone(),
            error_callback,
        ),
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &stream_config,
            channels,
            max,
            samples.clone(),
            level.clone(),
            error_callback,
        ),
        _ => {
            let _ = ready.send(Err("Unsupported microphone sample format.".into()));
            let _ = stopped.send(());
            return;
        }
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(error));
            let _ = stopped.send(());
            return;
        }
    };
    if let Err(error) = stream.play() {
        let _ = ready.send(Err(format!("Failed to start microphone: {error}")));
        let _ = stopped.send(());
        return;
    }
    let active = Arc::new(AtomicBool::new(true));
    let thread_active = active.clone();
    let thread_level = level.clone();
    let thread_events = events.clone();
    let levels = std::thread::spawn(move || {
        while thread_active.load(Ordering::Relaxed) {
            let value = f32::from_bits(thread_level.load(Ordering::Relaxed));
            let _ = thread_events.send(DictationEvent::Level { value });
            std::thread::sleep(Duration::from_millis(33));
        }
    });
    let _ = ready.send(Ok(rate));
    let _ = stop_rx.recv();
    active.store(false, Ordering::Relaxed);
    let _ = levels.join();
    drop(stream);
    let _ = stopped.send(());
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    max_samples: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    level: Arc<AtomicU32>,
    error: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if data.is_empty() {
                    return;
                }
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| {
                        frame.iter().map(|v| f32::from_sample(*v)).sum::<f32>()
                            / frame.len().max(1) as f32
                    })
                    .collect();
                if let Ok(mut target) = samples.lock() {
                    let remaining = max_samples.saturating_sub(target.len());
                    target.extend_from_slice(&mono[..mono.len().min(remaining)]);
                }
                let rms = (mono.iter().map(|value| value * value).sum::<f32>() / mono.len() as f32)
                    .sqrt();
                level.store((rms * 6.0).clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
            },
            error,
            None,
        )
        .map_err(|error| format!("Failed to build microphone stream: {error}"))
}

fn transcribe(
    samples: Vec<f32>,
    sample_rate: u32,
    context: &WhisperContext,
    language: Option<String>,
) -> Result<String, String> {
    if samples.is_empty() {
        return Ok(String::new());
    }
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    let mut normalized: Vec<f32> = samples.iter().map(|value| value - mean).collect();
    let max = normalized
        .iter()
        .map(|value| value.abs())
        .fold(0.0f32, f32::max);
    let gain = if max > 0.0 {
        (0.6 / max).min(10.0)
    } else {
        1.0
    };
    if gain != 1.0 {
        normalized
            .iter_mut()
            .for_each(|value| *value = (*value * gain).clamp(-1.0, 1.0));
    }
    if normalized.len() as f32 / (sample_rate as f32) < 0.2 {
        return Err("Audio too short for transcription.".into());
    }
    let audio = if sample_rate == 16_000 {
        normalized
    } else {
        resample(&normalized, sample_rate, 16_000)
    };
    let mut state = context
        .create_state()
        .map_err(|error| format!("Failed to initialize Whisper: {error}"))?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(false);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_translate(false);
    params.set_temperature(0.0);
    params.set_temperature_inc(0.0);
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    params.set_n_threads(threads as i32);
    let forced_language = if let Some(preferred) = language.filter(|value| !value.trim().is_empty())
    {
        if let Some(preferred_id) = get_lang_id(&preferred) {
            if state.pcm_to_mel(&audio, threads).is_ok() {
                if let Ok((_detected, probs)) = state.lang_detect(0, threads) {
                    let preferred_probability = probs
                        .get(preferred_id.max(0) as usize)
                        .copied()
                        .unwrap_or(0.0);
                    let best = probs.iter().copied().fold(0.0f32, f32::max);
                    if best > 0.0 && best - preferred_probability <= 0.30 {
                        Some(preferred)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    params.set_language(forced_language.as_deref().or(Some("auto")));
    state
        .full(params, &audio)
        .map_err(|error| format!("Transcription failed: {error}"))?;
    let mut result = String::new();
    let segments = state.full_n_segments().map_err(|error| error.to_string())?;
    for index in 0..segments {
        result.push_str(
            &state
                .full_get_segment_text(index)
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(result.trim().to_string())
}

fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 {
        return Vec::new();
    }
    let length = (input.len() as u64 * to as u64 / from as u64) as usize;
    (0..length)
        .map(|index| {
            let position = index as f64 * from as f64 / to as f64;
            let left = position.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (position - left as f64) as f32;
            input[left.min(input.len() - 1)] * (1.0 - fraction) + input[right] * fraction
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_catalog_is_closed_and_has_stable_default() {
        assert_eq!(DEFAULT_MODEL, "base");
        assert_eq!(valid_model_id("tiny"), Some("tiny"));
        assert_eq!(valid_model_id("large-v3"), Some("large-v3"));
        assert_eq!(valid_model_id("../../model.bin"), None);
    }

    #[test]
    fn resampling_keeps_endpoints_and_handles_invalid_rates() {
        let output = resample(&[0.0, 1.0, 0.0], 3, 6);
        assert_eq!(output.len(), 6);
        assert_eq!(output.first(), Some(&0.0));
        assert!(output.last().unwrap().abs() < f32::EPSILON);
        assert!(resample(&[1.0], 0, 16_000).is_empty());
        assert!(resample(&[], 16_000, 16_000).is_empty());
    }
}
