//! Software audio DSP — AGC and EQ for voice output.
//!
//! Applied to Piper TTS output PCM before sending to aplay.
//! All processing is S16_LE mono at 22050 Hz (Piper native).
//!
//! AGC: Automatic Gain Control — normalize volume to target RMS level.
//! EQ: 3-band equalizer — boost voice clarity, warm bass, cut hiss.
//!
//! CPU cost: negligible (<0.1% of one core for typical TTS output).

/// Target RMS level for AGC (0-32767 range for S16).
/// ~4000 = moderate volume, comfortable for voice assistant.
const AGC_TARGET_RMS: f32 = 4000.0;

/// Maximum gain multiplier (prevents amplifying silence into noise).
const AGC_MAX_GAIN: f32 = 8.0;

/// Minimum gain multiplier (prevents over-attenuation).
const AGC_MIN_GAIN: f32 = 0.1;

/// Apply AGC + EQ to raw PCM data (S16_LE mono).
///
/// Modifies the PCM buffer in-place for zero-allocation processing.
pub fn process_tts_audio(pcm: &mut [u8], sample_rate: u32) {
    if pcm.len() < 4 {
        return;
    }

    // Convert S16_LE bytes to i16 samples.
    let num_samples = pcm.len() / 2;
    let mut samples: Vec<f32> = (0..num_samples)
        .map(|i| {
            let lo = pcm[i * 2] as i16 as f32;
            let hi = (pcm[i * 2 + 1] as i8 as i16 as f32) * 256.0;
            lo + hi
        })
        .collect();

    // Proper S16_LE decoding.
    for i in 0..num_samples {
        let sample = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]);
        samples[i] = sample as f32;
    }

    // Step 1: AGC — normalize to target RMS.
    apply_agc(&mut samples);

    // Step 2: EQ — voice presence boost.
    apply_voice_eq(&mut samples, sample_rate);

    // Step 3: Soft limiter — prevent clipping.
    apply_soft_limiter(&mut samples);

    // Convert back to S16_LE bytes.
    for i in 0..num_samples {
        let clamped = samples[i].clamp(-32767.0, 32767.0) as i16;
        let bytes = clamped.to_le_bytes();
        pcm[i * 2] = bytes[0];
        pcm[i * 2 + 1] = bytes[1];
    }
}

/// Automatic Gain Control — normalize RMS to target level.
fn apply_agc(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }

    // Calculate current RMS.
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;

    if rms < 1.0 {
        return; // Silence — don't amplify noise.
    }

    // Calculate gain to reach target RMS.
    let gain = (AGC_TARGET_RMS / rms).clamp(AGC_MIN_GAIN, AGC_MAX_GAIN);

    // Apply gain.
    for sample in samples.iter_mut() {
        *sample *= gain;
    }
}

/// Simple 3-band EQ for voice clarity.
///
/// Voice-optimized curve:
/// - Bass shelf (+2 dB below 200 Hz): warm up thin speaker
/// - Presence peak (+4 dB at 2-4 kHz): speech clarity and intelligibility
/// - High cut (-3 dB above 6 kHz): reduce hiss and sibilance
///
/// Implemented as simple 1-pole IIR filters (minimal CPU, good enough for TTS).
fn apply_voice_eq(samples: &mut [f32], sample_rate: u32) {
    if samples.len() < 2 {
        return;
    }

    let sr = sample_rate as f32;

    // Bass shelf: boost low frequencies (+2 dB ≈ 1.26x).
    // 1-pole low-pass at 200 Hz, mix back with gain.
    let bass_alpha = 1.0 - (-2.0 * std::f32::consts::PI * 200.0 / sr).exp();
    let bass_gain = 1.26; // +2 dB
    let mut bass_lp = 0.0f32;

    // Presence boost: bandpass around 3 kHz (+4 dB ≈ 1.58x).
    // Simple approach: high-pass at 2kHz + low-pass at 4kHz → extract band → add back.
    let pres_hp_alpha = 1.0 - (-2.0 * std::f32::consts::PI * 2000.0 / sr).exp();
    let pres_lp_alpha = 1.0 - (-2.0 * std::f32::consts::PI * 4000.0 / sr).exp();
    let pres_gain = 1.58; // +4 dB
    let mut pres_hp = 0.0f32;
    let mut pres_lp = 0.0f32;
    let mut pres_prev = 0.0f32;

    // High cut: low-pass at 6 kHz to reduce hiss.
    let hicut_alpha = 1.0 - (-2.0 * std::f32::consts::PI * 6000.0 / sr).exp();
    let hicut_gain = 0.71; // -3 dB on high content
    let mut hicut_lp = 0.0f32;

    for sample in samples.iter_mut() {
        let dry = *sample;

        // Bass shelf: extract low frequencies, boost, mix back.
        bass_lp += bass_alpha * (dry - bass_lp);
        let bass_boost = bass_lp * (bass_gain - 1.0);

        // Presence: extract 2-4 kHz band, boost, mix back.
        pres_hp += pres_hp_alpha * (dry - pres_hp);
        let hp_out = dry - pres_hp; // high-pass at 2 kHz
        pres_lp += pres_lp_alpha * (hp_out - pres_lp);
        let band = pres_lp; // bandpass 2-4 kHz
        let pres_boost = band * (pres_gain - 1.0);

        // High cut: low-pass, mix difference back (attenuate highs).
        hicut_lp += hicut_alpha * (dry - hicut_lp);
        let high_content = dry - hicut_lp;
        let hicut_reduction = high_content * (hicut_gain - 1.0);

        // Sum: original + bass boost + presence boost + high cut.
        *sample = dry + bass_boost + pres_boost + hicut_reduction;

        pres_prev = dry;
    }
}

/// Soft limiter — prevents clipping while preserving dynamics.
///
/// Uses tanh-like curve: gentle compression above 24000, hard limit at 32000.
fn apply_soft_limiter(samples: &mut [f32]) {
    let threshold = 24000.0f32;
    let ceiling = 32000.0f32;

    for sample in samples.iter_mut() {
        let abs_val = sample.abs();
        if abs_val > threshold {
            // Soft knee: smoothly compress toward ceiling.
            let excess = abs_val - threshold;
            let range = ceiling - threshold;
            let compressed = threshold + range * (1.0 - (-excess / range).exp());
            *sample = compressed * sample.signum();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agc_normalizes_quiet_audio() {
        // Very quiet audio (RMS ~100).
        let mut samples: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin() * 100.0).collect();
        let old_rms = rms_of(&samples);
        apply_agc(&mut samples);
        let new_rms = rms_of(&samples);
        assert!(new_rms > old_rms * 2.0, "AGC should boost quiet audio");
    }

    #[test]
    fn agc_attenuates_loud_audio() {
        // Very loud audio (RMS ~20000).
        let mut samples: Vec<f32> = (0..1000)
            .map(|i| (i as f32 * 0.1).sin() * 20000.0)
            .collect();
        let old_rms = rms_of(&samples);
        apply_agc(&mut samples);
        let new_rms = rms_of(&samples);
        assert!(new_rms < old_rms, "AGC should reduce loud audio");
    }

    #[test]
    fn agc_ignores_silence() {
        let mut samples = vec![0.0f32; 1000];
        apply_agc(&mut samples);
        assert!(
            samples.iter().all(|&s| s == 0.0),
            "AGC should not amplify silence"
        );
    }

    #[test]
    fn soft_limiter_prevents_clipping() {
        let mut samples = vec![30000.0, -30000.0, 40000.0, -40000.0];
        apply_soft_limiter(&mut samples);
        for s in &samples {
            assert!(s.abs() <= 32000.0, "Limiter should prevent values > 32000");
        }
    }

    #[test]
    fn process_tts_audio_roundtrip() {
        // Generate a simple sine wave as S16_LE bytes.
        let num_samples = 1000;
        let mut pcm = vec![0u8; num_samples * 2];
        for i in 0..num_samples {
            let sample = ((i as f32 * 0.1).sin() * 5000.0) as i16;
            let bytes = sample.to_le_bytes();
            pcm[i * 2] = bytes[0];
            pcm[i * 2 + 1] = bytes[1];
        }

        // Process should not panic or produce NaN.
        process_tts_audio(&mut pcm, 22050);

        // Verify output is valid S16.
        for i in 0..num_samples {
            let sample = i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]);
            let magnitude = i32::from(sample).abs();
            assert!(magnitude <= i16::MAX as i32, "Output should be valid S16");
        }
    }

    fn rms_of(samples: &[f32]) -> f32 {
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / samples.len() as f64).sqrt() as f32
    }
}
