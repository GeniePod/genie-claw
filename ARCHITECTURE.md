# GenieClaw Repository Architecture

GenieClaw is the agent layer of the broader Genie ecosystem.

This repository should be understood as the Rust agent runtime that sits above:

- custom Jetson hardware
- the platform/OS layer for custom L4T, system image, drivers, and supervision
- an external voice boundary for wake/VAD/STT/TTS/audio
- an external home boundary for AI-native home automation and final actuation
- `genie-ai-runtime`, the external Jetson-first LLM inference runtime

It sits below:

- web apps
- mobile apps
- installer/operator tools
- future household and developer-facing product surfaces

The purpose of this repo is to make the local home AI useful, private, safe,
low-latency, and understandable. Accuracy should come from the right family
memory, current home context, typed device state, and deterministic tools. It
should not grow into the OS, the full home automation engine, the CUDA
inference runtime, or the product app.

The architectural invariant is now enforced in code as well as docs:

- `genie_core::runtime_boundary` names the AI, voice, and home runtime owners.
- `genie_core::agent_harness` validates prompt, tool, memory, response reserve,
  and optional-provider context against the Jetson 4096-token baseline.
- `[agent]` selects the deployment profile while keeping `jetson` as the
  flagship default.
- `[optional_ai_provider]` is opt-in and exists for testing, development
  portability, and transitional validation only. It cannot bypass the
  limited-context home-agent contract.

## Ecosystem Stack

```text
Application Layer
  Web app, mobile app, dashboard, setup, memory manager, confirmations

GenieClaw Agent Layer
  Agent policy, memory, tools, skills, channel adapters, spoken behavior

Runtime Layer
  voice boundary: wake, VAD, STT, TTS, audio streaming
  home boundary: local device graph, IoT adapters, automations, actuation safety, MCP
  genie-ai-runtime: Jetson-only LLM inference, CUDA kernels, model serving

Platform/OS Layer
  Custom L4T image, drivers, services, OTA, diagnostics

Hardware Layer
  Custom Jetson SOM carrier boards, audio, wireless, peripherals
```

## What This Repo Owns

GenieClaw owns the human-facing intelligence loop:

- spoken response behavior and voice-session policy
- prompt construction and reasoning-mode selection
- memory capture, policy, recall, and dashboard management
- skill and tool routing
- conversation history
- query rejection for shared-room audio
- channel adapters such as web chat, REPL, CLI, Telegram, and future app channels
- high-level action intent before handoff to the home runtime
- local confirmation UX for risky physical actions

The repo should optimize for repeated daily usefulness:

- remember useful household context
- answer and act quickly on Jetson-class hardware
- control the home through a bounded local runtime contract
- explain what happened
- work when the internet is down

## What This Repo Should Not Own Long Term

These responsibilities belong in lower or upper layers:

- Jetson board support, kernel/device-tree work, OTA base image ownership: platform/OS layer
- Matter/Thread/Zigbee/BLE device graph and automation engine: external home boundary
- final physical actuation safety checks: external home boundary
- llama.cpp fork, CUDA kernels, model memory planner: `genie-ai-runtime`
- wake word, VAD, STT, TTS, ALSA/audio-device handling, denoise, and AEC: external voice boundary
- full product app, account UX, mobile push, installer workflows: application layer

This repo can keep transitional implementations while those layers are still forming, but the code should be structured so those boundaries can be replaced by stable clients.

## Current Transitional Adapters

The current repo still contains pragmatic adapters used to ship on Jetson now.

| Current adapter | Target boundary | Notes |
| --- | --- | --- |
| `genie-ai-runtime` OpenAI-compatible client (default on Jetson) | external `genie-ai-runtime` service | `llama.cpp` remains a selectable fallback/development backend. Both local backends ship behind the `LlmClient` facade; per-deployment selection is via `[services.llm].backend` in `geniepod.toml`. Backend identity surfaces in `/api/health`, startup logs, and `genie-ctl status`. |
| In-repo voice pipeline under `crates/genie-core/src/voice/` and `voice_loop.rs` | external voice boundary | Keep current code as a transitional Jetson bring-up path. New wake/VAD/STT/TTS/audio ownership should move behind the external boundary. GenieClaw should consume transcripts and issue speak commands. |
| Home Assistant provider | external home boundary | Keep HA-specific behavior behind `ha/` and tools/home boundaries. Home Assistant is a transitional provider while the native local device graph and IoT boundary mature. |
| Actuation safety in `genie-core` | final safety in the external home boundary | Keep current safety as an agent-side guard and confirmation layer. |
| `genie-api` dashboard | application layer | Keep it operational and lightweight; avoid making it the long-term product app. |
| `genie-governor` service lifecycle | platform/system supervisor | Keep it useful for Jetson bring-up while OS ownership matures. |
| ESP32-C6 UART boundary | platform connectivity service | Agent sees health/capabilities, not raw radio/device-driver internals. |

## Desired Internal Shape

The repo should trend toward these conceptual modules, even if existing file names remain for now.

| Conceptual area | Current code |
| --- | --- |
| Agent orchestration | `crates/genie-core/src/server.rs`, `repl.rs`, `voice_loop.rs`, `reasoning.rs`, `prompt.rs` |
| AI runtime client | `crates/genie-core/src/llm/` |
| Home runtime client | `crates/genie-core/src/ha/`, `tools/home.rs` |
| Voice runtime client | transitional `crates/genie-core/src/voice/`, `voice_loop.rs`; target external voice-boundary client |
| Memory system | `crates/genie-core/src/memory/`, `conversation.rs` |
| Tool and skill routing | `crates/genie-core/src/tools/`, `skills/` |
| Channel adapters | `server.rs`, `repl.rs`, `telegram.rs`, `genie-ctl` |
| Runtime policy | `security/`, `memory/policy.rs`, `tools/actuation.rs`, `ha/policy.rs` |
| Operations | `genie-api`, `genie-health`, `genie-governor`, deploy assets |

The important rule is dependency direction:

```text
channels -> agent orchestration -> memory/tools/skills
tools -> runtime clients
runtime clients -> lower runtimes or transitional external systems
```

Code in memory, voice, prompt, and channels should not learn Home Assistant internals. Code in agent orchestration should not learn model-server implementation details beyond the `LlmClient` contract.

Voice code has one additional rule: GenieClaw may own spoken agent behavior,
but it should not own the long-term audio pipeline. Wake word, VAD, STT, TTS,
audio-device handling, denoise, AEC, and streaming voice events belong in
the external voice boundary.

## Process Topology Today

Current Jetson deployment:

```text
LLM backend (:8080)         # genie-ai-runtime by default on Jetson;
                            # fallback: [services.llm].backend = "llama_cpp"
        ^
        |
genie-core (:3000) <---- genie-ctl
        |
        +---- local chat UI / OpenAI-compatible clients
        +---- optional Telegram adapter
        +---- optional Home Assistant provider
        +---- optional ESP32-C6 connectivity controller boundary

genie-api      ---- dashboard/status service
genie-governor ---- pressure response and service lifecycle
genie-health   ---- health polling and history
```

Target deployment:

```text
genie-ai-runtime
        ^
        |
external voice boundary ---- transcript/speak events
        ^                         |
        |                         v
genie-claw
        |
        +---- web/mobile apps
        +---- skills and channels
        v
external home boundary
        |
        v
platform/OS + hardware
```

## Safety Ownership

GenieClaw must treat physical actuation as high-risk.

Current safety responsibilities in this repo:

- recognize risky home actions
- apply origin-specific restrictions
- create local confirmation tokens
- expose pending confirmations to dashboard/API
- write actuation audit events
- avoid relying on the model prompt as a safety boundary

Long-term safety responsibilities in the external home boundary:

- final deterministic actuation checks
- device availability and state checks
- automation policy
- scene and multi-device safety rules
- replayable action logs
- recovery and rollback behavior

The agent may propose or request actions. The home runtime decides whether physical execution is allowed.

## Portable Profiles

GenieClaw supports maintained SBC profiles without changing the native product
shape. `[agent].runtime_profile` describes where the same agent harness is
running:

| Profile | Purpose |
| --- | --- |
| `jetson` | Flagship NVIDIA Jetson Orin 8GB path; 4096-token baseline and local runtime default |
| `raspberry_pi` | Maintained SBC profile for the headless agent, memory, tools, HTTP/CLI, and home-provider boundaries |
| `portable_sbc` | Maintained generic SBC profile where voice, CUDA, and full home runtimes may be absent |
| `laptop` | Developer profile for local tests, docs, and provider integration |
| `mac` | macOS developer profile; no Jetson-specific runtime assumptions |

Profiles do not change ownership. They keep the same limited-context agent
contract portable enough for development, review, and maintained non-Jetson SBC
deployments. Jetson-only acceleration and voice/audio behavior may degrade or
be disabled, but prompt, memory, tools, safety policy, and home-runtime
boundaries should keep working.

## Optional Providers

API-key, OAuth-bearer, and OpenAI-compatible AI providers are optional
development and transitional validation boundaries, not the product runtime.
They must satisfy all of these before being used for serious test coverage:

- configured under `[optional_ai_provider]`, disabled by default
- API key comes from an environment variable, not the TOML value itself
- `context_window_tokens <= [agent].context_window_tokens`
- remote endpoints require `allow_remote_base_url = true`
- prompt/tool/memory budget passes `genie_core::agent_harness`

The flagship path remains local `genie-ai-runtime` on Jetson. Optional providers
exist to make development, CI, and non-Jetson testing easier during the
transition. They must not become a reason to increase context by default, send
household memory remotely by default, or weaken the limited-context home-agent
contract.

## Low-Latency Home Harness

The harness should optimize for small, high-signal context rather than larger
model inputs. A normal turn should assemble only:

- compact policy and role instructions
- the relevant family/speaker context
- top-k memory facts
- current room or device graph slice
- recent action summary when it helps explain or undo behavior
- the smallest useful tool manifest

The LLM should not receive raw history, every memory, or stale device state.
The agent should use deterministic quick routes and typed tools for repeated
daily tasks before spending model tokens.

## Memory Ownership

GenieClaw owns household memory because memory is part of the agent experience.

The memory system must stay:

- local by default
- policy-aware for shared-room use
- editable by the user
- auditable through canonical artifacts
- independent from any specific LLM backend

The application layer can expose memory management UI, but memory writes should go through GenieClaw APIs so policy metadata, event logs, and durable memory files stay consistent.

## Skill Ownership

GenieClaw owns skill routing and the skill ABI.

Skills should be:

- permissioned
- auditable
- removable
- testable locally
- isolated from lower runtime internals unless explicitly granted

Skills should call stable GenieClaw or home-runtime interfaces. They should not reach directly into Home Assistant, CUDA runtime details, or OS-level radio drivers.

## Naming Guidance

This repo is strategically `genie-claw`, even though some crate and service names still use `genie-core`.

Short-term rule:

- do not rename crates just for branding
- use docs and boundaries to clarify ownership first
- rename only when the lower runtimes and app layer boundaries are stable enough to avoid churn

Long-term direction:

- `genie-claw`: agent layer repository and product brain
- external voice boundary: voice I/O runtime for wake, VAD, STT, TTS, and audio streaming
- external home boundary: home automation and physical actuation runtime
- `genie-ai-runtime`: Jetson-only inference runtime
- platform/OS layer: custom L4T image and hardware bring-up layer

## Refactor Direction

The clean architecture path is incremental:

1. Make boundary language consistent in docs and config.
2. Keep Home Assistant and LLM backends behind narrow adapter traits (LLM side resolved via the `LlmClient` facade in `crates/genie-core/src/llm/`).
3. Tune the prompt/memory/tool harness for low-latency household context.
4. Move physical actuation authority and direct local IoT/device graph ownership downward into the external home boundary when it is ready.
5. Keep Jetson model-server specialization in `genie-ai-runtime`.
6. Move voice/audio pipeline ownership downward into the external voice boundary.
7. Keep GenieClaw focused on agent policy, memory, skills, tools, channels, and household interaction.

This prevents the agent repo from becoming a single large mixed runtime while still allowing today’s Jetson appliance to keep working.
