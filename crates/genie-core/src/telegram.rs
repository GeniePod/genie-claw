use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use tokio::process::Command;

const TELEGRAM_MAX_MESSAGE_LEN: usize = 4096;

#[derive(Debug, Clone)]
pub struct TelegramRuntimeConfig {
    pub api_base: String,
    pub bot_token: String,
    pub core_base_url: String,
    pub poll_timeout_secs: u64,
    pub allowed_chat_ids: Vec<i64>,
    pub allow_all_chats: bool,
    pub voice: TelegramVoiceRuntimeConfig,
}

/// Voice-message ingestion settings for the Telegram channel (issue #42).
///
/// The Telegram adapter stays out of process boundaries with `voice/*`
/// modules — it speaks to Whisper / Piper directly via subprocess + HTTP,
/// the same way the on-device voice loop drives them, so a chat-only
/// deployment without ALSA still gets voice-in (phase 1) and voice-out
/// (phase 2).
#[derive(Debug, Clone)]
pub struct TelegramVoiceRuntimeConfig {
    pub enabled: bool,
    pub max_voice_duration_secs: u32,
    pub delete_temp_audio: bool,
    pub ffmpeg_path: PathBuf,
    pub whisper_port: u16,
    pub whisper_cli_path: PathBuf,
    pub whisper_model: PathBuf,
    pub stt_language: String,
    // Phase 2 (issue #42): voice reply via Piper → ffmpeg → sendVoice.
    pub reply_as_voice: bool,
    pub max_reply_chars: usize,
    pub piper_path: PathBuf,
    pub piper_model: PathBuf,
}

pub async fn run(config: TelegramRuntimeConfig) -> Result<()> {
    let client = Client::builder()
        .user_agent("GenieClaw/1.0")
        .timeout(Duration::from_secs(
            config.poll_timeout_secs.saturating_add(15),
        ))
        .build()
        .context("failed to build Telegram HTTP client")?;

    let api = TelegramApi::new(client, config);
    let mut offset = match api.bootstrap_offset().await {
        Ok(offset) => offset,
        Err(e) => {
            tracing::warn!(error = %e, "telegram bootstrap failed; starting from offset 0");
            0
        }
    };

    loop {
        match api.get_updates(offset).await {
            Ok(updates) => {
                for update in updates {
                    offset = offset.max(update.update_id.saturating_add(1));
                    if let Err(e) = api.handle_update(update).await {
                        tracing::warn!(error = %e, "telegram update handling failed");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "telegram polling failed");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
}

struct TelegramApi {
    client: Client,
    config: TelegramRuntimeConfig,
}

impl TelegramApi {
    fn new(client: Client, config: TelegramRuntimeConfig) -> Self {
        Self { client, config }
    }

    async fn bootstrap_offset(&self) -> Result<i64> {
        let updates = self.get_updates_raw(None, 0).await?;
        let next = updates
            .iter()
            .map(|u| u.update_id)
            .max()
            .map(|id| id.saturating_add(1))
            .unwrap_or(0);
        if next > 0 {
            tracing::info!(
                dropped_updates = updates.len(),
                next_offset = next,
                "telegram bootstrap skipped pending updates"
            );
        }
        Ok(next)
    }

    async fn get_updates(&self, offset: i64) -> Result<Vec<TelegramUpdate>> {
        self.get_updates_raw(Some(offset), self.config.poll_timeout_secs)
            .await
    }

    async fn get_updates_raw(
        &self,
        offset: Option<i64>,
        timeout_secs: u64,
    ) -> Result<Vec<TelegramUpdate>> {
        let payload = match offset {
            Some(offset) => serde_json::json!({
                "timeout": timeout_secs,
                "offset": offset,
                "allowed_updates": ["message"]
            }),
            None => serde_json::json!({
                "timeout": timeout_secs,
                "allowed_updates": ["message"]
            }),
        };

        let req = self
            .client
            .post(self.method_url("getUpdates"))
            .json(&payload);

        let resp: TelegramEnvelope<Vec<TelegramUpdate>> = req
            .send()
            .await
            .context("Telegram getUpdates request failed")?
            .error_for_status()
            .context("Telegram getUpdates HTTP error")?
            .json()
            .await
            .context("Telegram getUpdates JSON decode failed")?;

        if !resp.ok {
            anyhow::bail!(
                "Telegram getUpdates API error {}",
                resp.description.unwrap_or_else(|| "unknown error".into())
            );
        }

        Ok(resp.result.unwrap_or_default())
    }

    async fn handle_update(&self, update: TelegramUpdate) -> Result<()> {
        let Some(message) = update.message else {
            return Ok(());
        };

        if message
            .from
            .as_ref()
            .and_then(|u| u.is_bot)
            .unwrap_or(false)
        {
            return Ok(());
        }

        let chat_id = message.chat.id;
        if !self.chat_allowed(chat_id) {
            let _ = self
                .send_text(chat_id, "This chat is not authorized for GenieClaw.")
                .await;
            return Ok(());
        }

        // Voice or audio messages (issue #42): download → transcode → STT →
        // /api/chat → reply. The text path falls through below.
        if let Some(voice) = message.voice.as_ref().or(message.audio.as_ref()) {
            return self.handle_voice_message(chat_id, voice).await;
        }

        let Some(text) = message
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            let _ = self
                .send_text(chat_id, "Telegram v1 supports text messages only.")
                .await;
            return Ok(());
        };

        let normalized = strip_bot_mention(text);
        let normalized = normalized.trim();
        if normalized.is_empty() {
            return Ok(());
        }

        let core_response = self.chat_core(chat_id, normalized).await?;
        self.send_text(chat_id, &core_response).await?;
        Ok(())
    }

    async fn handle_voice_message(&self, chat_id: i64, voice: &TelegramVoice) -> Result<()> {
        let voice_cfg = &self.config.voice;

        if !voice_cfg.enabled {
            let _ = self
                .send_text(chat_id, "Voice messages aren't enabled on this deployment.")
                .await;
            return Ok(());
        }

        if voice.duration > voice_cfg.max_voice_duration_secs {
            let _ = self
                .send_text(
                    chat_id,
                    &format!(
                        "Voice message is too long ({}s); the limit is {}s.",
                        voice.duration, voice_cfg.max_voice_duration_secs
                    ),
                )
                .await;
            return Ok(());
        }

        let pid = std::process::id();
        let nonce: u32 = rand_nonce();
        let ogg_path = format!("/tmp/geniepod-tg-voice-{pid}-{nonce}.ogg");
        let wav_path = format!("/tmp/geniepod-tg-voice-{pid}-{nonce}.wav");

        // RAII-style cleanup: drop guard removes both temp files on every exit
        // path (success, error, panic during unwind).
        let _cleanup = TempCleanup::new(
            voice_cfg.delete_temp_audio,
            ogg_path.clone(),
            wav_path.clone(),
        );

        if let Err(e) = self.download_voice_file(&voice.file_id, &ogg_path).await {
            tracing::warn!(error = %e, file_id = %voice.file_id, "telegram voice download failed");
            let _ = self
                .send_text(
                    chat_id,
                    "Sorry, I couldn't download that voice message from Telegram.",
                )
                .await;
            return Ok(());
        }

        if let Err(e) = self.transcode_to_wav(&ogg_path, &wav_path).await {
            tracing::warn!(error = %e, "telegram voice transcode failed");
            let _ = self
                .send_text(
                    chat_id,
                    "Sorry, I couldn't decode that voice message (ffmpeg failed).",
                )
                .await;
            return Ok(());
        }

        let transcript = match self.transcribe_wav(&wav_path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "telegram voice transcription failed");
                let _ = self
                    .send_text(chat_id, "Sorry, I couldn't transcribe that voice message.")
                    .await;
                return Ok(());
            }
        };

        let transcript = clean_transcript(&transcript);
        if transcript.is_empty() {
            // Whisper produced nothing useful — either silence, hallucination,
            // or unrecognized speech. Mirror the intent gate's "blank audio"
            // outcome from the on-device voice loop.
            let _ = self
                .send_text(
                    chat_id,
                    "I couldn't make out any speech in that voice message.",
                )
                .await;
            return Ok(());
        }

        tracing::info!(
            chat_id,
            duration_secs = voice.duration,
            transcript = %transcript,
            "telegram voice message transcribed"
        );

        let core_response = self.chat_core(chat_id, &transcript).await?;
        self.send_reply(chat_id, &core_response).await?;
        Ok(())
    }

    /// Phase 2 of issue #42: route an assistant reply through the
    /// voice-out path when `reply_as_voice = true` and the conditions
    /// are met. Falls back to plain `send_text` on any failure so the
    /// user is never left without a reply.
    async fn send_reply(&self, chat_id: i64, text: &str) -> Result<()> {
        let voice_cfg = &self.config.voice;

        match voice_reply_gate(text, voice_cfg.reply_as_voice, voice_cfg.max_reply_chars) {
            VoiceReplyGate::Text => return self.send_text(chat_id, text).await,
            VoiceReplyGate::SkipOverLength { chars } => {
                tracing::info!(
                    chat_id,
                    reply_chars = chars,
                    cap = voice_cfg.max_reply_chars,
                    "telegram voice reply skipped: reply over max_reply_chars; sending text"
                );
                return self.send_text(chat_id, text).await;
            }
            VoiceReplyGate::Voice => {}
        }

        let trimmed = text.trim();
        let pid = std::process::id();
        let nonce = rand_nonce();
        let wav_path = format!("/tmp/geniepod-tg-reply-{pid}-{nonce}.wav");
        let ogg_path = format!("/tmp/geniepod-tg-reply-{pid}-{nonce}.ogg");

        let _cleanup = TempCleanup::new(
            voice_cfg.delete_temp_audio,
            ogg_path.clone(),
            wav_path.clone(),
        );

        if let Err(e) = self.synthesize_reply_to_wav(trimmed, &wav_path).await {
            tracing::warn!(error = %e, "telegram voice reply: piper synth failed; falling back to text");
            return self.send_text(chat_id, text).await;
        }

        if let Err(e) = self.wav_to_ogg_opus(&wav_path, &ogg_path).await {
            tracing::warn!(error = %e, "telegram voice reply: ffmpeg ogg encode failed; falling back to text");
            return self.send_text(chat_id, text).await;
        }

        if let Err(e) = self.send_voice(chat_id, &ogg_path).await {
            tracing::warn!(error = %e, "telegram voice reply: sendVoice failed; falling back to text");
            return self.send_text(chat_id, text).await;
        }

        tracing::info!(chat_id, "telegram voice reply sent");
        Ok(())
    }

    async fn synthesize_reply_to_wav(&self, text: &str, wav_path: &str) -> Result<()> {
        // Piper reads text from stdin, writes WAV to --output_file. Matches
        // the file-mode invocation in voice/tts.rs but kept inline so the
        // adapter doesn't pull in the `voice` Cargo feature.
        let voice_cfg = &self.config.voice;
        let mut piper = Command::new(&voice_cfg.piper_path)
            .args([
                "--model",
                &voice_cfg.piper_model.to_string_lossy(),
                "--output_file",
                wav_path,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn piper at {:?}", voice_cfg.piper_path))?;

        // Newlines confuse Piper; collapse to spaces like voice/tts.rs does.
        let one_line = text.replace('\n', " ");
        if let Some(mut stdin) = piper.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(one_line.as_bytes())
                .await
                .context("write piper stdin")?;
            stdin.write_all(b"\n").await.context("write piper stdin")?;
        }

        let output = piper.wait_with_output().await.context("await piper")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("piper failed: {}", stderr.trim());
        }

        // Empty WAV = nothing useful synthesized; treat as failure so the
        // caller falls back to text.
        let meta = tokio::fs::metadata(wav_path)
            .await
            .with_context(|| format!("stat {wav_path}"))?;
        if meta.len() < 128 {
            anyhow::bail!("piper produced empty/undersized WAV ({} bytes)", meta.len());
        }
        Ok(())
    }

    async fn wav_to_ogg_opus(&self, wav_path: &str, ogg_path: &str) -> Result<()> {
        // ffmpeg ships with libopus in all standard distros and on JetPack.
        // The format Telegram's sendVoice expects is OGG/Opus; the explicit
        // container + codec args here let ffmpeg pick conservative bitrate
        // defaults that comfortably fit voice-message reads under the
        // sendVoice 1 MB cap for typical Piper output lengths.
        let voice_cfg = &self.config.voice;
        let output = Command::new(&voice_cfg.ffmpeg_path)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                wav_path,
                "-c:a",
                "libopus",
                "-b:a",
                "24k",
                "-ac",
                "1",
                "-f",
                "ogg",
                ogg_path,
            ])
            .output()
            .await
            .with_context(|| format!("failed to spawn ffmpeg at {:?}", voice_cfg.ffmpeg_path))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg ogg/opus encode failed: {}", stderr.trim());
        }
        Ok(())
    }

    async fn send_voice(&self, chat_id: i64, ogg_path: &str) -> Result<()> {
        let bytes = tokio::fs::read(ogg_path)
            .await
            .with_context(|| format!("read ogg {ogg_path}"))?;

        let file_part = reqwest::multipart::Part::bytes(bytes)
            .file_name("reply.ogg")
            .mime_str("audio/ogg")
            .context("invalid mime for voice part")?;
        let form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("voice", file_part);

        let resp: TelegramEnvelope<serde_json::Value> = self
            .client
            .post(self.method_url("sendVoice"))
            .multipart(form)
            .send()
            .await
            .context("Telegram sendVoice request failed")?
            .error_for_status()
            .context("Telegram sendVoice HTTP error")?
            .json()
            .await
            .context("Telegram sendVoice JSON decode failed")?;

        if !resp.ok {
            anyhow::bail!(
                "Telegram sendVoice API error: {}",
                resp.description.unwrap_or_else(|| "unknown error".into())
            );
        }
        Ok(())
    }

    async fn download_voice_file(&self, file_id: &str, dest_path: &str) -> Result<()> {
        // Telegram getFile → file_path, then GET the binary off the file CDN.
        let payload = serde_json::json!({ "file_id": file_id });
        let env: TelegramEnvelope<TelegramFile> = self
            .client
            .post(self.method_url("getFile"))
            .json(&payload)
            .send()
            .await
            .context("Telegram getFile request failed")?
            .error_for_status()
            .context("Telegram getFile HTTP error")?
            .json()
            .await
            .context("Telegram getFile JSON decode failed")?;

        if !env.ok {
            anyhow::bail!(
                "Telegram getFile API error: {}",
                env.description.unwrap_or_else(|| "unknown error".into())
            );
        }

        let file = env
            .result
            .context("Telegram getFile returned no result body")?;
        let file_path = file
            .file_path
            .context("Telegram getFile returned no file_path")?;

        let download_url = format!(
            "{}/file/bot{}/{}",
            self.config.api_base.trim_end_matches('/'),
            self.config.bot_token,
            file_path
        );

        let bytes = self
            .client
            .get(&download_url)
            .send()
            .await
            .context("Telegram file download failed")?
            .error_for_status()
            .context("Telegram file download HTTP error")?
            .bytes()
            .await
            .context("Telegram file body read failed")?;

        tokio::fs::write(dest_path, &bytes)
            .await
            .with_context(|| format!("write temp ogg to {dest_path}"))?;
        Ok(())
    }

    async fn transcode_to_wav(&self, ogg_path: &str, wav_path: &str) -> Result<()> {
        let ffmpeg = &self.config.voice.ffmpeg_path;
        let output = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-i",
                ogg_path,
                "-ar",
                "16000",
                "-ac",
                "1",
                "-f",
                "wav",
                wav_path,
            ])
            .output()
            .await
            .with_context(|| format!("failed to spawn ffmpeg at {ffmpeg:?}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ffmpeg transcode failed: {}", stderr.trim());
        }

        Ok(())
    }

    async fn transcribe_wav(&self, wav_path: &str) -> Result<String> {
        let voice_cfg = &self.config.voice;
        if voice_cfg.whisper_port > 0 {
            self.transcribe_via_whisper_server(voice_cfg.whisper_port, wav_path)
                .await
        } else {
            self.transcribe_via_whisper_cli(wav_path).await
        }
    }

    async fn transcribe_via_whisper_server(&self, port: u16, wav_path: &str) -> Result<String> {
        // Posts to whisper.cpp's /inference endpoint with the same form fields
        // the on-device voice loop uses: explicit language, deterministic temp,
        // JSON response, empty initial prompt. Lives parallel to
        // `voice::stt::SttEngine::transcribe_via_server` rather than reusing
        // it directly so the Telegram adapter stays callable from chat-only
        // builds where the `voice` feature is off.
        let wav_data = tokio::fs::read(wav_path)
            .await
            .with_context(|| format!("read wav {wav_path}"))?;

        let language = configured_language(&self.config.voice.stt_language);

        let mut form = reqwest::multipart::Form::new()
            .text("temperature", "0.0")
            .text("response_format", "json")
            .text("prompt", "");

        if let Some(lang) = language {
            form = form.text("language", lang);
        }

        let file_part = reqwest::multipart::Part::bytes(wav_data)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .context("invalid mime for whisper part")?;
        form = form.part("file", file_part);

        let url = format!("http://127.0.0.1:{port}/inference");
        let resp: serde_json::Value = self
            .client
            .post(url)
            .multipart(form)
            .send()
            .await
            .context("whisper-server request failed")?
            .error_for_status()
            .context("whisper-server HTTP error")?
            .json()
            .await
            .context("whisper-server JSON decode failed")?;

        Ok(resp
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string())
    }

    async fn transcribe_via_whisper_cli(&self, wav_path: &str) -> Result<String> {
        let voice_cfg = &self.config.voice;
        let cli = &voice_cfg.whisper_cli_path;
        let model = &voice_cfg.whisper_model;

        let mut args: Vec<String> = vec![
            "-m".into(),
            model.to_string_lossy().into_owned(),
            "-f".into(),
            wav_path.into(),
            "--no-timestamps".into(),
            "--no-prints".into(),
            "--threads".into(),
            "4".into(),
            "--suppress-nst".into(),
            "--no-speech-thold".into(),
            "0.8".into(),
        ];

        if let Some(lang) = configured_language(&voice_cfg.stt_language) {
            args.push("--language".into());
            args.push(lang);
        }

        let output = Command::new(cli)
            .args(&args)
            .output()
            .await
            .with_context(|| format!("failed to spawn whisper-cli at {cli:?}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("whisper-cli failed: {}", stderr.trim());
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn chat_core(&self, chat_id: i64, text: &str) -> Result<String> {
        let request = CoreChatRequest {
            message: text.to_string(),
            conversation_id: Some(format!("telegram-{chat_id}")),
        };

        let response: CoreChatResponse = self
            .client
            .post(format!("{}/api/chat", self.config.core_base_url))
            .header("X-Genie-Origin", "telegram")
            .json(&request)
            .send()
            .await
            .context("local GenieClaw /api/chat request failed")?
            .error_for_status()
            .context("local GenieClaw /api/chat HTTP error")?
            .json()
            .await
            .context("failed to decode GenieClaw /api/chat response")?;

        Ok(response.response)
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<()> {
        for chunk in split_message(text) {
            let payload = serde_json::json!({
                "chat_id": chat_id,
                "text": chunk,
            });

            let resp: TelegramEnvelope<serde_json::Value> = self
                .client
                .post(self.method_url("sendMessage"))
                .json(&payload)
                .send()
                .await
                .context("Telegram sendMessage request failed")?
                .error_for_status()
                .context("Telegram sendMessage HTTP error")?
                .json()
                .await
                .context("Telegram sendMessage JSON decode failed")?;

            if !resp.ok {
                anyhow::bail!(
                    "Telegram sendMessage API error {}",
                    resp.description.unwrap_or_else(|| "unknown error".into())
                );
            }
        }

        Ok(())
    }

    fn chat_allowed(&self, chat_id: i64) -> bool {
        self.config.allow_all_chats || self.config.allowed_chat_ids.contains(&chat_id)
    }

    fn method_url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{}",
            self.config.api_base.trim_end_matches('/'),
            self.config.bot_token,
            method
        )
    }
}

/// Drop guard that removes Telegram voice temp files on every exit path.
/// Honors `delete_temp_audio = false` for live debugging.
struct TempCleanup {
    delete: bool,
    ogg: String,
    wav: String,
}

impl TempCleanup {
    fn new(delete: bool, ogg: String, wav: String) -> Self {
        Self { delete, ogg, wav }
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if !self.delete {
            return;
        }
        let _ = std::fs::remove_file(&self.ogg);
        let _ = std::fs::remove_file(&self.wav);
    }
}

/// Cheap unique suffix to keep concurrent voice messages from colliding on
/// `/tmp` paths when whisper-server allows parallel transcribes.
fn rand_nonce() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0)
}

/// Normalize the configured STT language ("auto", "" → None; everything else
/// passed through trimmed). Mirrors `voice::language::configured_language`
/// without requiring the `voice` feature to be on.
fn configured_language(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Trim Whisper output and drop common no-speech / hallucination markers.
/// A small, conservative subset of `voice::stt::SttEngine::clean_hallucinations`;
/// the agent-side intent gate handles the rest once `/api/chat` runs.
fn clean_transcript(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let lower = trimmed.to_lowercase();
    const HALLUCINATIONS: &[&str] = &[
        "[blank_audio]",
        "[ blank_audio ]",
        "(blank audio)",
        "[silence]",
        "(silence)",
        "[music]",
        "(music)",
        "[applause]",
        "(applause)",
        "thank you.",
        "thanks for watching.",
        "you",
    ];
    if HALLUCINATIONS.iter().any(|h| lower == *h) {
        return String::new();
    }
    trimmed.to_string()
}

/// Pure decision for the voice-reply gate. Extracted from `send_reply` so
/// the policy can be unit-tested without spinning up HTTP or subprocesses.
#[derive(Debug, PartialEq, Eq)]
enum VoiceReplyGate {
    /// Send the assistant reply as plain text.
    Text,
    /// Reply was over `max_reply_chars` — skip the voice path. Caller logs.
    SkipOverLength { chars: usize },
    /// Try the voice-reply pipeline (Piper → ffmpeg → sendVoice).
    Voice,
}

fn voice_reply_gate(text: &str, reply_as_voice: bool, max_chars: usize) -> VoiceReplyGate {
    let trimmed = text.trim();
    if !reply_as_voice || trimmed.is_empty() {
        return VoiceReplyGate::Text;
    }
    let chars = trimmed.chars().count();
    if chars > max_chars {
        return VoiceReplyGate::SkipOverLength { chars };
    }
    VoiceReplyGate::Voice
}

fn strip_bot_mention(text: &str) -> String {
    text.split_whitespace()
        .filter(|part| !part.starts_with('@'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn split_message(message: &str) -> Vec<String> {
    if message.chars().count() <= TELEGRAM_MAX_MESSAGE_LEN {
        return vec![message.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = message;

    while !remaining.is_empty() {
        let split_idx = remaining
            .char_indices()
            .nth(TELEGRAM_MAX_MESSAGE_LEN)
            .map(|(idx, _)| idx)
            .unwrap_or(remaining.len());

        if split_idx == remaining.len() {
            chunks.push(remaining.to_string());
            break;
        }

        let search_area = &remaining[..split_idx];
        let chunk_end = search_area
            .rfind('\n')
            .or_else(|| search_area.rfind(' '))
            .unwrap_or(split_idx);

        let end = if chunk_end == 0 { split_idx } else { chunk_end };
        chunks.push(remaining[..end].trim().to_string());
        remaining = remaining[end..].trim_start();
    }

    chunks
}

#[derive(Debug, Deserialize)]
struct TelegramEnvelope<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    voice: Option<TelegramVoice>,
    #[serde(default)]
    audio: Option<TelegramVoice>,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
    #[serde(default)]
    duration: u32,
}

#[derive(Debug, Default, Deserialize)]
struct TelegramFile {
    #[serde(default)]
    file_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    #[serde(default)]
    is_bot: Option<bool>,
}

#[derive(Debug, Serialize)]
struct CoreChatRequest {
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CoreChatResponse {
    response: String,
}

#[cfg(test)]
mod tests {
    use super::{
        TELEGRAM_MAX_MESSAGE_LEN, VoiceReplyGate, clean_transcript, configured_language,
        split_message, strip_bot_mention, voice_reply_gate,
    };

    #[test]
    fn telegram_split_keeps_short_message() {
        let chunks = split_message("hello");
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn telegram_split_breaks_long_message() {
        let long = "x".repeat(TELEGRAM_MAX_MESSAGE_LEN + 10);
        let chunks = split_message(&long);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|c| c.chars().count() <= TELEGRAM_MAX_MESSAGE_LEN)
        );
    }

    #[test]
    fn telegram_strip_bot_mentions() {
        assert_eq!(strip_bot_mention("@geniebot hello there"), "hello there");
    }

    #[test]
    fn configured_language_normalizes_auto_and_blank() {
        assert_eq!(configured_language(""), None);
        assert_eq!(configured_language("auto"), None);
        assert_eq!(configured_language(" AUTO "), None);
        assert_eq!(configured_language(" en "), Some("en".to_string()));
    }

    #[test]
    fn clean_transcript_drops_whisper_hallucinations() {
        assert_eq!(clean_transcript("[BLANK_AUDIO]"), "");
        assert_eq!(clean_transcript(" Thank you. "), "");
        assert_eq!(clean_transcript("(silence)"), "");
        assert_eq!(
            clean_transcript("turn off the lights"),
            "turn off the lights"
        );
    }

    #[test]
    fn voice_reply_gate_off_returns_text() {
        // reply_as_voice = false → always text, regardless of length.
        assert_eq!(voice_reply_gate("hello", false, 100), VoiceReplyGate::Text);
        assert_eq!(voice_reply_gate("", false, 100), VoiceReplyGate::Text);
    }

    #[test]
    fn voice_reply_gate_empty_text_returns_text() {
        // Empty / whitespace replies don't synthesize — they'd produce
        // empty WAV and waste a Piper invocation.
        assert_eq!(voice_reply_gate("", true, 100), VoiceReplyGate::Text);
        assert_eq!(voice_reply_gate("   \n\t", true, 100), VoiceReplyGate::Text);
    }

    #[test]
    fn voice_reply_gate_over_cap_skips_with_char_count() {
        let long = "a".repeat(150);
        assert_eq!(
            voice_reply_gate(&long, true, 100),
            VoiceReplyGate::SkipOverLength { chars: 150 }
        );
    }

    #[test]
    fn voice_reply_gate_under_cap_returns_voice() {
        assert_eq!(
            voice_reply_gate("turn off the lights", true, 100),
            VoiceReplyGate::Voice
        );
        // Exactly at the cap is still voice (uses `>` not `>=`).
        let at_cap = "x".repeat(100);
        assert_eq!(voice_reply_gate(&at_cap, true, 100), VoiceReplyGate::Voice);
    }

    #[test]
    fn voice_reply_gate_char_count_uses_unicode_chars_not_bytes() {
        // 5 multi-byte chars should count as 5, not 15 bytes — otherwise
        // a Japanese / Chinese reply would be skipped at a much shorter
        // human-perceived length.
        let s = "東京こんにちは"; // 7 chars, ~21 bytes
        assert_eq!(voice_reply_gate(s, true, 7), VoiceReplyGate::Voice);
        assert_eq!(
            voice_reply_gate(s, true, 6),
            VoiceReplyGate::SkipOverLength { chars: 7 }
        );
    }

    #[test]
    fn telegram_voice_message_deserializes() {
        // Spot-check that the message struct accepts a real-looking voice
        // update payload from Telegram getUpdates. This keeps the wire-format
        // contract in the test suite rather than only in production traffic.
        let raw = serde_json::json!({
            "update_id": 1,
            "message": {
                "chat": { "id": 42 },
                "from": { "is_bot": false },
                "voice": { "file_id": "AwACAg...", "duration": 5 }
            }
        });
        let parsed: super::TelegramUpdate = serde_json::from_value(raw).unwrap();
        let msg = parsed.message.unwrap();
        let voice = msg.voice.unwrap();
        assert_eq!(voice.file_id, "AwACAg...");
        assert_eq!(voice.duration, 5);
        assert!(msg.audio.is_none());
        assert!(msg.text.is_none());
    }
}
