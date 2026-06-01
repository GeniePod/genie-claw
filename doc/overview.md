# Overview

## Purpose

GenieClaw is the limited-context local agent layer native to NVIDIA Jetson Orin
8GB and the broader Genie ecosystem.

The repo is optimized around a narrow goal:

- run locally on Jetson-class hardware as the flagship target
- maintain Raspberry Pi and generic portable SBC profiles for the headless
  agent, memory, tools, HTTP/CLI, and home-provider boundaries
- preserve a 4096-token small-context baseline before larger adaptive contexts
- favor low latency over larger default context windows
- tune accuracy through family memory, current home state, and typed tools
- keep the system understandable and debuggable
- keep agent/provider/home behavior testable through deterministic harnesses
- provide everyday household usefulness before broad platform ambition
- preserve privacy, bounded behavior, and graceful degradation

This is not a cloud orchestration shell and not a generic agent runtime. Remote
or API-key providers can be useful optional adapters for development, CI, and
transitional validation, but they must fit the limited-context home-agent
contract rather than redefine the product.

## Ecosystem Role

The long-term Genie stack is split by responsibility:

- custom Jetson hardware
- a platform/OS layer for custom L4T, drivers, OTA, diagnostics, and service supervision
- an external voice boundary for wake/VAD/STT/TTS/audio streaming and voice session events
- an external home boundary for device graph, automations, MCP, and final actuation safety
- `genie-ai-runtime` for Jetson-only LLM inference optimization
- `genie-claw` for agent policy, memory, tools, skills, smart-home intent, and interaction
- web/mobile apps for setup, control, memory management, and confirmations

This repository is `genie-claw`. It integrates with external lower runtimes
through narrow clients: `genie-ai-runtime` is the Jetson default LLM backend,
`llama.cpp` remains a selectable development/fallback backend, and Home
Assistant is the current transitional home provider.

The Jetson profile is the native product path. The `raspberry_pi` and
`portable_sbc` profiles are maintained compatibility targets for smaller
on-device installs where Jetson-only voice, CUDA, or service behavior may be
absent.

The intended accuracy path is a compact home context harness: identity and
family facts, relevant memories, device graph slices, recent actions, and
safety policy are selected before each turn instead of dumping raw history or
using remote context as a shortcut.

For the exact implemented/partial/planned breakdown, use
[implementation-status.md](implementation-status.md). In short, this repo
implements the agent runtime, memory, tools, a transitional voice adapter,
local HTTP/CLI surfaces, safety gates, and deploy assets. It does not implement
the final voice runtime, home runtime, `genie-ai-runtime`, platform/OS layer,
or full Matter/Thread device stack.

## Main Runtime Modes

`genie-core` supports three primary modes:

1. HTTP server mode
   The default daemon mode. It serves the local chat UI, local API consumers,
   OpenAI-compatible adapters, and direct tool surfaces.
2. REPL mode
   When stdin is interactive, `genie-core` starts a local text REPL instead of
   daemon-only behavior.
3. Voice mode
   Enabled by config, `--voice`, or `GENIEPOD_VOICE=1`. Today this still uses a
   transitional in-repo voice path. Long term, the external voice boundary owns
   microphone -> STT and TTS -> speaker, while GenieClaw owns transcript ->
   prompt/tool execution -> response text.

In daemon mode, Telegram can also be enabled as a side-channel adapter.

## High-Level Process Topology

Typical Jetson deployment:

```text
genie-ai-runtime (:8080)
        ^
        |
genie-core (:3000) <---- genie-ctl
        |
        +---- local chat UI / OpenAI-compatible clients
        +---- optional Telegram adapter
        +---- optional Home Assistant provider
        +---- optional ESP32-C6 connectivity controller boundary

Selectable fallback: llama.cpp `llama-server` can also serve the same
OpenAI-compatible LLM endpoint on `:8080`.

genie-governor ---- controls service modes and pressure response
genie-health   ---- polls health endpoints and stores health history
genie-api      ---- serves dashboard/status data
```

Target topology:

```text
genie-ai-runtime
        ^
        |
external voice boundary
        ^
        |
genie-claw
        |
        +---- web/mobile apps and local channels
        +---- memory, tools, skills
        v
external home boundary
        |
        v
platform/OS + custom Jetson hardware
```

The target home runtime owns direct local IoT interfaces and the final physical
actuation gate. GenieClaw owns the agent decision, memory, confirmation, and
audit layers above that boundary.

## Core User Flows

### Chat HTTP Flow

1. Client sends `POST /api/chat` or `POST /v1/chat/completions`.
2. `genie-core` appends the user message to the conversation store.
3. Fast-path routing may intercept deterministic requests.
   Examples: time, memory diagnostics, system status, explicit web search.
4. If not intercepted, GenieClaw builds the system prompt and injects relevant memory.
5. The LLM returns plain text or tool JSON.
6. If tool JSON is detected, the tool dispatcher executes it.
7. The result is either returned raw or summarized, depending on tool type.
8. Memory auto-capture runs on the user message.

### Voice Flow

1. Record audio from ALSA or auto-detected device.
2. Apply DSP, gating, and optional cleanup.
3. Send audio to Whisper CLI/server.
4. Detect language if configured as `auto`.
5. Run the same routing/prompt/tool pipeline as text.
6. Use Piper for spoken output, optionally with language-specific voices.

Current multilingual support is a pipeline capability, not a certified
full-language product guarantee. Quality depends on the installed Whisper and
Piper models for each language and on device-level audio testing.

### Governor Flow

1. Poll tegrastats and memory state.
2. Track day/night/media modes.
3. Stop or defer optional services under pressure.
4. Expose status and mode change control through the Unix control socket.

## Data At Rest

The runtime primarily stores data under `data_dir`.

Main current databases:

- `memory.db`: persistent memory and FTS-backed recall
- `conversations.db`: conversation history and exports
- `governor.db`: tegrastats history and governor state data
- `health.db`: service health history

The default production `data_dir` is `/opt/geniepod/data`.
The default development `data_dir` is `./data`.

## Why The Repo Is Split Into Small Crates

The crate split is pragmatic, not academic.

- `genie-common` keeps config and shared types reusable across binaries.
- `genie-core` currently contains the agent runtime and transitional runtime adapters.
- `genie-governor`, `genie-health`, and `genie-api` stay small and operationally separate.
- `genie-ctl` gives you a narrow operator interface without needing a browser.
- `genie-skill-sdk` keeps the native skill ABI explicit.

The result is easier bring-up on Jetson and clearer failure boundaries.
