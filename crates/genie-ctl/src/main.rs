//! GeniePod CLI — manage your GeniePod device from the terminal.
//!
//! Usage:
//!   genie-ctl status          Show system status (governor mode, memory, services)
//!   genie-ctl mode <MODE>     Change governor mode (day, night_a, night_b, media)
//!   genie-ctl chat <MESSAGE>  Send a chat message and print the response
//!   genie-ctl history         Show conversation history
//!   genie-ctl tools           List available tools
//!   genie-ctl skill ...       Manage loadable skill modules
//!   genie-ctl health          Check service health
//!   genie-ctl conversations   List all conversations
//!   genie-ctl version         Show version info

use anyhow::Result;
use genie_core::skills::{SkillLoader, skills_dir as runtime_skills_dir};
use std::path::{Path, PathBuf};
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
        "history" => cmd_history().await?,
        "tools" => cmd_tools().await?,
        "skill" | "skills" => {
            if args.len() < 3 {
                print_skill_usage();
                std::process::exit(1);
            }
            cmd_skill(&args[2..])?;
        }
        "health" => cmd_health().await?,
        "conversations" | "convos" => cmd_conversations().await?,
        "update-check" | "update" => cmd_update_check().await?,
        "diag" | "diagnostics" => cmd_diag().await?,
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
    history             Show conversation history
    tools               List available tools
    skill <SUBCOMMAND>  Manage loadable skill modules
    health              Service health check
    conversations       List all conversations
    update-check        Check for OTA updates
    diag                Full system diagnostics report
    version             Show version info
    help                Show this help",
        env!("CARGO_PKG_VERSION")
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
    let skill = validate_skill_file(source)?;
    std::fs::create_dir_all(skills_dir)?;

    let file_name = normalize_skill_filename(source, dest_name)?;
    let dest_path = skills_dir.join(file_name);
    let bytes_copied = std::fs::copy(source, &dest_path)?;

    Ok((
        InstalledSkillInfo {
            path: dest_path,
            ..skill
        },
        bytes_copied,
    ))
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
        std::fs::remove_file(&skill.path)?;
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
            std::fs::remove_file(&candidate)?;
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
    let response = http_post(CORE_URL, "/api/chat", &body).await?;
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
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timeout"))??;

    let (reader, mut writer) = stream.into_split();
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path,
        addr,
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
