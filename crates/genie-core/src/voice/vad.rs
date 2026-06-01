//! Voice Activity Detection using Silero VAD via Python subprocess.
//!
//! Silero VAD is a 2.2 MB neural network with 99%+ accuracy.
//! Runs as a Python subprocess that reads a WAV file and outputs
//! the speech segments (start/end timestamps in ms).
//!
//! This approach avoids ONNX Runtime Rust FFI complexity while
//! delivering the same accuracy. The Python call adds ~200ms overhead
//! but runs AFTER recording is complete (not in the critical path).

use anyhow::Result;
use std::time::Duration;
use tokio::process::Command;

const VAD_TIMEOUT: Duration = Duration::from_secs(10);

/// Return true when the Silero VAD model appears to be in the torch hub
/// on-disk cache. This is a fast, offline, filesystem-only check.
///
/// torch.hub caches under `${XDG_CACHE_HOME}/torch/hub/` (or
/// `~/.cache/torch/hub/` when XDG_CACHE_HOME is unset). The directory name
/// is derived from the repo slug: `snakers4/silero-vad` → `snakers4_silero-vad_master`.
pub fn silero_model_cached() -> bool {
    let xdg_cache = std::env::var("XDG_CACHE_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}/.cache", home)
    });
    std::path::Path::new(&xdg_cache)
        .join("torch/hub/snakers4_silero-vad_master")
        .exists()
}

/// Detect speech segments in a WAV file using Silero VAD.
///
/// Returns (has_speech, speech_end_ms) — whether speech was found,
/// and the timestamp (in ms) where speech ends.
/// If speech_end_ms < total duration, the file can be trimmed.
///
/// Returns `Err` immediately if the Silero model is not in the torch hub cache
/// (avoids a `torch.hub.load` network download that can block for tens of
/// minutes on a LAN-only Jetson). Also returns `Err` if the Python subprocess
/// exceeds the 10-second timeout or fails for any other reason. Callers should
/// treat the error as a non-fatal skip: log, remove the WAV, re-arm the voice
/// loop.
pub async fn detect_speech(wav_path: &str) -> Result<(bool, u64)> {
    if !silero_model_cached() {
        anyhow::bail!(
            "Silero VAD model not in torch hub cache \
             (~/.cache/torch/hub/snakers4_silero-vad_master/); \
             pre-cache it while online: python3 -c \
             \"import torch; torch.hub.load('snakers4/silero-vad', 'silero_vad', trust_repo=True)\""
        );
    }

    let child = Command::new("python3")
        .args([
            "-c",
            &format!(
                r#"
import sys, warnings
warnings.filterwarnings("ignore")
try:
    import torch
    model, utils = torch.hub.load(repo_or_dir='snakers4/silero-vad', model='silero_vad', trust_repo=True)
    (get_speech_timestamps, _, read_audio, _, _) = utils
    wav = read_audio('{}', sampling_rate=16000)
    timestamps = get_speech_timestamps(wav, model, sampling_rate=16000, threshold=0.5)
    if timestamps:
        last_end = timestamps[-1]['end']
        end_ms = int(last_end / 16)  # samples to ms at 16kHz
        print(f"SPEECH {{end_ms}}")
    else:
        print("SILENCE")
except Exception as e:
    print(f"ERROR {{e}}", file=sys.stderr)
    print("SILENCE")
"#,
                wav_path
            ),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn VAD python3 subprocess: {}", e))?;

    let output = tokio::time::timeout(VAD_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "VAD python3 subprocess timed out after {} s \
                 (torch.hub.load may be attempting a network download on a LAN-only host)",
                VAD_TIMEOUT.as_secs()
            )
        })?
        .map_err(|e| anyhow::anyhow!("VAD subprocess I/O error: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.trim();

    if line.starts_with("SPEECH") {
        let end_ms = line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        Ok((true, end_ms))
    } else {
        Ok((false, 0))
    }
}

/// Trim a WAV file to end at the specified millisecond.
///
/// Useful for removing trailing silence detected by VAD.
pub async fn trim_wav(wav_path: &str, end_ms: u64, sample_rate: u32) -> Result<()> {
    let data = tokio::fs::read(wav_path).await?;
    if data.len() <= 44 {
        return Ok(());
    }

    let bytes_per_ms = (sample_rate as u64 * 2) / 1000; // S16_LE mono
    let end_bytes = (end_ms * bytes_per_ms) as usize;

    // Add 500ms padding after speech end (don't cut too tight).
    let padding_bytes = (500 * bytes_per_ms) as usize;
    let trim_point = (end_bytes + padding_bytes).min(data.len() - 44);

    if trim_point >= data.len() - 44 {
        return Ok(()); // Nothing to trim.
    }

    // Rewrite WAV with trimmed data.
    let header = &data[..44];
    let pcm = &data[44..44 + trim_point];

    let data_size = pcm.len() as u32;
    let file_size = 36 + data_size;

    let mut output = header.to_vec();
    // Fix RIFF size.
    output[4..8].copy_from_slice(&file_size.to_le_bytes());
    // Fix data size.
    output[40..44].copy_from_slice(&data_size.to_le_bytes());
    output.extend_from_slice(pcm);

    tokio::fs::write(wav_path, &output).await?;

    tracing::info!(
        original_ms = (data.len() - 44) as u64 * 1000 / (sample_rate as u64 * 2),
        trimmed_ms = end_ms + 500,
        "VAD trimmed recording"
    );

    Ok(())
}

/// Check if Silero VAD is available (torch + silero-vad installed).
pub async fn is_available() -> bool {
    let child = Command::new("python3")
        .args(["-c", "import torch; print('OK')"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(_) => return false,
    };

    match tokio::time::timeout(Duration::from_secs(5), child.wait_with_output()).await {
        Ok(Ok(out)) => String::from_utf8_lossy(&out.stdout).contains("OK"),
        _ => false,
    }
}
