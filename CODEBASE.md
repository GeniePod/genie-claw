# GenieClaw Codebase Guide

This document is the file-level companion to `README.md`, `GETTING_STARTED.md`, and `ARCHITECTURE.md`.

Use it when you need to answer questions like:

- Where does a user chat turn enter the system?
- Which file owns Home Assistant resolution versus tool routing?
- Which crate is responsible for service health, mode switching, or deploy setup?
- If I need to change a behavior, where should I start reading?

It is intentionally practical. It describes what each major file owns today so the codebase is easier to navigate and extend.

## How To Read This Repo

The repository is organized around a small Rust workspace plus deployment assets:

- `crates/`
  Core binaries and libraries.
- `deploy/`
  Jetson setup, config templates, helper scripts, and systemd units.
- `skills/`
  Native loadable skill example plus authoring docs.
- top-level markdown files
  Product overview, getting started, roadmap, and architecture notes.

At runtime, the system is split across several processes:

1. `genie-core`
   The user-facing local AI runtime.
2. `genie-governor`
   The service and memory pressure controller.
3. `genie-health`
   The local service monitor.
4. `genie-api`
   The lightweight system dashboard API.
5. `genie-ctl`
   The CLI used by developers and operators.

## Runtime Flow

The most important runtime path is:

1. A request enters through the web UI, CLI, REPL, voice loop, or Telegram adapter.
2. `genie-core` builds prompt context from conversation history and household memory.
3. The LLM client talks to the local `llama.cpp`-compatible server.
4. If the model emits a tool call, the tool parser and dispatcher execute the tool.
5. Tool results may be returned directly or summarized, depending on the tool.
6. Conversation state and extracted memory are persisted to SQLite.

Around that path:

- `genie-governor` manages modes, memory pressure, and service lifecycle.
- `genie-health` polls local service endpoints and logs failures.
- `genie-api` exposes dashboard-friendly system endpoints.

## Top-Level Files

| Path | Purpose |
| --- | --- |
| `Cargo.toml` | Workspace definition and shared dependency versions. |
| `Cargo.lock` | Locked dependency graph. |
| `Makefile` | Main developer and Jetson deploy entry points: build, test, release, cross-compile, and deploy. |
| `Dockerfile` | Multi-stage container build for the local dev/runtime image. |
| `docker-compose.dev.yml` | Dev stack for `genie-core`, `genie-api`, and a local `llama.cpp` server. |
| `README.md` | Product-level overview and repo orientation. |
| `GETTING_STARTED.md` | Local dev, Docker, and Jetson bring-up guide. |
| `ARCHITECTURE.md` | Higher-level system architecture narrative. |
| `ROADMAP.md` | Product and engineering roadmap. |
| `.gitignore` | Ignored local build, deploy, and developer-only files. |
| `.dockerignore` | Docker build context exclusions. |

## Workspace Crates

### `crates/genie-common`

Shared types and parsers used across the workspace.

| Path | Purpose |
| --- | --- |
| `crates/genie-common/src/lib.rs` | Public exports for shared config, mode, and tegrastats helpers. |
| `crates/genie-common/src/config.rs` | The main TOML config model used by all services. Includes defaults, environment handling, and typed service settings. |
| `crates/genie-common/src/mode.rs` | `Mode` enum and mode-specific service/model behavior used by the governor. |
| `crates/genie-common/src/tegrastats.rs` | Parser for Jetson `tegrastats` output plus memory helpers based on `/proc/meminfo`. |

### `crates/genie-core`

The primary runtime. This is where most product behavior lives.

#### Entrypoints and Orchestration

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/main.rs` | Binary entrypoint. Loads config, builds core components, chooses HTTP, REPL, or voice mode, and optionally starts Telegram. |
| `crates/genie-core/src/lib.rs` | Public crate surface and module exports. Useful if another Rust program wants to embed GenieClaw components. |
| `crates/genie-core/src/server.rs` | Local HTTP server for chat, history, tools, health, OpenAI-compatible chat, and streaming responses. This is the main daemon-mode request path. |
| `crates/genie-core/src/repl.rs` | Terminal REPL for interactive local testing. |
| `crates/genie-core/src/voice_loop.rs` | Top-level voice interaction loop: wake path, STT, LLM turn, tool handling, and TTS playback coordination. |
| `crates/genie-core/src/context.rs` | Conversation window trimming and summary-based context compression. |
| `crates/genie-core/src/conversation.rs` | Persistent multi-conversation SQLite store for user and assistant turns. |
| `crates/genie-core/src/prompt.rs` | Model-family-aware system prompt builder. This is where tool instructions and behavior framing are defined. |
| `crates/genie-core/src/reasoning.rs` | Per-model think/no-think routing and lightweight prompt complexity heuristics. |

#### LLM Integration

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/llm/mod.rs` | LLM module exports. |
| `crates/genie-core/src/llm/client.rs` | Raw TCP/OpenAI-compatible chat client for the local inference server, including streaming and compatibility fallbacks. |
| `crates/genie-core/src/llm/retry.rs` | Retry and graceful fallback layer around the base client. |

#### Home Assistant Integration

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/ha/mod.rs` | Home Assistant module exports and provider construction helpers. |
| `crates/genie-core/src/ha/client.rs` | Low-level Home Assistant REST client. Handles URL parsing, auth headers, and state fetches. |
| `crates/genie-core/src/ha/provider.rs` | The provider boundary used by the rest of the runtime. Owns graph caching, entity resolution, scene/device listing, action execution, and HA health reporting. |

#### Tool System

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/tools/mod.rs` | Tool module exports and shared parser entry points. |
| `crates/genie-core/src/tools/dispatch.rs` | Tool registry plus execution router. This is where tool schemas are exposed to the LLM and where tool calls are executed. |
| `crates/genie-core/src/tools/parser.rs` | Extracts structured tool JSON from model output, including markdown-wrapped and embedded forms. |
| `crates/genie-core/src/tools/home.rs` | Home Assistant-specific tool behavior for control and status. |
| `crates/genie-core/src/tools/system.rs` | System status tool behavior: memory, uptime, governor mode, load, and Home Assistant connectivity. |
| `crates/genie-core/src/tools/weather.rs` | Weather and forecast integration via Open-Meteo. |
| `crates/genie-core/src/tools/calc.rs` | Small math parser/evaluator used by the `calculate` tool. |
| `crates/genie-core/src/tools/timer.rs` | In-memory countdown timer manager. |

#### Memory and User Profile

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/memory/mod.rs` | Main SQLite-backed memory store with FTS and memory lifecycle operations. |
| `crates/genie-core/src/memory/extract.rs` | Heuristics that extract durable facts from user text. |
| `crates/genie-core/src/memory/inject.rs` | Builds query-relevant memory context to feed into prompts. |
| `crates/genie-core/src/memory/decay.rs` | Temporal scoring and decay math for memory relevance. |
| `crates/genie-core/src/memory/recall.rs` | Recall scoring and consolidation logic used by the memory system. |
| `crates/genie-core/src/profile/mod.rs` | Profile module exports and orchestration for loading structured user profile data. |
| `crates/genie-core/src/profile/ingest.rs` | Ingests profile facts from markdown, text, and other documents into memory. |
| `crates/genie-core/src/profile/toml_profile.rs` | Loads structured profile data from TOML into memory. |

#### Security and Guardrails

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/security/mod.rs` | Security module exports. |
| `crates/genie-core/src/security/audit.rs` | Startup checks for dangerous filesystem or config conditions. |
| `crates/genie-core/src/security/credentials.rs` | Secret registration and safe credential injection for outbound calls. |
| `crates/genie-core/src/security/env_sanitize.rs` | Filters sensitive environment variables out of subprocess execution. |
| `crates/genie-core/src/security/injection.rs` | Input-side prompt injection and exfiltration pattern detection. |
| `crates/genie-core/src/security/loop_guard.rs` | Prevents repeated or ping-pong tool call loops. |
| `crates/genie-core/src/security/sandbox.rs` | Local output sanitization, inference URL validation, and OS-level sandbox helpers. |
| `crates/genie-core/src/security/taint.rs` | Taint propagation and sink checks for sensitive data flow. |

#### Voice Stack

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/voice/mod.rs` | Voice module exports. |
| `crates/genie-core/src/voice/stt.rs` | Speech-to-text engine integration, including CLI/server invocation and WAV generation. |
| `crates/genie-core/src/voice/tts.rs` | Text-to-speech engine integration and playback helpers. |
| `crates/genie-core/src/voice/pipeline.rs` | Higher-level orchestration for a full voice turn. |
| `crates/genie-core/src/voice/format.rs` | Cleans assistant output into speech-friendly text. |
| `crates/genie-core/src/voice/aec.rs` | Acoustic echo cancellation helpers. |
| `crates/genie-core/src/voice/dsp.rs` | Audio DSP helpers such as AGC and soft limiting. |
| `crates/genie-core/src/voice/noise.rs` | Noise suppression and related preprocessing helpers. |
| `crates/genie-core/src/voice/streaming.rs` | Splits and stages streaming text for voice playback. |
| `crates/genie-core/src/voice/vad.rs` | Voice activity detection helpers. |

#### Skills, OTA, and Channel Adapters

| Path | Purpose |
| --- | --- |
| `crates/genie-core/src/skills/mod.rs` | Skill module exports and runtime skill directory helpers. |
| `crates/genie-core/src/skills/loader.rs` | Shared-library skill loader, metadata validation, and fault handling. |
| `crates/genie-core/src/ota/mod.rs` | OTA update check and version handling logic. |
| `crates/genie-core/src/telegram.rs` | Telegram channel adapter that routes Telegram chats through the same local chat pipeline. |

#### Integration Tests

| Path | Purpose |
| --- | --- |
| `crates/genie-core/tests/tool_dispatch_test.rs` | Cross-module checks around tool exposure, config parsing, and binary shape. |
| `crates/genie-core/tests/tools_test.rs` | End-to-end tool parser and dispatcher integration checks. |

### `crates/genie-api`

The lightweight local dashboard service.

| Path | Purpose |
| --- | --- |
| `crates/genie-api/src/main.rs` | API process entrypoint. |
| `crates/genie-api/src/http.rs` | Minimal HTTP server implementation. |
| `crates/genie-api/src/routes.rs` | Dashboard routes for status, tegrastats history, services, mode changes, and static asset serving. |

### `crates/genie-ctl`

The operator and developer CLI.

| Path | Purpose |
| --- | --- |
| `crates/genie-ctl/src/main.rs` | Command parsing and implementations for status, mode, chat, tools, health, conversations, updates, diagnostics, and skill management. |

### `crates/genie-governor`

The memory and mode controller.

| Path | Purpose |
| --- | --- |
| `crates/genie-governor/src/main.rs` | Governor process entrypoint. |
| `crates/genie-governor/src/governor.rs` | Core loop for mode selection, transitions, pressure handling, and interaction with systemd-managed services. |
| `crates/genie-governor/src/control.rs` | Unix socket control plane used by the core runtime, CLI, and dashboard. |
| `crates/genie-governor/src/service_ctl.rs` | Service lifecycle operations such as `systemctl` calls, zram enablement, and LLM model swaps. |
| `crates/genie-governor/src/store.rs` | SQLite persistence for tegrastats samples and mode transition history. |
| `crates/genie-governor/src/tegra_reader.rs` | `tegrastats` subprocess reader and snapshot broadcast logic. |

### `crates/genie-health`

The local service monitor.

| Path | Purpose |
| --- | --- |
| `crates/genie-health/src/main.rs` | Health monitor entrypoint. |
| `crates/genie-health/src/checker.rs` | Endpoint polling, SQLite logging, alert deduplication, and optional local webhook forwarding. |

### `crates/genie-skill-sdk`

The ABI and macro surface for native loadable skills.

| Path | Purpose |
| --- | --- |
| `crates/genie-skill-sdk/src/lib.rs` | Public skill macro, exports, and ABI versioning. |
| `crates/genie-skill-sdk/src/args.rs` | Small helper API for reading structured arguments inside a skill. |
| `crates/genie-skill-sdk/src/result.rs` | Result type used by skills. |
| `crates/genie-skill-sdk/src/vtable.rs` | ABI-level skill vtable definitions used across the dynamic loading boundary. |

### `skills/hello-world`

Reference implementation of a native skill.

| Path | Purpose |
| --- | --- |
| `skills/hello-world/Cargo.toml` | Declares the sample skill as a `cdylib`. |
| `skills/hello-world/src/lib.rs` | Minimal example skill using the SDK macro. |
| `skills/SKILL-DEVELOPER-GUIDE.md` | Human-facing guide for building and installing new skills. |

## UI Assets

| Path | Purpose |
| --- | --- |
| `crates/dashboard/index.html` | Dashboard HTML served by `genie-api`. |
| `crates/dashboard/dashboard.js` | Dashboard frontend logic for polling and rendering system state. |
| `crates/genie-core/src/chat_ui.html` | Embedded local chat UI served by `genie-core`. |

## Deploy and Operations Assets

### Configuration Templates

| Path | Purpose |
| --- | --- |
| `deploy/config/geniepod.toml` | Main production config template. |
| `deploy/config/geniepod.dev.toml` | Development config template. |
| `deploy/config/mosquitto.conf` | MQTT broker config used by the deployment stack. |
| `deploy/config/profile.toml.example` | Example structured user profile file. |

### Systemd Units

| Path | Purpose |
| --- | --- |
| `deploy/systemd/genie-core.service` | Main runtime service. |
| `deploy/systemd/genie-llm.service` | Local LLM server service. |
| `deploy/systemd/genie-governor.service` | Governor service. |
| `deploy/systemd/genie-health.service` | Health monitor service. |
| `deploy/systemd/genie-api.service` | Dashboard API service. |
| `deploy/systemd/genie-audio.service` | Audio support service wiring. |
| `deploy/systemd/genie-mqtt.service` | MQTT-related service wiring. |
| `deploy/systemd/genie-wakeword.service` | Wakeword process service. |
| `deploy/systemd/homeassistant.service` | Home Assistant container/service wrapper. |
| `deploy/systemd/geniepod.target` | Main service grouping target. |
| `deploy/systemd/geniepod-late.target` | Late-start service grouping target. |

### Deploy Scripts and Helpers

| Path | Purpose |
| --- | --- |
| `deploy/setup-jetson.sh` | First-boot Jetson setup: directories, permissions, power profile, model checks, and service enablement. |
| `deploy/scripts/genie-restart-all.sh` | Convenience helper for restarting the deployed stack. |
| `deploy/scripts/detect-audio-device.sh` | Audio device discovery helper. |
| `deploy/scripts/genie-wake-listen.py` | Wakeword/audio helper script. |
| `deploy/scripts/genie-wakeword.py` | Wakeword-specific helper script. |
| `deploy/docker/docker-compose.yml` | Jetson-side compose file for containerized supporting services. |

## Where To Start For Common Changes

If you need to change a behavior, start here:

- Chat or API behavior:
  `crates/genie-core/src/server.rs`
- Prompting, tool choice, and model-specific instructions:
  `crates/genie-core/src/prompt.rs`
- Think/no-think or model routing:
  `crates/genie-core/src/reasoning.rs`
- Tool registration and execution:
  `crates/genie-core/src/tools/dispatch.rs`
- Home Assistant resolution or health behavior:
  `crates/genie-core/src/ha/provider.rs`
- Home Assistant action/status tool behavior:
  `crates/genie-core/src/tools/home.rs`
- System status answers:
  `crates/genie-core/src/tools/system.rs`
- Memory extraction or recall quality:
  `crates/genie-core/src/memory/extract.rs`
  `crates/genie-core/src/memory/mod.rs`
  `crates/genie-core/src/memory/inject.rs`
- Conversation retention and windowing:
  `crates/genie-core/src/conversation.rs`
  `crates/genie-core/src/context.rs`
- Voice pipeline:
  `crates/genie-core/src/voice_loop.rs`
  `crates/genie-core/src/voice/`
- Telegram:
  `crates/genie-core/src/telegram.rs`
- Governor mode switching and service lifecycle:
  `crates/genie-governor/src/governor.rs`
  `crates/genie-governor/src/service_ctl.rs`
- Deploy behavior on Jetson:
  `deploy/setup-jetson.sh`
  `deploy/systemd/`
  `deploy/config/`

## Notes On Maintenance

- `ARCHITECTURE.md` should explain the system shape.
- `CODEBASE.md` should explain where code lives and what owns what.
- `README.md` should stay product- and repo-oriented.
- `GETTING_STARTED.md` should stay operator-focused.

If you add a new crate, service, adapter, or major subsystem, update this document in the same change so the map stays current.
