# GenieClaw

GenieClaw is the agent layer of the **Genie** home AI ecosystem.

Current alpha: **1.0.0-alpha.4**.

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

This repo is the Rust agent runtime for a very specific product shape:

- a Jetson-first home AI appliance
- a full local voice pipeline: wake word, STT, LLM orchestration, tools, and TTS
- a local household memory system
- safe handoff to a home-control runtime
- transitional Home Assistant support while `genie-home-runtime` is not yet split out
- transitional `llama.cpp` support while `genie-ai-runtime` is not yet split out
- a privacy-first and security-first system
- a memory-footprint-conscious runtime built for constrained edge hardware

If you want a short definition:

> GenieClaw is the local agent layer for private physical AI at home.

## Ecosystem Position

The intended Genie stack has five product layers. Layer three has two runtime
components:

- custom Jetson hardware
- `genie-os`: custom L4T image, drivers, OTA, and service supervision
- `genie-home-runtime`: Rust AI-native home automation runtime and final actuation safety layer
- `genie-ai-runtime`: Jetson-only C++ LLM runtime customized from `llama.cpp`
- `genie-claw`: this repo, the Rust agent layer for voice, memory, tools, skills, and channels
- application layer: web and mobile app surfaces

This repo should not become all five layers. It can keep transitional adapters
for today, but the long-term architecture keeps physical control, inference,
OS bring-up, and product apps behind explicit boundaries.

## What It Does

Today, the system can:

- run a local LLM-backed chat and voice loop
- stay flexible around local model choice inside the Jetson deployment
- expose a local HTTP API and web UI
- store conversation history and household memory in SQLite
- integrate with Home Assistant for device control and status as a transitional provider
- search public web information through a no-key provider, with optional SearXNG support
- run companion services for health monitoring, governance, dashboards, and system control
- target Jetson-class hardware with a small-footprint Rust runtime
- provide the foundations for a tightly controlled native skill model

Home control now has an explicit safety model:

- first-pass local action policy
- final runtime actuation gate before Home Assistant service execution
- configurable request-origin allowlist for physical actuation
- configurable per-origin physical-action rate limits
- pending confirmation tokens for high-risk actions
- recent action ledger for "what did you do?" and bounded undo
- dashboard/API visibility for pending, executed, and audited home actions
- append-only actuation audit logging under the data directory

Alpha 4 also adds the runtime control-plane surfaces needed for safer local
agent operation:

- runtime contract fingerprints for prompt, tools, policy, and hydrated state
- optional contract drift detection after a known-good boot
- privacy-preserving tool audit logs
- origin-aware tool allow/deny policy
- native skill manifest audit metadata and configurable skill-load policy
- local support bundles for field diagnostics

## What It Is Not

`genie-core` is not:

- a hosted cloud assistant
- a thin wrapper around Home Assistant Assist
- a broad skill marketplace where feature count matters more than trust
- a general-purpose agent platform
- a messaging-bot framework
- the custom Jetson OS layer
- the final home automation and actuation runtime
- the Jetson CUDA inference runtime
- the whole product UI or mobile app

Home Assistant is currently a provider behind a boundary. Long term,
`genie-home-runtime` should own the device graph, automations, and final
physical actuation checks. GenieClaw owns the voice behavior, memory, session
logic, response style, channels, and skill routing.

## How It Fits Together

At a high level:

1. Today, `llama.cpp` provides the local model server. Longer term,
   `genie-ai-runtime` should provide the Jetson-only inference service.
2. `genie-core` handles prompts, tool calls, memory, chat, and voice orchestration.
3. Today, Home Assistant can provide device state and service execution. Longer term,
   `genie-home-runtime` should provide that boundary and the final actuation safety layer.
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

### Web Search

`genie-core` includes a built-in `web_search` tool for explicit lookup requests
such as “search the web for ESP32-C6 Thread support.” By default it uses
DuckDuckGo Instant Answer and requires no API key.

For a more private or controllable setup, point it at a local SearXNG instance:

```toml
[web_search]
enabled = true
provider = "searxng"
base_url = "http://127.0.0.1:8888"
allow_remote_base_url = false
timeout_secs = 8
max_results = 3
cache_enabled = true
cache_ttl_secs = 900
cache_max_entries = 64
```

Set `enabled = false` to remove the tool from the model prompt and quick router.

Direct local API test:

```bash
curl -s http://127.0.0.1:3000/api/web-search

curl -s http://127.0.0.1:3000/api/web-search \
  -H "Content-Type: application/json" \
  -d '{"query":"ESP32-C6 Thread support","limit":3,"fresh":false}'
```

The direct endpoint returns both a rendered `response` string and structured
`items`, along with `provider`, `cached`, `blocked`, and `result_count` fields.

## Documentation

- [doc/README.md](doc/README.md) for the current documentation entry point and repo-wide map
- [CHANGELOG.md](CHANGELOG.md) for alpha release notes
- [GETTING_STARTED.md](GETTING_STARTED.md) for local dev, Docker, and Jetson bring-up
- [ARCHITECTURE.md](ARCHITECTURE.md) for the Genie ecosystem and repo-boundary architecture
- [CODEBASE.md](CODEBASE.md) for the file-by-file code map
- [CONNECTIVITY.md](CONNECTIVITY.md) for the ESP32-C6 UART Thread/Matter sidecar plan and the boundary between `genie-core` and `genie-os`
- [VECTOR_MEMORY.md](VECTOR_MEMORY.md) for the semantic-memory and vector-search design
- [skills/SKILL-DEVELOPER-GUIDE.md](skills/SKILL-DEVELOPER-GUIDE.md) for native skill authoring
- Local-only `ROADMAP.md`, if present, for private execution planning

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
- reducing false activations and ambient-chatter waste in shared-room voice mode

## Memory Safety Notes

The current memory system is built for a shared-room appliance:

- memory rows persist policy metadata for `scope`, `sensitivity`, and `spoken_policy`
- prompt context, memory recall, and voice bootstrap all use shared-room-safe filtering by default
- promoted durable memory in `memory/MEMORY.md` only includes memories safe for shared household disclosure

## License

GNU Affero General Public License v3.0

See [LICENSE](LICENSE).
