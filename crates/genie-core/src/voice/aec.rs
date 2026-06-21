//! Acoustic Echo Cancellation (AEC) — removes speaker output from mic input.
//!
//! When GeniePod speaks (TTS → speaker) and the user also speaks (barge-in),
//! the mic picks up both. AEC subtracts the speaker component, leaving only
//! the user's voice for STT.
//!
//! Implementation: NLMS (Normalized Least Mean Squares) adaptive filter.
//! - Save TTS PCM as echo reference signal
//! - When processing mic input, estimate echo path using NLMS
//! - Subtract estimated echo from mic
//!
//! Note: On USB headphone (devkit), the earpiece provides physical isolation
//! so AEC has minimal effect. On production hardware (upward-firing speaker +
//! side mics), AEC is critical for barge-in and continuous conversation.

use std::sync::Mutex;

/// Global echo reference buffer.
/// Stores the last TTS output PCM for echo cancellation.
static ECHO_REF: Mutex<Option<EchoReference>> = Mutex::new(None);

/// Stored echo reference signal from TTS output.
struct EchoReference {
    /// PCM samples (f32, mono).
    samples: Vec<f32>,
    /// Sample rate of the reference.
    sample_rate: u32,
    /// Timestamp when TTS playback started.
    play_start_ms: u64,
}

/// Maximum room/acoustic tail after TTS playback where the reference is still
/// meaningful for echo cancellation. Push-to-talk recordings that happen after
/// this window should not be processed against old TTS audio.
const MAX_ECHO_TAIL_MS: u64 = 1_500;

/// Store TTS output as echo reference for future AEC processing.
///
/// Called by TTS speak() after Piper generates PCM, before sending to aplay.
/// The PCM is stored as the "what we sent to the speaker" reference.
pub fn set_echo_reference(pcm_s16: &[u8], sample_rate: u32) {
    let num_samples = pcm_s16.len() / 2;
    let samples: Vec<f32> = (0..num_samples)
        .map(|i| i16::from_le_bytes([pcm_s16[i * 2], pcm_s16[i * 2 + 1]]) as f32)
        .collect();

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    if let Ok(mut guard) = ECHO_REF.lock() {
        *guard = Some(EchoReference {
            samples,
            sample_rate,
            play_start_ms: now_ms,
        });
    }
}

/// Clear the echo reference (called after mic recording is done).
pub fn clear_echo_reference() {
    if let Ok(mut guard) = ECHO_REF.lock() {
        *guard = None;
    }
}

/// Apply echo cancellation to mic recording.
///
/// Uses NLMS adaptive filter to estimate and subtract the echo component.
/// If no echo reference is stored (TTS wasn't playing), this is a no-op.
///
/// `mic_samples`: mutable f32 samples from mic recording.
/// `mic_sample_rate`: sample rate of mic recording.
pub fn cancel_echo(mic_samples: &mut [f32], mic_sample_rate: u32) {
    let reference = match ECHO_REF.lock() {
        Ok(guard) => match &*guard {
            Some(r) => {
                // Resample reference if rates differ.
                if r.sample_rate == mic_sample_rate {
                    r.samples.clone()
                } else {
                    // Simple resample by nearest neighbor.
                    let ratio = r.sample_rate as f64 / mic_sample_rate as f64;
                    let new_len = (r.samples.len() as f64 / ratio) as usize;
                    (0..new_len)
                        .map(|i| {
                            let src_idx = ((i as f64) * ratio) as usize;
                            r.samples.get(src_idx).copied().unwrap_or(0.0)
                        })
                        .collect()
                }
            }
            None => return, // No reference — TTS wasn't playing.
        },
        Err(_) => return,
    };

    if reference.is_empty() || mic_samples.is_empty() {
        return;
    }

    // NLMS adaptive filter parameters.
    let filter_len = 256; // ~16ms at 16kHz, ~5ms at 48kHz — room echo tail.
    let mu = 0.3; // Step size (0.0-1.0). Higher = faster adaptation, more distortion.
    let eps = 1e-6; // Regularization (prevents division by zero).

    // Adaptive filter weights (starts at zero — learns the echo path).
    let mut weights = vec![0.0f32; filter_len];

    // Reference buffer for filter input.
    let ref_len = reference.len().min(mic_samples.len());
    let loop_end = mic_samples.len().min(ref_len);

    // The reference window for each step is `reference[i - filter_len .. i]`,
    // which is always exactly `filter_len` samples (the loop starts at
    // `filter_len`). That lets the hot loop drop two redundancies the naive form
    // pays per sample:
    //   * the per-tap `if j < ref_slice.len()` guard is always true, so taps run
    //     through `zip(rev())` with no index arithmetic or bounds checks;
    //   * the normalization energy (sum of squares of the window) is kept with a
    //     sliding update instead of a full recompute every sample — as the window
    //     advances by one, subtract the sample that left and add the one that
    //     entered. That removes a whole O(filter_len) pass per sample (~1/3 of
    //     the inner work) on the per-recording AEC loop. The echo estimate and
    //     weight update are unchanged; only the energy is accumulated, and AEC is
    //     a normalized adaptive filter, so the negligible float drift does not
    //     change the cancellation.
    if loop_end > filter_len {
        let mut ref_energy: f32 = reference[..filter_len].iter().map(|&s| s * s).sum();

        for i in filter_len..loop_end {
            let ref_slice = &reference[i - filter_len..i];

            // Estimate echo: convolution of weights with the reference window.
            let mut echo_estimate = 0.0f32;
            for (&w, &r) in weights.iter().zip(ref_slice.iter().rev()) {
                echo_estimate += w * r;
            }

            // Error: mic signal minus estimated echo = desired signal (voice).
            let error = mic_samples[i] - echo_estimate;

            // Update weights (NLMS step), normalized by the sliding energy.
            let step = mu / (ref_energy + eps);
            for (w, &r) in weights.iter_mut().zip(ref_slice.iter().rev()) {
                *w += step * error * r;
            }

            // Replace mic sample with the error signal (echo-cancelled).
            mic_samples[i] = error;

            // Slide the energy window to `reference[i + 1 - filter_len ..= i]`.
            ref_energy +=
                reference[i] * reference[i] - reference[i - filter_len] * reference[i - filter_len];
        }
    }

    tracing::debug!(
        ref_samples = reference.len(),
        mic_samples_len = mic_samples.len(),
        filter_len,
        "AEC applied"
    );
}

/// Apply AEC to a WAV file in-place.
///
/// Reads the WAV, applies echo cancellation, writes back.
/// Called from noise processing pipeline after recording.
pub async fn process_aec(wav_path: &str, sample_rate: u32) {
    // Check if we have a fresh echo reference. The current voice loop records
    // after TTS playback, not during it. Applying an old TTS reference to the
    // next push-to-talk capture corrupts real speech, so stale references are
    // discarded before NLMS runs.
    let has_ref = ECHO_REF
        .lock()
        .map(|mut guard| {
            let Some(reference) = guard.as_ref() else {
                return false;
            };

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let ref_duration_ms = if reference.sample_rate == 0 {
                0
            } else {
                (reference.samples.len() as u64 * 1_000) / reference.sample_rate as u64
            };
            let age_ms = now_ms.saturating_sub(reference.play_start_ms);
            let fresh_window_ms = ref_duration_ms.saturating_add(MAX_ECHO_TAIL_MS);

            if age_ms > fresh_window_ms {
                tracing::debug!(
                    age_ms,
                    ref_duration_ms,
                    fresh_window_ms,
                    "skipping stale echo reference"
                );
                *guard = None;
                false
            } else {
                true
            }
        })
        .unwrap_or(false);

    if !has_ref {
        return; // No echo reference — TTS wasn't playing during recording.
    }

    let data = match tokio::fs::read(wav_path).await {
        Ok(d) => d,
        Err(_) => return,
    };

    if data.len() <= 44 {
        return;
    }

    let header = &data[..44];
    let pcm = &data[44..];

    let num_samples = pcm.len() / 2;
    let mut samples: Vec<f32> = (0..num_samples)
        .map(|i| i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as f32)
        .collect();

    // Apply NLMS echo cancellation.
    cancel_echo(&mut samples, sample_rate);

    // Convert back.
    let mut output_pcm = vec![0u8; pcm.len()];
    for i in 0..num_samples {
        let clamped = samples[i].clamp(-32767.0, 32767.0) as i16;
        let bytes = clamped.to_le_bytes();
        output_pcm[i * 2] = bytes[0];
        output_pcm[i * 2 + 1] = bytes[1];
    }

    let mut output = Vec::with_capacity(header.len() + output_pcm.len());
    output.extend_from_slice(header);
    output.extend_from_slice(&output_pcm);
    let _ = tokio::fs::write(wav_path, &output).await;

    // Clear reference after use.
    clear_echo_reference();
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn cancel_echo_with_reference() {
        let _guard = TEST_LOCK.lock().unwrap();

        // Simulate: reference is a 440Hz tone, mic picks up the same tone + speech.
        let sample_rate = 16000;
        let duration = 1600; // 100ms at 16kHz

        // Reference: 440Hz sine (what speaker played).
        let reference: Vec<f32> = (0..duration)
            .map(|i| {
                (i as f32 / sample_rate as f32 * 440.0 * 2.0 * std::f32::consts::PI).sin() * 3000.0
            })
            .collect();

        // Mic: same 440Hz echo + different 1000Hz speech.
        let mut mic: Vec<f32> = (0..duration)
            .map(|i| {
                let echo = (i as f32 / sample_rate as f32 * 440.0 * 2.0 * std::f32::consts::PI)
                    .sin()
                    * 3000.0;
                let speech = (i as f32 / sample_rate as f32 * 1000.0 * 2.0 * std::f32::consts::PI)
                    .sin()
                    * 2000.0;
                echo + speech
            })
            .collect();

        // Store reference.
        let ref_pcm: Vec<u8> = reference
            .iter()
            .flat_map(|&s| {
                let clamped = s.clamp(-32767.0, 32767.0) as i16;
                clamped.to_le_bytes().to_vec()
            })
            .collect();
        set_echo_reference(&ref_pcm, sample_rate);

        // Apply AEC.
        let mic_rms_before = rms(&mic);
        cancel_echo(&mut mic, sample_rate);
        let mic_rms_after = rms(&mic);

        // The echo component should be reduced.
        // Note: NLMS needs convergence time, so early samples won't be cancelled.
        // Check the latter half where filter has converged.
        let latter_half = &mic[duration / 2..];
        let latter_rms = rms(latter_half);

        // The 440Hz echo should be significantly reduced in the latter half.
        // Original mic RMS was ~sqrt(3000² + 2000²) ≈ 3606
        // After AEC, ideally only the 1000Hz speech remains: ~2000 (but filter isn't perfect)
        assert!(
            latter_rms < mic_rms_before * 0.9,
            "AEC should reduce echo: before={:.0}, after_latter={:.0}",
            mic_rms_before,
            latter_rms
        );

        clear_echo_reference();
    }

    #[test]
    fn cancel_echo_no_reference() {
        let _guard = TEST_LOCK.lock().unwrap();

        // No reference stored — should be a no-op.
        clear_echo_reference();
        let mut mic: Vec<f32> = (0..1600).map(|i| (i as f32 * 0.1).sin() * 5000.0).collect();
        let original = mic.clone();

        cancel_echo(&mut mic, 16000);

        // Should be unchanged.
        for (a, b) in mic.iter().zip(original.iter()) {
            assert!(
                (a - b).abs() < 0.01,
                "Should be unchanged without reference"
            );
        }
    }

    #[test]
    fn set_and_clear_reference() {
        let _guard = TEST_LOCK.lock().unwrap();

        let pcm = vec![0u8; 3200]; // 1600 samples of silence.
        set_echo_reference(&pcm, 16000);

        let has_ref = ECHO_REF.lock().unwrap().is_some();
        assert!(has_ref, "Reference should be set");

        clear_echo_reference();

        let has_ref = ECHO_REF.lock().unwrap().is_some();
        assert!(!has_ref, "Reference should be cleared");
    }

    // TEST_LOCK serializes tests that touch the global ECHO_REF; the guard is
    // intentionally held across awaits because it only gates other tests in
    // this module, never production code.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn process_aec_skips_stale_reference() {
        let _guard = TEST_LOCK.lock().unwrap();

        let sample_rate = 16000;
        let pcm = vec![0u8; 3200];
        let path = format!("/tmp/geniepod-aec-stale-test-{}.wav", std::process::id());
        let wav = test_wav(&pcm, sample_rate);
        tokio::fs::write(&path, &wav).await.unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        *ECHO_REF.lock().unwrap() = Some(EchoReference {
            samples: vec![1000.0; 1600],
            sample_rate,
            play_start_ms: now_ms - 60_000,
        });

        process_aec(&path, sample_rate).await;

        let after = tokio::fs::read(&path).await.unwrap();
        assert_eq!(after, wav, "stale AEC reference should not modify WAV");
        assert!(
            ECHO_REF.lock().unwrap().is_none(),
            "stale reference should be cleared"
        );

        let _ = tokio::fs::remove_file(&path).await;
    }

    fn test_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let data_size = pcm.len() as u32;
        let byte_rate = sample_rate * channels as u32 * bits_per_sample as u32 / 8;
        let block_align = channels * bits_per_sample / 8;

        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_size).to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"fmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&byte_rate.to_le_bytes());
        wav.extend_from_slice(&block_align.to_le_bytes());
        wav.extend_from_slice(&bits_per_sample.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_size.to_le_bytes());
        wav.extend_from_slice(pcm);
        wav
    }

    fn rms(samples: &[f32]) -> f32 {
        let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum / samples.len() as f64).sqrt() as f32
    }
}
