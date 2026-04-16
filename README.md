# genie-core

`genie-core` is the local runtime for **GeniePod Home**.

In plain English: this repo is the software that turns a Jetson-based box into a
private, always-on home AI you can talk to in a shared room.

It runs the local language model loop, remembers household context, talks to Home
Assistant for smart-home control, and exposes a local API/UI for setup and control.

## What It Is

`genie-core` is for a specific product shape:

- a home AI appliance, not a cloud chatbot
- local-first, not SaaS-first
- shared-space voice interaction, not a personal phone assistant
- Home Assistant-aware, but not dependent on Home Assistant to have value

If you want a short definition:

> `genie-core` is the software brain behind GeniePod Home.

## What It Does

Today, the system can:

- run a local LLM-backed chat and voice loop
- expose a local HTTP API and web UI
- store conversation history and household memory in SQLite
- integrate with Home Assistant for device control and status
- run companion services for health monitoring, dashboards, and system control
- target Jetson-class hardware with a small-footprint Rust runtime

## What It Is Not

`genie-core` is not:

- a hosted cloud assistant
- a thin wrapper around Home Assistant Assist
- a general-purpose agent platform
- a messaging-bot framework
- the whole product UI or mobile app

Home Assistant is the home-control layer. `genie-core` still owns the voice behavior,
memory, session logic, response style, and product behavior.

## How It Fits Together

At a high level:

1. `llama.cpp` provides the local model server.
2. `genie-core` handles prompts, tool calls, memory, chat, and voice orchestration.
3. Home Assistant provides the device graph, states, scenes, and service execution.
4. GeniePod companion services handle health, governance, and dashboards.

That means the user talks to GeniePod, not directly to Home Assistant internals.

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
- local by default
- useful before smart-home integration
- stronger when connected to Home Assistant
- built to feel stable, understandable, and privacy-respecting

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

The main production target is Jetson Orin Nano 8 GB hardware.

The repo includes:

- Jetson deployment scripts
- systemd units
- default configs
- Home Assistant container deployment support
- wake-word helper scripts
- Docker support for local development

## Design Principles

- **Trust over breadth**: predictable local behavior matters more than feature count
- **Appliance over stack**: the system should feel like a product, not a hobby pile
- **Usefulness over demos**: timers, memory, home control, and daily utility come first
- **Small dependencies**: raw Tokio TCP, bundled SQLite, and minimal frameworks

## Current Focus

The current work is centered on:

- local shared-space voice interaction
- household memory
- Home Assistant integration
- stronger security defaults
- tightening the appliance-style deployment model

## License

GNU Affero General Public License v3.0

See [LICENSE](LICENSE).
