# GenieClaw

[![CI](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml)
[![Jetson cross-compile](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml)
[![Audit](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml)

**Limited-context AI harness for agentic smart homes: portable across SBCs and
native to GeniePod Home.**

GenieClaw is the Rust agent layer for GeniePod Home. It is built for small local
models, tight VRAM budgets, and a 4096-token Jetson baseline. This repo owns
prompt assembly, memory, tool routing, smart-home intent, safety policy, audit,
and channel/session adapters.

GenieClaw is not the voice pipeline, the LLM runtime, the OS, the final
home-control runtime, or the product app layer.

## Boundary

| Layer | Owner | Notes |
|-------|-------|-------|
| Agent layer | `genie-claw` | Prompt policy, limited-context harness, memory, tools, skills, smart-home intent, safety, audit, channels |
| LLM runtime | [`genie-ai-runtime`](https://github.com/GeniePod/genie-ai-runtime) | Jetson-first local inference runtime; `llama.cpp` remains selectable |
| Voice runtime | [`genie-voice-runtime`](https://github.com/GeniePod/genie-voice-runtime) | Wake, VAD, STT, TTS, audio streaming, voice session protocol |
| Home runtime | `genie-home-runtime` | Planned AI-native device graph and final actuation gate |
| Home Assistant | Transitional provider | Current integration target until `genie-home-runtime` exists |
| OS and apps | External layers | `genie-os`, web, and mobile surfaces stay outside this repo |

Full stack shape:

```text
user channel / voice runtime
          |
          v
   genie-claw agent layer
    |        |        |
 memory   tools   safety/audit
    |        |        |
    v        v        v
genie-ai-runtime   Home Assistant today
                   genie-home-runtime later
```

## What Works Today

- local chat through `genie-core`
- transitional voice-session adapter while voice moves to `genie-voice-runtime`
- LLM backend facade for `genie-ai-runtime` and selectable `llama.cpp`
- SQLite conversation history and household memory
- Home Assistant adapter with confirmations, rate limits, and audit logging
- local HTTP API, dashboard, CLI, health service, and governor service
- optional `web_search` tool with DuckDuckGo or SearXNG
- Jetson aarch64 cross-compile CI

Current workspace version: `v1.0.0-alpha.9`.

## Current Focus

- keep the agent reliable inside a 4096-token Jetson context
- harden prompt, memory, tool, and safety contracts
- split long-term wake/VAD/STT/TTS ownership into `genie-voice-runtime`
- keep Home Assistant behind a provider boundary until `genie-home-runtime`
- allow optional API-key providers only when they pass the same limited-context harness
- keep development usable on SBCs, laptops, and Macs without making Jetson less native

## Quick Start

```bash
make
make test

GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --bin genie-core
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --bin genie-api
```

For Jetson setup, deployment, and Home Assistant wiring, use
[`GETTING_STARTED.md`](GETTING_STARTED.md).

## Repo Layout

| Crate | Purpose |
|-------|---------|
| `genie-core` | Main agent runtime: prompt building, tools, memory, HTTP API, and channel/session adapters |
| `genie-common` | Shared config, mode types, and tegrastats parsing |
| `genie-ctl` | Local CLI for chat, status, tools, health, and diagnostics |
| `genie-governor` | Resource governor and service lifecycle controller |
| `genie-health` | Local health polling and alert forwarding |
| `genie-api` | Lightweight local dashboard |
| `genie-skill-sdk` | Rust SDK for native shared-library skills |

## Documentation

- [`GETTING_STARTED.md`](GETTING_STARTED.md) - local dev, Docker, Jetson bring-up, and deploy
- [`ARCHITECTURE.md`](ARCHITECTURE.md) - Genie ecosystem boundaries
- [`doc/README.md`](doc/README.md) - documentation map
- [`doc/implementation-status.md`](doc/implementation-status.md) - implemented, partial, external, and planned work
- [`CHANGELOG.md`](CHANGELOG.md) - alpha release notes
- [`CONTRIBUTING.md`](CONTRIBUTING.md) - PR and proof requirements
- [`SECURITY.md`](SECURITY.md) - vulnerability reporting

## Contributing

Every PR needs a **Real Behavior Proof** section: what you ran, where you ran it,
and what happened. CI/local proof is enough for docs, harness, provider, and
non-hardware work. Hardware-facing changes should include Jetson/device proof or
state the validation gap clearly.

## License

GNU Affero General Public License v3.0. See [`LICENSE`](LICENSE).
