//! GeniePod CLI — manage your GeniePod device from the terminal.
//!
//! Usage:
//!   genie-ctl status          Show system status (governor mode, memory, services)
//!   genie-ctl mode <MODE>     Change governor mode (day, night_a, night_b, media)
//!   genie-ctl chat <MESSAGE>  Send a chat message and print the response
//!   genie-ctl search [--fresh] [--limit N] <QUERY>
//!                              Search the web through genie-core
//!   genie-ctl history         Show conversation history
//!   genie-ctl tools           List available tools
//!   genie-ctl bfcl-score --cases CASES.jsonl --predictions PREDS.jsonl [--json]
//!                              Score tool-call accuracy from JSONL fixtures
//!   genie-ctl bfcl-predict-quick --cases CASES.jsonl --out PREDS.jsonl
//!                              Generate deterministic quick-router predictions
//!   genie-ctl bfcl-predict-llm --cases CASES.jsonl --out PREDS.jsonl
//!                              Generate local LLM tool-call predictions
//!   genie-ctl bfcl-score-llm --cases CASES.jsonl [--out PREDS.jsonl]
//!                              Generate and score local LLM tool-call predictions
//!   genie-ctl bfcl-import-ha-intents --source INTENTS_DIR --out CASES.jsonl
//!                              Convert Home Assistant Intents into BFCL cases
//!   genie-ctl skill ...       Manage loadable skill modules
//!   genie-ctl speaker ...     Manage local speaker identity profiles
//!   genie-ctl health          Check service health
//!   genie-ctl connectivity    Inspect the ESP32-C6 connectivity sidecar
//!   genie-ctl conversations   List all conversations
//!   genie-ctl support-bundle [PATH]
//!                              Write a local diagnostics support bundle
//!   genie-ctl version         Show version info

use anyhow::{Context, Result};
use genie_common::config::{Config, ServiceProbeTarget, parse_service_probe_target};
use genie_common::probe::{ProbeTimeouts, probe_service_target};
use genie_core::skills::{
    SkillLoader, SkillManifestAudit, find_manifest_sidecar, manifest_sidecar_candidates,
    skills_dir as runtime_skills_dir,
};
#[cfg(feature = "voice")]
use genie_core::voice::identity::{
    enroll_speaker_file, identify_speaker_file, list_speaker_profiles, remove_speaker_profile,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(feature = "voice")]
use std::process::Command;
use std::{fs, io::Write};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

mod bfcl_import;

const GOVERNOR_SOCK: &str = "/run/geniepod/governor.sock";
const BFCL_LLM_DEFAULT_MAX_TOKENS: u32 = 160;

fn load_core_addr() -> Result<String> {
    Ok(Config::load()?.core_http_addr())
}

/// One row in the `status` / `diag` / support-bundle service probe list.
#[derive(Debug, Clone)]
struct OptionalProbe {
    name: String,
    target: ServiceProbeTarget,
}

/// Probe targets for the optional HTTP services that `health`, `diag`, and
/// the support bundle inspect alongside `genie-core`. Reads `[services.api]`
/// and `[services.homeassistant]` from `geniepod.toml` so non-default
/// hosts/ports are honored. Home Assistant is omitted when not configured.
fn optional_http_probes(config: &Config) -> Vec<OptionalProbe> {
    let mut probes = vec![OptionalProbe {
        name: "genie-api".to_string(),
        target: parse_service_probe_target(&config.services.api.url),
    }];

    if let Some(ha) = config.homeassistant_service() {
        probes.push(OptionalProbe {
            name: "Home Assistant".to_string(),
            target: parse_service_probe_target(&ha.url),
        });
    }

    probes
}

/// Parsed probe target for genie-api when callers need extra HTTPS-aware
/// probes (support bundle `/api/security`, etc.).
fn api_probe_target(config: &Config) -> Result<ServiceProbeTarget, String> {
    match parse_service_probe_target(&config.services.api.url) {
        target @ (ServiceProbeTarget::Http { .. } | ServiceProbeTarget::Https { .. }) => Ok(target),
        ServiceProbeTarget::UnsupportedScheme { scheme } => Err(scheme),
    }
}

async fn probe_optional_target(target: &ServiceProbeTarget) -> Result<()> {
    probe_service_target(target, ProbeTimeouts::default()).await
}

const SKILL_RESTART_HINT: &str =
    "Restart genie-core to load skill changes, or wait until the next startup.";

#[derive(Debug, Clone)]
struct InstalledSkillInfo {
    name: String,
    version: String,
    description: String,
    path: PathBuf,
    manifest: SkillManifestAudit,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "status" => cmd_status().await?,
        "mode" => {
            if args.len() < 3 {
                eprintln!("Usage: genie-ctl mode <day|night_a|night_b|media>");
                std::process::exit(1);
            }
            cmd_mode(&args[2]).await?;
        }
        "chat" => {
            if args.len() < 3 {
                eprintln!("Usage: genie-ctl chat <message>");
                std::process::exit(1);
            }
            let message = args[2..].join(" ");
            cmd_chat(&message).await?;
        }
        "search" | "web-search" => {
            if args.len() < 3 {
                eprintln!("Usage: genie-ctl search [--fresh] [--limit N] <query>");
                std::process::exit(1);
            }
            let search_args = parse_search_args(&args[2..])?;
            cmd_search(&search_args.query, search_args.fresh, search_args.limit).await?;
        }
        "history" => cmd_history().await?,
        "tools" => cmd_tools().await?,
        "bfcl-score" | "eval-bfcl" => {
            let score_args = parse_bfcl_score_args(&args[2..])?;
            cmd_bfcl_score(&score_args)?;
        }
        "bfcl-score-llm" | "bfcl-score-local-llm" | "eval-bfcl-llm" => {
            let score_args = parse_bfcl_score_llm_args(&args[2..])?;
            cmd_bfcl_score_llm(&score_args).await?;
        }
        "bfcl-predict-quick" => {
            let predict_args = parse_bfcl_predict_quick_args(&args[2..])?;
            let generated = cmd_bfcl_predict_quick(&predict_args)?;
            println!(
                "generated {} quick-router BFCL predictions",
                generated.total_cases
            );
            println!("tool calls: {}", generated.tool_calls);
            println!("output: {}", predict_args.out.display());
        }
        "bfcl-predict-llm" | "bfcl-predict-local-llm" => {
            let predict_args = parse_bfcl_predict_llm_args(&args[2..])?;
            let generated = cmd_bfcl_predict_llm(&predict_args).await?;
            println!(
                "generated {} local-LLM BFCL predictions",
                generated.total_cases
            );
            println!("backend: {}", generated.backend);
            println!("tool calls: {}", generated.tool_calls);
            println!("output: {}", predict_args.out.display());
        }
        "bfcl-import-ha-intents" => {
            let import_args = bfcl_import::parse_ha_intents_import_args(&args[2..])?;
            let report = bfcl_import::import_ha_intents(&import_args)?;
            println!(
                "generated {} BFCL cases from {} Home Assistant Intents sentence templates",
                report.generated_cases, report.read_sentences
            );
            println!("output: {}", import_args.out.display());
        }
        "connectivity" | "radio" => cmd_connectivity().await?,
        "skill" | "skills" => {
            if args.len() < 3 {
                print_skill_usage();
                std::process::exit(1);
            }
            cmd_skill(&args[2..])?;
        }
        "speaker" | "speakers" => {
            #[cfg(feature = "voice")]
            {
                if args.len() < 3 {
                    print_speaker_usage();
                    std::process::exit(1);
                }
                cmd_speaker(&args[2..])?;
            }
            #[cfg(not(feature = "voice"))]
            {
                eprintln!(
                    "speaker subcommand is unavailable: this genie-ctl build was compiled \
                     without the 'voice' feature (issue #41). Rebuild with default features \
                     (or --features voice) to manage local speaker profiles."
                );
                std::process::exit(1);
            }
        }
        "health" => cmd_health().await?,
        "conversations" | "convos" => cmd_conversations().await?,
        "update-check" | "update" => cmd_update_check().await?,
        "diag" | "diagnostics" => cmd_diag().await?,
        "support-bundle" | "bundle" => {
            let output_path = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(default_support_bundle_path);
            cmd_support_bundle(&output_path).await?;
        }
        "version" | "--version" | "-v" => cmd_version(),
        "help" | "--help" | "-h" => print_usage(),
        other => {
            eprintln!("Unknown command: {}", other);
            print_usage();
            std::process::exit(1);
        }
    }

    Ok(())
}

fn print_usage() {
    println!("{}", usage_text());
}

fn usage_text() -> String {
    // Gated per issue #41: `speaker` is only listed when this build includes
    // the `voice` feature, so the help text matches what the binary can do.
    #[cfg(feature = "voice")]
    const SPEAKER_HELP_LINE: &str = "    speaker <SUBCOMMAND>\n                        Manage local speaker identity profiles\n";
    #[cfg(not(feature = "voice"))]
    const SPEAKER_HELP_LINE: &str = "";

    format!(
        "\
GeniePod CLI v{version}

USAGE:
    genie-ctl <COMMAND> [ARGS]

COMMANDS:
    status              System status (governor mode, memory, uptime)
    mode <MODE>         Change mode (day, night_a, night_b, media)
    chat <MESSAGE>      Send a chat message
    search [--fresh] [--limit N] <QUERY>
                        Search the web through genie-core
    history             Show conversation history
    tools               List available tools
    bfcl-score --cases C --predictions P [--json]
                        Score tool-call accuracy from JSONL fixtures
    bfcl-score-llm --cases C [--out P] [--json] [--max-tokens N] [--limit N]
                        Generate and score local LLM predictions without executing tools
    bfcl-predict-quick --cases C --out P
                        Generate deterministic quick-router predictions
    bfcl-predict-llm --cases C --out P [--max-tokens N] [--limit N]
                        Generate local LLM predictions without executing tools
    bfcl-import-ha-intents --source DIR --out CASES.jsonl [--language en] [--limit N]
                        Convert Home Assistant Intents into attributed BFCL cases
    connectivity        Inspect ESP32-C6 Thread/Matter sidecar status
    skill <SUBCOMMAND>  Manage loadable skill modules
{speaker}    health              Service health check
    conversations       List all conversations
    update-check        Check for OTA updates
    diag                Full system diagnostics report
    support-bundle [P]  Write JSON diagnostics bundle to path P
    version             Show version info
    help                Show this help",
        version = env!("CARGO_PKG_VERSION"),
        speaker = SPEAKER_HELP_LINE,
    )
}

#[cfg(feature = "voice")]
fn print_speaker_usage() {
    println!(
        "\
USAGE:
    genie-ctl speaker list [--profile-dir DIR]
    genie-ctl speaker enroll <NAME> <WAV> [--profile-dir DIR]
    genie-ctl speaker enroll-live <NAME> [--device DEV] [--sample-rate N] [--duration SECS] [--profile-dir DIR]
    genie-ctl speaker record <OUT.wav> [--device DEV] [--sample-rate N] [--duration SECS]
    genie-ctl speaker identify <WAV> [--profile-dir DIR] [--min-score N]
    genie-ctl speaker remove <NAME> [--profile-dir DIR]

SUBCOMMANDS:
    list                List enrolled local speaker profiles
    enroll              Enroll a local speaker profile from a short WAV sample
    enroll-live         Record a local sample, then enroll it
    record              Record a WAV sample using arecord
    identify            Match a WAV sample against enrolled local profiles
    remove              Delete an enrolled local speaker profile

NOTES:
    Speaker identification is local and optional. It helps route household
    memory in voice mode, but it is not a hostile-user authentication boundary."
    );
}

fn print_skill_usage() {
    println!(
        "\
USAGE:
    genie-ctl skill list
    genie-ctl skill install <SOURCE.so> [DEST_NAME]
    genie-ctl skill remove <SKILL_NAME|FILE_NAME>
    genie-ctl skill dir

SUBCOMMANDS:
    list                List loadable skills from the runtime skills directory
    install             Validate and copy a skill into the runtime skills directory
    remove              Remove an installed skill by tool name or filename
    dir                 Show the runtime skills directory"
    );
}

fn cmd_version() {
    println!("genie-ctl v{}", env!("CARGO_PKG_VERSION"));
    match load_core_addr() {
        Ok(addr) => println!("  core: {}", addr),
        Err(err) => println!("  core: (config error: {err})"),
    }
    println!("  governor: {}", GOVERNOR_SOCK);
}

#[cfg(feature = "voice")]
fn cmd_speaker(args: &[String]) -> Result<()> {
    match args[0].as_str() {
        "list" | "ls" => {
            let opts = parse_speaker_options(&args[1..])?;
            cmd_speaker_list(&opts.profile_dir)
        }
        "enroll" => {
            if args.len() < 3 {
                anyhow::bail!("Usage: genie-ctl speaker enroll <NAME> <WAV> [--profile-dir DIR]");
            }
            let name = &args[1];
            let wav = Path::new(&args[2]);
            let opts = parse_speaker_options(&args[3..])?;
            cmd_speaker_enroll(name, wav, &opts.profile_dir)
        }
        "enroll-live" | "enroll-record" => {
            if args.len() < 2 {
                anyhow::bail!(
                    "Usage: genie-ctl speaker enroll-live <NAME> [--device DEV] [--sample-rate N] [--duration SECS] [--profile-dir DIR]"
                );
            }
            let name = &args[1];
            let opts = parse_speaker_options(&args[2..])?;
            cmd_speaker_enroll_live(name, &opts)
        }
        "record" => {
            if args.len() < 2 {
                anyhow::bail!(
                    "Usage: genie-ctl speaker record <OUT.wav> [--device DEV] [--sample-rate N] [--duration SECS]"
                );
            }
            let output = Path::new(&args[1]);
            let opts = parse_speaker_options(&args[2..])?;
            cmd_speaker_record(output, &opts)
        }
        "identify" | "id" => {
            if args.len() < 2 {
                anyhow::bail!(
                    "Usage: genie-ctl speaker identify <WAV> [--profile-dir DIR] [--min-score N]"
                );
            }
            let wav = Path::new(&args[1]);
            let opts = parse_speaker_options(&args[2..])?;
            cmd_speaker_identify(wav, &opts.profile_dir, opts.min_score)
        }
        "remove" | "rm" | "delete" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: genie-ctl speaker remove <NAME> [--profile-dir DIR]");
            }
            let opts = parse_speaker_options(&args[2..])?;
            cmd_speaker_remove(&args[1], &opts.profile_dir)
        }
        other => {
            anyhow::bail!("Unknown speaker subcommand: {}", other);
        }
    }
}

#[cfg(feature = "voice")]
#[derive(Debug, Clone)]
struct SpeakerCliOptions {
    profile_dir: PathBuf,
    min_score: f32,
    device: String,
    sample_rate: u32,
    duration_secs: u32,
}

#[cfg(feature = "voice")]
fn parse_speaker_options(args: &[String]) -> Result<SpeakerCliOptions> {
    let defaults = default_speaker_options();
    let mut profile_dir = defaults.profile_dir;
    let mut min_score = defaults.min_score;
    let mut device = defaults.device;
    let mut sample_rate = defaults.sample_rate;
    let mut duration_secs = defaults.duration_secs;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--profile-dir" => {
                let Some(value) = args.get(i + 1) else {
                    anyhow::bail!("--profile-dir requires a directory");
                };
                profile_dir = PathBuf::from(value);
                i += 2;
            }
            "--min-score" => {
                let Some(value) = args.get(i + 1) else {
                    anyhow::bail!("--min-score requires a number");
                };
                min_score = value.parse::<f32>()?;
                i += 2;
            }
            "--device" => {
                let Some(value) = args.get(i + 1) else {
                    anyhow::bail!("--device requires an ALSA device, e.g. plughw:2,0");
                };
                device = value.to_string();
                i += 2;
            }
            "--sample-rate" => {
                let Some(value) = args.get(i + 1) else {
                    anyhow::bail!("--sample-rate requires a number");
                };
                sample_rate = value.parse::<u32>()?;
                i += 2;
            }
            "--duration" | "--secs" => {
                let Some(value) = args.get(i + 1) else {
                    anyhow::bail!("--duration requires a number of seconds");
                };
                duration_secs = value.parse::<u32>()?;
                i += 2;
            }
            other => anyhow::bail!("unknown speaker option: {}", other),
        }
    }

    Ok(SpeakerCliOptions {
        profile_dir,
        min_score,
        device,
        sample_rate,
        duration_secs,
    })
}

#[cfg(feature = "voice")]
fn default_speaker_options() -> SpeakerCliOptions {
    Config::load()
        .map(|config| SpeakerCliOptions {
            profile_dir: config.core.speaker_identity.local_profile_dir,
            device: if config.core.audio_device.is_empty() || config.core.audio_device == "auto" {
                "default".into()
            } else {
                config.core.audio_device
            },
            sample_rate: config.core.audio_sample_rate,
            duration_secs: config.core.voice_record_secs.max(3),
            min_score: config.core.speaker_identity.local_min_score,
        })
        .unwrap_or_else(|_| SpeakerCliOptions {
            profile_dir: PathBuf::from("/opt/geniepod/data/speakers"),
            device: "default".into(),
            sample_rate: 48_000,
            duration_secs: 5,
            min_score: 0.82,
        })
}

#[cfg(feature = "voice")]
fn cmd_speaker_list(profile_dir: &Path) -> Result<()> {
    let profiles = list_speaker_profiles(profile_dir)?;
    if profiles.is_empty() {
        println!("(no speaker profiles found in {})", profile_dir.display());
        return Ok(());
    }

    println!(
        "{} speaker profile{} in {}:\n",
        profiles.len(),
        if profiles.len() == 1 { "" } else { "s" },
        profile_dir.display()
    );
    for profile in profiles {
        println!(
            "  {} — {} samples, {} Hz, {}",
            profile.name, profile.sample_count, profile.sample_rate, profile.fingerprint_version
        );
    }
    Ok(())
}

#[cfg(feature = "voice")]
fn cmd_speaker_enroll(name: &str, wav: &Path, profile_dir: &Path) -> Result<()> {
    let profile = enroll_speaker_file(profile_dir, name, wav)?;
    println!(
        "Enrolled speaker '{}' in {} ({} samples, {} Hz)",
        profile.name,
        profile_dir.display(),
        profile.sample_count,
        profile.sample_rate
    );
    println!("Enable with: [core.speaker_identity] provider = \"local_biometric\"");
    Ok(())
}

#[cfg(feature = "voice")]
fn cmd_speaker_enroll_live(name: &str, opts: &SpeakerCliOptions) -> Result<()> {
    let wav_path = std::env::temp_dir().join(format!(
        "geniepod-speaker-enroll-{}-{}.wav",
        std::process::id(),
        unix_time_ms()
    ));
    println!(
        "Recording {} seconds for '{}' on {}...",
        opts.duration_secs, name, opts.device
    );
    record_speaker_wav(&wav_path, opts)?;
    let result = cmd_speaker_enroll(name, &wav_path, &opts.profile_dir);
    let _ = std::fs::remove_file(&wav_path);
    result
}

#[cfg(feature = "voice")]
fn cmd_speaker_record(output: &Path, opts: &SpeakerCliOptions) -> Result<()> {
    println!(
        "Recording {} seconds on {} -> {}",
        opts.duration_secs,
        opts.device,
        output.display()
    );
    record_speaker_wav(output, opts)?;
    println!("Recorded {}", output.display());
    Ok(())
}

#[cfg(feature = "voice")]
fn cmd_speaker_identify(wav: &Path, profile_dir: &Path, min_score: f32) -> Result<()> {
    match identify_speaker_file(profile_dir, wav, min_score)? {
        Some(result) => {
            println!(
                "Matched speaker '{}' with score {:.3} ({})",
                result.name,
                result.score,
                result.profile_path.display()
            );
        }
        None => {
            println!(
                "No speaker matched at min_score {:.3} in {}",
                min_score,
                profile_dir.display()
            );
        }
    }
    Ok(())
}

#[cfg(feature = "voice")]
fn cmd_speaker_remove(name: &str, profile_dir: &Path) -> Result<()> {
    let removed = remove_speaker_profile(profile_dir, name)?;
    println!("Removed speaker profile {}", removed.display());
    Ok(())
}

#[cfg(feature = "voice")]
fn record_speaker_wav(output: &Path, opts: &SpeakerCliOptions) -> Result<()> {
    if opts.duration_secs == 0 {
        anyhow::bail!("recording duration must be greater than zero");
    }
    if opts.sample_rate == 0 {
        anyhow::bail!("sample rate must be greater than zero");
    }
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let status = Command::new("arecord")
        .args([
            "-D",
            &opts.device,
            "-f",
            "S16_LE",
            "-r",
            &opts.sample_rate.to_string(),
            "-c",
            "1",
            "-d",
            &opts.duration_secs.to_string(),
            output
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("output path is not valid UTF-8"))?,
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("arecord failed with status {}", status);
    }

    let metadata = std::fs::metadata(output)?;
    if metadata.len() <= 44 {
        anyhow::bail!("recording produced empty audio; check microphone device");
    }
    Ok(())
}

fn cmd_skill(args: &[String]) -> Result<()> {
    match args[0].as_str() {
        "list" | "ls" => cmd_skill_list(),
        "install" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: genie-ctl skill install <SOURCE.so> [DEST_NAME]");
            }
            cmd_skill_install(Path::new(&args[1]), args.get(2).map(String::as_str))
        }
        "remove" | "rm" | "uninstall" => {
            if args.len() < 2 {
                anyhow::bail!("Usage: genie-ctl skill remove <SKILL_NAME|FILE_NAME>");
            }
            cmd_skill_remove(&args[1])
        }
        "dir" | "path" => {
            println!("{}", runtime_skills_path().display());
            Ok(())
        }
        other => {
            anyhow::bail!("Unknown skill subcommand: {}", other);
        }
    }
}

fn cmd_skill_list() -> Result<()> {
    let skills_dir = runtime_skills_path();
    let skills = load_installed_skills(&skills_dir)?;

    if skills.is_empty() {
        println!("(no loadable skills found in {})", skills_dir.display());
        return Ok(());
    }

    println!(
        "{} loadable skills in {}:\n",
        skills.len(),
        skills_dir.display()
    );
    for skill in skills {
        let file_name = skill
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| skill.path.display().to_string());
        println!("  {} v{} ({})", skill.name, skill.version, file_name);
        println!("    {}", skill.description);
        println!("    manifest: {}", skill.manifest.status);
        if !skill.manifest.permissions.is_empty() {
            println!("    permissions: {}", skill.manifest.permissions.join(", "));
        }
        if !skill.manifest.capabilities.is_empty() {
            println!(
                "    capabilities: {}",
                skill.manifest.capabilities.join(", ")
            );
        }
        if !skill.manifest.reviewed_by.is_empty() || skill.manifest.signed {
            let reviewer = if skill.manifest.reviewed_by.is_empty() {
                "unreviewed"
            } else {
                &skill.manifest.reviewed_by
            };
            // `signed` is true only when a trusted key cryptographically
            // verified the .so bytes; name the key that did.
            let signed = if skill.manifest.signed {
                format!("verified (key: {})", skill.manifest.key_id)
            } else {
                "no".to_string()
            };
            println!("    reviewed: {reviewer}; signed: {signed}");
        }
        if !skill.manifest.error.is_empty() {
            println!("    manifest note: {}", skill.manifest.error);
        }
    }

    Ok(())
}

fn cmd_skill_install(source: &Path, dest_name: Option<&str>) -> Result<()> {
    let skills_dir = runtime_skills_path();
    let (installed, bytes_copied) = install_skill(source, &skills_dir, dest_name)?;

    println!(
        "Installed skill '{}' v{} to {} ({:.1} KB)",
        installed.name,
        installed.version,
        installed.path.display(),
        bytes_copied as f64 / 1024.0
    );
    println!("{}", SKILL_RESTART_HINT);
    Ok(())
}

fn cmd_skill_remove(target: &str) -> Result<()> {
    let skills_dir = runtime_skills_path();
    let removed_path = remove_skill(target, &skills_dir)?;

    println!("Removed {}", removed_path.display());
    println!("{}", SKILL_RESTART_HINT);
    Ok(())
}

fn runtime_skills_path() -> PathBuf {
    runtime_skills_dir()
}

fn load_installed_skills(skills_dir: &Path) -> Result<Vec<InstalledSkillInfo>> {
    let mut loader = SkillLoader::new(skills_dir);
    let _ = loader.load_all();

    let mut skills = loader
        .loaded()
        .iter()
        .map(|skill| InstalledSkillInfo {
            name: skill.name.clone(),
            version: skill.version.clone(),
            description: skill.description.clone(),
            path: skill.path.clone(),
            manifest: skill.manifest.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

fn validate_skill_file(path: &Path) -> Result<InstalledSkillInfo> {
    if !path.exists() {
        anyhow::bail!("skill file not found: {}", path.display());
    }
    if !path.is_file() {
        anyhow::bail!("skill path is not a file: {}", path.display());
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut loader = SkillLoader::new(parent);
    let loaded_name = loader.load_skill(path)?;
    let skill = loader
        .loaded()
        .iter()
        .find(|skill| skill.name == loaded_name)
        .ok_or_else(|| anyhow::anyhow!("validated skill '{}' disappeared", loaded_name))?;

    Ok(InstalledSkillInfo {
        name: skill.name.clone(),
        version: skill.version.clone(),
        description: skill.description.clone(),
        path: skill.path.clone(),
        manifest: skill.manifest.clone(),
    })
}

fn normalize_skill_filename(source: &Path, dest_name: Option<&str>) -> Result<String> {
    let file_name = match dest_name {
        Some(name) if !name.trim().is_empty() => {
            let trimmed = name.trim();
            if Path::new(trimmed).extension().is_some() {
                trimmed.to_string()
            } else {
                format!("{}.so", trimmed)
            }
        }
        _ => source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| anyhow::anyhow!("cannot determine filename for {}", source.display()))?,
    };

    if file_name.contains('/') {
        anyhow::bail!("destination name must be a filename, not a path");
    }

    Ok(file_name)
}

fn install_skill(
    source: &Path,
    skills_dir: &Path,
    dest_name: Option<&str>,
) -> Result<(InstalledSkillInfo, u64)> {
    let _source_skill = validate_skill_file(source)?;
    std::fs::create_dir_all(skills_dir)?;

    let file_name = normalize_skill_filename(source, dest_name)?;
    let dest_path = skills_dir.join(file_name);
    let bytes_copied = std::fs::copy(source, &dest_path)?;
    let _ = copy_skill_manifest_sidecar(source, &dest_path)?;

    let installed = validate_skill_file(&dest_path)?;

    Ok((installed, bytes_copied))
}

fn copy_skill_manifest_sidecar(source: &Path, dest_path: &Path) -> Result<Option<PathBuf>> {
    let Some(source_manifest) = find_manifest_sidecar(source) else {
        return Ok(None);
    };

    let dest_manifest = dest_path.with_extension("skill.json");
    std::fs::copy(&source_manifest, &dest_manifest)?;
    Ok(Some(dest_manifest))
}

fn remove_skill_and_sidecars(skill_path: &Path) -> Result<()> {
    std::fs::remove_file(skill_path)?;
    for sidecar in manifest_sidecar_candidates(skill_path) {
        if sidecar.exists() {
            std::fs::remove_file(sidecar)?;
        }
    }
    Ok(())
}

fn remove_skill(target: &str, skills_dir: &Path) -> Result<PathBuf> {
    let installed = load_installed_skills(skills_dir)?;
    if let Some(skill) = installed.iter().find(|skill| {
        skill.name == target
            || skill
                .path
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == target)
    }) {
        remove_skill_and_sidecars(&skill.path)?;
        return Ok(skill.path.clone());
    }

    let direct_candidates = if Path::new(target).extension().is_some() {
        vec![skills_dir.join(target)]
    } else {
        vec![
            skills_dir.join(target),
            skills_dir.join(format!("{}.so", target)),
        ]
    };

    for candidate in direct_candidates {
        if candidate.exists() {
            remove_skill_and_sidecars(&candidate)?;
            return Ok(candidate);
        }
    }

    anyhow::bail!("skill '{}' not found in {}", target, skills_dir.display())
}

async fn cmd_status() -> Result<()> {
    let core = load_core_addr()?;
    // Try governor first.
    if let Some(gov) = governor_cmd(r#"{"cmd":"status"}"#).await {
        let mode = gov
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let mem = gov
            .get("mem_available_mb")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let uptime = gov.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
        let hours = uptime / 3600;
        let mins = (uptime % 3600) / 60;

        println!("Governor:  {} mode", mode);
        println!("Memory:    {} MB available", mem);
        println!("Uptime:    {}h {}m", hours, mins);
    } else {
        println!("Governor:  offline");
    }

    // Try core health.
    match http_get(&core, "/api/health").await {
        Ok(body) => {
            let data: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            let status = data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            println!("Core:      {}", status);
            if let Some(connectivity) = data.get("connectivity") {
                let state = connectivity
                    .get("state")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!("Radio:     {}", state);
            }
            if let Some(web_search) = data.get("web_search") {
                let enabled = web_search
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let provider = web_search
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                println!(
                    "WebSearch: {} ({})",
                    if enabled { "enabled" } else { "disabled" },
                    provider
                );
            }
            // System-prompt SHA (issue #110): same config + hydrated state must
            // produce an identical digest across a full-stack restart.
            if let Some(sha) = data.get("system_prompt_sha").and_then(|v| v.as_str()) {
                println!("Prompt:    {}", sha);
            }
            if let Some(harness) = data.get("agent_harness")
                && let Some(summary) = format_agent_harness_status(harness)
            {
                println!("Harness:   {}", summary);
            }
        }
        Err(_) => println!("Core:      offline"),
    }

    Ok(())
}

async fn cmd_mode(mode: &str) -> Result<()> {
    let cmd = format!(r#"{{"cmd":"set_mode","mode":"{}"}}"#, mode);
    match governor_cmd(&cmd).await {
        Some(resp) => {
            let ok = resp.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
            if ok {
                println!("Mode changed to: {}", mode);
            } else {
                let err = resp
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown error");
                eprintln!("Failed: {}", err);
            }
        }
        None => eprintln!("Governor offline — cannot change mode"),
    }
    Ok(())
}

async fn cmd_chat(message: &str) -> Result<()> {
    let core = load_core_addr()?;
    let body = serde_json::json!({"message": message}).to_string();
    let response = http_post_with_origin(&core, "/api/chat", &body, "api").await?;
    let data: serde_json::Value = serde_json::from_str(&response)?;

    if let Some(resp) = data.get("response").and_then(|v| v.as_str()) {
        if let Some(tool) = data.get("tool").and_then(|v| v.as_str()) {
            println!("[{}] {}", tool, resp);
        } else {
            println!("{}", resp);
        }
    } else if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
        eprintln!("Error: {}", err);
    }

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchArgs {
    fresh: bool,
    limit: u64,
    query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclScoreArgs {
    cases: PathBuf,
    predictions: PathBuf,
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclScoreLlmArgs {
    cases: PathBuf,
    out: Option<PathBuf>,
    max_tokens: u32,
    limit: Option<usize>,
    json_mode: bool,
    json: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclPredictQuickArgs {
    cases: PathBuf,
    out: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclPredictLlmArgs {
    cases: PathBuf,
    out: PathBuf,
    max_tokens: u32,
    limit: Option<usize>,
    json_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclPredictQuickReport {
    total_cases: usize,
    tool_calls: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BfclPredictLlmReport {
    total_cases: usize,
    tool_calls: usize,
    backend: String,
}

#[derive(Debug, Clone, PartialEq)]
struct BfclLlmPredictionRun {
    cases: Vec<genie_core::eval::bfcl::BfclCase>,
    predictions: Vec<genie_core::eval::bfcl::BfclPrediction>,
    tool_calls: usize,
    backend: String,
}

fn parse_search_args(args: &[String]) -> Result<SearchArgs> {
    let mut fresh = false;
    let mut limit = 3;
    let mut query_parts = Vec::new();
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--fresh" | "--no-cache" => {
                fresh = true;
                idx += 1;
            }
            "--limit" | "-n" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--limit requires a value");
                };
                limit = parse_search_limit(value)?;
                idx += 2;
            }
            _ if arg.starts_with("--limit=") => {
                limit = parse_search_limit(arg.trim_start_matches("--limit="))?;
                idx += 1;
            }
            _ => {
                query_parts.push(arg.clone());
                idx += 1;
            }
        }
    }

    Ok(SearchArgs {
        fresh,
        limit,
        query: query_parts.join(" "),
    })
}

fn parse_bfcl_score_args(args: &[String]) -> Result<BfclScoreArgs> {
    let mut cases = None;
    let mut predictions = None;
    let mut json = false;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--cases" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--cases requires a JSONL path");
                };
                cases = Some(PathBuf::from(value));
                idx += 2;
            }
            "--predictions" | "--preds" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--predictions requires a JSONL path");
                };
                predictions = Some(PathBuf::from(value));
                idx += 2;
            }
            "--json" => {
                json = true;
                idx += 1;
            }
            _ if arg.starts_with("--cases=") => {
                cases = Some(PathBuf::from(arg.trim_start_matches("--cases=")));
                idx += 1;
            }
            _ if arg.starts_with("--predictions=") => {
                predictions = Some(PathBuf::from(arg.trim_start_matches("--predictions=")));
                idx += 1;
            }
            _ if arg.starts_with("--preds=") => {
                predictions = Some(PathBuf::from(arg.trim_start_matches("--preds=")));
                idx += 1;
            }
            other => anyhow::bail!("unknown bfcl-score option: {}", other),
        }
    }

    let Some(cases) = cases else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-score --cases CASES.jsonl --predictions PREDICTIONS.jsonl [--json]"
        );
    };
    let Some(predictions) = predictions else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-score --cases CASES.jsonl --predictions PREDICTIONS.jsonl [--json]"
        );
    };

    Ok(BfclScoreArgs {
        cases,
        predictions,
        json,
    })
}

fn parse_bfcl_score_llm_args(args: &[String]) -> Result<BfclScoreLlmArgs> {
    let mut cases = None;
    let mut out = None;
    let mut max_tokens = BFCL_LLM_DEFAULT_MAX_TOKENS;
    let mut limit = None;
    let mut json_mode = true;
    let mut json = false;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--cases" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--cases requires a JSONL path");
                };
                cases = Some(PathBuf::from(value));
                idx += 2;
            }
            "--out" | "--predictions" | "--preds" | "--predictions-out" | "--preds-out" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--out requires a JSONL output path");
                };
                out = Some(PathBuf::from(value));
                idx += 2;
            }
            "--max-tokens" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--max-tokens requires a value");
                };
                max_tokens = parse_positive_u32("--max-tokens", value)?;
                idx += 2;
            }
            "--limit" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--limit requires a value");
                };
                limit = Some(parse_positive_usize("--limit", value)?);
                idx += 2;
            }
            "--json-mode" => {
                json_mode = true;
                idx += 1;
            }
            "--no-json-mode" => {
                json_mode = false;
                idx += 1;
            }
            "--json" => {
                json = true;
                idx += 1;
            }
            _ if arg.starts_with("--cases=") => {
                cases = Some(PathBuf::from(arg.trim_start_matches("--cases=")));
                idx += 1;
            }
            _ if arg.starts_with("--out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--out=")));
                idx += 1;
            }
            _ if arg.starts_with("--predictions=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--predictions=")));
                idx += 1;
            }
            _ if arg.starts_with("--preds=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--preds=")));
                idx += 1;
            }
            _ if arg.starts_with("--predictions-out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--predictions-out=")));
                idx += 1;
            }
            _ if arg.starts_with("--preds-out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--preds-out=")));
                idx += 1;
            }
            _ if arg.starts_with("--max-tokens=") => {
                max_tokens =
                    parse_positive_u32("--max-tokens", arg.trim_start_matches("--max-tokens="))?;
                idx += 1;
            }
            _ if arg.starts_with("--limit=") => {
                limit = Some(parse_positive_usize(
                    "--limit",
                    arg.trim_start_matches("--limit="),
                )?);
                idx += 1;
            }
            other => anyhow::bail!("unknown bfcl-score-llm option: {}", other),
        }
    }

    let Some(cases) = cases else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-score-llm --cases CASES.jsonl [--out PREDICTIONS.jsonl] [--json] [--max-tokens N] [--limit N]"
        );
    };

    Ok(BfclScoreLlmArgs {
        cases,
        out,
        max_tokens,
        limit,
        json_mode,
        json,
    })
}

fn parse_bfcl_predict_quick_args(args: &[String]) -> Result<BfclPredictQuickArgs> {
    let mut cases = None;
    let mut out = None;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--cases" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--cases requires a JSONL path");
                };
                cases = Some(PathBuf::from(value));
                idx += 2;
            }
            "--out" | "--predictions" | "--preds" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--out requires a JSONL output path");
                };
                out = Some(PathBuf::from(value));
                idx += 2;
            }
            _ if arg.starts_with("--cases=") => {
                cases = Some(PathBuf::from(arg.trim_start_matches("--cases=")));
                idx += 1;
            }
            _ if arg.starts_with("--out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--out=")));
                idx += 1;
            }
            _ if arg.starts_with("--predictions=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--predictions=")));
                idx += 1;
            }
            _ if arg.starts_with("--preds=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--preds=")));
                idx += 1;
            }
            other => anyhow::bail!("unknown bfcl-predict-quick option: {}", other),
        }
    }

    let Some(cases) = cases else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-predict-quick --cases CASES.jsonl --out PREDICTIONS.jsonl"
        );
    };
    let Some(out) = out else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-predict-quick --cases CASES.jsonl --out PREDICTIONS.jsonl"
        );
    };

    Ok(BfclPredictQuickArgs { cases, out })
}

fn parse_bfcl_predict_llm_args(args: &[String]) -> Result<BfclPredictLlmArgs> {
    let mut cases = None;
    let mut out = None;
    let mut max_tokens = BFCL_LLM_DEFAULT_MAX_TOKENS;
    let mut limit = None;
    let mut json_mode = true;
    let mut idx = 0;

    while idx < args.len() {
        let arg = &args[idx];
        match arg.as_str() {
            "--cases" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--cases requires a JSONL path");
                };
                cases = Some(PathBuf::from(value));
                idx += 2;
            }
            "--out" | "--predictions" | "--preds" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--out requires a JSONL output path");
                };
                out = Some(PathBuf::from(value));
                idx += 2;
            }
            "--max-tokens" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--max-tokens requires a value");
                };
                max_tokens = parse_positive_u32("--max-tokens", value)?;
                idx += 2;
            }
            "--limit" => {
                let Some(value) = args.get(idx + 1) else {
                    anyhow::bail!("--limit requires a value");
                };
                limit = Some(parse_positive_usize("--limit", value)?);
                idx += 2;
            }
            "--json-mode" => {
                json_mode = true;
                idx += 1;
            }
            "--no-json-mode" => {
                json_mode = false;
                idx += 1;
            }
            _ if arg.starts_with("--cases=") => {
                cases = Some(PathBuf::from(arg.trim_start_matches("--cases=")));
                idx += 1;
            }
            _ if arg.starts_with("--out=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--out=")));
                idx += 1;
            }
            _ if arg.starts_with("--predictions=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--predictions=")));
                idx += 1;
            }
            _ if arg.starts_with("--preds=") => {
                out = Some(PathBuf::from(arg.trim_start_matches("--preds=")));
                idx += 1;
            }
            _ if arg.starts_with("--max-tokens=") => {
                max_tokens =
                    parse_positive_u32("--max-tokens", arg.trim_start_matches("--max-tokens="))?;
                idx += 1;
            }
            _ if arg.starts_with("--limit=") => {
                limit = Some(parse_positive_usize(
                    "--limit",
                    arg.trim_start_matches("--limit="),
                )?);
                idx += 1;
            }
            other => anyhow::bail!("unknown bfcl-predict-llm option: {}", other),
        }
    }

    let Some(cases) = cases else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-predict-llm --cases CASES.jsonl --out PREDICTIONS.jsonl [--max-tokens N] [--limit N]"
        );
    };
    let Some(out) = out else {
        anyhow::bail!(
            "Usage: genie-ctl bfcl-predict-llm --cases CASES.jsonl --out PREDICTIONS.jsonl [--max-tokens N] [--limit N]"
        );
    };

    Ok(BfclPredictLlmArgs {
        cases,
        out,
        max_tokens,
        limit,
        json_mode,
    })
}

fn parse_search_limit(value: &str) -> Result<u64> {
    let limit = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid --limit value: {}", value))?;
    if !(1..=5).contains(&limit) {
        anyhow::bail!("--limit must be between 1 and 5");
    }
    Ok(limit)
}

fn parse_positive_u32(name: &str, value: &str) -> Result<u32> {
    let parsed = value
        .parse::<u32>()
        .map_err(|_| anyhow::anyhow!("invalid {name} value: {value}"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be greater than 0");
    }
    Ok(parsed)
}

fn parse_positive_usize(name: &str, value: &str) -> Result<usize> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| anyhow::anyhow!("invalid {name} value: {value}"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be greater than 0");
    }
    Ok(parsed)
}

async fn cmd_search(query: &str, fresh: bool, limit: u64) -> Result<()> {
    let core = load_core_addr()?;
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("Usage: genie-ctl search [--fresh] [--limit N] <query>");
    }

    let body = serde_json::json!({"query": query, "fresh": fresh, "limit": limit}).to_string();
    let response = http_post(&core, "/api/web-search", &body).await?;
    let data: serde_json::Value = serde_json::from_str(&response)?;

    if let Some(resp) = data.get("response").and_then(|v| v.as_str()) {
        let tool = data
            .get("tool")
            .and_then(|v| v.as_str())
            .unwrap_or("web_search");
        println!("[{}] {}", tool, resp);
    } else if let Some(err) = data.get("error").and_then(|v| v.as_str()) {
        eprintln!("Error: {}", err);
    }

    Ok(())
}

fn cmd_bfcl_score(args: &BfclScoreArgs) -> Result<()> {
    let cases = genie_core::eval::bfcl::load_cases_jsonl(&args.cases)?;
    let predictions = genie_core::eval::bfcl::load_predictions_jsonl(&args.predictions)?;
    let report = genie_core::eval::bfcl::score_cases(&cases, &predictions);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    print_bfcl_report("BFCL tool-call score", &report);

    Ok(())
}

async fn cmd_bfcl_score_llm(args: &BfclScoreLlmArgs) -> Result<()> {
    let run =
        generate_bfcl_llm_predictions(&args.cases, args.max_tokens, args.limit, args.json_mode)
            .await?;

    if let Some(out) = &args.out {
        write_bfcl_predictions_jsonl(out, &run.predictions)?;
    }

    let report = genie_core::eval::bfcl::score_cases(&run.cases, &run.predictions);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "backend": run.backend,
                "generated_predictions": run.predictions.len(),
                "tool_calls": run.tool_calls,
                "predictions_out": args.out,
                "report": report,
            }))?
        );
        return Ok(());
    }

    println!("BFCL local-LLM tool-call score");
    println!("backend:             {}", run.backend);
    println!("generated_predictions: {}", run.predictions.len());
    println!("tool_calls:          {}", run.tool_calls);
    if let Some(out) = &args.out {
        println!("predictions:         {}", out.display());
    }
    print_bfcl_report_metrics(&report);

    Ok(())
}

fn print_bfcl_report(title: &str, report: &genie_core::eval::bfcl::BfclReport) {
    println!("{title}");
    print_bfcl_report_metrics(report);
}

fn print_bfcl_report_metrics(report: &genie_core::eval::bfcl::BfclReport) {
    println!("cases:               {}", report.total_cases);
    println!(
        "parse_accuracy:      {}",
        format_score_rate(report.parse_accuracy)
    );
    println!(
        "tool_name_accuracy:  {}",
        format_score_rate(report.tool_name_accuracy)
    );
    println!(
        "argument_accuracy:   {}",
        format_score_rate(report.argument_accuracy)
    );
    println!(
        "grounded_arg_acc:    {}",
        format_score_rate(report.grounded_argument_accuracy)
    );
    println!(
        "strict_accuracy:     {}",
        format_score_rate(report.strict_accuracy)
    );
    println!(
        "grounded_strict_acc: {}",
        format_score_rate(report.grounded_strict_accuracy)
    );
    println!("missing_predictions: {}", report.missing_predictions);
    println!("failures:            {}", report.failure_count);

    let failures = report
        .case_scores
        .iter()
        .filter(|score| !score.strict_match)
        .take(10)
        .collect::<Vec<_>>();

    if !failures.is_empty() {
        println!("\nFirst failures:");
        for failure in failures {
            let detail = if failure.diagnostics.is_empty() {
                "no diagnostic".to_string()
            } else {
                failure.diagnostics.join("; ")
            };
            println!("  {}: {}", failure.id, detail);
        }
    }
}

fn cmd_bfcl_predict_quick(args: &BfclPredictQuickArgs) -> Result<BfclPredictQuickReport> {
    let cases = genie_core::eval::bfcl::load_cases_jsonl(&args.cases)?;
    if let Some(parent) = args.out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let mut writer = std::io::BufWriter::new(fs::File::create(&args.out)?);
    let mut tool_calls = 0;
    for case in &cases {
        let response = if let Some(call) =
            genie_core::tools::quick::route_for_available_tools(&case.prompt, true, true)
        {
            tool_calls += 1;
            serde_json::json!({
                "tool": call.name,
                "arguments": call.arguments,
            })
            .to_string()
        } else {
            String::new()
        };
        let prediction = genie_core::eval::bfcl::BfclPrediction {
            id: case.id.clone(),
            response,
        };
        serde_json::to_writer(&mut writer, &prediction)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    Ok(BfclPredictQuickReport {
        total_cases: cases.len(),
        tool_calls,
    })
}

async fn cmd_bfcl_predict_llm(args: &BfclPredictLlmArgs) -> Result<BfclPredictLlmReport> {
    let run =
        generate_bfcl_llm_predictions(&args.cases, args.max_tokens, args.limit, args.json_mode)
            .await?;

    write_bfcl_predictions_jsonl(&args.out, &run.predictions)?;

    Ok(BfclPredictLlmReport {
        total_cases: run.predictions.len(),
        tool_calls: run.tool_calls,
        backend: run.backend,
    })
}

async fn generate_bfcl_llm_predictions(
    cases_path: &Path,
    max_tokens: u32,
    limit: Option<usize>,
    json_mode: bool,
) -> Result<BfclLlmPredictionRun> {
    let cases = genie_core::eval::bfcl::load_cases_jsonl(cases_path)?;
    let selected_cases = select_bfcl_cases(cases, limit);
    let system_prompt = build_bfcl_llm_system_prompt(&selected_cases);
    let config = Config::load()?;
    let timeouts = genie_core::llm::LlmTimeouts::from_secs(
        config.core.llm_connect_timeout_secs,
        config.core.llm_read_timeout_secs,
        config.core.llm_request_timeout_secs,
    );
    let llm = genie_core::llm::LlmClient::from_service_config_with_timeouts(
        &config.services.llm,
        timeouts,
    );
    let backend = llm.backend_name().to_string();

    let mut predictions = Vec::with_capacity(selected_cases.len());
    let mut tool_calls = 0;

    for case in &selected_cases {
        let messages = build_bfcl_llm_messages(&system_prompt, &case.prompt);
        let hints =
            genie_core::llm::LlmRequestHints::agent_turn(format!("bfcl-{}", case.id), max_tokens);
        let response_format = json_mode.then(genie_core::llm::ResponseFormat::json);
        let response = llm
            .chat_with_format_and_hints(&messages, Some(max_tokens), response_format, Some(&hints))
            .await
            .with_context(|| format!("local LLM prediction failed for BFCL case {}", case.id))?;

        tool_calls += genie_core::tools::parse_tool_calls_for_eval(&response).len();
        predictions.push(genie_core::eval::bfcl::BfclPrediction {
            id: case.id.clone(),
            response,
        });
    }

    Ok(BfclLlmPredictionRun {
        cases: selected_cases,
        predictions,
        tool_calls,
        backend,
    })
}

fn select_bfcl_cases(
    cases: Vec<genie_core::eval::bfcl::BfclCase>,
    limit: Option<usize>,
) -> Vec<genie_core::eval::bfcl::BfclCase> {
    cases
        .into_iter()
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

fn write_bfcl_predictions_jsonl(
    path: &Path,
    predictions: &[genie_core::eval::bfcl::BfclPrediction],
) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let mut writer = std::io::BufWriter::new(fs::File::create(path)?);
    for prediction in predictions {
        serde_json::to_writer(&mut writer, prediction)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;

    Ok(())
}

fn build_bfcl_llm_messages(system_prompt: &str, prompt: &str) -> Vec<genie_core::llm::Message> {
    vec![
        genie_core::llm::Message {
            role: "system".to_string(),
            content: system_prompt.to_string(),
        },
        genie_core::llm::Message {
            role: "user".to_string(),
            content: format!("User utterance:\n{prompt}\n\nReturn the BFCL tool-call JSON now."),
        },
    ]
}

fn build_bfcl_llm_system_prompt(cases: &[genie_core::eval::bfcl::BfclCase]) -> String {
    let catalog = bfcl_llm_tool_catalog(cases).join("\n");
    let devices = genie_core::eval::bfcl::bfcl_reference_home_device_catalog().join(", ");
    format!(
        "\
You are GenieClaw's BFCL tool-call evaluator for a private local home agent.
The target runtime is NVIDIA Jetson Orin 8GB with a 4096-token context, so be terse.
Return only JSON. Do not explain. Do not execute tools.

If the utterance needs one tool, return:
{{\"tool\":\"tool_name\",\"arguments\":{{...}}}}

If the utterance needs multiple tools, return a JSON array:
[{{\"tool\":\"tool_name\",\"arguments\":{{...}}}}]

If no tool is needed, return:
{{\"answer\":\"no_tool\"}}

Use only these tools and compact arguments:
{catalog}

Devices in this home (use these exact lowercase names; no other rooms or devices exist): {devices}.
Map vague references to one of them (\"lights\" -> \"kitchen lights\", \"fan\" -> \"kitchen fan\"); never invent a room such as \"upstairs\". Use the schema's exact action values (turn_on, turn_off, set_temperature), not synonyms like \"deactivate\".

Normalize obvious speech-to-text noise, casing, and misspellings before choosing a tool.
Prefer deterministic home state, memory retrieval, and typed-tool arguments over natural-language answers."
    )
}

fn bfcl_llm_tool_catalog(cases: &[genie_core::eval::bfcl::BfclCase]) -> Vec<String> {
    let runtime_catalog = genie_core::eval::bfcl::bfcl_llm_runtime_tool_catalog();
    let expected_tools = cases
        .iter()
        .flat_map(|case| {
            case.expected_tool_calls
                .iter()
                .map(|call| call.name.as_str())
        })
        .collect::<BTreeSet<_>>();
    let include_all = expected_tools.is_empty();
    let mut rows = Vec::new();

    for (name, summary) in &runtime_catalog {
        if include_all || expected_tools.contains(name.as_str()) {
            rows.push(format!("- {name}: {summary}"));
        }
    }

    for name in expected_tools {
        if !runtime_catalog.contains_key(name) {
            rows.push(format!(
                "- {name}: dynamic local skill tool. Return only the required JSON arguments."
            ));
        }
    }

    rows
}

fn format_agent_harness_status(harness: &serde_json::Value) -> Option<String> {
    let pass = harness.get("pass")?.as_bool()?;
    if pass {
        return Some("pass".to_string());
    }

    let failed_checks: Vec<String> = harness
        .get("checks")
        .and_then(|checks| checks.as_array())
        .into_iter()
        .flatten()
        .filter(|check| check.get("pass").and_then(|v| v.as_bool()) == Some(false))
        .filter_map(|check| {
            check
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect();

    if failed_checks.is_empty() {
        Some("FAIL".to_string())
    } else {
        Some(format!("FAIL ({})", failed_checks.join(", ")))
    }
}

fn format_agent_harness_health_line(harness: &serde_json::Value) -> Option<String> {
    let pass = harness.get("pass")?.as_bool()?;
    if pass {
        return Some("  [OK]   harness".to_string());
    }

    let summary = format_agent_harness_status(harness)?;
    if let Some(detail) = summary.strip_prefix("FAIL ") {
        Some(format!("  [FAIL] harness {detail}"))
    } else {
        Some("  [FAIL] harness".to_string())
    }
}

fn format_score_rate(rate: f64) -> String {
    format!("{:.2}%", rate * 100.0)
}

async fn cmd_history() -> Result<()> {
    let core = load_core_addr()?;
    let body = http_get(&core, "/api/chat/history").await?;
    let messages: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();

    if messages.is_empty() {
        println!("(no messages yet)");
        return Ok(());
    }

    for msg in &messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let content = msg.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let prefix = match role {
            "user" => "You",
            "assistant" => "GeniePod",
            "system" => "System",
            _ => role,
        };
        println!("{}: {}", prefix, content);
    }

    Ok(())
}

async fn cmd_tools() -> Result<()> {
    let core = load_core_addr()?;
    let body = http_get(&core, "/api/tools").await?;
    let tools: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();

    if tools.is_empty() {
        println!("(no tools available — is genie-core running?)");
        return Ok(());
    }

    println!("{} tools available:\n", tools.len());
    for tool in &tools {
        let name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let desc = tool
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        println!("  {:20} {}", name, desc);
    }

    Ok(())
}

async fn cmd_connectivity() -> Result<()> {
    let core = load_core_addr()?;
    let body = http_get(&core, "/api/connectivity").await?;
    let data: serde_json::Value = serde_json::from_str(&body)?;

    let health = data
        .get("health")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let state = health
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let transport = health
        .get("transport")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let device = health
        .get("device")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let message = health.get("message").and_then(|v| v.as_str()).unwrap_or("");

    let capabilities = data
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    println!("Connectivity: {}", state);
    println!("Transport:    {}", transport);
    println!("Device:       {}", device);
    if capabilities.is_empty() {
        println!("Capabilities: none");
    } else {
        println!("Capabilities: {}", capabilities.join(", "));
    }
    if !message.is_empty() {
        println!("Message:      {}", message);
    }

    Ok(())
}

async fn cmd_health() -> Result<()> {
    let config = Config::load()?;
    let core = config.core_http_addr();
    let core_health = match http_get(&core, "/api/health").await {
        Ok(body) => {
            println!("  [OK]   genie-core");
            serde_json::from_str::<serde_json::Value>(&body).ok()
        }
        Err(_) => {
            println!("  [DOWN] genie-core");
            None
        }
    };

    if let Some(health) = &core_health {
        let label = llm_service_label(health.get("llm_backend").and_then(|v| v.as_str()));
        match health.get("llm").and_then(|v| v.as_str()) {
            Some("connected") => println!("  [OK]   {}", label),
            Some(_) => println!("  [DOWN] {}", label),
            None => println!("  [DOWN] {}", label),
        }

        // Chat-path liveness (issue #181): a wedged turn must be visible here
        // rather than hidden behind an overall "ok" status.
        if let Some(chat) = health.get("chat") {
            let in_flight = chat
                .get("in_flight")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let wedged = chat
                .get("wedged")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if wedged {
                let age = chat
                    .get("current_turn_age_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!(
                    "  [WEDGED] chat turn stuck for {age}s — restart genie-core if it persists"
                );
            } else if in_flight {
                println!("  [BUSY] chat turn in progress");
            } else {
                println!("  [OK]   chat path idle");
            }
        }

        if let Some(harness) = health.get("agent_harness")
            && let Some(line) = format_agent_harness_health_line(harness)
        {
            println!("{line}");
        }
    }

    // Check each remaining HTTP/TLS service, honoring [services.*] in geniepod.toml.
    for probe in optional_http_probes(&config) {
        match probe_optional_target(&probe.target).await {
            Ok(()) => println!("  [OK]   {}", probe.name),
            Err(_) => match probe.target {
                ServiceProbeTarget::UnsupportedScheme { scheme } => {
                    println!(
                        "  [SKIP] {} ({} probe not supported by genie-ctl)",
                        probe.name, scheme
                    );
                }
                _ => println!("  [DOWN] {}", probe.name),
            },
        }
    }

    // Governor (Unix socket, not HTTP).
    match governor_cmd(r#"{"cmd":"status"}"#).await {
        Some(_) => println!("  [OK]   genie-governor"),
        None => println!("  [DOWN] genie-governor"),
    }

    Ok(())
}

async fn cmd_conversations() -> Result<()> {
    let core = load_core_addr()?;
    let body = http_get(&core, "/api/conversations").await?;
    let convos: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();

    if convos.is_empty() {
        println!("(no conversations yet)");
        return Ok(());
    }

    println!("{} conversations:\n", convos.len());
    for conv in &convos {
        let id = conv.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let title = conv
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("untitled");
        let count = conv
            .get("message_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        println!("  {} — {} ({} messages)", id, title, count);
    }

    Ok(())
}

async fn cmd_update_check() -> Result<()> {
    println!("Checking for updates...\n");

    // Check GitHub Releases via curl (handles TLS).
    let output = tokio::process::Command::new("curl")
        .args([
            "-sS",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: GeniePod-OTA",
            "https://api.github.com/repos/GeniePod/genie-claw/releases/latest",
        ])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            let release: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::json!({}));

            let tag = release
                .get("tag_name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let published = release
                .get("published_at")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let current = env!("CARGO_PKG_VERSION");

            println!("  Current: v{}", current);
            println!("  Latest:  {}", tag);
            println!("  Published: {}", published);

            let latest_clean = tag
                .strip_prefix('v')
                .unwrap_or(tag)
                .split('-')
                .next()
                .unwrap_or(tag);
            let current_clean = current.split('-').next().unwrap_or(current);

            if latest_clean > current_clean {
                println!("\n  Update available! Download from:");
                println!(
                    "  https://github.com/GeniePod/genie-claw/releases/tag/{}",
                    tag
                );
            } else {
                println!("\n  You're up to date.");
            }
        }
        Ok(out) => {
            eprintln!("GitHub API error: {}", String::from_utf8_lossy(&out.stderr));
        }
        Err(e) => {
            eprintln!("Failed to check (is curl installed?): {}", e);
        }
    }

    Ok(())
}

async fn cmd_diag() -> Result<()> {
    let config = Config::load()?;
    let core = config.core_http_addr();
    println!("=== GeniePod Diagnostics ===\n");

    // Version.
    println!("[Version]");
    println!("  genie-ctl: v{}", env!("CARGO_PKG_VERSION"));

    // Core health.
    println!("\n[Services]");
    let core_status = match http_get(&core, "/api/health").await {
        Ok(_) => "UP",
        Err(_) => "DOWN",
    };
    println!("  {:20} {}", "genie-core", core_status);
    for probe in optional_http_probes(&config) {
        let status = match probe_optional_target(&probe.target).await {
            Ok(()) => "UP".to_string(),
            Err(_) => match probe.target {
                ServiceProbeTarget::UnsupportedScheme { scheme } => {
                    format!("SKIP ({scheme} probe not supported)")
                }
                _ => "DOWN".to_string(),
            },
        };
        println!("  {:20} {}", probe.name, status);
    }
    let gov_status = match governor_cmd(r#"{"cmd":"status"}"#).await {
        Some(_) => "UP",
        None => "DOWN",
    };
    println!("  {:20} {}", "genie-governor", gov_status);

    // Governor details.
    if let Some(gov) = governor_cmd(r#"{"cmd":"status"}"#).await {
        println!("\n[Governor]");
        let mode = gov.get("mode").and_then(|v| v.as_str()).unwrap_or("?");
        let mem = gov
            .get("mem_available_mb")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let uptime = gov.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
        println!("  Mode:    {}", mode);
        println!("  Memory:  {} MB available", mem);
        println!("  Uptime:  {}h {}m", uptime / 3600, (uptime % 3600) / 60);
    }

    // Core details.
    if let Ok(body) = http_get(&core, "/api/health").await
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&body)
    {
        println!("\n[Core]");
        if let Some(v) = data.get("version").and_then(|v| v.as_str()) {
            println!("  Version:       v{}", v);
        }
        if let Some(v) = data.get("llm").and_then(|v| v.as_str()) {
            println!("  LLM:           {}", v);
        }
        if let Some(v) = data.get("llm_backend").and_then(|v| v.as_str()) {
            println!("  LLM Backend:   {}", v);
        }
        if let Some(v) = data.get("memories").and_then(|v| v.as_u64()) {
            println!("  Memories:      {}", v);
        }
        if let Some(v) = data.get("conversations").and_then(|v| v.as_u64()) {
            println!("  Conversations: {}", v);
        }
        if let Some(contract) = data.get("runtime_contract") {
            if let Some(v) = contract.get("contract_hash").and_then(|v| v.as_str()) {
                println!("  Runtime Hash:  {}", v);
            }
            if let Some(v) = contract.get("tool_count").and_then(|v| v.as_u64()) {
                println!("  Runtime Tools: {}", v);
            }
            if let Some(validation) = contract.get("validation")
                && let Some(v) = validation.get("status").and_then(|v| v.as_str())
            {
                println!("  Runtime Drift: {}", v);
            }
        }
        if let Some(harness) = data.get("agent_harness")
            && let Some(summary) = format_agent_harness_status(harness)
        {
            println!("\n[Harness]");
            println!("  Status:  {}", summary);
        }
    }

    // System info.
    println!("\n[System]");

    // Memory.
    if let Ok(meminfo) = tokio::fs::read_to_string("/proc/meminfo").await {
        for line in meminfo.lines().take(3) {
            println!("  {}", line);
        }
    }

    // Load.
    if let Ok(loadavg) = tokio::fs::read_to_string("/proc/loadavg").await {
        println!("  Load: {}", loadavg.trim());
    }

    // Uptime.
    if let Ok(uptime) = tokio::fs::read_to_string("/proc/uptime").await
        && let Some(secs) = uptime.split_whitespace().next()
        && let Ok(s) = secs.parse::<f64>()
    {
        println!("  Uptime: {:.0}h {:.0}m", s / 3600.0, (s % 3600.0) / 60.0);
    }

    // Disk.
    let df = tokio::process::Command::new("df")
        .args(["-h", "/opt/geniepod"])
        .output()
        .await;
    if let Ok(out) = df
        && out.status.success()
    {
        let output = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = output.lines().nth(1) {
            println!(
                "  Disk: {}",
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            );
        }
    }

    // Binaries.
    println!("\n[Binaries]");
    let bin_dir = "/opt/geniepod/bin";
    for name in &[
        "genie-core",
        "genie-ctl",
        "genie-governor",
        "genie-health",
        "genie-api",
        "llama-server",
        "genie-ai-runtime",
    ] {
        let path = format!("{}/{}", bin_dir, name);
        if std::path::Path::new(&path).exists() {
            let meta = std::fs::metadata(&path).ok();
            let size = meta
                .map(|m| format!("{:.1} MB", m.len() as f64 / 1_048_576.0))
                .unwrap_or("?".into());
            println!("  {:20} present ({})", name, size);
        } else {
            println!("  {:20} MISSING", name);
        }
    }

    // Models.
    println!("\n[Models]");
    let model_dir = "/opt/geniepod/models";
    if let Ok(entries) = std::fs::read_dir(model_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let size = entry
                .metadata()
                .ok()
                .map(|m| format!("{:.1} GB", m.len() as f64 / 1_073_741_824.0))
                .unwrap_or("?".into());
            println!("  {} ({})", name, size);
        }
    } else {
        println!("  (directory not found: {})", model_dir);
    }

    // Config.
    println!("\n[Config]");
    for path in &[
        "/etc/geniepod/geniepod.toml",
        "/etc/geniepod/mosquitto.conf",
    ] {
        let status = if std::path::Path::new(path).exists() {
            "present"
        } else {
            "MISSING"
        };
        println!("  {:40} {}", path, status);
    }

    println!("\n=== End Diagnostics ===");
    Ok(())
}

async fn cmd_support_bundle(output_path: &Path) -> Result<()> {
    let config = Config::load()?;
    let core = config.core_http_addr();
    let api_target = api_probe_target(&config);

    let mut service_status = vec![serde_json::json!({
        "service": "genie-core",
        "addr": core.clone(),
        "path": "/api/health",
        "reachable": http_get(&core, "/api/health").await.is_ok(),
    })];

    for probe in optional_http_probes(&config) {
        let row = match &probe.target {
            ServiceProbeTarget::Http { addr, path } | ServiceProbeTarget::Https { addr, path } => {
                let reachable = probe_optional_target(&probe.target).await.is_ok();
                let scheme = if matches!(probe.target, ServiceProbeTarget::Https { .. }) {
                    "https"
                } else {
                    "http"
                };
                serde_json::json!({
                    "service": probe.name,
                    "scheme": scheme,
                    "addr": addr,
                    "path": path,
                    "reachable": reachable,
                })
            }
            ServiceProbeTarget::UnsupportedScheme { scheme } => serde_json::json!({
                "service": probe.name,
                "skipped": true,
                "unsupported_scheme": scheme,
            }),
        };
        service_status.push(row);
    }

    // Resolve once for the two extra genie-api probes below.
    let (api_security, api_audit) = match &api_target {
        Ok(target) => (
            probe_json_value(target, "/api/security").await,
            probe_json_value(target, "/api/actuation/audit").await,
        ),
        Err(scheme) => (
            serde_json::json!({
                "skipped": true,
                "reason": "unsupported_scheme",
                "scheme": scheme,
            }),
            serde_json::json!({
                "skipped": true,
                "reason": "unsupported_scheme",
                "scheme": scheme,
            }),
        ),
    };

    let bundle = serde_json::json!({
        "schema_version": 1,
        "created_ms": unix_time_ms(),
        "tool": {
            "name": "genie-ctl",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "services": service_status,
        "governor": governor_cmd(r#"{"cmd":"status"}"#).await,
        "core": {
            "health": http_json_value(&core, "/api/health").await,
            "runtime_contract": http_json_value(&core, "/api/runtime/contract").await,
            "connectivity": http_json_value(&core, "/api/connectivity").await,
        },
        "security": api_security,
        "actuation": {
            "pending": http_json_value(&core, "/api/actuation/pending").await,
            "actions": http_json_value(&core, "/api/actuation/actions").await,
            "audit": api_audit,
        },
        "system": {
            "meminfo": read_file_lines("/proc/meminfo", 8),
            "loadavg": read_file_string("/proc/loadavg"),
            "uptime": read_file_string("/proc/uptime"),
            "disk_opt_geniepod": disk_summary("/opt/geniepod").await,
        },
        "files": {
            "config_presence": config_presence(),
            "binaries": binary_inventory(),
            "models": model_inventory(),
            "runtime_contract_log_tail": tail_jsonl_file(Path::new("/opt/geniepod/data/runtime/contracts.jsonl"), 5),
            "tool_audit_log_tail": tail_jsonl_file(Path::new("/opt/geniepod/data/runtime/tool-audit.jsonl"), 20),
            "actuation_audit_log_tail": tail_jsonl_file(Path::new("/opt/geniepod/data/safety/actuation-audit.jsonl"), 20),
        },
    });

    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, serde_json::to_string_pretty(&bundle)?)?;
    println!("support bundle written: {}", output_path.display());
    Ok(())
}

fn default_support_bundle_path() -> PathBuf {
    PathBuf::from(format!("/tmp/geniepod-support-{}.json", unix_time_ms()))
}

fn unix_time_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn probe_json_value(target: &ServiceProbeTarget, path: &str) -> serde_json::Value {
    use genie_common::probe::probe_target_body;

    match probe_target_body(target, path, ProbeTimeouts::default()).await {
        Ok(body) => serde_json::from_str(&body).unwrap_or_else(|e| {
            serde_json::json!({
                "error": format!("invalid JSON: {e}"),
                "raw": body,
            })
        }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

async fn http_json_value(addr: &str, path: &str) -> serde_json::Value {
    match http_get(addr, path).await {
        Ok(body) => serde_json::from_str(&body).unwrap_or_else(|e| {
            serde_json::json!({
                "error": format!("invalid JSON: {e}"),
                "raw": body,
            })
        }),
        Err(e) => serde_json::json!({ "error": e.to_string() }),
    }
}

fn llm_service_label(backend: Option<&str>) -> String {
    match backend.filter(|value| !value.trim().is_empty()) {
        Some(backend) => format!("LLM ({backend})"),
        None => "LLM".into(),
    }
}

fn read_file_string(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

fn read_file_lines(path: &str, limit: usize) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| {
            text.lines()
                .take(limit)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

async fn disk_summary(path: &str) -> serde_json::Value {
    match tokio::process::Command::new("df")
        .args(["-h", path])
        .output()
        .await
    {
        Ok(out) if out.status.success() => {
            let output = String::from_utf8_lossy(&out.stdout);
            let columns = output
                .lines()
                .nth(1)
                .map(|line| line.split_whitespace().collect::<Vec<_>>())
                .unwrap_or_default();
            serde_json::json!({
                "path": path,
                "summary": columns,
            })
        }
        Ok(out) => serde_json::json!({
            "path": path,
            "error": String::from_utf8_lossy(&out.stderr).trim(),
        }),
        Err(e) => serde_json::json!({
            "path": path,
            "error": e.to_string(),
        }),
    }
}

fn config_presence() -> Vec<serde_json::Value> {
    let mut paths = vec![
        "/etc/geniepod/geniepod.toml",
        "/etc/geniepod/mosquitto.conf",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();

    match Config::load() {
        Ok(config) => {
            paths.push(systemd_unit_file_path(&config.services.core.systemd_unit));
            paths.push(systemd_unit_file_path(&config.services.llm.systemd_unit));
        }
        Err(_) => {
            paths.push(systemd_unit_file_path("genie-core.service"));
            paths.push(systemd_unit_file_path("genie-llm.service"));
        }
    }

    paths
        .into_iter()
        .map(|path| {
            serde_json::json!({
                "path": path,
                "present": Path::new(&path).exists(),
            })
        })
        .collect()
}

fn systemd_unit_file_path(unit: &str) -> String {
    let unit = if unit.contains('.') {
        unit.to_string()
    } else {
        format!("{unit}.service")
    };
    format!("/etc/systemd/system/{unit}")
}

fn binary_inventory() -> Vec<serde_json::Value> {
    [
        "genie-core",
        "genie-ctl",
        "genie-governor",
        "genie-health",
        "genie-api",
        "llama-server",
        "genie-ai-runtime",
    ]
    .into_iter()
    .map(|name| {
        let path = PathBuf::from("/opt/geniepod/bin").join(name);
        let size_bytes = std::fs::metadata(&path).ok().map(|meta| meta.len());
        serde_json::json!({
            "name": name,
            "path": path,
            "present": path.exists(),
            "size_bytes": size_bytes,
        })
    })
    .collect()
}

fn model_inventory() -> Vec<serde_json::Value> {
    let model_dir = Path::new("/opt/geniepod/models");
    let Ok(entries) = std::fs::read_dir(model_dir) else {
        return Vec::new();
    };

    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            let size_bytes = entry.metadata().ok().map(|meta| meta.len());
            serde_json::json!({
                "name": entry.file_name().to_string_lossy(),
                "path": path,
                "size_bytes": size_bytes,
            })
        })
        .collect()
}

fn tail_jsonl_file(path: &Path, limit: usize) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut items = text
        .lines()
        .rev()
        .take(limit)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>();
    items.reverse();
    items
}

// ── HTTP helpers ───────────────────────────────────────────────

async fn http_get(addr: &str, path: &str) -> Result<String> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timeout"))??;

    let (reader, mut writer) = stream.into_split();
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, addr
    );
    writer.write_all(req.as_bytes()).await?;

    read_http_body(reader).await
}

async fn http_post(addr: &str, path: &str, body: &str) -> Result<String> {
    http_post_inner(addr, path, body, None).await
}

async fn http_post_with_origin(addr: &str, path: &str, body: &str, origin: &str) -> Result<String> {
    http_post_inner(addr, path, body, Some(origin)).await
}

async fn http_post_inner(
    addr: &str,
    path: &str,
    body: &str,
    origin: Option<&str>,
) -> Result<String> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timeout"))??;

    let (reader, mut writer) = stream.into_split();
    let origin_header = origin
        .map(|origin| format!("X-Genie-Origin: {}\r\n", origin))
        .unwrap_or_default();
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        path,
        addr,
        origin_header,
        body.len(),
        body
    );
    writer.write_all(req.as_bytes()).await?;

    read_http_body(reader).await
}

async fn read_http_body(reader: tokio::net::tcp::OwnedReadHalf) -> Result<String> {
    let mut buf_reader = BufReader::new(reader);
    let mut body = String::new();
    let mut in_body = false;

    loop {
        let mut line = String::new();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if in_body {
            body.push_str(&line);
        } else if line.trim().is_empty() {
            in_body = true;
        }
    }

    Ok(body.trim().to_string())
}

async fn governor_cmd(json: &str) -> Option<serde_json::Value> {
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(GOVERNOR_SOCK).await.ok()?;
    let (reader, mut writer) = stream.into_split();

    writer.write_all(json.as_bytes()).await.ok()?;
    writer.write_all(b"\n").await.ok()?;

    let mut lines = BufReader::new(reader).lines();
    let line = tokio::time::timeout(std::time::Duration::from_secs(3), lines.next_line())
        .await
        .ok()?
        .ok()?;

    line.and_then(|l| serde_json::from_str(&l).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_common::config::ServiceEndpoint;
    use std::process::Command;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn default_test_config() -> Config {
        toml::from_str("").expect("default config should parse from empty TOML")
    }

    fn expect_probe_target(target: &ServiceProbeTarget) -> (&str, &str, bool) {
        match target {
            ServiceProbeTarget::Http { addr, path } => (addr.as_str(), path.as_str(), false),
            ServiceProbeTarget::Https { addr, path } => (addr.as_str(), path.as_str(), true),
            ServiceProbeTarget::UnsupportedScheme { scheme } => {
                panic!("expected Http/Https target, got UnsupportedScheme({scheme})")
            }
        }
    }

    #[test]
    fn optional_http_probes_uses_configured_api_url() {
        let mut config = default_test_config();
        config.services.api.url = "http://10.0.0.7:4080/api/status".into();

        let probes = optional_http_probes(&config);
        // Default config has no homeassistant; only the api probe is present.
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].name, "genie-api");
        let (addr, path, tls) = expect_probe_target(&probes[0].target);
        assert_eq!(addr, "10.0.0.7:4080");
        assert_eq!(path, "/api/status");
        assert!(!tls);
    }

    #[test]
    fn optional_http_probes_skips_homeassistant_when_unconfigured() {
        let config = default_test_config();
        assert!(config.homeassistant_service().is_none());

        let probes = optional_http_probes(&config);
        assert!(
            !probes.iter().any(|p| p.name == "Home Assistant"),
            "HA probe must be omitted when [services.homeassistant] is absent",
        );
    }

    #[test]
    fn optional_http_probes_includes_homeassistant_when_configured() {
        let mut config = default_test_config();
        config.services.homeassistant = Some(ServiceEndpoint {
            url: "http://192.168.1.50:8123/".into(),
            systemd_unit: "homeassistant.service".into(),
            backend: Default::default(),
        });

        let probes = optional_http_probes(&config);
        let ha = probes
            .iter()
            .find(|p| p.name == "Home Assistant")
            .expect("HA probe should be present");
        let (addr, path, tls) = expect_probe_target(&ha.target);
        assert_eq!(addr, "192.168.1.50:8123");
        assert_eq!(path, "/");
        assert!(!tls);
    }

    #[test]
    fn optional_http_probes_parses_https_homeassistant() {
        let mut config = default_test_config();
        config.services.homeassistant = Some(ServiceEndpoint {
            url: "https://ha.example/api/".into(),
            systemd_unit: "homeassistant.service".into(),
            backend: Default::default(),
        });

        let probes = optional_http_probes(&config);
        let ha = probes
            .iter()
            .find(|p| p.name == "Home Assistant")
            .expect("HA probe should be present even when https");
        let (addr, path, tls) = expect_probe_target(&ha.target);
        assert_eq!(addr, "ha.example:443");
        assert_eq!(path, "/api/");
        assert!(tls);
    }

    #[test]
    fn api_probe_target_returns_https_api_target() {
        let mut config = default_test_config();
        config.services.api.url = "https://api.example/api/status".into();
        let target = api_probe_target(&config).expect("https api should parse");
        let (addr, path, tls) = expect_probe_target(&target);
        assert_eq!(addr, "api.example:443");
        assert_eq!(path, "/api/status");
        assert!(tls);
    }

    #[test]
    fn api_probe_target_returns_addr_for_ipv6_without_port() {
        // Pairs with the IPv6 bracket regression in genie-common.
        let mut config = default_test_config();
        config.services.api.url = "http://[::1]/api/status".into();
        let target = api_probe_target(&config).expect("ipv6 api should parse");
        let (addr, path, tls) = expect_probe_target(&target);
        assert_eq!(addr, "[::1]:80");
        assert_eq!(path, "/api/status");
        assert!(!tls);
    }

    fn workspace_root() -> PathBuf {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().parent().unwrap().to_path_buf()
    }

    fn sample_skill_path() -> &'static Path {
        static SAMPLE_SKILL_PATH: OnceLock<PathBuf> = OnceLock::new();
        SAMPLE_SKILL_PATH.get_or_init(|| {
            let root = workspace_root();
            let build_dir = std::env::temp_dir().join(format!(
                "geniepod-sample-skill-build-ctl-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&build_dir);
            std::fs::create_dir_all(&build_dir).unwrap();
            let output = Command::new("cargo")
                .args(["build", "-p", "genie-skill-hello", "--target-dir"])
                .arg(&build_dir)
                .current_dir(&root)
                .output()
                .expect("failed to build sample skill");

            assert!(
                output.status.success(),
                "sample skill build failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );

            let candidates = [
                build_dir.join("debug/libgenie_skill_hello.so"),
                build_dir.join("debug/libgenie_skill_hello.dylib"),
                build_dir.join("debug/genie_skill_hello.dll"),
            ];

            candidates
                .into_iter()
                .find(|path| path.exists())
                .expect("sample skill artifact not found")
        })
    }

    fn temp_skills_dir() -> PathBuf {
        static TEMP_DIR_COUNTER: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "geniepod-ctl-skill-test-{}-{}",
            std::process::id(),
            TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn version_string() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(!version.is_empty());
        assert!(version.contains('.')); // Semver: x.y.z
    }

    #[test]
    fn usage_command_lines_are_indented() {
        // Regression: a `\` line-continuation after the `{speaker}` block used to
        // strip the four-space indent off the `health` line, leaving it flush-left
        // while every other command stayed indented.
        let usage = usage_text();
        assert!(
            usage.contains("\n    health              Service health check"),
            "`health` command should keep its four-space indent:\n{usage}"
        );

        let in_commands = usage
            .split_once("COMMANDS:\n")
            .map(|(_, rest)| rest)
            .unwrap_or(&usage);
        for line in in_commands.lines() {
            if line.is_empty() {
                continue;
            }
            assert!(
                line.starts_with("    "),
                "every command/continuation line must be indented, got: {line:?}"
            );
        }
    }

    #[test]
    fn core_addr_from_config_uses_bind_host_and_port() {
        use genie_common::config::{
            Config, CoreConfig, GovernorConfig, HealthConfig, ServicesConfig,
        };
        use std::path::PathBuf;

        let config = Config {
            data_dir: PathBuf::from("./data"),
            core: CoreConfig {
                port: 3001,
                bind_host: "127.0.0.1".into(),
                ..CoreConfig::default()
            },
            agent: Default::default(),
            optional_ai_provider: Default::default(),
            governor: GovernorConfig::default(),
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: Default::default(),
            web_search: Default::default(),
            connectivity: Default::default(),
            http: Default::default(),
        };

        assert_eq!(config.core_http_addr(), "127.0.0.1:3001");
    }

    #[test]
    fn core_addr_maps_listen_all_to_loopback_for_local_cli() {
        use genie_common::config::{
            Config, CoreConfig, GovernorConfig, HealthConfig, ServicesConfig,
        };
        use std::path::PathBuf;

        let config = Config {
            data_dir: PathBuf::from("./data"),
            core: CoreConfig {
                port: 3000,
                bind_host: "0.0.0.0".into(),
                ..CoreConfig::default()
            },
            agent: Default::default(),
            optional_ai_provider: Default::default(),
            governor: GovernorConfig::default(),
            health: HealthConfig::default(),
            services: ServicesConfig::default(),
            telegram: Default::default(),
            web_search: Default::default(),
            connectivity: Default::default(),
            http: Default::default(),
        };

        assert_eq!(config.core_http_addr(), "127.0.0.1:3000");
    }

    #[test]
    fn parse_search_args_supports_fresh_flag() {
        let args = vec![
            "--fresh".to_string(),
            "ESP32-C6".to_string(),
            "Thread".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();

        assert!(parsed.fresh);
        assert_eq!(parsed.limit, 3);
        assert_eq!(parsed.query, "ESP32-C6 Thread");
    }

    #[test]
    fn parse_search_args_supports_no_cache_alias() {
        let args = vec![
            "Matter".to_string(),
            "--no-cache".to_string(),
            "news".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();

        assert!(parsed.fresh);
        assert_eq!(parsed.query, "Matter news");
    }

    #[test]
    fn parse_search_args_supports_limit_flag() {
        let args = vec![
            "--limit".to_string(),
            "5".to_string(),
            "Home".to_string(),
            "Assistant".to_string(),
        ];
        let parsed = parse_search_args(&args).unwrap();

        assert_eq!(parsed.limit, 5);
        assert_eq!(parsed.query, "Home Assistant");
    }

    #[test]
    fn parse_search_args_supports_limit_equals() {
        let args = vec!["--limit=2".to_string(), "Matter".to_string()];
        let parsed = parse_search_args(&args).unwrap();

        assert_eq!(parsed.limit, 2);
        assert_eq!(parsed.query, "Matter");
    }

    #[test]
    fn parse_search_args_rejects_invalid_limit() {
        let args = vec!["--limit".to_string(), "9".to_string(), "Matter".to_string()];

        assert!(parse_search_args(&args).is_err());
    }

    #[test]
    fn parse_bfcl_score_args_supports_required_paths_and_json() {
        let args = vec![
            "--cases".to_string(),
            "tests/bfcl/home_tool_cases.jsonl".to_string(),
            "--predictions".to_string(),
            "tests/bfcl/home_tool_predictions.jsonl".to_string(),
            "--json".to_string(),
        ];

        let parsed = parse_bfcl_score_args(&args).unwrap();

        assert_eq!(
            parsed.cases,
            PathBuf::from("tests/bfcl/home_tool_cases.jsonl")
        );
        assert_eq!(
            parsed.predictions,
            PathBuf::from("tests/bfcl/home_tool_predictions.jsonl")
        );
        assert!(parsed.json);
    }

    #[test]
    fn parse_bfcl_score_args_supports_equals_form() {
        let args = vec![
            "--cases=cases.jsonl".to_string(),
            "--preds=predictions.jsonl".to_string(),
        ];

        let parsed = parse_bfcl_score_args(&args).unwrap();

        assert_eq!(parsed.cases, PathBuf::from("cases.jsonl"));
        assert_eq!(parsed.predictions, PathBuf::from("predictions.jsonl"));
        assert!(!parsed.json);
    }

    #[test]
    fn parse_bfcl_score_args_rejects_missing_prediction_path() {
        let args = vec![
            "--cases".to_string(),
            "tests/bfcl/home_tool_cases.jsonl".to_string(),
        ];

        assert!(parse_bfcl_score_args(&args).is_err());
    }

    #[test]
    fn parse_bfcl_score_llm_args_supports_scoring_options() {
        let args = vec![
            "--cases=tests/bfcl/local/ha_home_cases.jsonl".to_string(),
            "--out=tests/bfcl/local/ha_home_llm_predictions.jsonl".to_string(),
            "--max-tokens=128".to_string(),
            "--limit".to_string(),
            "40".to_string(),
            "--json".to_string(),
            "--no-json-mode".to_string(),
        ];

        let parsed = parse_bfcl_score_llm_args(&args).unwrap();

        assert_eq!(
            parsed.cases,
            PathBuf::from("tests/bfcl/local/ha_home_cases.jsonl")
        );
        assert_eq!(
            parsed.out,
            Some(PathBuf::from(
                "tests/bfcl/local/ha_home_llm_predictions.jsonl"
            ))
        );
        assert_eq!(parsed.max_tokens, 128);
        assert_eq!(parsed.limit, Some(40));
        assert!(parsed.json);
        assert!(!parsed.json_mode);
    }

    #[test]
    fn parse_bfcl_predict_quick_args_supports_output_aliases() {
        let args = vec![
            "--cases=tests/bfcl/local/ha_home_cases.jsonl".to_string(),
            "--preds=tests/bfcl/local/ha_home_predictions.jsonl".to_string(),
        ];

        let parsed = parse_bfcl_predict_quick_args(&args).unwrap();

        assert_eq!(
            parsed.cases,
            PathBuf::from("tests/bfcl/local/ha_home_cases.jsonl")
        );
        assert_eq!(
            parsed.out,
            PathBuf::from("tests/bfcl/local/ha_home_predictions.jsonl")
        );
    }

    #[test]
    fn parse_bfcl_predict_llm_args_supports_runtime_options() {
        let args = vec![
            "--cases=tests/bfcl/local/ha_home_cases.jsonl".to_string(),
            "--predictions=tests/bfcl/local/ha_home_llm_predictions.jsonl".to_string(),
            "--max-tokens".to_string(),
            "96".to_string(),
            "--limit=25".to_string(),
            "--no-json-mode".to_string(),
        ];

        let parsed = parse_bfcl_predict_llm_args(&args).unwrap();

        assert_eq!(
            parsed.cases,
            PathBuf::from("tests/bfcl/local/ha_home_cases.jsonl")
        );
        assert_eq!(
            parsed.out,
            PathBuf::from("tests/bfcl/local/ha_home_llm_predictions.jsonl")
        );
        assert_eq!(parsed.max_tokens, 96);
        assert_eq!(parsed.limit, Some(25));
        assert!(!parsed.json_mode);
    }

    #[test]
    fn bfcl_llm_prompt_is_compact_and_tool_focused() {
        let cases = vec![genie_core::eval::bfcl::BfclCase {
            id: "home-light".to_string(),
            category: Some("home_control".to_string()),
            source: None,
            prompt: "Sarah: turn on the kitchen light".to_string(),
            expected_tool_calls: vec![genie_core::eval::bfcl::ExpectedToolCall {
                name: "home_control".to_string(),
                arguments: serde_json::json!({
                    "entity": "kitchen light",
                    "action": "turn_on",
                }),
            }],
            allow_extra_arguments: true,
        }];

        let system_prompt = build_bfcl_llm_system_prompt(&cases);
        let messages = build_bfcl_llm_messages(&system_prompt, &cases[0].prompt);

        assert!(system_prompt.contains("4096-token"));
        assert!(system_prompt.contains("home_control"));
        assert!(system_prompt.contains("toggle"));
        assert!(system_prompt.contains("lock"));
        assert!(system_prompt.contains("unlock"));
        assert!(system_prompt.contains(r#""tool":"tool_name""#));
        assert!(system_prompt.len() < 2500);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, "user");
        assert!(messages[1].content.contains("kitchen light"));
    }

    #[test]
    fn bfcl_llm_case_limit_scores_only_selected_cases() {
        let cases = vec![
            genie_core::eval::bfcl::BfclCase {
                id: "case-1".to_string(),
                category: None,
                source: None,
                prompt: "what time is it".to_string(),
                expected_tool_calls: vec![genie_core::eval::bfcl::ExpectedToolCall {
                    name: "get_time".to_string(),
                    arguments: serde_json::json!({}),
                }],
                allow_extra_arguments: false,
            },
            genie_core::eval::bfcl::BfclCase {
                id: "case-2".to_string(),
                category: None,
                source: None,
                prompt: "turn on the kitchen lights".to_string(),
                expected_tool_calls: vec![genie_core::eval::bfcl::ExpectedToolCall {
                    name: "home_control".to_string(),
                    arguments: serde_json::json!({
                        "entity": "kitchen lights",
                        "action": "turn_on",
                    }),
                }],
                allow_extra_arguments: false,
            },
        ];

        let selected = select_bfcl_cases(cases, Some(1));
        let predictions = vec![genie_core::eval::bfcl::BfclPrediction {
            id: "case-1".to_string(),
            response: r#"{"tool":"get_time","arguments":{}}"#.to_string(),
        }];
        let report = genie_core::eval::bfcl::score_cases(&selected, &predictions);

        assert_eq!(report.total_cases, 1);
        assert_eq!(report.missing_predictions, 0);
        assert_eq!(report.strict_matches, 1);
    }

    #[test]
    fn bfcl_predict_quick_writes_prediction_jsonl() {
        let dir = std::env::temp_dir().join(format!(
            "genie-claw-bfcl-predict-test-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let cases_path = dir.join("cases.jsonl");
        let predictions_path = dir.join("predictions.jsonl");
        std::fs::write(
            &cases_path,
            concat!(
                "{\"id\":\"time-1\",\"prompt\":\"what time is it\",",
                "\"expected_tool_calls\":[{\"name\":\"get_time\",\"arguments\":{}}]}\n"
            ),
        )
        .unwrap();

        let report = cmd_bfcl_predict_quick(&BfclPredictQuickArgs {
            cases: cases_path.clone(),
            out: predictions_path.clone(),
        })
        .unwrap();

        assert_eq!(report.total_cases, 1);
        assert_eq!(report.tool_calls, 1);

        let predictions =
            genie_core::eval::bfcl::load_predictions_jsonl(&predictions_path).unwrap();
        assert_eq!(predictions.len(), 1);
        assert_eq!(predictions[0].id, "time-1");

        let cases = genie_core::eval::bfcl::load_cases_jsonl(&cases_path).unwrap();
        let score = genie_core::eval::bfcl::score_cases(&cases, &predictions);
        assert_eq!(score.strict_matches, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "voice")]
    #[test]
    fn parse_speaker_options_supports_recording_flags() {
        let args = vec![
            "--profile-dir".to_string(),
            "/tmp/speakers".to_string(),
            "--min-score".to_string(),
            "0.91".to_string(),
            "--device".to_string(),
            "plughw:2,0".to_string(),
            "--sample-rate".to_string(),
            "16000".to_string(),
            "--duration".to_string(),
            "7".to_string(),
        ];

        let parsed = parse_speaker_options(&args).unwrap();

        assert_eq!(parsed.profile_dir, PathBuf::from("/tmp/speakers"));
        assert!((parsed.min_score - 0.91).abs() < f32::EPSILON);
        assert_eq!(parsed.device, "plughw:2,0");
        assert_eq!(parsed.sample_rate, 16_000);
        assert_eq!(parsed.duration_secs, 7);
    }

    #[cfg(feature = "voice")]
    #[test]
    fn parse_speaker_options_rejects_unknown_flag() {
        let args = vec!["--bad".to_string()];

        assert!(parse_speaker_options(&args).is_err());
    }

    #[test]
    fn support_bundle_default_path_is_json_under_tmp() {
        let path = default_support_bundle_path();
        assert!(path.starts_with("/tmp"));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("json"));
    }

    #[test]
    fn llm_service_label_includes_backend_when_known() {
        assert_eq!(
            llm_service_label(Some("genie-ai-runtime")),
            "LLM (genie-ai-runtime)"
        );
        assert_eq!(llm_service_label(None), "LLM");
    }

    #[test]
    fn systemd_unit_file_path_normalizes_unit_name() {
        assert_eq!(
            systemd_unit_file_path("genie-ai-runtime"),
            "/etc/systemd/system/genie-ai-runtime.service"
        );
        assert_eq!(
            systemd_unit_file_path("genie-llm.service"),
            "/etc/systemd/system/genie-llm.service"
        );
    }

    #[test]
    fn tail_jsonl_file_returns_recent_valid_events_in_original_order() {
        let path = std::env::temp_dir().join(format!(
            "geniepod-tail-jsonl-test-{}.jsonl",
            std::process::id()
        ));
        std::fs::write(
            &path,
            concat!("{\"n\":1}\n", "not json\n", "{\"n\":2}\n", "{\"n\":3}\n"),
        )
        .unwrap();

        let items = tail_jsonl_file(&path, 2);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["n"], 2);
        assert_eq!(items[1]["n"], 3);
    }

    #[test]
    fn install_and_list_skill() {
        let skills_dir = temp_skills_dir();
        let sample_skill = sample_skill_path();

        let (installed, _) = install_skill(sample_skill, &skills_dir, Some("hello")).unwrap();
        assert_eq!(installed.name, "hello_world");
        assert_eq!(
            installed.path.file_name().unwrap().to_string_lossy(),
            "hello.so"
        );

        let installed_skills = load_installed_skills(&skills_dir).unwrap();
        assert_eq!(installed_skills.len(), 1);
        assert_eq!(installed_skills[0].name, "hello_world");
        assert!(installed_skills[0].description.contains("greeting"));
        assert_eq!(installed_skills[0].manifest.status, "missing");
    }

    #[test]
    fn install_copies_and_remove_deletes_skill_manifest() {
        let source_dir = temp_skills_dir();
        let skills_dir = temp_skills_dir();
        let sample_skill = sample_skill_path();
        let source_path = source_dir.join(sample_skill.file_name().unwrap());
        std::fs::copy(sample_skill, &source_path).unwrap();
        std::fs::write(
            source_path.with_extension("skill.json"),
            r#"{
                "name": "hello_world",
                "version": "0.1.0",
                "description": "Sample hello skill",
                "permissions": ["speech.output"],
                "capabilities": ["demo.greeting"],
                "reviewed_by": "test",
                "signature": "test-signature"
            }"#,
        )
        .unwrap();

        let (installed, _) = install_skill(&source_path, &skills_dir, Some("hello")).unwrap();
        assert_eq!(installed.manifest.status, "ok");

        let dest_manifest = skills_dir.join("hello.skill.json");
        assert!(dest_manifest.exists());

        let installed_skills = load_installed_skills(&skills_dir).unwrap();
        assert_eq!(
            installed_skills[0].manifest.permissions,
            vec!["speech.output"]
        );
        // A bare "signature" string is not "signed": `signed` is set only by
        // cryptographic verification against a trusted key, not by presence.
        assert!(!installed_skills[0].manifest.signed);

        let removed = remove_skill("hello_world", &skills_dir).unwrap();
        assert_eq!(removed.file_name().unwrap().to_string_lossy(), "hello.so");
        assert!(!dest_manifest.exists());
    }

    #[test]
    fn remove_skill_by_name() {
        let skills_dir = temp_skills_dir();
        let sample_skill = sample_skill_path();
        let _ = install_skill(sample_skill, &skills_dir, Some("hello")).unwrap();

        let removed = remove_skill("hello_world", &skills_dir).unwrap();
        assert_eq!(removed.file_name().unwrap().to_string_lossy(), "hello.so");
        assert!(load_installed_skills(&skills_dir).unwrap().is_empty());
    }

    #[test]
    fn format_agent_harness_status_reports_pass() {
        let harness = serde_json::json!({
            "pass": true,
            "checks": [
                {"name": "memory_hydration_budget", "pass": true, "detail": "ok"}
            ]
        });

        assert_eq!(
            format_agent_harness_status(&harness).as_deref(),
            Some("pass")
        );
    }

    #[test]
    fn format_agent_harness_status_lists_failed_checks() {
        let harness = serde_json::json!({
            "pass": false,
            "checks": [
                {"name": "memory_hydration_budget", "pass": false, "detail": "over budget"},
                {"name": "tool_manifest_budget", "pass": true, "detail": "ok"}
            ]
        });

        assert_eq!(
            format_agent_harness_status(&harness).as_deref(),
            Some("FAIL (memory_hydration_budget)")
        );
    }

    #[test]
    fn format_agent_harness_health_line_reports_pass() {
        let harness = serde_json::json!({
            "pass": true,
            "checks": [
                {"name": "memory_hydration_budget", "pass": true, "detail": "ok"}
            ]
        });

        assert_eq!(
            format_agent_harness_health_line(&harness).as_deref(),
            Some("  [OK]   harness")
        );
    }

    #[test]
    fn format_agent_harness_health_line_lists_failed_checks() {
        let harness = serde_json::json!({
            "pass": false,
            "checks": [
                {"name": "memory_hydration_budget", "pass": false, "detail": "over budget"}
            ]
        });

        assert_eq!(
            format_agent_harness_health_line(&harness).as_deref(),
            Some("  [FAIL] harness (memory_hydration_budget)")
        );
    }
}
