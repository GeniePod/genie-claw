use crate::memory::policy::{IdentityConfidence, MemoryReadContext};
use genie_common::config::{
    SpeakerIdentityConfig, SpeakerIdentityProvider as SpeakerIdentityProviderKind,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerIdentity {
    pub name: Option<String>,
    pub confidence: IdentityConfidence,
}

impl Default for SpeakerIdentity {
    fn default() -> Self {
        Self {
            name: None,
            confidence: IdentityConfidence::Unknown,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeakerIdentityRequest<'a> {
    pub wav_path: Option<&'a str>,
    pub transcript: &'a str,
    pub detected_language: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct LocalBiometricRecognizer {
    pub profile_dir: PathBuf,
    pub min_score: f32,
}

#[derive(Debug, Clone)]
pub struct SpeakerMatch {
    pub name: String,
    pub score: f32,
    pub profile_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerProfile {
    pub schema_version: u32,
    pub fingerprint_version: String,
    pub name: String,
    pub created_ms: u128,
    pub sample_rate: u32,
    pub sample_count: usize,
    pub fingerprint: Vec<f32>,
}

#[derive(Debug, Clone)]
struct WavAudio {
    sample_rate: u32,
    samples: Vec<f32>,
}

const PROFILE_SCHEMA_VERSION: u32 = 1;
const FINGERPRINT_VERSION: &str = "genie_acoustic_v1";
const MIN_PROFILE_SAMPLES: usize = 1600;
const BAND_RANGES_HZ: &[(f32, f32)] = &[
    (80.0, 180.0),
    (180.0, 300.0),
    (300.0, 500.0),
    (500.0, 800.0),
    (800.0, 1200.0),
    (1200.0, 1800.0),
    (1800.0, 2600.0),
    (2600.0, 3600.0),
];

#[derive(Debug, Clone, Default)]
pub enum SpeakerIdentityProvider {
    #[default]
    None,
    Fixed(SpeakerIdentity),
    LocalBiometric(LocalBiometricRecognizer),
}

impl SpeakerIdentityProvider {
    pub fn from_config(config: &SpeakerIdentityConfig) -> Self {
        if !config.enabled {
            return Self::None;
        }

        match config.provider {
            SpeakerIdentityProviderKind::None => Self::None,
            SpeakerIdentityProviderKind::Fixed => {
                let name = config.fixed_name.trim();
                if name.is_empty() {
                    Self::None
                } else {
                    Self::Fixed(SpeakerIdentity {
                        name: Some(name.to_string()),
                        confidence: identity_confidence_from_str(&config.fixed_confidence),
                    })
                }
            }
            SpeakerIdentityProviderKind::LocalBiometric => {
                Self::LocalBiometric(LocalBiometricRecognizer {
                    profile_dir: config.local_profile_dir.clone(),
                    min_score: config.local_min_score,
                })
            }
        }
    }

    pub fn identify(&self, request: &SpeakerIdentityRequest<'_>) -> SpeakerIdentity {
        match self {
            Self::None => SpeakerIdentity::default(),
            Self::Fixed(identity) => identity.clone(),
            Self::LocalBiometric(recognizer) => recognizer.identify(request),
        }
    }
}

impl LocalBiometricRecognizer {
    pub fn identify(&self, request: &SpeakerIdentityRequest<'_>) -> SpeakerIdentity {
        let _ = (request.transcript, request.detected_language);
        let Some(wav_path) = request.wav_path else {
            return SpeakerIdentity::default();
        };

        match identify_speaker_file(&self.profile_dir, wav_path, self.min_score) {
            Ok(Some(result)) => {
                let confidence = if result.score >= 0.92 {
                    IdentityConfidence::High
                } else {
                    IdentityConfidence::Medium
                };
                SpeakerIdentity {
                    name: Some(result.name),
                    confidence,
                }
            }
            Ok(None) => SpeakerIdentity::default(),
            Err(err) => {
                tracing::warn!(error = %err, "local speaker identification failed");
                SpeakerIdentity::default()
            }
        }
    }
}

/// Enroll a local speaker profile from a short WAV sample.
///
/// The generated profile is local-only JSON. It stores a compact acoustic
/// fingerprint, not raw audio. This is useful for routing household memory, but
/// it is not a hostile-user authentication boundary.
pub fn enroll_speaker_file(
    profile_dir: impl AsRef<Path>,
    name: &str,
    wav_path: impl AsRef<Path>,
) -> anyhow::Result<SpeakerProfile> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("speaker name is required");
    }

    let audio = read_wav_mono_f32(wav_path.as_ref())?;
    let fingerprint = extract_fingerprint(&audio)?;
    let profile = SpeakerProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        fingerprint_version: FINGERPRINT_VERSION.to_string(),
        name: name.to_string(),
        created_ms: unix_time_ms(),
        sample_rate: audio.sample_rate,
        sample_count: audio.samples.len(),
        fingerprint,
    };

    let profile_dir = profile_dir.as_ref();
    std::fs::create_dir_all(profile_dir)?;
    secure_profile_dir(profile_dir)?;
    let path = profile_path(profile_dir, name);
    let bytes = serde_json::to_vec_pretty(&profile)?;
    std::fs::write(path, bytes)?;
    Ok(profile)
}

/// Identify the best enrolled speaker for a WAV file.
pub fn identify_speaker_file(
    profile_dir: impl AsRef<Path>,
    wav_path: impl AsRef<Path>,
    min_score: f32,
) -> anyhow::Result<Option<SpeakerMatch>> {
    let audio = read_wav_mono_f32(wav_path.as_ref())?;
    let fingerprint = extract_fingerprint(&audio)?;
    identify_fingerprint(profile_dir, &fingerprint, min_score)
}

pub fn list_speaker_profiles(profile_dir: impl AsRef<Path>) -> anyhow::Result<Vec<SpeakerProfile>> {
    let profile_dir = profile_dir.as_ref();
    if !profile_dir.exists() {
        return Ok(Vec::new());
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(profile_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_profile_file(&path) {
            continue;
        }
        if let Ok(profile) = read_profile(&path) {
            profiles.push(profile);
        }
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(profiles)
}

pub fn remove_speaker_profile(
    profile_dir: impl AsRef<Path>,
    name: &str,
) -> anyhow::Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        anyhow::bail!("speaker name is required");
    }

    let path = profile_path(profile_dir.as_ref(), name);
    if !path.exists() {
        anyhow::bail!("speaker profile not found: {}", path.display());
    }
    std::fs::remove_file(&path)?;
    Ok(path)
}

fn identify_fingerprint(
    profile_dir: impl AsRef<Path>,
    fingerprint: &[f32],
    min_score: f32,
) -> anyhow::Result<Option<SpeakerMatch>> {
    let profile_dir = profile_dir.as_ref();
    if !profile_dir.exists() {
        return Ok(None);
    }

    let mut best: Option<SpeakerMatch> = None;
    for entry in std::fs::read_dir(profile_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !is_profile_file(&path) {
            continue;
        }
        let Ok(profile) = read_profile(&path) else {
            continue;
        };
        if profile.fingerprint_version != FINGERPRINT_VERSION {
            continue;
        }
        let score = fingerprint_score(fingerprint, &profile.fingerprint);
        if score >= min_score && best.as_ref().is_none_or(|current| score > current.score) {
            best = Some(SpeakerMatch {
                name: profile.name,
                score,
                profile_path: path,
            });
        }
    }

    Ok(best)
}

fn read_profile(path: &Path) -> anyhow::Result<SpeakerProfile> {
    let bytes = std::fs::read(path)?;
    let profile: SpeakerProfile = serde_json::from_slice(&bytes)?;
    Ok(profile)
}

fn is_profile_file(path: &Path) -> bool {
    path.is_file()
        && path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".speaker.json"))
}

fn profile_path(profile_dir: &Path, name: &str) -> PathBuf {
    profile_dir.join(format!("{}.speaker.json", sanitize_profile_name(name)))
}

#[cfg(unix)]
fn secure_profile_dir(profile_dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(profile_dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_profile_dir(_profile_dir: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn sanitize_profile_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "speaker".into()
    } else {
        trimmed
    }
}

fn read_wav_mono_f32(path: &Path) -> anyhow::Result<WavAudio> {
    let data = std::fs::read(path)?;
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        anyhow::bail!("unsupported WAV file: expected RIFF/WAVE");
    }

    let mut offset = 12usize;
    let mut audio_format = None;
    let mut channels = None;
    let mut sample_rate = None;
    let mut bits_per_sample = None;
    let mut data_range = None;

    while offset + 8 <= data.len() {
        let chunk_id = &data[offset..offset + 4];
        let chunk_size = u32::from_le_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        let chunk_start = offset + 8;
        let chunk_end = chunk_start.saturating_add(chunk_size).min(data.len());

        match chunk_id {
            b"fmt " => {
                if chunk_size < 16 || chunk_end > data.len() {
                    anyhow::bail!("invalid WAV fmt chunk");
                }
                audio_format = Some(u16::from_le_bytes([
                    data[chunk_start],
                    data[chunk_start + 1],
                ]));
                channels = Some(u16::from_le_bytes([
                    data[chunk_start + 2],
                    data[chunk_start + 3],
                ]));
                sample_rate = Some(u32::from_le_bytes([
                    data[chunk_start + 4],
                    data[chunk_start + 5],
                    data[chunk_start + 6],
                    data[chunk_start + 7],
                ]));
                bits_per_sample = Some(u16::from_le_bytes([
                    data[chunk_start + 14],
                    data[chunk_start + 15],
                ]));
            }
            b"data" => {
                data_range = Some((chunk_start, chunk_end));
            }
            _ => {}
        }

        offset = chunk_end + (chunk_size % 2);
    }

    if audio_format != Some(1) || bits_per_sample != Some(16) {
        anyhow::bail!("unsupported WAV file: expected 16-bit PCM");
    }
    let channels = channels.unwrap_or(0);
    if channels == 0 {
        anyhow::bail!("unsupported WAV file: missing channel count");
    }
    let sample_rate = sample_rate.unwrap_or(0);
    if sample_rate == 0 {
        anyhow::bail!("unsupported WAV file: missing sample rate");
    }
    let Some((start, end)) = data_range else {
        anyhow::bail!("unsupported WAV file: missing data chunk");
    };

    let frame_bytes = usize::from(channels) * 2;
    let mut samples = Vec::with_capacity((end - start) / frame_bytes);
    let mut cursor = start;
    while cursor + frame_bytes <= end {
        let mut mixed = 0.0f32;
        for ch in 0..usize::from(channels) {
            let i = cursor + ch * 2;
            let sample = i16::from_le_bytes([data[i], data[i + 1]]) as f32 / 32768.0;
            mixed += sample;
        }
        samples.push(mixed / f32::from(channels));
        cursor += frame_bytes;
    }

    Ok(WavAudio {
        sample_rate,
        samples,
    })
}

fn extract_fingerprint(audio: &WavAudio) -> anyhow::Result<Vec<f32>> {
    let voiced = voiced_samples(&audio.samples);
    if voiced.len() < MIN_PROFILE_SAMPLES {
        anyhow::bail!("voice sample is too short or too quiet for speaker identification");
    }

    let frame_len = ((audio.sample_rate as f32 * 0.032).round() as usize).clamp(256, 2048);
    let hop = (frame_len / 2).max(128);
    let mut features = Vec::new();
    let mut cursor = 0usize;
    while cursor + frame_len <= voiced.len() && features.len() < 160 {
        features.push(frame_features(
            &voiced[cursor..cursor + frame_len],
            audio.sample_rate,
        ));
        cursor += hop;
    }

    if features.is_empty() {
        anyhow::bail!("voice sample has no usable voiced frames");
    }

    let feature_len = features[0].len();
    let mut mean = vec![0.0f32; feature_len];
    for frame in &features {
        for (idx, value) in frame.iter().enumerate() {
            mean[idx] += *value;
        }
    }
    for value in &mut mean {
        *value /= features.len() as f32;
    }

    let mut stddev = vec![0.0f32; feature_len];
    for frame in &features {
        for (idx, value) in frame.iter().enumerate() {
            let delta = *value - mean[idx];
            stddev[idx] += delta * delta;
        }
    }
    for value in &mut stddev {
        *value = (*value / features.len() as f32).sqrt();
    }

    let mut fingerprint = mean;
    fingerprint.extend(stddev);
    normalize_vector(&mut fingerprint);
    Ok(fingerprint)
}

fn voiced_samples(samples: &[f32]) -> Vec<f32> {
    let max_amp = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if max_amp <= 0.0001 {
        return Vec::new();
    }
    let threshold = (max_amp * 0.08).max(0.006);
    samples
        .iter()
        .copied()
        .filter(|sample| sample.abs() >= threshold)
        .collect()
}

fn frame_features(frame: &[f32], sample_rate: u32) -> Vec<f32> {
    let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt();
    let zcr = zero_crossing_rate(frame);
    let bands = band_powers(frame, sample_rate);
    let total_power = bands.iter().sum::<f32>().max(1e-9);

    let mut out = Vec::with_capacity(3 + bands.len());
    out.push((rms.max(1e-6).log10() + 6.0) / 6.0);
    out.push(zcr);
    out.push(spectral_centroid(&bands) / BAND_RANGES_HZ.len() as f32);
    for power in bands {
        out.push((power / total_power).max(1e-6).log10() / 6.0 + 1.0);
    }
    out
}

fn zero_crossing_rate(frame: &[f32]) -> f32 {
    if frame.len() < 2 {
        return 0.0;
    }
    let crossings = frame
        .windows(2)
        .filter(|pair| (pair[0] >= 0.0 && pair[1] < 0.0) || (pair[0] < 0.0 && pair[1] >= 0.0))
        .count();
    crossings as f32 / (frame.len() - 1) as f32
}

fn spectral_centroid(bands: &[f32]) -> f32 {
    let total = bands.iter().sum::<f32>();
    if total <= 1e-9 {
        return 0.0;
    }
    bands
        .iter()
        .enumerate()
        .map(|(idx, power)| (idx as f32 + 0.5) * power)
        .sum::<f32>()
        / total
}

fn band_powers(frame: &[f32], sample_rate: u32) -> Vec<f32> {
    BAND_RANGES_HZ
        .iter()
        .map(|(low, high)| {
            let center = (low + high) * 0.5;
            goertzel_power(frame, sample_rate, center)
        })
        .collect()
}

fn goertzel_power(frame: &[f32], sample_rate: u32, frequency_hz: f32) -> f32 {
    let omega = 2.0 * std::f32::consts::PI * frequency_hz / sample_rate as f32;
    let coeff = 2.0 * omega.cos();
    let mut q0;
    let mut q1 = 0.0f32;
    let mut q2 = 0.0f32;

    for (idx, sample) in frame.iter().enumerate() {
        let window = 0.5
            - 0.5
                * (2.0 * std::f32::consts::PI * idx as f32 / (frame.len() - 1).max(1) as f32).cos();
        q0 = coeff * q1 - q2 + sample * window;
        q2 = q1;
        q1 = q0;
    }

    (q1 * q1 + q2 * q2 - coeff * q1 * q2).max(0.0)
}

fn normalize_vector(values: &mut [f32]) {
    let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-9 {
        for value in values {
            *value /= norm;
        }
    }
}

fn fingerprint_score(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || b.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let dot = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>();
    let distance = a
        .iter()
        .zip(b.iter())
        .map(|(x, y)| {
            let delta = x - y;
            delta * delta
        })
        .sum::<f32>()
        .sqrt();
    let similarity = dot.clamp(0.0, 1.0);
    let distance_penalty = (distance / 0.35).clamp(0.0, 1.0);
    let raw = (similarity * (1.0 - distance_penalty)).clamp(0.0, 1.0);
    raw.powf(0.45)
}

fn unix_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

pub fn build_memory_read_context(text: &str, speaker: &SpeakerIdentity) -> MemoryReadContext {
    let lower = text.trim().to_ascii_lowercase();
    MemoryReadContext {
        identity_confidence: speaker.confidence,
        explicit_named_person: mentions_named_person(&lower),
        explicit_private_intent: contains_any(
            &lower,
            &[
                "private",
                "privately",
                "for me only",
                "don't say this aloud",
                "do not say this aloud",
            ],
        ),
        shared_space_voice: true,
    }
}

fn mentions_named_person(lower: &str) -> bool {
    starts_with_any(
        lower,
        &[
            "what does ",
            "what did ",
            "tell me about ",
            "who is ",
            "does ",
            "is ",
            "ask ",
            "call ",
            "text ",
            "message ",
            "remind ",
        ],
    ) || contains_any(
        lower,
        &[
            " my wife",
            " my husband",
            " my son",
            " my daughter",
            " my mom",
            " my mother",
            " my dad",
            " my father",
            " my friend",
            " my partner",
        ],
    )
}

fn starts_with_any(text: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| text.starts_with(prefix))
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn identity_confidence_from_str(value: &str) -> IdentityConfidence {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => IdentityConfidence::High,
        "medium" => IdentityConfidence::Medium,
        "low" => IdentityConfidence::Low,
        _ => IdentityConfidence::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_memory_read_context_uses_speaker_confidence() {
        let ctx = build_memory_read_context(
            "what do you remember about me",
            &SpeakerIdentity {
                name: Some("Jared".into()),
                confidence: IdentityConfidence::High,
            },
        );
        assert_eq!(ctx.identity_confidence, IdentityConfidence::High);
        assert!(!ctx.explicit_named_person);
        assert!(ctx.shared_space_voice);
    }

    #[test]
    fn build_memory_read_context_detects_named_person_request() {
        let ctx =
            build_memory_read_context("what does Maya like to drink", &SpeakerIdentity::default());
        assert!(ctx.explicit_named_person);
    }

    #[test]
    fn build_memory_read_context_detects_private_intent() {
        let ctx = build_memory_read_context(
            "remember this privately and do not say this aloud",
            &SpeakerIdentity::default(),
        );
        assert!(ctx.explicit_private_intent);
    }

    #[test]
    fn fixed_provider_returns_configured_identity() {
        let provider = SpeakerIdentityProvider::from_config(&SpeakerIdentityConfig {
            enabled: true,
            provider: SpeakerIdentityProviderKind::Fixed,
            fixed_name: "Jared".into(),
            fixed_confidence: "high".into(),
            local_profile_dir: PathBuf::from("/opt/geniepod/data/speakers"),
            local_min_score: 0.82,
        });
        let identity = provider.identify(&SpeakerIdentityRequest {
            wav_path: None,
            transcript: "what do you remember about me",
            detected_language: Some("en"),
        });
        assert_eq!(identity.name.as_deref(), Some("Jared"));
        assert_eq!(identity.confidence, IdentityConfidence::High);
    }

    #[test]
    fn local_biometric_provider_builds_with_future_runtime_boundary() {
        let provider = SpeakerIdentityProvider::from_config(&SpeakerIdentityConfig {
            enabled: true,
            provider: SpeakerIdentityProviderKind::LocalBiometric,
            fixed_name: String::new(),
            fixed_confidence: "high".into(),
            local_profile_dir: PathBuf::from("/opt/geniepod/data/speakers"),
            local_min_score: 0.88,
        });

        match provider {
            SpeakerIdentityProvider::LocalBiometric(recognizer) => {
                assert_eq!(
                    recognizer.profile_dir,
                    PathBuf::from("/opt/geniepod/data/speakers")
                );
                assert!((recognizer.min_score - 0.88).abs() < f32::EPSILON);
            }
            _ => panic!("expected local biometric provider"),
        }
    }

    #[test]
    fn local_biometric_enrolls_and_identifies_matching_voice() {
        let dir =
            std::env::temp_dir().join(format!("geniepod-speaker-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let jared_train = dir.join("jared-train.wav");
        let jared_test = dir.join("jared-test.wav");
        let maya_test = dir.join("maya-test.wav");
        write_test_wav(&jared_train, 180.0, 620.0);
        write_test_wav(&jared_test, 182.0, 615.0);
        write_test_wav(&maya_test, 420.0, 1180.0);

        let profile = enroll_speaker_file(&dir, "Jared", &jared_train).unwrap();
        assert_eq!(profile.name, "Jared");
        assert_eq!(list_speaker_profiles(&dir).unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }

        let matched = identify_speaker_file(&dir, &jared_test, 0.82)
            .unwrap()
            .expect("expected match");
        assert_eq!(matched.name, "Jared");
        assert!(matched.score >= 0.82);

        let rejected = identify_speaker_file(&dir, &maya_test, 0.82).unwrap();
        assert!(rejected.is_none());

        let removed = remove_speaker_profile(&dir, "Jared").unwrap();
        assert!(removed.ends_with("jared.speaker.json"));
        assert!(list_speaker_profiles(&dir).unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_biometric_provider_returns_identity_when_profile_matches() {
        let dir = std::env::temp_dir().join(format!(
            "geniepod-speaker-provider-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let train = dir.join("train.wav");
        let test = dir.join("test.wav");
        write_test_wav(&train, 220.0, 760.0);
        write_test_wav(&test, 221.0, 755.0);
        enroll_speaker_file(&dir, "Jared", &train).unwrap();

        let provider = SpeakerIdentityProvider::LocalBiometric(LocalBiometricRecognizer {
            profile_dir: dir.clone(),
            min_score: 0.82,
        });
        let identity = provider.identify(&SpeakerIdentityRequest {
            wav_path: Some(test.to_str().unwrap()),
            transcript: "what do you remember about me",
            detected_language: Some("en"),
        });

        assert_eq!(identity.name.as_deref(), Some("Jared"));
        assert!(identity.confidence >= IdentityConfidence::Medium);

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_test_wav(path: &Path, f1: f32, f2: f32) {
        let sample_rate = 16_000u32;
        let duration_secs = 2.0f32;
        let sample_count = (sample_rate as f32 * duration_secs) as usize;
        let mut pcm = Vec::with_capacity(sample_count * 2);
        for i in 0..sample_count {
            let t = i as f32 / sample_rate as f32;
            let envelope = if i < 800 {
                i as f32 / 800.0
            } else if i > sample_count - 800 {
                (sample_count - i) as f32 / 800.0
            } else {
                1.0
            };
            let sample = ((2.0 * std::f32::consts::PI * f1 * t).sin() * 0.42
                + (2.0 * std::f32::consts::PI * f2 * t).sin() * 0.18)
                * envelope;
            let sample = (sample * i16::MAX as f32) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }

        let channels = 1u16;
        let bits_per_sample = 16u16;
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
        let block_align = channels * bits_per_sample / 8;
        let data_size = pcm.len() as u32;
        let file_size = 36 + data_size;

        let mut wav = Vec::with_capacity(44 + pcm.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&file_size.to_le_bytes());
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
        wav.extend_from_slice(&pcm);
        std::fs::write(path, wav).unwrap();
    }
}
