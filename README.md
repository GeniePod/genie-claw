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

GenieClaw owns the **agent layer**: prompts, memory, tool routing, voice
orchestration, channel adapters. It does **not** own the LLM kernels (see
[`genie-ai-runtime`](https://github.com/GeniePod/genie-ai-runtime)) or the
eventual device-control runtime (`genie-home-runtime`, planned). See
[`ARCHITECTURE.md`](ARCHITECTURE.md) for the full stack.

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

## Alpha.5 Verified Deploy (2026-05-11)

End-to-end deploy from an x86_64 build VM (`tiny@tiny-virtual-machine`) to a
Jetson Orin Nano (`aihpc@192.168.55.1`) confirms the alpha.5 workflow: the
build host only cross-compiles and SCPs; the target receives binaries,
canonical config, systemd units, and helper scripts in one `make deploy`,
then `setup-jetson.sh` audits every prereq before enabling services.

### From the build host (cross-compile + deploy via SSH)

```
tiny@tiny-virtual-machine:~/genie-claw$ make deploy JETSON_HOST=192.168.55.1 JETSON_USER=aihpc

CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
cargo build --release --target aarch64-unknown-linux-gnu -p genie-core
    Finished `release` profile [optimized] target(s) in 0.32s
cargo build --release --target aarch64-unknown-linux-gnu -p genie-ctl -p genie-governor -p genie-health -p genie-api
    Finished `release` profile [optimized] target(s) in 0.28s

Jetson binaries:
-rwxrwxr-x  2.2M  target/aarch64-unknown-linux-gnu/release/genie-api
-rwxrwxr-x  4.3M  target/aarch64-unknown-linux-gnu/release/genie-core
-rwxrwxr-x  917K  target/aarch64-unknown-linux-gnu/release/genie-ctl
-rwxrwxr-x  2.3M  target/aarch64-unknown-linux-gnu/release/genie-governor
-rwxrwxr-x  2.2M  target/aarch64-unknown-linux-gnu/release/genie-health

# deploy-binaries: scp + sudo mv for each of the 5 aarch64 binaries
genie-core      100% 4317KB   7.0MB/s   00:00
genie-ctl       100%  917KB   6.5MB/s   00:00
genie-governor  100% 2282KB   9.2MB/s   00:00
genie-health    100% 2157KB  10.5MB/s   00:00
genie-api       100% 2249KB  10.3MB/s   00:00

# deploy-config: force-overwrite — repo is the single source of truth
Config deployed — /etc/geniepod/geniepod.toml refreshed from repo.
WARNING: any hand-edits on the target were overwritten. Keep secrets in env vars
         (HA_TOKEN, TELEGRAM_BOT_TOKEN, etc.), not in geniepod.toml directly.

# deploy-systemd: 11 units (9 .service + 2 .target)
# deploy-docker:  compose.yml -> /opt/geniepod/docker/
# deploy-setup:   5 helper scripts + setup-jetson.sh -> /opt/geniepod/bin/

=== Deployed to aihpc@192.168.55.1 ===
  Binaries: /opt/geniepod/bin/
  Config:   /etc/geniepod/
  Systemd:  /etc/systemd/system/

Run first-time setup on the Jetson:
  ssh aihpc@192.168.55.1 'bash /opt/geniepod/setup-jetson.sh'
```

### On the Jetson (first-time setup audit)

```
aihpc@ubuntu:/opt/geniepod$ bash /opt/geniepod/setup-jetson.sh
=== GeniePod Jetson Setup ===

[1/6] Creating directories...
[2/6] Checking binaries...
  OK: genie-core (4.3M)
  OK: genie-governor (2.3M)
  OK: genie-health (2.2M)
  OK: genie-api (2.2M)
  OK: genie-ctl (920K)
  OK: genie-audio-init (4.0K)
[3/6] Checking config...
  OK: /etc/geniepod/geniepod.toml
  Secured config permissions
[4/6] Checking LLM model...
  OK: phi-4-mini-instruct-q4_k_m.gguf (2.4G)
[5/6] Checking llama.cpp...
  OK: llama-server
  OK: docker compose
[5b/6] Setting Jetson performance mode...
  Set nvpmodel to mode 1 (25W / max speed)
  Clocks locked to max
[5c/6] Applying memory optimizations...
  sysctl already configured
[5e/6] Checking voice runtime prerequisites...
  OK: whisper-cli (928K) at /opt/geniepod/bin/whisper-cli
  OK: ggml-small.bin (466M)
  OK: piper (5.1M) at /opt/geniepod/piper/piper
  OK: en_US-amy-medium.onnx (61M)
[6/6] Enabling systemd services...
  Enabled: geniepod.target
  Enabled: homeassistant
  Enabled: genie-audio
  Enabled: genie-llm
  Enabled: genie-core
  Enabled: genie-governor
  Enabled: genie-health
  Enabled: genie-api
  Enabled: genie-mqtt
[genie-audio-init] WARN: card APE has no I2S2 controls — overlay not applied? skipping route setup.

=== Setup complete ===
```

The final `[genie-audio-init] WARN` is benign in this snapshot — the Jetson's
40-pin I2S2 overlay was not active at this run; on hosts where the overlay is
applied via `sudo /opt/nvidia/jetson-io/jetson-io.py`, the same script
reports each of the 10 amixer `cset` lines and exits with status 0. See
[`doc/lyrat-jetson-audio.md`](doc/lyrat-jetson-audio.md) for the full LyraT
audio frontend setup.

## Alpha.7 Verified Voice Cycle (2026-05-13)

After the alpha.7 PRs landed —
[#13](https://github.com/GeniePod/genie-claw/pull/13) (DeepFilterNet capture
denoise), [#16](https://github.com/GeniePod/genie-claw/pull/16) (half-duplex
post-TTS gate), [#18](https://github.com/GeniePod/genie-claw/pull/18)
(`genie-whisper-warmup.service`), and
[#20](https://github.com/GeniePod/genie-claw/pull/20) (first-reply latency
banner) — a fresh push-to-talk run on the same Jetson Orin Nano + LyraT V4.3
hardware reports the following first-cycle behavior:

```
aihpc@ubuntu:~$ sudo -E /opt/geniepod/bin/genie-core
[voice] Capture device: plughw:APE,0  |  Playback device: plughw:0,0
INFO STT using long-running whisper-server (model stays loaded in GPU) port=8178 model=/opt/geniepod/models/ggml-small.bin
[voice] LLM server connected

=== GeniePod Voice Mode (Push-to-Talk) ===
Press Enter to speak (3 sec), 'quit' to exit.

[voice] Press Enter to speak >
[voice] Recording 3 seconds — speak now!
INFO recording complete path=/tmp/geniepod-rec-23998.wav size_bytes=288044
INFO preprocessed audio with DeepFilterNet chain (bandpass, deep-filter denoise, peak-normalize -3 dBFS) dfn_ms=808 atten_lim_db=100.0
[voice] Transcribing...
[voice] You said: "Hello, this is Christine." (STT: 285 ms)
[voice] Thinking...
[voice] Tool: memory_recall → Your name is Jared
[voice] Speaking...
INFO Piper generated audio, playing... pcm_bytes=216808
[voice] GeniePod: Understood, Christine is Jared. How can I assist you further? (LLM+TTS: 1018 ms)

=== first voice reply latency ===
  speech end -> STT done:      285 ms
  STT done   -> first audio:   3679 ms
  total (first reply):         3964 ms
=================================

[voice] Total cycle: 15395 ms
```

What this confirms:

- **DFN denoise** runs in ~808 ms per 3 s capture (`dfn_ms=808`); whisper
  receives clean audio.
- **Half-duplex gate** keeps Piper's previous response out of the next
  capture — STT correctly transcribes "Hello, this is Christine." instead
  of bleeding the assistant's voice.
- **Whisper warmup** has the model resident in iGPU: STT 285 ms warm,
  vs the 60-90 s cold path before #18.
- **Memory recall** tool fires and surfaces "Your name is Jared" from
  the durable namespace tree.
- **First-reply banner** prints a one-shot 3-line summary on the first
  successful cycle, then stops. (Latest `main` further decomposes the
  `STT done -> first audio` line into LLM-until-first-sentence and
  TTS-first-synth phases for diagnostic clarity — see
  [#20](https://github.com/GeniePod/genie-claw/pull/20).)

Total first-reply latency of **~4 seconds** from end-of-user-speech to
first audible TTS audio, on a 7.6 GB Orin Nano running Phi-4-mini Q4_K_M
LLM + whisper-small + Piper en_US-amy concurrently.

## License

GNU Affero General Public License v3.0

See [LICENSE](LICENSE).
