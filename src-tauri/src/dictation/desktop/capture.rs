use super::{emit_event, DictationEvent, DictationSessionState};
use crate::state::AppState;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

const MAX_CAPTURE_SECONDS: u32 = 120;

pub(super) fn start_capture_thread(
    app: AppHandle,
    audio: Arc<Mutex<Vec<f32>>>,
    stop_rx: mpsc::Receiver<()>,
    stop_tx: mpsc::Sender<()>,
    stopped_tx: oneshot::Sender<()>,
    ready_tx: oneshot::Sender<Result<u32, String>>,
) {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| "No microphone input device available.".to_string());
    let device = match device {
        Ok(device) => device,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            let _ = stopped_tx.send(());
            return;
        }
    };
    let config = device
        .default_input_config()
        .map_err(|error| format!("Failed to read microphone config: {error}"));
    let config = match config {
        Ok(config) => config,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            let _ = stopped_tx.send(());
            return;
        }
    };
    let sample_rate = config.sample_rate().0;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let channels = stream_config.channels as usize;
    let max_samples = (sample_rate as usize)
        .saturating_mul(MAX_CAPTURE_SECONDS as usize)
        .max(1);
    let app_handle = app.clone();
    let audio_capture = audio.clone();
    let stop_on_error = stop_tx.clone();

    let err_fn = move |error| {
        emit_event(
            &app_handle,
            DictationEvent::Error {
                message: format!("Microphone error: {error}"),
            },
        );
        let _ = stop_on_error.send(());
        let state_app = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let state_handle = state_app.state::<AppState>();
            let should_emit = {
                let mut dictation = state_handle.dictation.lock().await;
                if dictation.session_state == DictationSessionState::Idle {
                    false
                } else {
                    dictation.session_state = DictationSessionState::Idle;
                    dictation.session = None;
                    true
                }
            };
            if should_emit {
                emit_event(
                    &state_app,
                    DictationEvent::State {
                        state: DictationSessionState::Idle,
                    },
                );
            }
        });
    };

    let level_value = Arc::new(AtomicU32::new(0));

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &stream_config,
            channels,
            max_samples,
            audio_capture,
            level_value.clone(),
            err_fn,
        ),
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &stream_config,
            channels,
            max_samples,
            audio_capture,
            level_value.clone(),
            err_fn,
        ),
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &stream_config,
            channels,
            max_samples,
            audio_capture,
            level_value.clone(),
            err_fn,
        ),
        _ => {
            let _ = ready_tx.send(Err("Unsupported microphone sample format.".to_string()));
            let _ = stopped_tx.send(());
            return;
        }
    };

    let stream = match stream {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            let _ = stopped_tx.send(());
            return;
        }
    };
    if let Err(error) = stream.play() {
        let _ = ready_tx.send(Err(format!("Failed to start microphone: {error}")));
        let _ = stopped_tx.send(());
        return;
    }

    let running = Arc::new(AtomicBool::new(true));
    let level_task_app = app.clone();
    let level_task_value = level_value.clone();
    let level_task_running = running.clone();
    std::thread::spawn(move || {
        while level_task_running.load(Ordering::Relaxed) {
            let value = f32::from_bits(level_task_value.load(Ordering::Relaxed));
            emit_event(&level_task_app, DictationEvent::Level { value });
            std::thread::sleep(Duration::from_millis(33));
        }
    });

    eprintln!(
        "dictation: capture started (rate={}Hz, channels={}, format={:?})",
        sample_rate, channels, sample_format
    );
    let _ = ready_tx.send(Ok(sample_rate));
    let _ = stop_rx.recv();
    running.store(false, Ordering::Relaxed);
    drop(stream);
    let _ = stopped_tx.send(());
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    max_samples: usize,
    audio: Arc<Mutex<Vec<f32>>>,
    level_value: Arc<AtomicU32>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, String>
where
    T: Sample + SizedSample,
    f32: FromSample<T>,
{
    let channels = channels.max(1);
    let mut mono_buffer: Vec<f32> = Vec::with_capacity(2048);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                if data.is_empty() {
                    return;
                }
                let mut sum = 0.0f32;
                let mut frames = 0usize;
                mono_buffer.clear();
                let target_len = data.len() / channels;
                if mono_buffer.capacity() < target_len {
                    mono_buffer.reserve(target_len - mono_buffer.capacity());
                }
                for frame in data.chunks(channels) {
                    let mut frame_sum = 0.0f32;
                    let mut count = 0usize;
                    for sample in frame {
                        let value: f32 = sample.to_sample();
                        frame_sum += value;
                        count += 1;
                    }
                    if count == 0 {
                        continue;
                    }
                    let mono = frame_sum / count as f32;
                    mono_buffer.push(mono);
                    sum += mono * mono;
                    frames += 1;
                }
                if frames == 0 {
                    return;
                }
                if let Ok(mut buffer) = audio.lock() {
                    if buffer.len() < max_samples {
                        let remaining = max_samples.saturating_sub(buffer.len());
                        let slice_len = remaining.min(mono_buffer.len());
                        if slice_len > 0 {
                            buffer.extend_from_slice(&mono_buffer[..slice_len]);
                        }
                    }
                }
                let rms = (sum / frames as f32).sqrt();
                let scaled = (rms * 6.0).clamp(0.0, 1.0);
                level_value.store(scaled.to_bits(), Ordering::Relaxed);
            },
            err_fn,
            None,
        )
        .map_err(|error| format!("Failed to build microphone stream: {error}"))
}
