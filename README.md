# GenieClaw

[![CI](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml)
[![Jetson cross-compile](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml)
[![Audit](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml)

**A private, always-on AI for your home. Runs entirely on a Jetson Orin Nano.
Voice in, voice out, controls Home Assistant, no cloud.**

- 🎙️ Local voice loop — wake word → STT (Whisper) → LLM → TTS (Piper) → action
- 🧠 Local memory — conversations and household context kept in SQLite on the device
- 🏠 Home Assistant control behind a safety gate (rate-limited, confirmed, audited)
- 🔒 Private by default — no audio, no transcripts, no model traffic leaves the box
- 🦀 Rust runtime, ~8 GB Jetson Orin Nano target, alpha-grade today

![GenieClaw](doc/assets/genie-claw.png)

> **Status:** `v1.0.0-alpha.9`. The voice loop, the Home Assistant integration,
> the LLM-backend facade (llama.cpp / genie-ai-runtime), Telegram voice
> ingestion, and the safety/audit surfaces are working end-to-end on Jetson
> Orin Nano Super 8 GB (see [`CHANGELOG.md`](CHANGELOG.md) for the alpha.5
> verified-deploy notes, the alpha.7 verified voice cycle, and the alpha.9
> CI / supply-chain / voice-optional bundle). Setup is currently a 30-60 min
> Jetson bring-up, not a one-line install — see
> [`GETTING_STARTED.md`](GETTING_STARTED.md).

## How it works

A complete voice cycle never leaves the appliance. Audio is captured on
the Jetson, routed through five on-device stages, and answered in audio.
No audio, no transcripts, no model traffic crosses your network boundary.

```
   you speak                      you hear
       │                              ▲
       ▼                              │
   ┌────────┐   ┌────────┐   ┌──────────────┐   ┌───────┐
   │ Wake + │ → │ STT    │ → │ GenieClaw    │ → │ TTS   │
   │ VAD    │   │ Whisper│   │ agent (Rust) │   │ Piper │
   └────────┘   └────────┘   └──────┬───────┘   └───────┘
                                    │
                       memory ◄─────┼─────► local LLM
                       (SQLite)     │       (genie-ai-runtime by default,
                                    │        llama.cpp still selectable
                                    │        via [services.llm].backend)
                                    ▼
                          Home Assistant
                          (rate-limited, audited)
```

### Per-stage walkthrough

1. **Wake + VAD** — wake-word detection plus voice-activity tail. The
   rest of the pipeline stays cold until both fire.

2. **STT (Whisper)** — local transcription on the Jetson. Transcripts
   live in process memory only; nothing is written to disk by default.

3. **Agent layer (GenieClaw, Rust)** — assembles the system prompt from
   frozen identity blocks, hydrated household memory, and the tool
   manifest. Routes to the local LLM. Dispatches tool calls through the
   safety gate — per-origin ACL, rate limits, confirmation tokens for
   high-risk actions. Every decision and dispatched call lands in an
   append-only audit ledger.

4. **Local LLM (`genie-ai-runtime`)** — default backend, a Jetson-tuned
   C++ / CUDA inference runtime derived from `llama.cpp`. On the 8 GB
   Orin Nano the system runs **Phi-4-mini Q4_K_M** with a 4096-token
   context budget. The stock `llama.cpp` server remains selectable
   per-deployment via `[services.llm].backend = "llama_cpp"`.

5. **TTS (Piper)** — streamed sentence-by-sentence so the first audible
   reply begins before the LLM finishes generating.

**Side outputs:** SQLite-backed household memory (scope, sensitivity,
spoken-policy filtering, durable promotion under `memory/MEMORY.md`),
and Home Assistant integration behind a final actuation safety gate
(rate limit + confirmation + audit).

### What stays local — always

- **audio capture** — never leaves the device
- **transcripts** — in-memory only, no disk write
- **LLM inference** — runs on the Jetson's GPU
- **household memory** — SQLite, never synced
- **Home Assistant traffic** — local network only
- **audit ledger** — append-only, on-device

The only network egress GenieClaw makes by default is the optional
`web_search` tool, which calls DuckDuckGo Instant Answer (no API key,
no account, no telemetry). Disable it via `[web_search] enabled = false`
and the appliance is fully air-gappable.

GenieClaw owns the **agent layer**: prompts, memory, tool routing, voice
orchestration, channel adapters. It does **not** own the LLM kernels (see
[`genie-ai-runtime`](https://github.com/GeniePod/genie-ai-runtime)) or the
eventual device-control runtime (`genie-home-runtime`, planned). See
[`ARCHITECTURE.md`](ARCHITECTURE.md) for the full stack.

## Roadmap

### Milestones

- [M1 — Stable Voice Loop on `genie-ai-runtime` v1](https://github.com/GeniePod/genie-claw/milestone/1) — *active*
- [M2 — Native Telegram, Stable Memory, Clarified Agent Harness](https://github.com/GeniePod/genie-claw/milestone/2) — *planned*
- [M3 — Smart Home Native: HA + `genie-home-runtime` + First Skill + Security Hardening](https://github.com/GeniePod/genie-claw/milestone/3) — *planned*
- [M4 — Community Buildout: Discord, X, Reddit, GitHub → 500 stars](https://github.com/GeniePod/genie-claw/milestone/4) — *planned*
- [M5 — Hardware, OS, and Mobile App: GeniePod Home V1 appliance shape](https://github.com/GeniePod/genie-claw/milestone/5) — *planned*
- [M6 — Ship GeniePod Home V1 + `genie-hub` skill ecosystem + premium audio](https://github.com/GeniePod/genie-claw/milestone/6) — *planned*
- [M7 — AI Model Optimization + Public Home Dataset + `genie-ai-model` Fine-Tuning](https://github.com/GeniePod/genie-claw/milestone/7) — *planned*
- [M8 — Satellite Devices: Contactless Sleep Tracker + Multi-Room Satellite Speaker](https://github.com/GeniePod/genie-claw/milestone/8) — *planned*
- [M9 — Product Line: GeniePod Home / Pro / Max + Tiered Stack Integration](https://github.com/GeniePod/genie-claw/milestone/9) — *planned*

### Milestone 1 — stable voice loop on `genie-ai-runtime` v1

The first milestone is intentionally narrow: stabilize one end-to-end path
(voice in, voice out) on the current first release of `genie-ai-runtime`,
and nothing else. Breadth comes after M1.

In scope:

- **system prompt path** — deterministic prompt assembly, reproducible across restarts, no silent prompt drift between runs
- **`genie-ai-runtime` v1 integration reliability** — every chat/voice cycle reaches the runtime, every response parses cleanly, every failure mode surfaces in `/api/health` and `genie-ctl status`
- **memory recall** — household context written to SQLite is retrievable and referenced in subsequent turns; recall failures are observable, not silent
- **tool dispatch** — the tool-call gate routes correctly, applies per-origin ACLs, rate-limits, and audits; every dispatched tool either completes or fails loudly
- **voice pipeline strength** — wake → VAD → STT → LLM → TTS round-trip under the alpha latency budget on Jetson Orin Nano Super 8 GB; no silent stalls, no torn audio, no stuck push-to-talk loops

Out of scope for M1:

- new channels beyond the existing voice + Telegram phase 2 bridges
- new skills / skill marketplace work
- Home Assistant feature expansion (current transitional adapter only)
- `genie-home-runtime` split-out
- hardware variants beyond Orin Nano Super 8 GB
- web UI features off the M1 observability path

PRs outside this scope are welcome but will be tagged `post-m1` and queued.

**Contribution surface during M1**: bug reports and PRs are welcome in **both**
[`GeniePod/genie-claw`](https://github.com/GeniePod/genie-claw) and
[`GeniePod/genie-ai-runtime`](https://github.com/GeniePod/genie-ai-runtime). If
a bug crosses the boundary, file it where the symptom appears; a maintainer
will move or mirror it.

M1 closes when, on a clean Jetson Orin Nano Super 8 GB:

- [ ] 100 consecutive voice cycles pass with zero stalls and zero silent drops
- [ ] system prompt SHA is identical across full-stack restart
- [ ] memory recall test set (≥ 20 cases) passes ≥ 95%
- [ ] tool dispatch ACL + rate-limit + audit log proven by integration test
- [ ] `genie-ai-runtime` v1 backend stable for 24h continuous run
- [ ] CI green on: fmt, clippy, test, aarch64 cross-compile, audit, deny, shellcheck, ruff, AI-attribution check, proof-checklist

---

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
- pluggable local LLM backend (`genie-ai-runtime` default on Jetson; `llama.cpp` remains selectable via `[services.llm].backend = "llama_cpp"`)
- a privacy-first and security-first system
- a memory-footprint-conscious runtime built for constrained edge hardware
- a household trust model that exposes redacted posture, not raw config files

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
- redacted `/api/security` posture for dashboard/support use instead of raw TOML exposure
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

1. The local model server defaults to `genie-ai-runtime` on Jetson; the
   legacy `llama.cpp` server remains selectable per-deployment via
   `[services.llm].backend = "llama_cpp"` in `geniepod.toml`.
   Backend identity flows through `LlmClient::backend_name()` into
   logs, `/api/health`, and `genie-ctl status` for operator visibility.
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
latency, and overall memory pressure. GenieClaw defaults to a 4096-token
runtime context on this class of device because larger contexts can be too
tight across full-stack restarts, causing slower replies or worse appliance
behavior.

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
- [doc/implementation-status.md](doc/implementation-status.md) for what is implemented, partial, external, and planned
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

### Recommended LLM Pairing

The bundled default is **Phi-4-mini Q4_K_M** on llama.cpp. `setup-jetson.sh`
auto-downloads it on first run.

For deployments that want stronger reasoning, cleaner JSON tool calls, and
better multilingual support (matching the per-language Piper voice models),
the recommended pairing is **Qwen3-4B Q4_K_M** running on
[`genie-ai-runtime`](https://github.com/GeniePod/genie-ai-runtime) once the
runtime backend is enabled (`[services.llm].backend = "genie_ai_runtime"`).
Qwen3-4B's slower per-token decode is exactly what genie-ai-runtime's
prefill and TTFT improvements address.

Phase 1 is opt-in only — Phi-4-mini remains the default:

```bash
# On the Jetson, after `make deploy`:
sudo /opt/geniepod/setup-jetson.sh --model qwen3-4b

# Then edit /etc/geniepod/geniepod.toml:
#   llm_model_name = "qwen"
#   llm_model_path = "/opt/geniepod/models/Qwen3-4B-Q4_K_M.gguf"
# And update GENIEPOD_LLM_MODEL in /etc/systemd/system/genie-llm.service,
# then: sudo systemctl restart genie-llm genie-core
```

See [issue #44](https://github.com/GeniePod/genie-claw/issues/44) for the
full rollout plan; flipping the default ships in Phase 2 alongside
[issue #33](https://github.com/GeniePod/genie-claw/issues/33).

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
- promoted durable memory is also projected into a local namespace tree under `memory/namespaces/`
- `memory/INDEX.md` acts as the generated entry point for the durable memory tree
- person/private/restricted durable namespace notes are kept structured, but non-shared-safe entries are redacted in the markdown projection by default

## Contributing

Quality, engineering, and bug fixes are always welcome. Every PR must include a **Real Behavior Proof** section in the description — a brief statement of what you ran, where you ran it, and what happened (Jetson hardware preferred). CI enforces the structure; reviewers read the content. See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide.

## Security

Found a vulnerability? **Do not open a public issue.** Email <contact@genieclaw.org> with the details. See [SECURITY.md](SECURITY.md) for the response timeline and scope.

## License

GNU Affero General Public License v3.0

See [LICENSE](LICENSE).
