# GenieClaw

GenieClaw is the core brain of **GeniePod Home**.

This repository is built first for Jetson, especially Jetson Orin Nano 8 GB (67 TOPS).
Its job is to turn a Jetson-based box into a private, always-on local AI for
the home and other shared spaces: local voice, local memory, local control,
and strong local security boundaries.

## Why It Exists

OpenClaw proved that people want AI that feels present, remembers context, and
fits into everyday life. GenieClaw exists to keep what people wanted and fix the
problems: tighter architecture, stronger privacy boundaries, better security,
lower memory footprint, and a more appliance-like deployment model.

Its direction comes from deep analysis of OpenClaw, ZeroClaw, NanoClaw,
NemoClaw, and OpenFang. The ambition is simple: build the best Claw in the
world for the home.

## What It Is

`genie-core` is for a very specific product shape:

- a Jetson-first home AI appliance
- a full local voice pipeline: wake word, STT, LLM orchestration, tools, and TTS
- a Jetson-first local LLM runtime: `llama.cpp` today, and later `genie-llm`,
  purpose-built to run useful models within constrained Jetson memory budgets
- a local household memory system
- a Home Assistant-aware runtime, but not one that depends on Home Assistant to matter
- a privacy-first and security-first system
- a memory-footprint-conscious runtime built for constrained edge hardware

If you want a short definition:

> GenieClaw is the software brain behind GeniePod Home.

## What It Does

Today, the system can:

- run a local LLM-backed chat and voice loop
- stay flexible around local model choice inside the Jetson deployment
- expose a local HTTP API and web UI
- store conversation history and household memory in SQLite
- integrate with Home Assistant for device control and status
- run companion services for health monitoring, governance, dashboards, and system control
- target Jetson-class hardware with a small-footprint Rust runtime
- provide the foundations for a tightly controlled native skill model

## What It Is Not

`genie-core` is not:

- a hosted cloud assistant
- a thin wrapper around Home Assistant Assist
- a broad skill marketplace where feature count matters more than trust
- a general-purpose agent platform
- a messaging-bot framework
- the whole product UI or mobile app

Home Assistant is the home-control layer. `genie-core` still owns the voice behavior,
memory, session logic, response style, and product behavior.

## How It Fits Together

At a high level:

1. Today, `llama.cpp` provides the local model server. Longer term, the goal is
   `genie-llm`: a Jetson-first inference runtime tuned for constrained memory
   and appliance-style reliability.
2. `genie-core` handles prompts, tool calls, memory, chat, and voice orchestration.
3. Home Assistant provides the device graph, states, scenes, and service execution.
4. GeniePod companion services handle health, governance, and dashboards.

That means the user talks to GeniePod, not directly to Home Assistant internals.

## Why Minimal-First On Jetson

GenieClaw is intentionally narrower than a broad general-agent stack.

That is a hardware decision as much as a product decision. In practical Jetson
Orin Nano 8 GB testing, heavier agent shells can require very large context
windows just to stay coherent, which drives up KV cache size, first-token
latency, and overall memory pressure. Even `8192` context can already be tight
on this class of device, and the result is often slower replies and worse
appliance behavior.

For GenieClaw, that means:

- shorter prompts and shorter default context windows
- fewer orchestration layers between the user and the model
- tighter tool routing instead of general agent abstraction
- model-specific tuning for Jetson-class hardware
- treating larger Claw systems as idea sources, not as the runtime to ship

The target is not “the most features.” The target is the best private local
assistant that still feels fast and reliable on 8 GB unified memory.

## Repo Layout

| Crate | Purpose |
|-------|---------|
| `genie-core` | Main runtime: prompt building, tools, memory, voice loop, HTTP API |
| `genie-common` | Shared config, mode types, and tegrastats parsing |
| `genie-ctl` | Local CLI for chat, status, tools, health, and diagnostics |
| `genie-governor` | Resource governor and service lifecycle controller |
| `genie-health` | Local health polling and alert forwarding |
| `genie-api` | Lightweight system dashboard |
| `genie-skill-sdk` | Rust SDK for native shared-library skills |

## Product Direction

The current product target is **GeniePod Home**:

- a shared-space AI appliance for the living room or kitchen
- Jetson-first rather than everywhere-first
- useful before smart-home integration
- stronger when connected to Home Assistant
- built around privacy, security, and bounded extensions
- designed to feel stable, understandable, and privacy-respecting

## Quick Start

If you just want to run the software locally:

```bash
# Build and test
make
make test

# Run the main runtime with the development config
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --bin genie-core

# Run the local dashboard
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --bin genie-api
```

For the full setup flow, including Jetson deploy and Home Assistant wiring, see
[GETTING_STARTED.md](GETTING_STARTED.md).

## Deployment

The main production target is Jetson Orin Nano 8 GB (67 TOPS) hardware.

The repo includes:

- Jetson deployment scripts
- systemd units
- default configs
- Home Assistant container deployment support
- wake-word helper scripts
- Docker support for local development

## Design Principles

- **Privacy and security over broad skills**: trust matters more than a giant extension catalog
- **Memory footprint is a core optimization target**: this is not cleanup work after the fact
- **Appliance over stack**: the system should feel like a product, not a hobby pile
- **Usefulness over demos**: timers, memory, home control, and daily utility come first
- **Small dependencies**: raw Tokio TCP, bundled SQLite, and minimal frameworks

## Current Focus

The current work is centered on:

- hardening the Jetson voice pipeline
- improving the household memory system
- tightening the Home Assistant boundary
- building a tightly controlled native skill model
- pushing the appliance-style deployment model further

## License

GNU Affero General Public License v3.0

See [LICENSE](LICENSE).
