use whisper_rs::{get_lang_id, FullParams, SamplingStrategy, WhisperContext};

pub(super) fn transcribe_audio(
    samples: Vec<f32>,
    sample_rate: u32,
    context: &WhisperContext,
    preferred_language: Option<String>,
) -> Result<String, String> {
    if samples.is_empty() {
        return Ok(String::new());
    }
    let mut max = 0.0f32;
    let mut sum = 0.0f32;
    let mean = samples.iter().copied().sum::<f32>() / samples.len() as f32;
    let mut normalized = Vec::with_capacity(samples.len());
    for value in &samples {
        let centered = value - mean;
        let abs = centered.abs();
        if abs > max {
            max = abs;
        }
        sum += centered * centered;
        normalized.push(centered);
    }
    let rms = (sum / samples.len() as f32).sqrt();
    let duration = samples.len() as f32 / sample_rate as f32;
    let gain = if max > 0.0 {
        (0.6 / max).min(10.0)
    } else {
        1.0
    };
    if gain != 1.0 {
        for value in &mut normalized {
            *value = (*value * gain).clamp(-1.0, 1.0);
        }
    }
    eprintln!(
        "dictation: captured {} samples ({:.2}s), max={:.4}, rms={:.4}, gain={:.2}",
        samples.len(),
        duration,
        max,
        rms,
        gain
    );
    if duration < 0.2 {
        return Err("Audio too short for transcription.".to_string());
    }
    let audio = if sample_rate == 16_000 {
        normalized
    } else {
        resample_audio(&normalized, sample_rate, 16_000)
    };

    let mut state = context
        .create_state()
        .map_err(|error| format!("Failed to initialize Whisper: {error}"))?;
    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_timestamps(true);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_single_segment(false);
    let mut forced_language: Option<String> = None;
    if let Some(preferred) = preferred_language.clone() {
        if let Some(pref_id) = get_lang_id(&preferred) {
            if state.pcm_to_mel(&audio, threads).is_ok() {
                if let Ok((_detected, probs)) = state.lang_detect(0, threads) {
                    let pref_index = pref_id.max(0) as usize;
                    let pref_prob = probs.get(pref_index).copied().unwrap_or(0.0);
                    let best_prob = probs
                        .iter()
                        .copied()
                        .fold(0.0_f32, |acc, value| acc.max(value));
                    if best_prob > 0.0 && (best_prob - pref_prob) <= 0.30 {
                        forced_language = Some(preferred);
                    }
                }
            }
        }
    }

    if let Some(language) = forced_language.as_deref() {
        // Use the preferred language only when detection is ambiguous.
        params.set_language(Some(language));
    } else {
        // Auto-detect language while still running transcription.
        params.set_language(Some("auto"));
    }
    params.set_n_threads(threads as i32);

    state
        .full(params, &audio)
        .map_err(|error| format!("Transcription failed: {error}"))?;

    let segments = state
        .full_n_segments()
        .map_err(|error| format!("Failed to read segments: {error}"))?;
    eprintln!("dictation: whisper segments={}", segments);
    let mut transcript = String::new();
    for index in 0..segments {
        let segment = state
            .full_get_segment_text(index)
            .map_err(|error| format!("Failed to read segment: {error}"))?;
        transcript.push_str(&segment);
    }
    let cleaned = transcript.trim().to_string();
    if cleaned.is_empty() {
        eprintln!(
            "dictation: no speech detected (rms={:.4}, max={:.4}, duration={:.2}s, segments={})",
            rms, max, duration, segments
        );
        return Ok(String::new());
    }
    Ok(cleaned)
}

fn resample_audio(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let new_len = (samples.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(new_len.max(1));
    for i in 0..new_len {
        let pos = i as f64 / ratio;
        let idx = pos.floor() as usize;
        let frac = pos - idx as f64;
        let s0 = samples.get(idx).copied().unwrap_or(0.0);
        let s1 = samples.get(idx + 1).copied().unwrap_or(s0);
        out.push((s0 as f64 + (s1 as f64 - s0 as f64) * frac) as f32);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::resample_audio;

    #[test]
    fn resampling_preserves_empty_and_native_rate_audio() {
        assert!(resample_audio(&[], 48_000, 16_000).is_empty());
        assert_eq!(
            resample_audio(&[-0.5, 0.0, 0.5], 16_000, 16_000),
            [-0.5, 0.0, 0.5]
        );
    }

    #[test]
    fn resampling_interpolates_and_holds_the_last_sample() {
        assert_eq!(
            resample_audio(&[0.0, 1.0], 8_000, 16_000),
            [0.0, 0.5, 1.0, 1.0]
        );
        assert_eq!(
            resample_audio(&[0.0, 0.25, 0.5, 0.75], 32_000, 16_000),
            [0.0, 0.5]
        );
    }
}
