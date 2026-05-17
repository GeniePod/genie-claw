# GenieClaw Repository Architecture

GenieClaw is the agent layer of the broader Genie ecosystem.

This repository should be understood as the Rust agent runtime that sits above:

- custom Jetson hardware
- GenieOS, the custom L4T and system image layer
- `genie-home-runtime`, the future AI-native home automation runtime
- `genie-ai-runtime`, the future Jetson-only LLM inference runtime

It sits below:

- web apps
- mobile apps
- installer/operator tools
- future household and developer-facing product surfaces

The purpose of this repo is to make the local home AI useful, private, safe, and understandable. It should not grow into the OS, the full home automation engine, the CUDA inference runtime, or the product app.

## Ecosystem Stack

```text
Application Layer
  Web app, mobile app, dashboard, setup, memory manager, confirmations

GenieClaw Agent Layer
  Offline voice, memory, tools, skills, channel adapters, agent policy

Runtime Layer
  genie-home-runtime: device graph, automations, actuation safety, MCP
  genie-ai-runtime: Jetson-only LLM inference, CUDA kernels, model serving

GenieOS Layer
  Custom L4T image, drivers, services, OTA, diagnostics

Hardware Layer
  Custom Jetson SOM carrier boards, audio, wireless, peripherals
```

## What This Repo Owns

GenieClaw owns the human-facing intelligence loop:

- wake/speech interaction and spoken response behavior
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
- control the home through a bounded runtime contract
- explain what happened
- work when the internet is down

## What This Repo Should Not Own Long Term

These responsibilities belong in lower or upper layers:

- Jetson board support, kernel/device-tree work, OTA base image ownership: `genie-os`
- Matter/Thread/Zigbee/BLE device graph and automation engine: `genie-home-runtime`
- final physical actuation safety checks: `genie-home-runtime`
- llama.cpp fork, CUDA kernels, model memory planner: `genie-ai-runtime`
- full product app, account UX, mobile push, installer workflows: application layer

This repo can keep transitional implementations while those layers are still forming, but the code should be structured so those boundaries can be replaced by stable clients.

## Current Transitional Adapters

The current repo still contains pragmatic adapters used to ship on Jetson now.

| Current adapter | Long-term replacement | Notes |
| --- | --- | --- |
| `genie-ai-runtime` OpenAI-compatible client (default on Jetson) | `llama.cpp` client (selectable fallback) | Both backends ship behind the `LlmClient` facade; per-deployment selection via `[services.llm].backend` in `geniepod.toml`. Backend identity surfaces in `/api/health`, startup logs, and `genie-ctl status`. |
| Home Assistant provider | `genie-home-runtime` MCP/API client | Keep HA-specific behavior behind `ha/` and tools/home boundaries. |
| Actuation safety in `genie-core` | final safety in `genie-home-runtime` | Keep current safety as an agent-side guard and confirmation layer. |
| `genie-api` dashboard | application layer | Keep it operational and lightweight; avoid making it the long-term product app. |
| `genie-governor` service lifecycle | GenieOS/system supervisor | Keep it useful for Jetson bring-up while OS ownership matures. |
| ESP32-C6 UART boundary | GenieOS connectivity service | Agent sees health/capabilities, not raw radio/device-driver internals. |

## Desired Internal Shape

The repo should trend toward these conceptual modules, even if existing file names remain for now.

| Conceptual area | Current code |
| --- | --- |
| Agent orchestration | `crates/genie-core/src/server.rs`, `repl.rs`, `voice_loop.rs`, `reasoning.rs`, `prompt.rs` |
| AI runtime client | `crates/genie-core/src/llm/` |
| Home runtime client | `crates/genie-core/src/ha/`, `tools/home.rs` |
| Voice pipeline | `crates/genie-core/src/voice/`, `voice_loop.rs` |
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
genie-claw
        |
        +---- web/mobile apps
        +---- local voice pipeline
        +---- skills and channels
        v
genie-home-runtime
        |
        v
GenieOS + hardware
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

Long-term safety responsibilities in `genie-home-runtime`:

- final deterministic actuation checks
- device availability and state checks
- automation policy
- scene and multi-device safety rules
- replayable action logs
- recovery and rollback behavior

The agent may propose or request actions. The home runtime decides whether physical execution is allowed.

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
- `genie-home-runtime`: home automation and physical actuation runtime
- `genie-ai-runtime`: Jetson-only inference runtime
- `genie-os`: custom L4T image and hardware bring-up layer

## Refactor Direction

The clean architecture path is incremental:

1. Make boundary language consistent in docs and config.
2. Keep Home Assistant and LLM backends behind narrow adapter traits (LLM side resolved via the `LlmClient` facade in `crates/genie-core/src/llm/`).
3. Move physical actuation authority downward into `genie-home-runtime` when it exists.
4. Move Jetson model-server specialization downward into `genie-ai-runtime`.
5. Keep GenieClaw focused on voice, memory, skills, tools, channels, and household interaction.

This prevents the agent repo from becoming a single large mixed runtime while still allowing today’s Jetson appliance to keep working.
