use anyhow::Result;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};

/// Extra headroom beyond PCM duration before we kill a stuck `aplay` (#617).
const APLAY_TIMEOUT_MARGIN: Duration = Duration::from_secs(10);
/// Upper bound so a pathological buffer cannot get a multi-minute deadline.
const APLAY_TIMEOUT_MAX: Duration = Duration::from_secs(120);
/// Fixed deadline for WAV file playback (duration unknown without parsing).
const APLAY_WAV_TIMEOUT: Duration = Duration::from_secs(120);

fn aplay_timeout_for_pcm(pcm_len: usize, sample_rate: u32) -> Duration {
    let bytes_per_sec = (sample_rate as usize).saturating_mul(2).max(1);
    let audio_secs = pcm_len.saturating_div(bytes_per_sec).max(1) as u64;
    Duration::from_secs(
        audio_secs
            .saturating_add(APLAY_TIMEOUT_MARGIN.as_secs())
            .min(APLAY_TIMEOUT_MAX.as_secs()),
    )
}

async fn wait_aplay_or_timeout(
    mut child: Child,
    timeout: Duration,
    label: &'static str,
) -> Result<()> {
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if !status.success() {
                tracing::warn!(label, "aplay exited with error");
            }
            Ok(())
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            tracing::warn!(
                label,
                timeout_secs = timeout.as_secs(),
                "aplay timed out — voice loop continuing"
            );
            Ok(())
        }
    }
}

/// Phase markers for the first-voice-reply latency banner (issue #19).
///
/// The voice loop resets these to `None` before each cycle's TTS phase via
/// `reset_first_audio_marker`. They're stamped at distinct moments inside
/// `TtsEngine::speak()` so the banner can break "STT done -> first audio"
/// into LLM-thinking-until-first-sentence vs first-sentence-Piper-synth.
///
/// - `FIRST_SPEAK_CALLED_AT` — first `speak()` entry, i.e. the moment
///   `stream_and_speak` decided it has enough text to start synthesizing.
/// - `FIRST_AUDIO_AT` — the moment the first PCM byte is about to hit
///   `aplay`'s stdin. ALSA adds a few ms before audio is audible.
static FIRST_SPEAK_CALLED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
static FIRST_AUDIO_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Reset the first-audio and first-speak timestamps. Call before the TTS
/// phase of a voice cycle whose latency you want to measure.
pub fn reset_first_audio_marker() {
    if let Ok(mut g) = FIRST_AUDIO_AT.lock() {
        *g = None;
    }
    if let Ok(mut g) = FIRST_SPEAK_CALLED_AT.lock() {
        *g = None;
    }
}

/// Read the first-audio timestamp captured since the last reset, if any.
pub fn first_audio_at() -> Option<std::time::Instant> {
    FIRST_AUDIO_AT.lock().ok().and_then(|g| *g)
}

/// Read the timestamp of the first `speak()` call since the last reset.
pub fn first_speak_called_at() -> Option<std::time::Instant> {
    FIRST_SPEAK_CALLED_AT.lock().ok().and_then(|g| *g)
}

fn mark_first_audio() {
    if let Ok(mut g) = FIRST_AUDIO_AT.lock()
        && g.is_none()
    {
        *g = Some(std::time::Instant::now());
    }
}

fn mark_first_speak_called() {
    if let Ok(mut g) = FIRST_SPEAK_CALLED_AT.lock()
        && g.is_none()
    {
        *g = Some(std::time::Instant::now());
    }
}

/// Piper TTS subprocess manager.
///
/// Piper reads text from stdin and writes raw PCM audio to stdout.
/// This makes it perfect for streaming: pipe LLM tokens → Piper → speaker
/// as they arrive, reducing perceived latency to ~200ms first audio.
///
/// Two modes:
/// 1. **Pipe mode** (production): long-running Piper subprocess, feed text lines
/// 2. **File mode** (prototype): one-shot, write WAV per utterance
pub struct TtsEngine {
    model_path: String,
    /// Path to the Piper binary.
    piper_path: String,
    mode: TtsMode,
    child: Option<Child>,
    /// PCM output sample rate (Piper default: 22050).
    pub sample_rate: u32,
    /// ALSA device for playback (e.g. "plughw:0,0").
    audio_device: String,
    /// Half-duplex post-TTS silence (issue #15): milliseconds to sleep after
    /// `aplay` exits before returning from speak(). Gives the ALSA hardware
    /// playback buffer time to drain and the room reverb time to decay below
    /// the whisper-server no-speech threshold, so the next mic capture does
    /// not pick up the assistant's own TTS.
    post_silence_ms: u64,
}

#[derive(Clone, Copy)]
enum TtsMode {
    /// Long-running subprocess: stdin → text, stdout → raw PCM.
    Pipe,
    /// One-shot per utterance: outputs a WAV file.
    File,
    /// No-op TTS for tests — `speak`, `synthesize`, `synthesize_to_file`
    /// return immediately without spawning Piper or aplay. Used by
    /// `tests/voice_loop_integration.rs` so the voice cycle can be driven
    /// on hosts with no Piper binary and no audio output device
    /// (issue #21, IS-1 / IS-2).
    Silent,
}

impl TtsEngine {
    /// Create TTS engine in pipe mode (production — low latency streaming).
    pub fn pipe(model_path: &str) -> Self {
        Self {
            model_path: model_path.to_string(),
            piper_path: "piper".to_string(),
            mode: TtsMode::Pipe,
            child: None,
            sample_rate: 22050,
            audio_device: String::new(),
            post_silence_ms: 0,
        }
    }

    /// Create TTS engine in file mode (prototype — writes WAV files).
    pub fn file(model_path: &str) -> Self {
        Self {
            model_path: model_path.to_string(),
            piper_path: "piper".to_string(),
            mode: TtsMode::File,
            child: None,
            sample_rate: 22050,
            audio_device: String::new(),
            post_silence_ms: 0,
        }
    }

    /// Create a no-op TTS engine for tests. `speak`, `synthesize`,
    /// `synthesize_to_file`, `start`, and `stop` are no-ops; the engine
    /// never spawns Piper or aplay. Used by the voice-cycle integration
    /// test (issue #21).
    pub fn silent() -> Self {
        Self {
            model_path: String::new(),
            piper_path: String::new(),
            mode: TtsMode::Silent,
            child: None,
            sample_rate: 22050,
            audio_device: String::new(),
            post_silence_ms: 0,
        }
    }

    /// Create TTS engine with full configuration.
    pub fn configured(
        model_path: &str,
        piper_path: &str,
        audio_device: &str,
        pipe_mode: bool,
    ) -> Self {
        Self {
            model_path: model_path.to_string(),
            piper_path: piper_path.to_string(),
            mode: if pipe_mode {
                TtsMode::Pipe
            } else {
                TtsMode::File
            },
            child: None,
            sample_rate: 22050,
            audio_device: audio_device.to_string(),
            post_silence_ms: 0,
        }
    }

    /// Set the half-duplex post-TTS silence (ms) — see issue #15.
    pub fn with_post_silence_ms(mut self, ms: u64) -> Self {
        self.post_silence_ms = ms;
        self
    }

    pub fn for_model(&self, model_path: &str) -> Self {
        Self {
            model_path: model_path.to_string(),
            piper_path: self.piper_path.clone(),
            mode: self.mode,
            child: None,
            sample_rate: self.sample_rate,
            audio_device: self.audio_device.clone(),
            post_silence_ms: self.post_silence_ms,
        }
    }

    /// Return a config-only copy of this engine (no child process).
    /// `speak()` and `synthesize()` spawn a fresh Piper per call anyway,
    /// so the new copy is independently usable. Needed so callers that
    /// receive a `&TtsEngine` (e.g. the integration test's
    /// `tts_engine_override`) can move it into an `Arc<TtsEngine>` for
    /// `streaming::stream_and_speak`.
    pub fn snapshot(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            piper_path: self.piper_path.clone(),
            mode: self.mode,
            child: None,
            sample_rate: self.sample_rate,
            audio_device: self.audio_device.clone(),
            post_silence_ms: self.post_silence_ms,
        }
    }

    /// Start the Piper subprocess (pipe mode only).
    /// Piper stays running and accepts text lines on stdin.
    pub async fn start(&mut self) -> Result<()> {
        if let TtsMode::Pipe = self.mode {
            tracing::info!(
                model = %self.model_path,
                piper = %self.piper_path,
                "starting Piper TTS subprocess"
            );

            let child = Command::new(&self.piper_path)
                .args(["--model", &self.model_path, "--output_raw"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()?;

            self.child = Some(child);
            tracing::info!("Piper TTS subprocess started (pipe mode)");
        }
        Ok(())
    }

    /// Synthesize text to raw PCM audio bytes.
    ///
    /// In pipe mode: writes text to subprocess stdin, reads PCM from stdout.
    /// In file mode: runs piper one-shot, reads the output WAV.
    pub async fn synthesize(&mut self, text: &str) -> Result<Vec<u8>> {
        match &self.mode {
            TtsMode::Pipe => self.synthesize_pipe(text).await,
            TtsMode::File => self.synthesize_file(text).await,
            TtsMode::Silent => Ok(Vec::new()),
        }
    }

    /// Synthesize text and play directly through the speaker.
    ///
    /// Pipes text to Piper stdin, raw PCM stdout goes to aplay.
    /// Uses process pipes instead of shell to avoid escaping issues.
    pub async fn speak(&self, text: &str) -> Result<()> {
        if matches!(self.mode, TtsMode::Silent) {
            return Ok(());
        }

        let clean = text.replace('\n', " ");
        tracing::info!(text_len = text.len(), "speaking via Piper → aplay");

        let pcm = self.synthesize_only(&clean).await?;
        if pcm.is_empty() {
            tracing::warn!("Piper produced no audio");
            return Ok(());
        }

        tracing::info!(pcm_bytes = pcm.len(), "Piper generated audio, playing...");
        self.play_pcm_data(&pcm).await?;

        if self.post_silence_ms > 0 {
            tracing::debug!(
                post_silence_ms = self.post_silence_ms,
                "half-duplex gate: sleeping for room decay"
            );
            tokio::time::sleep(std::time::Duration::from_millis(self.post_silence_ms)).await;
        }

        Ok(())
    }

    /// Synthesize text to raw PCM bytes (without playback).
    ///
    /// Used by the pipelined streaming path to overlap synthesis of sentence
    /// N+1 with playback of sentence N. Also stamps the first-speak-called
    /// latency marker on the first invocation per response cycle.
    pub async fn synthesize_only(&self, text: &str) -> Result<Vec<u8>> {
        mark_first_speak_called();

        if matches!(self.mode, TtsMode::Silent) {
            return Ok(Vec::new());
        }

        let clean = text.replace('\n', " ");
        tracing::debug!(text_len = text.len(), "synthesizing via Piper");

        let mut piper = Command::new(&self.piper_path)
            .args(["--model", &self.model_path, "--output_raw"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        if let Some(mut stdin) = piper.stdin.take() {
            stdin.write_all(clean.as_bytes()).await?;
            stdin.write_all(b"\n").await?;
        }

        let out = piper.wait_with_output().await?;
        if !out.status.success() {
            anyhow::bail!("Piper failed: {}", String::from_utf8_lossy(&out.stderr));
        }

        let mut pcm = out.stdout;
        if pcm.is_empty() {
            tracing::warn!("Piper produced no audio");
            return Ok(pcm);
        }

        super::dsp::process_tts_audio(&mut pcm, self.sample_rate);
        super::aec::set_echo_reference(&pcm, self.sample_rate);
        Ok(pcm)
    }

    /// Play pre-synthesized PCM through the speaker (no post-silence).
    ///
    /// Callers must invoke `post_silence()` once after the final sentence.
    /// Stamps the first-audio latency marker on the first invocation.
    pub async fn play_pcm_data(&self, pcm: &[u8]) -> Result<()> {
        if matches!(self.mode, TtsMode::Silent) || pcm.is_empty() {
            return Ok(());
        }

        let rate_str = self.sample_rate.to_string();
        let mut aplay_args: Vec<&str> = Vec::new();
        if !self.audio_device.is_empty() {
            aplay_args.push("-D");
            aplay_args.push(&self.audio_device);
        }
        aplay_args.extend_from_slice(&["-f", "S16_LE", "-r", &rate_str, "-c", "1", "-t", "raw"]);

        let mut aplay = Command::new("aplay")
            .args(&aplay_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;

        if let Some(mut stdin) = aplay.stdin.take() {
            mark_first_audio();
            stdin.write_all(pcm).await?;
        }

        wait_aplay_or_timeout(
            aplay,
            aplay_timeout_for_pcm(pcm.len(), self.sample_rate),
            "play_pcm_data",
        )
        .await?;
        Ok(())
    }

    /// Apply the half-duplex post-TTS silence gate once.
    ///
    /// In the pipelined streaming path this fires once after the last
    /// sentence, rather than after every sentence as `speak()` does.
    pub async fn post_silence(&self) {
        if self.post_silence_ms > 0 {
            tracing::debug!(
                post_silence_ms = self.post_silence_ms,
                "half-duplex gate: sleeping for room decay"
            );
            tokio::time::sleep(std::time::Duration::from_millis(self.post_silence_ms)).await;
        }
    }

    /// Synthesize and write directly to a WAV file.
    pub async fn synthesize_to_file(&self, text: &str, output_path: &str) -> Result<()> {
        if matches!(self.mode, TtsMode::Silent) {
            // No-op for tests: create an empty file so any downstream
            // existence check still passes.
            tokio::fs::write(output_path, &[][..]).await?;
            return Ok(());
        }

        let clean = text.replace('\'', "'\\''");

        let output = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "echo '{}' | '{}' --model '{}' --output_file '{}'",
                    clean, self.piper_path, self.model_path, output_path,
                ),
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("piper failed: {}", stderr);
        }

        Ok(())
    }

    async fn synthesize_pipe(&mut self, text: &str) -> Result<Vec<u8>> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("TTS subprocess not started — call start() first"))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("TTS stdin not available"))?;

        // Write text line to Piper stdin.
        let line = format!("{}\n", text.replace('\n', " "));
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;

        // Read PCM output from stdout.
        // Piper outputs raw PCM in one continuous stream.
        // We need to read until silence or use a timeout.
        let stdout = child
            .stdout
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("TTS stdout not available"))?;

        // Read with a timeout — Piper outputs raw PCM, no length header.
        // In production, we'd use a VAD on the output or read in chunks
        // and stream to the speaker. For prototype, read for up to 10 seconds.
        let mut pcm = Vec::new();
        let mut buf = [0u8; 4096];

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);

        loop {
            let result =
                tokio::time::timeout_at(deadline, tokio::io::AsyncReadExt::read(stdout, &mut buf))
                    .await;

            match result {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    pcm.extend_from_slice(&buf[..n]);
                    // Heuristic: if we got less than a full buffer, likely done.
                    if n < buf.len() {
                        // Small delay to check if more data is coming.
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let peek = tokio::time::timeout(
                            std::time::Duration::from_millis(200),
                            tokio::io::AsyncReadExt::read(stdout, &mut buf),
                        )
                        .await;
                        match peek {
                            Ok(Ok(n)) if n > 0 => pcm.extend_from_slice(&buf[..n]),
                            _ => break,
                        }
                    }
                }
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => break, // Timeout
            }
        }

        Ok(pcm)
    }

    async fn synthesize_file(&self, text: &str) -> Result<Vec<u8>> {
        let tmp_path = format!("/tmp/geniepod-tts-{}.wav", std::process::id());
        self.synthesize_to_file(text, &tmp_path).await?;

        let wav_data = tokio::fs::read(&tmp_path).await?;
        let _ = tokio::fs::remove_file(&tmp_path).await;

        // Strip WAV header (44 bytes) to get raw PCM.
        if wav_data.len() > 44 && &wav_data[0..4] == b"RIFF" {
            Ok(wav_data[44..].to_vec())
        } else {
            Ok(wav_data)
        }
    }

    /// Stop the TTS subprocess.
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            tracing::info!("Piper TTS subprocess stopped");
        }
    }
}

/// Play raw PCM audio through ALSA (aplay).
///
/// Production will use direct ALSA bindings (alsa-rs or cpal).
/// For prototype, shell out to `aplay`.
pub async fn play_pcm(pcm: &[u8], sample_rate: u32, audio_device: &str) -> Result<()> {
    let mut args = vec![
        "-r".to_string(),
        sample_rate.to_string(),
        "-f".to_string(),
        "S16_LE".to_string(),
        "-c".to_string(),
        "1".to_string(),
        "-t".to_string(),
        "raw".to_string(),
        "-q".to_string(),
    ];

    if !audio_device.is_empty() {
        args.insert(0, "-D".to_string());
        args.insert(1, audio_device.to_string());
    }

    let mut child = Command::new("aplay")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(pcm).await?;
    }

    wait_aplay_or_timeout(
        child,
        aplay_timeout_for_pcm(pcm.len(), sample_rate),
        "play_pcm",
    )
    .await
}

/// Play a WAV file through ALSA.
pub async fn play_wav(wav_path: &str, audio_device: &str) -> Result<()> {
    let mut args = vec!["-q".to_string(), wav_path.to_string()];

    if !audio_device.is_empty() {
        args.insert(0, "-D".to_string());
        args.insert(1, audio_device.to_string());
    }

    let mut cmd = Command::new("aplay");
    cmd.args(&args).kill_on_drop(true);

    let output = match tokio::time::timeout(APLAY_WAV_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => {
            tracing::warn!(
                wav_path,
                timeout_secs = APLAY_WAV_TIMEOUT.as_secs(),
                "aplay WAV timed out"
            );
            return Ok(());
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("aplay failed: {}", stderr);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_pipe_engine() {
        let engine = TtsEngine::pipe("/opt/geniepod/voices/en_US-amy-medium.onnx");
        assert_eq!(
            engine.model_path,
            "/opt/geniepod/voices/en_US-amy-medium.onnx"
        );
        assert_eq!(engine.sample_rate, 22050);
    }

    #[test]
    fn create_file_engine() {
        let engine = TtsEngine::file("/opt/geniepod/voices/en_US-amy-medium.onnx");
        assert_eq!(
            engine.model_path,
            "/opt/geniepod/voices/en_US-amy-medium.onnx"
        );
    }

    #[test]
    fn create_configured_engine() {
        let engine = TtsEngine::configured(
            "/opt/geniepod/voices/en_US-amy-medium.onnx",
            "/opt/geniepod/piper/piper",
            "plughw:0,0",
            false,
        );
        assert_eq!(engine.piper_path, "/opt/geniepod/piper/piper");
        assert_eq!(engine.audio_device, "plughw:0,0");
    }
}
