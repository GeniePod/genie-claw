//! Microphone input noise processing — noise gate + spectral suppression.
//!
//! Applied to recorded WAV audio BEFORE sending to Whisper STT.
//! Improves STT accuracy by removing background noise that confuses the model.
//!
//! Processing chain:
//!   Raw mic recording (WAV)
//!     → High-pass filter (remove DC offset + low rumble below 80 Hz)
//!     → Noise gate (silence segments below threshold)
//!     → Spectral noise estimation + suppression
//!     → AGC (normalize for consistent Whisper input)
//!     → Cleaned WAV → Whisper STT
//!
//! All processing is S16_LE mono. Sample rate agnostic (works at 16kHz or 48kHz).

/// Noise processing configuration.
pub struct NoiseConfig {
    /// Noise gate threshold (0-32767). Audio below this RMS is silenced.
    /// Default: 300 (~-40 dB). Higher = more aggressive gating.
    pub gate_threshold: f32,

    /// High-pass filter cutoff frequency (Hz). Removes DC offset and rumble.
    /// Default: 80 Hz.
    pub highpass_hz: f32,

    /// Noise suppression strength (0.0 = off, 1.0 = maximum).
    /// Default: 0.6 (moderate, preserves speech quality).
    pub suppression_strength: f32,

    /// AGC target RMS for Whisper input normalization.
    /// Default: 3000.
    pub agc_target: f32,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            gate_threshold: 300.0,
            highpass_hz: 80.0,
            suppression_strength: 0.6,
            agc_target: 3000.0,
        }
    }
}

/// Process a WAV file in-place: noise gate + spectral suppression + AGC.
///
/// Reads the WAV, processes PCM data, writes back.
/// Returns true if speech was detected, false if the recording is all silence.
pub async fn process_recording(wav_path: &str, sample_rate: u32) -> bool {
    let data = match tokio::fs::read(wav_path).await {
        Ok(d) => d,
        Err(_) => return true, // Can't read — skip processing, let Whisper handle it.
    };

    // WAV header is 44 bytes for standard PCM.
    if data.len() <= 44 {
        return false; // Empty recording.
    }

    let header = &data[..44];
    let mut pcm = data[44..].to_vec();

    // Convert to f32 samples.
    let num_samples = pcm.len() / 2;
    let mut samples: Vec<f32> = (0..num_samples)
        .map(|i| i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as f32)
        .collect();

    let config = NoiseConfig::default();

    // Step 1: High-pass filter (remove DC offset + rumble).
    apply_highpass(&mut samples, config.highpass_hz, sample_rate);

    // Step 2: Noise gate (silence low-level segments).
    let has_speech = apply_noise_gate(&mut samples, config.gate_threshold);

    if !has_speech {
        return false; // All silence — no point sending to Whisper.
    }

    // Step 3: Simple spectral noise suppression.
    apply_noise_suppression(&mut samples, config.suppression_strength, sample_rate);

    // Step 4: AGC (normalize for Whisper).
    apply_mic_agc(&mut samples, config.agc_target);

    // Convert back to S16_LE bytes.
    for i in 0..num_samples {
        let clamped = samples[i].clamp(-32767.0, 32767.0) as i16;
        let bytes = clamped.to_le_bytes();
        pcm[i * 2] = bytes[0];
        pcm[i * 2 + 1] = bytes[1];
    }

    // Write processed WAV back.
    let mut output = Vec::with_capacity(header.len() + pcm.len());
    output.extend_from_slice(header);
    output.extend_from_slice(&pcm);
    let _ = tokio::fs::write(wav_path, &output).await;

    true
}

/// High-pass filter — removes DC offset and low rumble below cutoff Hz.
/// 1-pole IIR high-pass filter.
fn apply_highpass(samples: &mut [f32], cutoff_hz: f32, sample_rate: u32) {
    if samples.is_empty() {
        return;
    }

    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
    let dt = 1.0 / sample_rate as f32;
    let alpha = rc / (rc + dt);

    let mut prev_input = samples[0];
    let mut prev_output = samples[0];

    for sample in samples.iter_mut().skip(1) {
        let input = *sample;
        let output = alpha * (prev_output + input - prev_input);
        *sample = output;
        prev_input = input;
        prev_output = output;
    }
}

/// Noise gate — silence audio segments where RMS is below threshold.
///
/// Processes in 20ms frames. Returns true if any frame contains speech.
fn apply_noise_gate(samples: &mut [f32], threshold: f32) -> bool {
    let frame_size = samples.len().min(960); // ~20ms at 48kHz, ~320 at 16kHz
    let frame_size = if frame_size < 100 {
        samples.len()
    } else {
        frame_size
    };

    let mut has_speech = false;
    let mut attack_frames = 0; // Hold gate open for a few frames after speech.

    for chunk in samples.chunks_mut(frame_size) {
        let rms = frame_rms(chunk);

        if rms > threshold {
            has_speech = true;
            attack_frames = 5; // Hold open for 5 frames (~100ms) after speech.
        } else if attack_frames > 0 {
            attack_frames -= 1;
            // Keep audio during hold time (don't gate tail of speech).
        } else {
            // Below threshold and past hold time — gate (silence).
            for sample in chunk.iter_mut() {
                *sample *= 0.01; // Not zero — slight noise floor sounds more natural.
            }
        }
    }

    has_speech
}

/// Simple spectral noise suppression.
///
/// Estimates noise floor from the quietest frames, then subtracts it
/// from all frames. This is a simplified version of spectral subtraction.
///
/// For production, replace with RNNoise (neural net, much better quality).
fn apply_noise_suppression(samples: &mut [f32], strength: f32, sample_rate: u32) {
    if samples.is_empty() || strength <= 0.0 {
        return;
    }

    let frame_size = (sample_rate as usize / 50).max(100); // 20ms frames
    let num_frames = samples.len() / frame_size;

    if num_frames < 3 {
        return; // Too short to estimate noise.
    }

    // Per-frame RMS, computed once and reused for both the noise-floor
    // estimate and the suppression pass below (the loop used to recompute
    // frame_rms for every frame — a second full pass over the samples).
    let frame_rms_by_index: Vec<f32> = (0..num_frames)
        .map(|i| {
            let start = i * frame_size;
            let end = (start + frame_size).min(samples.len());
            frame_rms(&samples[start..end])
        })
        .collect();

    // Average RMS of quietest 20% = estimated noise floor.
    let mut sorted_energies = frame_rms_by_index.clone();
    sorted_energies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let noise_frame_count = (num_frames / 5).max(1);
    let noise_floor: f32 =
        sorted_energies[..noise_frame_count].iter().sum::<f32>() / noise_frame_count as f32;

    if noise_floor < 10.0 {
        return; // Very quiet — no noise to suppress.
    }

    // Apply suppression: attenuate frames that are close to the noise floor.
    let suppression_threshold = noise_floor * 3.0; // Frames below 3× noise floor get suppressed.

    for (i, &frame_rms) in frame_rms_by_index.iter().enumerate() {
        let start = i * frame_size;
        let end = (start + frame_size).min(samples.len());

        if frame_rms < suppression_threshold {
            // This frame is mostly noise — attenuate.
            let attenuation = if frame_rms < noise_floor * 1.5 {
                1.0 - strength // Strong suppression for very noisy frames.
            } else {
                // Gradual: closer to threshold = less suppression.
                let ratio = (frame_rms - noise_floor) / (suppression_threshold - noise_floor);
                1.0 - strength * (1.0 - ratio)
            };

            let gain = attenuation.clamp(0.05, 1.0);
            for sample in &mut samples[start..end] {
                *sample *= gain;
            }
        }
    }
}

/// AGC for microphone input — normalize RMS for consistent Whisper input.
fn apply_mic_agc(samples: &mut [f32], target_rms: f32) {
    if samples.is_empty() {
        return;
    }

    let rms = frame_rms(samples);
    if rms < 10.0 {
        return; // Silence.
    }

    let gain = (target_rms / rms).clamp(0.1, 10.0);

    for sample in samples.iter_mut() {
        *sample *= gain;
    }
}

/// Calculate RMS of a frame.
fn frame_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highpass_removes_dc_offset() {
        // Signal with DC offset of 1000.
        let mut samples: Vec<f32> = (0..4800)
            .map(|i| 1000.0 + (i as f32 * 0.5).sin() * 500.0)
            .collect();

        let mean_before: f32 = samples.iter().sum::<f32>() / samples.len() as f32;
        assert!(mean_before > 900.0, "Should have DC offset before");

        apply_highpass(&mut samples, 80.0, 48000);

        let mean_after: f32 = samples.iter().sum::<f32>() / samples.len() as f32;
        assert!(
            mean_after.abs() < 200.0,
            "DC offset should be mostly removed, got {}",
            mean_after
        );
    }

    #[test]
    fn noise_gate_silences_quiet_audio() {
        // Very quiet audio (RMS ~50).
        let mut samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.1).sin() * 50.0).collect();

        let has_speech = apply_noise_gate(&mut samples, 300.0);
        assert!(!has_speech, "Should detect no speech in quiet audio");

        // Verify audio is nearly silenced.
        let rms = frame_rms(&samples);
        assert!(
            rms < 5.0,
            "Gated audio should be nearly silent, got {}",
            rms
        );
    }

    #[test]
    fn noise_gate_preserves_speech() {
        // Loud audio (RMS ~5000) — should be preserved.
        let mut samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.1).sin() * 5000.0).collect();

        let has_speech = apply_noise_gate(&mut samples, 300.0);
        assert!(has_speech, "Should detect speech in loud audio");

        let rms = frame_rms(&samples);
        assert!(rms > 1000.0, "Speech should be preserved, got {}", rms);
    }

    #[test]
    fn noise_suppression_reduces_quiet_frames() {
        // Mix: first half is noise (RMS ~100), second half is speech (RMS ~5000).
        let mut samples: Vec<f32> = (0..9600)
            .map(|i| {
                if i < 4800 {
                    (i as f32 * 0.3).sin() * 100.0 // Noise
                } else {
                    (i as f32 * 0.1).sin() * 5000.0 // Speech
                }
            })
            .collect();

        let noise_rms_before = frame_rms(&samples[..4800]);
        apply_noise_suppression(&mut samples, 0.6, 48000);
        let noise_rms_after = frame_rms(&samples[..4800]);
        let speech_rms_after = frame_rms(&samples[4800..]);

        assert!(
            noise_rms_after < noise_rms_before * 0.8,
            "Noise should be reduced: before={}, after={}",
            noise_rms_before,
            noise_rms_after
        );
        assert!(
            speech_rms_after > 1000.0,
            "Speech should be preserved: {}",
            speech_rms_after
        );
    }

    #[test]
    fn mic_agc_normalizes() {
        let mut samples: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.1).sin() * 500.0).collect();
        let rms_before = frame_rms(&samples);

        apply_mic_agc(&mut samples, 3000.0);
        let rms_after = frame_rms(&samples);

        assert!(
            rms_after > rms_before,
            "AGC should boost quiet audio: before={}, after={}",
            rms_before,
            rms_after
        );
    }

    #[test]
    fn noise_suppression_ignores_too_short_input() {
        // Fewer than 3 frames can't estimate a noise floor (and strength <= 0 is a
        // no-op): the buffer comes back untouched despite carrying suppressible signal.
        let original: Vec<f32> = (0..1500).map(|i| (i as f32 * 0.3).sin() * 3000.0).collect();
        let mut samples = original.clone();
        apply_noise_suppression(&mut samples, 0.6, 48000); // frame_size 960 -> 1 frame < 3
        assert_eq!(samples, original);

        let mut zero_strength = original.clone();
        apply_noise_suppression(&mut zero_strength, 0.0, 48000);
        assert_eq!(zero_strength, original);
    }

    #[test]
    fn noise_suppression_ignores_near_silence() {
        // A long but near-silent buffer (per-frame RMS ~3.5) sits below the 10.0
        // noise-floor threshold, so suppression bails and leaves it unchanged.
        let original: Vec<f32> = (0..9600).map(|i| (i as f32 * 0.3).sin() * 5.0).collect();
        let mut samples = original.clone();
        apply_noise_suppression(&mut samples, 0.6, 48000); // 10 frames, noise_floor ~3.5 < 10
        assert_eq!(samples, original);
    }
}
