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

/// Deadline for Silero VAD inference — covers torch.hub cold-load on first use
/// but still reaps a hung Python child so the voice loop can recover (#617).
const VAD_DETECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Probe for `import torch` should be near-instant; 5s is generous.
const VAD_AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);

async fn python_output_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    label: &'static str,
) -> Result<std::process::Output> {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => anyhow::bail!("{label} timed out after {}s", timeout.as_secs()),
    }
}

/// Detect speech segments in a WAV file using Silero VAD.
///
/// Returns (has_speech, speech_end_ms) — whether speech was found,
/// and the timestamp (in ms) where speech ends.
/// If speech_end_ms < total duration, the file can be trimmed.
pub async fn detect_speech(wav_path: &str) -> Result<(bool, u64)> {
    let mut cmd = Command::new("python3");
    cmd.args([
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
    ]);

    let output = match python_output_with_timeout(cmd, VAD_DETECT_TIMEOUT, "silero-vad").await {
        Ok(output) => output,
        Err(e) => {
            tracing::warn!(error = %e, "VAD detect_speech failed — treating as silence");
            return Ok((false, 0));
        }
    };

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
    let mut cmd = Command::new("python3");
    cmd.args(["-c", "import torch; print('OK')"]);

    match python_output_with_timeout(cmd, VAD_AVAILABILITY_TIMEOUT, "vad-availability").await {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains("OK"),
        Err(_) => false,
    }
}
