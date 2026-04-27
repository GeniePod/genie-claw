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
//!   genie-ctl skill ...       Manage loadable skill modules
//!   genie-ctl speaker ...     Manage local speaker identity profiles
//!   genie-ctl health          Check service health
//!   genie-ctl connectivity    Inspect the ESP32-C6 connectivity sidecar
//!   genie-ctl conversations   List all conversations
//!   genie-ctl support-bundle [PATH]
//!                              Write a local diagnostics support bundle
//!   genie-ctl version         Show version info

use anyhow::Result;
use genie_common::config::Config;
use genie_core::skills::{
    SkillLoader, SkillManifestAudit, find_manifest_sidecar, manifest_sidecar_candidates,
    skills_dir as runtime_skills_dir,
};
use genie_core::voice::identity::{
    enroll_speaker_file, identify_speaker_file, list_speaker_profiles, remove_speaker_profile,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const CORE_URL: &str = "127.0.0.1:3000";
const GOVERNOR_SOCK: &str = "/run/geniepod/governor.sock";
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
        "connectivity" | "radio" => cmd_connectivity().await?,
        "skill" | "skills" => {
            if args.len() < 3 {
                print_skill_usage();
                std::process::exit(1);
            }
            cmd_skill(&args[2..])?;
        }
        "speaker" | "speakers" => {
            if args.len() < 3 {
                print_speaker_usage();
                std::process::exit(1);
            }
            cmd_speaker(&args[2..])?;
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
    println!(
        "\
GeniePod CLI v{}

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
    connectivity        Inspect ESP32-C6 Thread/Matter sidecar status
    skill <SUBCOMMAND>  Manage loadable skill modules
    speaker <SUBCOMMAND>
                        Manage local speaker identity profiles
    health              Service health check
    conversations       List all conversations
    update-check        Check for OTA updates
    diag                Full system diagnostics report
    support-bundle [P]  Write JSON diagnostics bundle to path P
    version             Show version info
    help                Show this help",
        env!("CARGO_PKG_VERSION")
    );
}

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
    println!("  core: {}", CORE_URL);
    println!("  governor: {}", GOVERNOR_SOCK);
}

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

#[derive(Debug, Clone)]
struct SpeakerCliOptions {
    profile_dir: PathBuf,
    min_score: f32,
    device: String,
    sample_rate: u32,
    duration_secs: u32,
}

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

fn cmd_speaker_remove(name: &str, profile_dir: &Path) -> Result<()> {
    let removed = remove_speaker_profile(profile_dir, name)?;
    println!("Removed speaker profile {}", removed.display());
    Ok(())
}

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
            println!(
                "    reviewed: {}; signed: {}",
                reviewer, skill.manifest.signed
            );
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
    match http_get(CORE_URL, "/api/health").await {
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
    let body = serde_json::json!({"message": message}).to_string();
    let response = http_post_with_origin(CORE_URL, "/api/chat", &body, "api").await?;
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

fn parse_search_limit(value: &str) -> Result<u64> {
    let limit = value
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid --limit value: {}", value))?;
    if !(1..=5).contains(&limit) {
        anyhow::bail!("--limit must be between 1 and 5");
    }
    Ok(limit)
}

async fn cmd_search(query: &str, fresh: bool, limit: u64) -> Result<()> {
    let query = query.trim();
    if query.is_empty() {
        anyhow::bail!("Usage: genie-ctl search [--fresh] [--limit N] <query>");
    }

    let body = serde_json::json!({"query": query, "fresh": fresh, "limit": limit}).to_string();
    let response = http_post(CORE_URL, "/api/web-search", &body).await?;
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

async fn cmd_history() -> Result<()> {
    let body = http_get(CORE_URL, "/api/chat/history").await?;
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
    let body = http_get(CORE_URL, "/api/tools").await?;
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
    let body = http_get(CORE_URL, "/api/connectivity").await?;
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
    // Check each service.
    let services = [
        ("genie-core", CORE_URL, "/api/health"),
        ("llama.cpp", "127.0.0.1:8080", "/health"),
        ("Home Assistant", "127.0.0.1:8123", "/api/"),
        ("genie-api", "127.0.0.1:3080", "/api/status"),
    ];

    for (name, addr, path) in &services {
        match http_get(addr, path).await {
            Ok(_) => println!("  [OK]   {}", name),
            Err(_) => println!("  [DOWN] {}", name),
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
    let body = http_get(CORE_URL, "/api/conversations").await?;
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
    println!("=== GeniePod Diagnostics ===\n");

    // Version.
    println!("[Version]");
    println!("  genie-ctl: v{}", env!("CARGO_PKG_VERSION"));

    // Core health.
    println!("\n[Services]");
    let services = [
        ("genie-core", CORE_URL, "/api/health"),
        ("llama.cpp", "127.0.0.1:8080", "/health"),
        ("genie-api", "127.0.0.1:3080", "/api/status"),
        ("Home Assistant", "127.0.0.1:8123", "/api/"),
    ];
    for (name, addr, path) in &services {
        let status = match http_get(addr, path).await {
            Ok(_) => "UP",
            Err(_) => "DOWN",
        };
        println!("  {:20} {}", name, status);
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
    if let Ok(body) = http_get(CORE_URL, "/api/health").await
        && let Ok(data) = serde_json::from_str::<serde_json::Value>(&body)
    {
        println!("\n[Core]");
        if let Some(v) = data.get("version").and_then(|v| v.as_str()) {
            println!("  Version:       v{}", v);
        }
        if let Some(v) = data.get("llm").and_then(|v| v.as_str()) {
            println!("  LLM:           {}", v);
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
    let services = [
        ("genie-core", CORE_URL, "/api/health"),
        ("llama.cpp", "127.0.0.1:8080", "/health"),
        ("genie-api", "127.0.0.1:3080", "/api/status"),
        ("Home Assistant", "127.0.0.1:8123", "/api/"),
    ];

    let mut service_status = Vec::new();
    for (name, addr, path) in &services {
        service_status.push(serde_json::json!({
            "service": name,
            "addr": addr,
            "path": path,
            "reachable": http_get(addr, path).await.is_ok(),
        }));
    }

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
            "health": http_json_value(CORE_URL, "/api/health").await,
            "runtime_contract": http_json_value(CORE_URL, "/api/runtime/contract").await,
            "connectivity": http_json_value(CORE_URL, "/api/connectivity").await,
        },
        "security": http_json_value("127.0.0.1:3080", "/api/security").await,
        "actuation": {
            "pending": http_json_value(CORE_URL, "/api/actuation/pending").await,
            "actions": http_json_value(CORE_URL, "/api/actuation/actions").await,
            "audit": http_json_value("127.0.0.1:3080", "/api/actuation/audit").await,
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
    [
        "/etc/geniepod/geniepod.toml",
        "/etc/geniepod/mosquitto.conf",
        "/etc/systemd/system/genie-core.service",
        "/etc/systemd/system/genie-llm.service",
    ]
    .into_iter()
    .map(|path| {
        serde_json::json!({
            "path": path,
            "present": Path::new(path).exists(),
        })
    })
    .collect()
}

fn binary_inventory() -> Vec<serde_json::Value> {
    [
        "genie-core",
        "genie-ctl",
        "genie-governor",
        "genie-health",
        "genie-api",
        "llama-server",
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
    use std::process::Command;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        assert!(installed_skills[0].manifest.signed);

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
}
