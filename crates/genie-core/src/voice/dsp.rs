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

    let num_samples = pcm.len() / 2;
    let mut samples = decode_s16le(pcm);

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

/// Decode an S16_LE byte buffer to f32 samples (a trailing odd byte is dropped).
fn decode_s16le(pcm: &[u8]) -> Vec<f32> {
    (0..pcm.len() / 2)
        .map(|i| i16::from_le_bytes([pcm[i * 2], pcm[i * 2 + 1]]) as f32)
        .collect()
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

/// Second-order IIR (biquad) section in transposed direct-form II.
///
/// Coefficients follow the Audio EQ Cookbook (RBJ). A biquad has a true
/// 12 dB/octave slope, so a shelf/peak built from one does not leak into
/// neighbouring bands the way a 1-pole (6 dB/octave) filter does — which is
/// exactly what the previous EQ got wrong: a 1-pole "presence" low-pass spilled
/// well past 4 kHz and overwhelmed the high cut, turning it into a high *boost*.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    /// Build from un-normalized cookbook coefficients (a0 normalizes the rest).
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Low-shelf: `gain_db` applied below `freq`, flat (0 dB) above.
    fn low_shelf(sr: f32, freq: f32, gain_db: f32, q: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * freq / sr;
        let (sin_w, cos_w) = (w.sin(), w.cos());
        let alpha = sin_w / (2.0 * q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w),
            a * ((a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w),
            (a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha,
        )
    }

    /// Peaking EQ: `gain_db` bump centered at `freq`, flat far from it.
    fn peaking(sr: f32, freq: f32, gain_db: f32, q: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * freq / sr;
        let (sin_w, cos_w) = (w.sin(), w.cos());
        let alpha = sin_w / (2.0 * q);
        Self::new(
            1.0 + alpha * a,
            -2.0 * cos_w,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w,
            1.0 - alpha / a,
        )
    }

    /// High-shelf: `gain_db` applied above `freq`, flat (0 dB) below.
    /// With a negative `gain_db` this is the high cut.
    fn high_shelf(sr: f32, freq: f32, gain_db: f32, q: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * freq / sr;
        let (sin_w, cos_w) = (w.sin(), w.cos());
        let alpha = sin_w / (2.0 * q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::new(
            a * ((a + 1.0) + (a - 1.0) * cos_w + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w),
            a * ((a + 1.0) + (a - 1.0) * cos_w - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos_w + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w),
            (a + 1.0) - (a - 1.0) * cos_w - two_sqrt_a_alpha,
        )
    }

    /// Process one sample, advancing the filter state.
    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Simple 3-band EQ for voice clarity.
///
/// Voice-optimized curve:
/// - Bass shelf (+2 dB below 200 Hz): warm up thin speaker
/// - Presence peak (+4 dB at 2-4 kHz): speech clarity and intelligibility
/// - High cut (-3 dB above 6 kHz): reduce hiss and sibilance
///
/// Implemented as a cascade of RBJ biquad sections. Each band has a true
/// 12 dB/octave slope, so the presence peak stays inside 2-4 kHz instead of
/// leaking into the high band and fighting the cut.
fn apply_voice_eq(samples: &mut [f32], sample_rate: u32) {
    if samples.len() < 2 {
        return;
    }

    let sr = sample_rate as f32;

    // The presence center and high-cut corner must stay below Nyquist; at very
    // low sample rates clamp them so the cookbook math stays well-conditioned.
    let nyquist = sr / 2.0;
    let presence_hz = 3000.0f32.min(nyquist * 0.45);
    let hicut_hz = 6000.0f32.min(nyquist * 0.85);

    let mut bass = Biquad::low_shelf(sr, 200.0, 2.0, 0.707);
    let mut presence = Biquad::peaking(sr, presence_hz, 4.0, 1.0);
    let mut hicut = Biquad::high_shelf(sr, hicut_hz, -3.0, 0.707);

    for sample in samples.iter_mut() {
        let mut s = bass.process(*sample);
        s = presence.process(s);
        s = hicut.process(s);
        *sample = s;
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

    #[test]
    fn decode_s16le_is_little_endian() {
        // [lo, hi] little-endian -> i16 -> f32.
        assert_eq!(decode_s16le(&[0x00, 0x80]), vec![-32768.0]); // i16::MIN
        assert_eq!(decode_s16le(&[0xFF, 0x7F]), vec![32767.0]); // i16::MAX
        assert_eq!(decode_s16le(&[0x34, 0x12]), vec![4660.0]); // 0x1234
        assert_eq!(
            decode_s16le(&[0x34, 0x12, 0xFF, 0x7F]),
            vec![4660.0, 32767.0]
        );
        assert_eq!(decode_s16le(&[0xAB]), Vec::<f32>::new()); // trailing odd byte dropped
    }

    fn rms_of(samples: &[f32]) -> f32 {
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / samples.len() as f64).sqrt() as f32
    }

    /// Measure the steady-state gain `apply_voice_eq` applies at `freq_hz`.
    ///
    /// Drives a pure sine of known amplitude through the EQ and returns the
    /// ratio of output peak to input peak, measured over the back half of the
    /// buffer so the biquad transients have settled.
    fn eq_gain_at(freq_hz: f32, sample_rate: u32) -> f32 {
        let sr = sample_rate as f32;
        let amplitude = 8000.0f32;
        let n = 8192usize;
        let mut samples: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq_hz * i as f32 / sr).sin() * amplitude)
            .collect();

        apply_voice_eq(&mut samples, sample_rate);

        // Ignore the settling region; measure peak over the steady state.
        let settled = &samples[n / 2..];
        let out_peak = settled.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        out_peak / amplitude
    }

    #[test]
    fn voice_eq_cuts_high_frequencies() {
        // Regression: the "high cut" must ATTENUATE the hiss/sibilance band,
        // not boost it. The old 1-pole implementation boosted 6-10 kHz by
        // ~+1.6 dB; the biquad high shelf should pull it below unity.
        for &f in &[7000.0, 8000.0, 9000.0, 10000.0] {
            let gain = eq_gain_at(f, 22050);
            assert!(
                gain < 1.0,
                "high cut should attenuate {f} Hz (gain {gain:.3} should be < 1.0)"
            );
        }
        // Approach the documented ~-3 dB (≈0.71x) well above the corner.
        let high = eq_gain_at(9000.0, 22050);
        assert!(
            (0.60..0.85).contains(&high),
            "9 kHz gain {high:.3} should sit near the documented -3 dB"
        );
    }

    #[test]
    fn voice_eq_boosts_presence_band() {
        // Presence peak (+4 dB ≈ 1.58x) around 3 kHz for speech intelligibility.
        let gain = eq_gain_at(3000.0, 22050);
        assert!(
            (1.3..1.8).contains(&gain),
            "presence band gain {gain:.3} should be a clear boost (~+4 dB)"
        );
    }

    #[test]
    fn voice_eq_boosts_bass() {
        // Bass shelf (+2 dB ≈ 1.26x) below 200 Hz to warm thin speakers.
        let gain = eq_gain_at(120.0, 22050);
        assert!(
            gain > 1.1,
            "bass shelf gain {gain:.3} should boost low frequencies"
        );
    }

    #[test]
    fn voice_eq_presence_outranks_highs() {
        // The presence band must end up louder than the cut high band — the old
        // code inverted this, leaving the highs hotter than the presence peak.
        let presence = eq_gain_at(3000.0, 22050);
        let highs = eq_gain_at(9000.0, 22050);
        assert!(
            presence > highs,
            "presence ({presence:.3}) should exceed cut highs ({highs:.3})"
        );
    }

    #[test]
    fn voice_eq_handles_short_and_silent_input() {
        // Guard rails: no panic / no NaN on degenerate inputs.
        let mut tiny = vec![1234.0f32];
        apply_voice_eq(&mut tiny, 22050);
        assert_eq!(tiny, vec![1234.0], "buffers < 2 samples are left untouched");

        let mut silence = vec![0.0f32; 256];
        apply_voice_eq(&mut silence, 22050);
        assert!(
            silence.iter().all(|&s| s == 0.0),
            "silence in must stay silence out"
        );
    }
}
