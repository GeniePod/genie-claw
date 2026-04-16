# GeniePod Skill Developer Guide

**Version:** 1.0 | **SDK:** genie-skill-sdk 1.0.0-alpha.3
**Target:** GeniePod Home / aarch64 Jetson-class hardware | **Language:** Rust

---

## What is a Skill?

A skill is a **loadable shared library** (`.so`) that adds new tools to GeniePod's voice AI assistant. When loaded, the LLM can call your skill just like built-in tools — through voice commands.

**Example:** You build a `spotify.so` skill. A user says:

> "Play jazz on Spotify"

GeniePod's LLM recognizes the intent, calls your skill's `execute()` function with `{"action":"play","query":"jazz"}`, gets back `"Playing Jazz Essentials"`, and speaks it through the speaker.

### How It Works (The Full Chain)

```
User: "Play jazz on Spotify"
  │
  ▼
Wake phrase or push-to-talk activates GeniePod
  │
  ▼
Microphone records 5 seconds of audio
  │
  ▼
Whisper STT transcribes: "play jazz on Spotify"
  │
  ▼
LLM (Nemotron 4B) sees ALL available tools:
  ├── built-in: get_time, calculate, home_control, ...
  └── loaded skills: spotify_control, calendar_event, ...
  │
  LLM outputs: {"tool":"spotify_control","arguments":{"action":"play","query":"jazz"}}
  │
  ▼
Tool Dispatcher routes to your skill:
  │
  ├── Is "spotify_control" a built-in tool? No.
  ├── Is "spotify_control" a loaded skill? Yes!
  └── Call: skill_vtable.execute('{"action":"play","query":"jazz"}')
  │
  ▼
Your skill's execute() function runs:
  │
  └── Returns: '{"success":true,"output":"Playing Jazz Essentials"}'
  │
  ▼
LLM summarizes: "I'm playing Jazz Essentials on Spotify."
  │
  ▼
Piper TTS speaks through the speaker
```

---

## Quick Start: Your First Skill in 5 Minutes

### Step 1: Create the project

```bash
mkdir my-skill && cd my-skill
cargo init --lib
```

### Step 2: Configure Cargo.toml

```toml
[package]
name = "geniepod-skill-myskill"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]  # IMPORTANT: produces a .so shared library

[dependencies]
genie-skill-sdk = { git = "https://github.com/GeniePod/genie-claw", package = "genie-skill-sdk" }
serde_json = "1"
```

### Step 3: Write your skill (src/lib.rs)

```rust
use genie_skill_sdk::prelude::*;

skill! {
    name: "my_tool",
    description: "Describe what your tool does — the LLM reads this to decide when to use it",
    version: "0.1.0",
    parameters: {
        "query" => "string",
        "count" => "integer"
    },
    execute: |args| {
        let query = args.get_str("query").unwrap_or("default");
        let count = args.get_i64("count").unwrap_or(1);

        // Your logic here — call APIs, read files, compute things
        Ok(format!("Processed '{}' {} time(s)", query, count))
    }
}
```

### Step 4: Build

```bash
# For local testing (same architecture):
cargo build --release
# Output: target/release/libgeniepod_skill_myskill.so

# For Jetson (if cross-compiling from x86):
cargo build --release --target aarch64-unknown-linux-gnu
```

### Step 5: Install on GeniePod

```bash
# Copy the .so to the skills directory:
sudo cp target/release/libgeniepod_skill_myskill.so /opt/geniepod/skills/myskill.so

# Restart genie-core (or it loads on next startup):
sudo systemctl restart genie-core
```

### Step 6: Test

```
> Use my tool with query "hello" and count 3
GeniePod: Processed 'hello' 3 time(s)
```

---

## The skill! Macro — Anatomy

```rust
skill! {
    // REQUIRED: Tool name (what the LLM calls it).
    // Must be snake_case, unique across all tools.
    // Bad:  "My Tool"  (spaces)
    // Bad:  "myTool"   (camelCase)
    // Good: "spotify_control"
    name: "spotify_control",

    // REQUIRED: Human-readable description.
    // The LLM reads this to decide WHEN to use your tool.
    // Be specific about capabilities and trigger phrases.
    description: "Control Spotify playback. Use for: play music, pause, skip, \
                  search tracks, queue songs, get now playing.",

    // REQUIRED: Semantic version.
    version: "0.1.0",

    // REQUIRED: Parameter schema.
    // Uses JSON Schema types: "string", "integer", "number", "boolean"
    // The LLM generates these values from the user's voice command.
    parameters: {
        "action" => "string",    // "play", "pause", "skip", "search"
        "query"  => "string",    // search term or track name
        "volume" => "integer"    // 0-100
    },

    // REQUIRED: Execute function.
    // Receives parsed arguments, returns Result<String, String>.
    // Ok(text) = success, LLM will summarize this for voice
    // Err(text) = failure, LLM will report the error
    execute: |args| {
        let action = args.get_str("action").unwrap_or("play");
        let query = args.get_str("query").unwrap_or("");

        match action {
            "play" => {
                // Call Spotify API, start playback...
                Ok(format!("Playing: {}", query))
            }
            "pause" => {
                Ok("Playback paused.".to_string())
            }
            _ => Err(format!("Unknown action: {}", action))
        }
    }
}
```

---

## What the Macro Generates

The `skill!` macro expands to approximately this code (you don't write this — the macro does it for you):

```rust
// 1. Static vtable — lives for the lifetime of the .so
static VTABLE: LazyLock<SkillVTable> = LazyLock::new(|| {
    SkillVTable {
        abi_version: 1,
        name: "spotify_control\0".as_ptr(),
        description: "Control Spotify...\0".as_ptr(),
        version: "0.1.0\0".as_ptr(),
        parameters_json: "{\"type\":\"object\",...}\0".as_ptr(),
        execute: c_execute,       // C ABI wrapper
        destroy: c_destroy,       // free returned strings
    }
});

// 2. C ABI execute wrapper — handles JSON ↔ Rust conversion
extern "C" fn c_execute(args_json: *const c_char) -> *mut c_char {
    // Parse C string → Rust SkillArgs
    // Call your execute function
    // Wrap in catch_unwind (crash containment)
    // Serialize result to JSON C string
}

// 3. C ABI string destructor
extern "C" fn c_destroy(ptr: *mut c_char) {
    // Free the CString returned by c_execute
}

// 4. Entry point — called by genie-core's dlsym()
#[no_mangle]
pub extern "C" fn genie_skill_init() -> *const SkillVTable {
    &*VTABLE
}
```

---

## SkillArgs API Reference

The `SkillArgs` struct provides safe accessors for JSON parameters:

```rust
execute: |args| {
    // String parameters
    let name: Option<&str> = args.get_str("name");
    let name: &str = args.get_str("name").unwrap_or("default");

    // Integer parameters
    let count: Option<i64> = args.get_i64("count");
    let count: i64 = args.get_i64("count").unwrap_or(1);

    // Float parameters
    let temp: Option<f64> = args.get_f64("temperature");

    // Boolean parameters
    let verbose: Option<bool> = args.get_bool("verbose");

    // Raw JSON value (for complex nested params)
    let raw: Option<&serde_json::Value> = args.get("config");

    // Full args as JSON
    let all: &serde_json::Value = args.as_value();

    Ok("done".to_string())
}
```

---

## How Skills Get Loaded (Internal Flow)

### At Startup

```
genie-core main()
  │
  ├── Load config from /etc/geniepod/geniepod.toml
  ├── Initialize LLM client, memory, tools
  │
  ├── SkillLoader::new("/opt/geniepod/skills/")
  │     │
  │     └── load_all()
  │           │
  │           ├── Read directory: find *.so files
  │           │
  │           ├── For each .so:
  │           │     │
  │           │     ├── dlopen(path)
  │           │     │   └── OS loads the .so into process memory
  │           │     │
  │           │     ├── dlsym("genie_skill_init")
  │           │     │   └── Find the entry point function
  │           │     │
  │           │     ├── Call genie_skill_init()
  │           │     │   └── Returns pointer to SkillVTable
  │           │     │
  │           │     ├── Verify abi_version == 1
  │           │     │
  │           │     ├── Read vtable fields:
  │           │     │   ├── name → "spotify_control"
  │           │     │   ├── description → "Control Spotify..."
  │           │     │   ├── version → "0.1.0"
  │           │     │   └── parameters_json → '{"type":"object",...}'
  │           │     │
  │           │     └── Store as LoadedSkill {vtable, name, lib_handle}
  │           │
  │           └── Return list of loaded skill names
  │
  ├── Register loaded skills in ToolDispatcher
  │     └── LLM now sees: built-in tools + loaded skills
  │
  └── Build system prompt with ALL tool definitions
        └── LLM knows about every available tool
```

### When a Skill Is Called

```
LLM outputs: {"tool":"spotify_control","arguments":{"action":"play","query":"jazz"}}
  │
  ▼
ToolDispatcher::execute()
  │
  ├── Match tool name against built-in tools → not found
  ├── Match tool name against loaded skills → FOUND: spotify_control
  │
  ├── Serialize arguments to JSON: '{"action":"play","query":"jazz"}'
  ├── Convert to C string: CString::new(json)
  │
  ├── std::panic::catch_unwind(|| {
  │     vtable.execute(c_args_ptr)    // Call the skill's C function
  │   })
  │
  ├── If panic: increment fault_count, return error
  │   If fault_count >= 3: auto-unload skill, log warning
  │
  ├── Read result: CStr::from_ptr(result_ptr) → Rust String
  ├── Free C string: vtable.destroy(result_ptr)
  │
  ├── Parse JSON result:
  │   {"success": true, "output": "Playing Jazz Essentials"}
  │
  └── Return ToolResult { tool: "spotify_control", success: true, output: "..." }
        │
        ▼
  LLM summarizes for voice: "I'm playing Jazz Essentials on Spotify."
```

---

## Memory Layout: How .so Loading Works

```
Process Memory (genie-core)
┌─────────────────────────────────────────────┐
│ genie-core binary (~10 MB)                │
│   .text: compiled Rust code                  │
│   .data: static data, config                 │
│   .bss:  zero-initialized globals            │
│   heap:  Memory DB, conversations, LLM state │
├─────────────────────────────────────────────┤
│ libloading → dlopen("spotify.so")            │
│                                              │
│ spotify.so (~330 KB)                         │
│   .text: skill's compiled execute() code     │
│   .data: vtable, string constants            │
│   .dynamic: symbol table (genie_skill_init)│
│                                              │
│ calendar.so (~200 KB)                        │
│   .text: skill's compiled code               │
│   .data: vtable, string constants            │
│                                              │
├─────────────────────────────────────────────┤
│ Shared libs: libc, libm, libdl, etc.        │
└─────────────────────────────────────────────┘

Key: Skills share the SAME address space as genie-core.
     No IPC. No serialization overhead. Direct function calls.
     Like Linux kernel modules sharing the kernel's address space.
```

---

## Security Model

### What Skills CAN Do

- Read/write files in their own data dir: `/opt/geniepod/skills/<name>/data/`
- Make network requests (HTTP, TCP, etc.)
- Use the Rust standard library
- Allocate heap memory
- Call any safe Rust code

### What Skills CANNOT Do

- Access other skills' data directories
- Read system config files (`/etc/geniepod/geniepod.toml`)
- Access home directories (`/home/*`)
- Read SSH keys, cloud credentials, API tokens
- Access `/proc`, `/sys` (restricted by Landlock)
- See sensitive environment variables (sanitized before load)
- Crash genie-core (catch_unwind + fault counting)

### Defense in Depth (5 Layers)

```
Layer 1: Landlock filesystem sandbox
         → Skill can only access its own /data/ directory

Layer 2: Environment sanitization
         → 60+ sensitive vars blocked (API keys, passwords, cloud creds)

Layer 3: catch_unwind crash containment
         → Panics don't crash core, skill gets fault count

Layer 4: Auto-unload after 3 faults
         → Misbehaving skills are automatically removed

Layer 5: Signature verification (optional)
         → Unsigned skills trigger taint warning
```

### Trust Levels

| Level | Source | Indicator | Access |
|-------|--------|-----------|--------|
| **Built-in** | Compiled into genie-core | `BUILT-IN` | Full core access |
| **Trusted** | Skill store, signed by GeniePod team | `TRUSTED` | Sandboxed, verified |
| **Community** | Skill store, community author | `COMMUNITY` | Sandboxed, hash-verified |
| **Tainted** | Local file, unsigned | `TAINTED` | Sandboxed, user-acknowledged |

---

## Skill Lifecycle Diagram

```
┌──────────┐     ┌──────────┐     ┌──────────┐     ┌──────────┐
│  DEVELOP  │────▶│  BUILD   │────▶│ INSTALL  │────▶│  LOADED  │
│           │     │          │     │          │     │          │
│ Write     │     │ cargo    │     │ ctl skill│     │ dlopen   │
│ Rust code │     │ build    │     │ install  │     │ vtable   │
│ using SDK │     │ --release│     │          │     │ registered│
└──────────┘     └──────────┘     └──────────┘     └────┬─────┘
                                                        │
                                              ┌─────────┴─────────┐
                                              │                   │
                                        ┌─────▼─────┐     ┌──────▼─────┐
                                        │ EXECUTING  │     │  FAULTED   │
                                        │            │     │            │
                                        │ LLM calls  │     │ panic ×3   │
                                        │ execute()  │     │ auto-unload│
                                        └────────────┘     └────────────┘
                                              │
                                        ┌─────▼─────┐
                                        │ UNLOADED   │
                                        │            │
                                        │ ctl skill  │
                                        │ remove     │
                                        │ dlclose    │
                                        └────────────┘
```

---

## Best Practices

### 1. Write Good Descriptions

The description is how the LLM decides to use your tool. Be specific:

```rust
// BAD: Too vague — LLM won't know when to use it
description: "A useful tool"

// BAD: Too long — wastes context tokens
description: "This tool allows you to control Spotify music playback including
              playing, pausing, skipping tracks, searching for music, creating
              playlists, managing queue, adjusting volume, and more..."

// GOOD: Specific triggers + capabilities
description: "Control Spotify playback. Use for: play music, pause, skip track, \
              search songs, get now playing info."
```

### 2. Handle Missing Parameters Gracefully

```rust
execute: |args| {
    // DON'T panic on missing args:
    // let name = args.get_str("name").unwrap();  // BAD: panics!

    // DO provide defaults:
    let name = args.get_str("name").unwrap_or("world");

    // DO validate and return errors:
    let action = args.get_str("action")
        .ok_or_else(|| "Missing required parameter: action".to_string())?;

    Ok(format!("Action: {}, Name: {}", action, name))
}
```

### 3. Keep Skills Small

GeniePod runs on 8 GB RAM. Every MB counts.

- Target: < 500 KB per `.so`
- Avoid heavy dependencies (no `reqwest`, use raw TCP if needed)
- Don't embed large data files in the binary
- Use `/opt/geniepod/skills/<name>/data/` for persistent storage

### 4. Return Clear Output

The LLM will summarize your output for voice. Make it human-readable:

```rust
// BAD: Raw JSON — LLM has to parse it
Ok(r#"{"status":200,"tracks":[{"name":"Jazz"}]}"#.to_string())

// GOOD: Human-readable text
Ok("Now playing: Jazz Essentials by Various Artists".to_string())

// GOOD: List format for multiple items
Ok("Found 3 tracks:\n1. Take Five - Dave Brubeck\n2. So What - Miles Davis\n3. Blue Train - John Coltrane".to_string())
```

### 5. Test Locally Before Deploying

```bash
# Build the .so
cargo build --release

# Verify the symbol is exported
nm -D target/release/lib*.so | grep genie_skill_init
# Should show: T genie_skill_init

# Check file size
ls -lh target/release/lib*.so
# Should be < 500 KB for simple skills

# The hello-world skill is a good reference:
# software/skills/hello-world/src/lib.rs
```

---

## Publishing to the Skill Store

### 1. Build for aarch64

```bash
# If building on the Jetson directly:
cargo build --release

# If cross-compiling from x86:
cargo build --release --target aarch64-unknown-linux-gnu
```

### 2. Generate SHA256 hash

```bash
sha256sum target/release/libgeniepod_skill_myskill.so
# a1b2c3d4e5f6... libgeniepod_skill_myskill.so
```

### 3. Create a GitHub Release

Upload the `.so` file as a release asset with the naming convention:

```
<skill-name>-<version>-aarch64.so
```

Example: `spotify-0.1.0-aarch64.so`

### 4. Submit to registry

Open a PR to add your skill to the skill registry:

```toml
[[skills]]
name = "my_awesome_skill"
description = "Does something amazing"
version = "0.1.0"
author = "your-github-username"
license = "AGPL-3.0-only"
size_bytes = 345000
sha256 = "a1b2c3d4..."
url = "https://github.com/you/my-skill/releases/download/v0.1.0/myskill-0.1.0-aarch64.so"
signed = false
```

---

## Troubleshooting

### "symbol not found: genie_skill_init"

Your `Cargo.toml` is missing `crate-type = ["cdylib"]`:
```toml
[lib]
crate-type = ["cdylib"]
```

### "ABI version mismatch"

Your SDK version doesn't match the core. Update:
```bash
cargo update -p genie-skill-sdk
cargo build --release
```

### Skill loads but LLM never calls it

Your `description` doesn't match user intent. Test with explicit phrasing:
```
> Use the my_tool tool with query "test"
```

If that works, improve the description to match natural language.

### Skill panics and gets unloaded

Check your code for `unwrap()` on `None` or division by zero. Use `unwrap_or()` and error handling:
```rust
execute: |args| {
    let x = args.get_i64("x").unwrap_or(0);
    if x == 0 {
        return Err("x cannot be zero".to_string());
    }
    Ok(format!("Result: {}", 100 / x))
}
```

---

## Example Skills

### Weather Skill (Network Access)

```rust
use genie_skill_sdk::prelude::*;

skill! {
    name: "weather_detailed",
    description: "Get detailed weather with humidity, wind, and UV index for any city.",
    version: "0.1.0",
    parameters: {
        "city" => "string"
    },
    execute: |args| {
        let city = args.get_str("city").unwrap_or("Denver");

        // Simple HTTP request using std::net (no heavy deps)
        let response = std::process::Command::new("curl")
            .args(["-sf", &format!("https://wttr.in/{}?format=j1", city)])
            .output()
            .map_err(|e| format!("HTTP error: {}", e))?;

        if !response.status.success() {
            return Err(format!("Weather API error for '{}'", city));
        }

        let body = String::from_utf8_lossy(&response.stdout);
        // Parse and format...
        Ok(format!("Weather in {}: {}", city, body.chars().take(200).collect::<String>()))
    }
}
```

### File Reader Skill (Sandboxed)

```rust
use genie_skill_sdk::prelude::*;

skill! {
    name: "read_note",
    description: "Read a note from the user's notes directory.",
    version: "0.1.0",
    parameters: {
        "filename" => "string"
    },
    execute: |args| {
        let filename = args.get_str("filename").unwrap_or("readme.txt");

        // Sandboxed: can only read from skill's data dir
        let path = format!("/opt/geniepod/skills/read_note/data/{}", filename);

        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Cannot read '{}': {}", filename, e))?;

        Ok(format!("Contents of {}:\n{}", filename, content))
    }
}
```
