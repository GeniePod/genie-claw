use anyhow::Result;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Monotonic per-process counter for unique temp-file suffixes. Pairs with
/// the PID so two concurrent `transcribe_pcm` calls in the same process
/// cannot collide on `/tmp/geniepod-stt-*.wav`.
static TEMP_NONCE: AtomicU64 = AtomicU64::new(0);

fn next_temp_nonce() -> u64 {
    TEMP_NONCE.fetch_add(1, Ordering::Relaxed)
}

/// RAII guard that deletes a temp file on drop. Ensures cleanup runs on
/// every exit path — `?`-propagated errors and panic-unwinds included.
struct TempFile(String);

impl TempFile {
    fn new(path: String) -> Self {
        Self(path)
    }
    fn path(&self) -> &str {
        &self.0
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// "Speech end" marker for the #19 latency banner: stamped inside
/// `record_audio` the moment arecord returns, before DFN/sox preprocessing
/// starts. The voice loop reads it back to compute capture-to-first-audio.
static AUDIO_CAPTURED_AT: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Reset the audio-captured timestamp tracker. Call before each cycle.
pub fn reset_audio_captured_marker() {
    if let Ok(mut g) = AUDIO_CAPTURED_AT.lock() {
        *g = None;
    }
}

/// Read the audio-captured timestamp captured since the last reset.
pub fn audio_captured_at() -> Option<std::time::Instant> {
    AUDIO_CAPTURED_AT.lock().ok().and_then(|g| *g)
}

fn mark_audio_captured() {
    if let Ok(mut g) = AUDIO_CAPTURED_AT.lock()
        && g.is_none()
    {
        *g = Some(std::time::Instant::now());
    }
}

/// Whisper STT subprocess manager.
///
/// Spawns `whisper-server` (whisper.cpp HTTP server) or uses `whisper-cli`
/// for file-based transcription. Two modes:
///
/// 1. **Server mode** (production): whisper.cpp `--server` on localhost,
///    genie-core POSTs audio chunks via HTTP.
/// 2. **CLI mode** (prototype): pipes a WAV file to `whisper-cli`, reads text output.
///
/// On Jetson, whisper.cpp uses CUDA for GPU acceleration (~0.35x RTF).
pub struct SttEngine {
    mode: SttMode,
    model_path: String,
    /// Path to the whisper-cli binary.
    cli_path: String,
    /// Forced language or None for auto-detection.
    language_hint: Option<String>,
    /// Force CPU-only inference (--no-gpu). Required when LLM holds the GPU.
    no_gpu: bool,
    child: Option<Child>,
}

enum SttMode {
    /// whisper.cpp --server on a port, accepts audio via HTTP POST.
    Server { port: u16 },
    /// whisper CLI — transcribe individual WAV files.
    Cli,
    /// In-memory scripted transcript queue. Used by
    /// `tests/voice_loop_integration.rs` so the voice cycle can be
    /// exercised without a real whisper-cpp binary or audio device
    /// (issue #21, IS-1 / IS-2).
    Mock {
        transcripts: std::sync::Mutex<Vec<MockTranscript>>,
    },
}

#[derive(Debug, Clone)]
pub struct MockTranscript {
    pub text: String,
    pub language: Option<String>,
}

impl MockTranscript {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            language: None,
        }
    }

    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

impl From<&str> for MockTranscript {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

impl From<String> for MockTranscript {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

/// Transcription result from STT.
#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub duration_ms: u64,
    pub language: Option<String>,
}

impl SttEngine {
    /// Create STT engine in server mode (production).
    pub fn server(model_path: &str, port: u16) -> Self {
        Self {
            mode: SttMode::Server { port },
            model_path: model_path.to_string(),
            cli_path: "whisper-server".to_string(),
            language_hint: None,
            no_gpu: false,
            child: None,
        }
    }

    /// Create STT engine in CLI mode (prototype/dev).
    pub fn cli(model_path: &str) -> Self {
        Self {
            mode: SttMode::Cli,
            model_path: model_path.to_string(),
            cli_path: "whisper-cli".to_string(),
            language_hint: None,
            no_gpu: false,
            child: None,
        }
    }

    /// Create STT engine in CLI mode with a custom binary path.
    pub fn cli_with_path(model_path: &str, cli_path: &str) -> Self {
        Self {
            mode: SttMode::Cli,
            model_path: model_path.to_string(),
            cli_path: cli_path.to_string(),
            language_hint: None,
            no_gpu: false,
            child: None,
        }
    }

    /// Create STT engine in CLI mode, CPU-only (for when LLM holds the GPU).
    pub fn cli_cpu(model_path: &str, cli_path: &str) -> Self {
        Self {
            mode: SttMode::Cli,
            model_path: model_path.to_string(),
            cli_path: cli_path.to_string(),
            language_hint: None,
            no_gpu: true,
            child: None,
        }
    }

    /// Create an in-memory STT engine that replays the given scripted
    /// transcripts in order. `transcribe_file` ignores its argument and
    /// pops the next scripted transcript instead. When the queue is
    /// exhausted, `transcribe_file` returns an error.
    ///
    /// Used by `tests/voice_loop_integration.rs` (issue #21, IS-1 / IS-2).
    pub fn mock<I, T>(transcripts: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<MockTranscript>,
    {
        let mut q: Vec<MockTranscript> = transcripts.into_iter().map(Into::into).collect();
        q.reverse(); // pop from the back so callers see insertion order
        Self {
            mode: SttMode::Mock {
                transcripts: std::sync::Mutex::new(q),
            },
            model_path: String::new(),
            cli_path: String::new(),
            language_hint: None,
            no_gpu: false,
            child: None,
        }
    }

    pub fn with_language_hint(mut self, language: Option<String>) -> Self {
        self.language_hint =
            language.and_then(|value| super::language::configured_language(&value));
        self
    }

    /// Start the whisper server subprocess (server mode only).
    pub async fn start_server(&mut self) -> Result<()> {
        if let SttMode::Server { port } = self.mode {
            tracing::info!(port, model = %self.model_path, "starting whisper server");

            let child = Command::new(&self.cli_path)
                .args([
                    "--model",
                    &self.model_path,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port.to_string(),
                    "--threads",
                    "2",
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            self.child = Some(child);

            // Wait briefly for server to start.
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            tracing::info!(port, "whisper server started");
        }
        Ok(())
    }

    /// Transcribe a WAV file (works in all modes).
    pub async fn transcribe_file(&self, wav_path: &str) -> Result<Transcript> {
        let start = std::time::Instant::now();

        match &self.mode {
            SttMode::Server { port } => self.transcribe_via_server(*port, wav_path).await,
            SttMode::Cli => self.transcribe_via_cli(wav_path).await,
            SttMode::Mock { transcripts } => {
                let mut q = transcripts
                    .lock()
                    .expect("mock STT transcript queue poisoned");
                if let Some(next) = q.pop() {
                    Ok(Transcript {
                        text: next.text,
                        duration_ms: 0,
                        language: next.language,
                    })
                } else {
                    anyhow::bail!("SttEngine::mock transcript queue exhausted")
                }
            }
        }
        .map(|mut t| {
            t.duration_ms = start.elapsed().as_millis() as u64;
            t
        })
    }

    /// Transcribe raw PCM audio bytes (16kHz, 16-bit, mono).
    /// Writes to a temp WAV file, then transcribes.
    pub async fn transcribe_pcm(&self, pcm_data: &[u8], sample_rate: u32) -> Result<Transcript> {
        let pid = std::process::id();
        let nonce = next_temp_nonce();
        let tmp = TempFile::new(format!("/tmp/geniepod-stt-{pid}-{nonce}.wav"));
        write_wav(tmp.path(), pcm_data, sample_rate).await?;
        self.transcribe_file(tmp.path()).await
    }

    async fn transcribe_via_server(&self, port: u16, wav_path: &str) -> Result<Transcript> {
        // POST the WAV file to whisper server's /inference endpoint.
        // We also send `language`, `temperature`, and `response_format` form
        // fields so the server uses the English-only decoder (when configured),
        // deterministic decoding, and a structured JSON response. Without
        // language, whisper-server runs the multilingual decoder, which is
        // noticeably less accurate on conversational English.
        let wav_data = tokio::fs::read(wav_path).await?;
        let addr = format!("127.0.0.1:{}", port);

        let boundary = "----GeniePodBoundary";

        // Build multipart body parts: language (optional), temperature,
        // response_format, then the file.
        let mut text_parts = String::new();
        if let Some(language) = &self.language_hint {
            text_parts.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\n{language}\r\n"
            ));
        }
        text_parts.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"temperature\"\r\n\r\n0.0\r\n"
        ));
        text_parts.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n"
        ));
        // Explicitly send an empty initial-prompt so whisper-server cannot
        // condition the decoder on any prior context. Defensive — current
        // whisper.cpp server keeps state per-request anyway, but this future-
        // proofs us against any version that might cache the last prompt.
        text_parts.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\n\r\n"
        ));

        let file_part = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        );
        let body_end = format!("\r\n--{boundary}--\r\n");

        let content_length = text_parts.len() + file_part.len() + wav_data.len() + body_end.len();

        let stream = tokio::net::TcpStream::connect(&addr).await?;
        let (reader, mut writer) = stream.into_split();

        let request = format!(
            "POST /inference HTTP/1.1\r\nHost: {addr}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
        );

        writer.write_all(request.as_bytes()).await?;
        writer.write_all(text_parts.as_bytes()).await?;
        writer.write_all(file_part.as_bytes()).await?;
        writer.write_all(&wav_data).await?;
        writer.write_all(body_end.as_bytes()).await?;

        // Read response.
        let mut buf_reader = BufReader::new(reader);
        let mut response_body = String::new();
        let mut in_body = false;

        loop {
            let mut line = String::new();
            let n = buf_reader.read_line(&mut line).await?;
            if n == 0 {
                break;
            }
            if in_body {
                response_body.push_str(&line);
            } else if line.trim().is_empty() {
                in_body = true;
            }
        }

        // Parse whisper server JSON response.
        // Format: {"text": " Hello, turn on the lights."}
        let parsed: serde_json::Value = serde_json::from_str(response_body.trim())
            .unwrap_or_else(|_| serde_json::json!({"text": response_body.trim()}));

        let text = Self::clean_hallucinations(
            parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim(),
        );
        let detected_language = super::language::detect_language_from_text(&text);

        Ok(Transcript {
            text,
            duration_ms: 0,
            language: parsed
                .get("language")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| self.language_hint.clone())
                .or(detected_language),
        })
    }

    async fn transcribe_via_cli(&self, wav_path: &str) -> Result<Transcript> {
        tracing::info!(cli = %self.cli_path, model = %self.model_path, file = wav_path, "running whisper-cli");

        // Drop page cache before CUDA allocation — NvMap needs contiguous blocks.
        let _ = Command::new("sh")
            .args([
                "-c",
                "sync && echo 3 > /proc/sys/vm/drop_caches 2>/dev/null",
            ])
            .output()
            .await;

        let mut args = vec![
            "-m".to_string(),
            self.model_path.clone(),
            "-f".to_string(),
            wav_path.to_string(),
            "--no-timestamps".to_string(),
            "--no-prints".to_string(),
            "--threads".to_string(),
            "4".to_string(),
            // Suppress non-speech tokens: prevents hallucinations like
            // [GUNFIRE], [coughing], (music), etc. on noisy/bleed audio.
            "--suppress-nst".to_string(),
            // Higher no-speech threshold: if confidence is low, output nothing.
            "--no-speech-thold".to_string(),
            "0.8".to_string(),
        ];

        if let Some(language) = &self.language_hint {
            args.push("--language".to_string());
            args.push(language.clone());
        }

        if self.no_gpu {
            args.push("--no-gpu".to_string());
        }

        let output = Command::new(&self.cli_path).args(&args).output().await?;

        // If GPU allocation fails (NvMap error), retry with --no-gpu.
        if !output.status.success() && !self.no_gpu {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("NvMap")
                || stderr.contains("cuda")
                || stderr.contains("failed to initialize")
            {
                tracing::warn!(
                    "GPU STT failed, retrying on CPU: {}",
                    stderr.lines().next().unwrap_or("")
                );
                args.push("--no-gpu".to_string());
                let retry = Command::new(&self.cli_path).args(&args).output().await?;
                if !retry.status.success() {
                    let stderr2 = String::from_utf8_lossy(&retry.stderr);
                    anyhow::bail!("whisper-cli failed (CPU retry): {}", stderr2);
                }
                let raw = String::from_utf8_lossy(&retry.stdout);
                let text = Self::clean_hallucinations(raw.trim());
                let language = self
                    .language_hint
                    .clone()
                    .or_else(|| super::language::detect_language_from_text(&text));
                tracing::info!(text = %text, mode = "cpu-fallback", "whisper transcription complete");
                return Ok(Transcript {
                    text,
                    duration_ms: 0,
                    language,
                });
            }
            anyhow::bail!("whisper-cli failed: {}", stderr);
        }

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("whisper-cli failed: {}", stderr);
        }

        let raw = String::from_utf8_lossy(&output.stdout);
        let text = Self::clean_hallucinations(raw.trim());
        let language = self
            .language_hint
            .clone()
            .or_else(|| super::language::detect_language_from_text(&text));
        tracing::info!(text = %text, "whisper transcription complete");

        Ok(Transcript {
            text,
            duration_ms: 0,
            language,
        })
    }

    /// Strip common Whisper tiny hallucination artifacts from transcription.
    ///
    /// The tiny model hallucinates bracketed sound effects, parenthetical labels,
    /// and ghost phrases when it hears bleed audio or background noise.
    fn clean_hallucinations(text: &str) -> String {
        let mut result = text.to_string();

        // Strip [ANYTHING] and (ANYTHING) markers — regex-free.
        loop {
            if let Some(start) = result.find('[')
                && let Some(end) = result[start..].find(']')
            {
                result = format!("{}{}", &result[..start], &result[start + end + 1..]);
                continue;
            }
            if let Some(start) = result.find('(')
                && let Some(end) = result[start..].find(')')
            {
                result = format!("{}{}", &result[..start], &result[start + end + 1..]);
                continue;
            }
            break;
        }

        // Known ghost phrases the tiny model produces on near-silence or bleed.
        let ghosts = [
            "thank you",
            "thanks for watching",
            "good night",
            "goodbye",
            "i'm sorry",
            "you're welcome",
            "subscribe",
            "like and subscribe",
            "see you next time",
            "bye bye",
            "the end",
            "thank you for watching",
            "please subscribe",
            "thanks for listening",
        ];
        let lower = result.trim().to_lowercase();
        for ghost in &ghosts {
            if lower == *ghost {
                tracing::debug!(ghost = ghost, "filtered ghost phrase from tiny model");
                return String::new();
            }
        }

        // Collapse whitespace from removals.
        let result = result.split_whitespace().collect::<Vec<_>>().join(" ");
        result.trim().to_string()
    }

    /// Stop the server subprocess.
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            tracing::info!("whisper server stopped");
        }
    }
}

/// Drain stale samples from the ALSA capture queue before a real recording.
/// Without this, between-cycle residue (kernel DMA carry-over from the prior
/// arecord, plus a few hundred ms of acoustic echo from TTS playback bleeding
/// speaker→room→mic) lands in the next capture and biases whisper toward
/// assistant-stock hallucinations like "I'm here to help".
///
/// 1 second of throwaway capture is enough to settle the I2S DMA on Jetson +
/// LyraT V4.3. The arecord open/close also fully releases and re-acquires the
/// device, resetting any kernel-side state.
pub async fn flush_mic_buffer(device: &str, sample_rate: u32) {
    let flush_path = format!("/tmp/geniepod-flush-{}.wav", std::process::id());
    // Use -c 2 (stereo) for the same reason as the real capture: -c 1 on the
    // Tegra/LyraT plughw stack returns samples in half real time, so a 1 s
    // mono flush actually drains only ~0.5 s of pending audio. Stereo keeps
    // arecord properly throttled.
    let _ = Command::new("arecord")
        .args([
            "-D",
            device,
            "-q",
            "-f",
            "S16_LE",
            "-r",
            &sample_rate.to_string(),
            "-c",
            "2",
            "-d",
            "1",
            &flush_path,
        ])
        .output()
        .await;
    let _ = tokio::fs::remove_file(&flush_path).await;
}

/// Capture preprocessing backend.
///
/// Variants in increasing strength / latency:
/// - `None`        — bandpass + peak-normalize only (debug / no-denoise A/B)
/// - `Sox`         — sox `noisered` spectral subtraction against a per-host
///   noise profile (alpha.6 baseline, see PR #11)
/// - `DeepFilterNet` — neural denoiser via the `deep-filter` subprocess
///   (alpha.7, see issue #12). Handles non-stationary noise
///   without a noise profile.
#[derive(Debug, Clone)]
pub enum Denoiser {
    None,
    Sox {
        noise_profile_path: String,
    },
    DeepFilterNet {
        binary_path: String,
        atten_lim_db: f32,
    },
}

impl Denoiser {
    /// Build from VoiceConfig strings. Unknown values default to `None`.
    pub fn from_config(kind: &str, deep_filter_path: &str, deep_filter_atten_lim_db: f32) -> Self {
        match kind {
            "deepfilternet" | "deep_filter" | "dfn" => Denoiser::DeepFilterNet {
                binary_path: deep_filter_path.to_string(),
                atten_lim_db: deep_filter_atten_lim_db,
            },
            "sox" => Denoiser::Sox {
                noise_profile_path: "/opt/geniepod/data/mic-noise.prof".to_string(),
            },
            _ => Denoiser::None,
        }
    }
}

/// Record audio with fixed duration.
///
/// Returns the path to the recorded WAV file.
pub async fn record_audio(
    device: &str,
    sample_rate: u32,
    duration_secs: u32,
    denoiser: Denoiser,
) -> Result<String> {
    // NOTE: callers are expected to call `flush_mic_buffer` themselves BEFORE
    // printing any "speak now" prompt to the user. Doing the flush here would
    // cause the throwaway 1 s arecord to run *after* the user sees the
    // prompt — chopping ~1 s off the start of the captured speech.

    let wav_path = format!("/tmp/geniepod-rec-{}.wav", std::process::id());

    tracing::info!(
        device,
        sample_rate,
        duration_secs,
        "recording audio via arecord"
    );

    // Capture STEREO (`-c 2`) even though whisper wants mono. On Tegra ALSA
    // (Jetson Orin Nano APE card with the LyraT I2S2 frontend at 24 kHz),
    // asking arecord for `-c 1` triggers a plughw rate-bug: it returns the
    // requested number of mono samples but in HALF the wall-clock time, as
    // if it interprets each stereo frame as two mono samples instead of
    // downmixing. A 3-second `arecord -c 1 -r 24000 -d 3` finishes in 1.5 s
    // and the recorded audio is timing-distorted. Capturing native stereo
    // and downmixing to mono in the sox stage below gives clean 3-second
    // real-time captures with correct sample timing.
    let output = Command::new("arecord")
        .args([
            "-D",
            device,
            "-f",
            "S16_LE",
            "-r",
            &sample_rate.to_string(),
            "-c",
            "2",
            "-d",
            &duration_secs.to_string(),
            &wav_path,
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("arecord failed: {}", stderr);
    }

    // Verify file has actual audio data (not just a 44-byte header).
    let metadata = tokio::fs::metadata(&wav_path).await?;
    if metadata.len() <= 44 {
        anyhow::bail!(
            "recording produced empty audio ({} bytes) — check mic device {}",
            metadata.len(),
            device
        );
    }

    tracing::info!(
        path = %wav_path,
        size_bytes = metadata.len(),
        "recording complete"
    );

    // Stamp "speech end" for the #19 latency banner: the moment arecord
    // finished (i.e. the user's 3-second window closed). Everything after
    // this point — DFN, sox, STT, LLM, TTS — counts against first-reply
    // latency.
    mark_audio_captured();

    // Preprocess captured audio for STT. The chain branches on the configured
    // denoiser; common stages are bandpass (highpass 100 + lowpass 7000) and
    // final peak-normalize (gain -n -3). Bandpass kills DC/rumble and ADC hiss
    // above the speech band so whisper sees clean speech-only audio.
    //
    //   sox `channels 1` downmixes stereo (captured by `arecord -c 2` above)
    //                   to mono. On the LyraT this is a broadside
    //                   delay-and-sum beamformer pointed at the speaker.
    //
    // Variant-specific stages:
    //   - DeepFilterNet: neural denoise (handles non-stationary noise without
    //                    a noise profile). Sox compand is skipped — DFN's
    //                    internal gating preserves quiet phonemes better than
    //                    a hard compand gate.
    //   - Sox:           spectral subtraction with a per-host noise profile
    //                    captured by setup-jetson.sh, plus a compand gate +
    //                    quiet-speech lift (alpha.6 baseline).
    //   - None:          bandpass + compand + normalize only (alpha.6 fallback
    //                    when no noise profile is available).
    let normalized_path = preprocess_capture(&wav_path, &denoiser).await?;
    let _ = tokio::fs::copy(&normalized_path, "/tmp/geniepod-last-rec.wav").await;
    if normalized_path != wav_path {
        let _ = tokio::fs::remove_file(&wav_path).await;
    }
    Ok(normalized_path)
}

/// Run the configured denoise + normalize chain over a captured WAV. Returns
/// the path of the cleaned WAV (or the raw recording on best-effort fallback
/// when the configured backend's binary or noise profile is unavailable).
async fn preprocess_capture(wav_path: &str, denoiser: &Denoiser) -> Result<String> {
    let pid = std::process::id();
    let normalized_path = format!("/tmp/geniepod-rec-{}-norm.wav", pid);

    match denoiser {
        Denoiser::DeepFilterNet {
            binary_path,
            atten_lim_db,
        } => run_deepfilternet_chain(wav_path, &normalized_path, binary_path, *atten_lim_db).await,
        Denoiser::Sox { noise_profile_path } => {
            run_sox_chain(
                wav_path,
                &normalized_path,
                Some(noise_profile_path.as_str()),
            )
            .await
        }
        Denoiser::None => run_sox_chain(wav_path, &normalized_path, None).await,
    }
}

/// alpha.6 sox-only chain: downmix → bandpass → (optional noisered) → compand
/// gate+lift → peak-normalize. Falls back to the raw WAV on sox failure.
async fn run_sox_chain(
    wav_path: &str,
    normalized_path: &str,
    noise_profile: Option<&str>,
) -> Result<String> {
    let have_noise_profile = match noise_profile {
        Some(p) => tokio::fs::metadata(p).await.is_ok(),
        None => false,
    };

    let mut sox_cmd = Command::new("sox");
    sox_cmd
        .arg(wav_path)
        .arg(normalized_path)
        .args(["channels", "1"])
        .args(["highpass", "100"])
        .args(["lowpass", "7000"]);
    if have_noise_profile {
        // 0.21 amount — conservative, picked so the subtraction is audibly
        // helpful on the ES8388 ADC noise floor without producing the
        // swirling "musical noise" artifact that aggressive spectral
        // subtraction is famous for.
        sox_cmd.args(["noisered", noise_profile.unwrap(), "0.21"]);
    }
    // compand: 20 ms attack / 200 ms release, floor at -50 dBFS, +13 dB lift on
    // quiet speech (-25 -> -12), loud speech (-5) held to avoid clip, -2 dB
    // makeup offset.
    sox_cmd.args(["compand", "0.02,0.20", "-50,-50,-25,-12,-5,-5", "-2"]);
    sox_cmd.args(["gain", "-n", "-3"]);

    match sox_cmd.output().await {
        Ok(out)
            if out.status.success()
                && tokio::fs::metadata(normalized_path)
                    .await
                    .map(|m| m.len() > 44)
                    .unwrap_or(false) =>
        {
            tracing::info!(
                path = %normalized_path,
                noisered = have_noise_profile,
                "preprocessed audio with sox chain (bandpass, noisered if profile, compand, peak-normalize -3 dBFS)"
            );
            Ok(normalized_path.to_string())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(
                stderr = stderr.lines().next().unwrap_or(""),
                "sox normalization failed (status {:?}); using raw recording",
                out.status.code()
            );
            Ok(wav_path.to_string())
        }
        Err(e) => {
            tracing::warn!(error = %e, "sox not available; using raw recording (install sox for better STT accuracy on quiet captures)");
            Ok(wav_path.to_string())
        }
    }
}

/// alpha.7 DeepFilterNet chain:
///   sox(channels 1, highpass 100, lowpass 7000) → mono_path
///   deep-filter mono_path -o tmp_dir → tmp_dir/<basename>
///   sox(gain -n -3) → normalized_path
///
/// DFN takes mono input and writes to an output directory preserving the
/// input filename. We feed it the already-bandpassed mono WAV (so its
/// internal STFT doesn't waste capacity on rumble/hiss bands whisper can't
/// use anyway) and apply only peak-normalize on the way out — DFN's
/// implicit gate preserves quiet phonemes better than a hard sox compand.
///
/// Falls back to the sox-only chain (no noise profile) if the deep-filter
/// binary is missing or any subprocess step fails.
async fn run_deepfilternet_chain(
    wav_path: &str,
    normalized_path: &str,
    binary_path: &str,
    atten_lim_db: f32,
) -> Result<String> {
    if tokio::fs::metadata(binary_path).await.is_err() {
        tracing::warn!(
            binary = binary_path,
            "deep-filter binary missing; falling back to sox chain. Run setup-jetson.sh to install."
        );
        return run_sox_chain(wav_path, normalized_path, None).await;
    }

    let pid = std::process::id();
    let mono_path = format!("/tmp/geniepod-rec-{}-mono.wav", pid);
    let dfn_dir = format!("/tmp/geniepod-rec-{}-dfn", pid);
    let mono_basename = std::path::Path::new(&mono_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("mono.wav");
    let dfn_out = format!("{}/{}", dfn_dir, mono_basename);

    let _ = tokio::fs::remove_dir_all(&dfn_dir).await;
    if let Err(e) = tokio::fs::create_dir_all(&dfn_dir).await {
        tracing::warn!(error = %e, "could not create DFN tmp dir; falling back to sox chain");
        return run_sox_chain(wav_path, normalized_path, None).await;
    }

    // Stage 1: stereo → mono + bandpass.
    let stage1 = Command::new("sox")
        .arg(wav_path)
        .arg(&mono_path)
        .args(["channels", "1"])
        .args(["highpass", "100"])
        .args(["lowpass", "7000"])
        .output()
        .await;
    let stage1_ok = matches!(stage1, Ok(ref o) if o.status.success())
        && tokio::fs::metadata(&mono_path)
            .await
            .map(|m| m.len() > 44)
            .unwrap_or(false);
    if !stage1_ok {
        if let Ok(o) = stage1 {
            tracing::warn!(
                stderr = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or(""),
                "sox stage 1 (downmix+bandpass) failed; falling back to sox chain"
            );
        }
        let _ = tokio::fs::remove_dir_all(&dfn_dir).await;
        return run_sox_chain(wav_path, normalized_path, None).await;
    }

    // Stage 2: deep-filter <mono.wav> -o <dfn_dir> --atten-lim-db <db>.
    // (The v0.5.6 prebuilt binary names the flag `--atten-lim-db` / `-a`,
    // not `--atten-lim` as the current main-branch enhance_wav.rs source
    // suggests.)
    let dfn_start = std::time::Instant::now();
    let dfn_res = Command::new(binary_path)
        .arg(&mono_path)
        .args(["-o", &dfn_dir])
        .args(["--atten-lim-db", &format!("{}", atten_lim_db)])
        .output()
        .await;
    let dfn_ok = matches!(dfn_res, Ok(ref o) if o.status.success())
        && tokio::fs::metadata(&dfn_out)
            .await
            .map(|m| m.len() > 44)
            .unwrap_or(false);
    if !dfn_ok {
        if let Ok(o) = dfn_res {
            tracing::warn!(
                stderr = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or(""),
                "deep-filter run failed; falling back to sox chain"
            );
        } else if let Err(ref e) = dfn_res {
            tracing::warn!(error = %e, "could not spawn deep-filter; falling back to sox chain");
        }
        let _ = tokio::fs::remove_file(&mono_path).await;
        let _ = tokio::fs::remove_dir_all(&dfn_dir).await;
        return run_sox_chain(wav_path, normalized_path, None).await;
    }
    let dfn_ms = dfn_start.elapsed().as_millis();

    // Stage 3: peak-normalize cleaned audio.
    let stage3 = Command::new("sox")
        .arg(&dfn_out)
        .arg(normalized_path)
        .args(["gain", "-n", "-3"])
        .output()
        .await;
    let stage3_ok = matches!(stage3, Ok(ref o) if o.status.success())
        && tokio::fs::metadata(normalized_path)
            .await
            .map(|m| m.len() > 44)
            .unwrap_or(false);
    let _ = tokio::fs::remove_file(&mono_path).await;
    let _ = tokio::fs::remove_dir_all(&dfn_dir).await;
    if !stage3_ok {
        if let Ok(o) = stage3 {
            tracing::warn!(
                stderr = String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .next()
                    .unwrap_or(""),
                "sox stage 3 (peak-normalize) failed; falling back to sox chain"
            );
        }
        return run_sox_chain(wav_path, normalized_path, None).await;
    }

    tracing::info!(
        path = %normalized_path,
        dfn_ms = dfn_ms as u64,
        atten_lim_db = atten_lim_db as f64,
        "preprocessed audio with DeepFilterNet chain (bandpass, deep-filter denoise, peak-normalize -3 dBFS)"
    );
    Ok(normalized_path.to_string())
}

/// Write raw PCM data as a WAV file.
async fn write_wav(path: &str, pcm: &[u8], sample_rate: u32) -> Result<()> {
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_size = pcm.len() as u32;
    let file_size = 36 + data_size;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&file_size.to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&bits_per_sample.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_size.to_le_bytes());

    let mut file_data = header;
    file_data.extend_from_slice(pcm);
    tokio::fs::write(path, &file_data).await?;
    Ok(())
}

/// Spawn a background task that captures audio via ALSA and sends
/// transcripts through a channel. This is the production audio pipeline.
///
/// Not yet implemented — requires ALSA bindings (cpal or alsa-rs crate).
/// For now, the orchestrator uses stdin as input source.
pub fn spawn_audio_pipeline(_stt: Arc<SttEngine>) -> mpsc::Receiver<Transcript> {
    let (_tx, rx) = mpsc::channel(16);
    // TODO: ALSA capture → VAD → chunk → STT → tx.send(transcript)
    tracing::warn!("audio pipeline not yet implemented — using stdin mode");
    rx
}

use std::sync::Arc;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_wav_header() {
        let pcm = vec![0u8; 32000]; // 1 second of 16kHz 16-bit mono
        let path = format!("/tmp/geniepod-stt-test-{}.wav", std::process::id());
        write_wav(&path, &pcm, 16000).await.unwrap();

        let data = tokio::fs::read(&path).await.unwrap();
        assert_eq!(&data[0..4], b"RIFF");
        assert_eq!(&data[8..12], b"WAVE");
        assert_eq!(&data[12..16], b"fmt ");

        // Sample rate at offset 24.
        let sr = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
        assert_eq!(sr, 16000);

        let _ = tokio::fs::remove_file(&path).await;
    }

    #[test]
    fn create_cli_engine() {
        let engine = SttEngine::cli("/opt/geniepod/models/whisper-small.bin");
        assert_eq!(engine.model_path, "/opt/geniepod/models/whisper-small.bin");
        assert_eq!(engine.language_hint, None);
    }

    // Regression: prior to the per-call nonce, `transcribe_pcm` named its
    // temp WAV by PID only, so concurrent calls in the same process collided
    // on `/tmp/geniepod-stt-<pid>.wav` and the later writer would corrupt
    // the earlier reader's input. The path now embeds an atomic nonce; this
    // test asserts the nonce actually varies across concurrent calls.
    #[tokio::test]
    async fn transcribe_pcm_temp_paths_are_unique_under_concurrency() {
        use std::collections::HashSet;
        use std::sync::Mutex;

        let pid = std::process::id();
        let observed: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let observed = observed.clone();
            handles.push(tokio::spawn(async move {
                let nonce = next_temp_nonce();
                let path = format!("/tmp/geniepod-stt-{pid}-{nonce}.wav");
                observed.lock().unwrap().insert(path);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(observed.lock().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn temp_file_guard_removes_on_drop() {
        let pid = std::process::id();
        let nonce = next_temp_nonce();
        let path = format!("/tmp/geniepod-stt-droptest-{pid}-{nonce}.wav");
        tokio::fs::write(&path, b"x").await.unwrap();
        {
            let _guard = TempFile::new(path.clone());
            assert!(tokio::fs::metadata(&path).await.is_ok());
        }
        assert!(tokio::fs::metadata(&path).await.is_err());
    }

    #[test]
    fn create_cli_engine_with_path() {
        let engine =
            SttEngine::cli_with_path("/opt/geniepod/models/whisper-small.bin", "/usr/bin/whisper");
        assert_eq!(engine.cli_path, "/usr/bin/whisper");
    }

    #[test]
    fn create_cli_engine_with_language_hint() {
        let engine = SttEngine::cli("/opt/geniepod/models/whisper-small.bin")
            .with_language_hint(Some("de-DE".into()));
        assert_eq!(engine.language_hint.as_deref(), Some("de"));
    }

    #[test]
    fn create_server_engine() {
        let engine = SttEngine::server("/opt/geniepod/models/whisper-small.bin", 8178);
        if let SttMode::Server { port } = engine.mode {
            assert_eq!(port, 8178);
        } else {
            panic!("expected server mode");
        }
    }

    #[test]
    fn clean_hallucinations_brackets() {
        assert_eq!(SttEngine::clean_hallucinations("[GUNFIRE] hello"), "hello");
        assert_eq!(
            SttEngine::clean_hallucinations("hi [coughing] there"),
            "hi there"
        );
        assert_eq!(SttEngine::clean_hallucinations("(music) test"), "test");
        assert_eq!(SttEngine::clean_hallucinations("[BLANK_AUDIO]"), "");
    }

    #[test]
    fn clean_hallucinations_ghost_phrases() {
        assert_eq!(SttEngine::clean_hallucinations("Thank you"), "");
        assert_eq!(SttEngine::clean_hallucinations("good night"), "");
        assert_eq!(SttEngine::clean_hallucinations("Thanks for watching"), "");
        assert_eq!(SttEngine::clean_hallucinations("I'm sorry"), "");
        assert_eq!(SttEngine::clean_hallucinations("Goodbye"), "");
    }

    #[test]
    fn clean_hallucinations_preserves_real_speech() {
        assert_eq!(
            SttEngine::clean_hallucinations("turn on the lights"),
            "turn on the lights"
        );
        assert_eq!(
            SttEngine::clean_hallucinations("what's the weather like"),
            "what's the weather like"
        );
    }
}
