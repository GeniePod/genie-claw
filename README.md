# GenieClaw

[![Discord](https://img.shields.io/badge/Discord-Join-5865F2?logo=discord&logoColor=white)](https://discord.gg/r6B22DP83k)
[![CI](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/ci.yml)
[![Jetson cross-compile](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/cross.yml)
[![Audit](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml/badge.svg)](https://github.com/GeniePod/genie-claw/actions/workflows/audit.yml)

**Low-latency, limited-context AI harness for private on-device homes.**

GenieClaw is the Rust agent layer native to NVIDIA Jetson Orin 8GB. It is built
for small local models, tight VRAM budgets, and a 4096-token Jetson baseline.
This repo owns prompt assembly, memory, tool routing, smart-home intent, safety
policy, audit, and channel/session adapters.

The product goal is a private household agent that is fast because it receives
the right family memory, room/device state, and safety context, not because it
sends large prompts to a remote model.

This is a real engineering project, not a toy demo or token-burning issue
target. The OpenClaw engineering posture here is simple: make the local agent
more native, deterministic, measurable, and reliable on Jetson-class hardware.

The default agent contract is intentionally small: the Jetson profile uses
`[agent].context_window_tokens = 4096`. Larger adaptive contexts can exist for
stronger models, but provider/runtime paths must pass the 4096-token harness
first.

## The Edge Bet

The hard version of "edge AI" is delivering — on one private device, with no
cloud — the bundle the industry says needs a data center:

- **quick response** — no network round-trip
- **on-device processing** — everything runs on the Jetson Orin Nano 8 GB
- **high accuracy for the home** — the hard one
- **data privacy** — household and family data never leave the device
- **energy efficiency** — small quantized models inside a tight power and memory budget
- **zero subscription** — fully local; the user owns it, with no recurring cloud cost

These pillars fight each other. Accuracy usually buys itself with a bigger model
on a cloud GPU — which immediately costs you on-device, energy, privacy, and the
no-subscription promise. GenieClaw resolves that tension with one bet:

> **Accuracy comes from deterministic grounding — family memory and live
> room/device state — not from model scale.**

A small local model reaches household-class tool-call accuracy because it is
handed the right grounded context and its outputs are resolved against real
device state, not because it ships a large prompt to a remote model. That is the
keystone: hold the accuracy pillar with grounding and the other five stay
affordable. The BFCL harness and the 4096-token Jetson baseline exist to keep
that bet honest and measurable — accuracy is earned against grounded device
state, not asserted.

## What Works Today

- local chat through `genie-core`
- transitional voice-session adapter
- LLM backend facade for `genie-ai-runtime` and selectable `llama.cpp`
- SQLite conversation history and policy-aware family/household memory
- Home Assistant adapter with confirmations, rate limits, and audit logging
- local HTTP API, dashboard, CLI, health service, and governor service
- optional `web_search` tool with DuckDuckGo or SearXNG
- cache-aware `genie-ai-runtime` requests with `conversation_id`,
  `nvext.agent_hints`, and system-prompt prefix cache metadata for KV reuse
- system-prompt SHA exposed in boot logs, `/api/health`, and `genie-ctl status`
  to prove deterministic prompt assembly across restarts
- BFCL-style local tool-call scoring through `genie-ctl bfcl-score`,
  `genie-ctl bfcl-score-llm`, `genie-ctl bfcl-predict-quick`, and
  `genie-ctl bfcl-predict-llm`
- Jetson aarch64 cross-compile CI

Current workspace version: `v1.0.0-alpha.10`.

## Current Focus

- BFCL scoring for quick-router and local-LLM tool-call accuracy is the
  immediate product gate
- keep the agent fast and reliable inside a 4096-token Jetson context
- tune the AI harness around high-signal home context, family memory, and typed tools
- improve accuracy through deterministic device state and memory retrieval, not larger prompts
- validate hardware-facing and performance-sensitive changes on Jetson Orin Nano 8GB whenever possible
- reject broad changes that make the agent less native, slower, less deterministic, or harder to test

Everything else is noise until the local home agent is fast, accurate, and
measurable under the Jetson 4096-token constraint. Routing, memory retrieval,
typed tools, BFCL score, and Jetson behavior are the work.

## Milestones

GenieClaw tracks three open milestones. The text below is kept word-for-word
identical to the GitHub milestone descriptions and the milestone cards on
[genieclaw.org](https://genieclaw.org) — one source of truth. If you are
opening an issue or PR, the milestone it belongs to (or doesn't) decides
whether it lands.

### [M1 — Jetson 4096-token BFCL Agent Harness](https://github.com/GeniePod/genie-claw/milestone/1) · in progress

Keep GenieClaw fast, reliable, and measurable on NVIDIA Jetson Orin Nano 8 GB
with a 4096-token local context.

**In scope:**

- BFCL quick-router and local-LLM tool-call scoring
- High-signal home / family memory fixtures
- Deterministic device state
- Typed-tool routing
- Memory retrieval accuracy
- Compact prompt and tool budgeting
- Jetson / aarch64 CI and Jetson hardware validation when possible

**Out of scope:**

- Broad prompt growth
- Generic chatbot or provider churn
- UI, product, community, or hardware work
- Toy demos
- Untested native or runtime changes
- PRs that make the agent less native, slower, less deterministic, or harder to test

### [M2 — Portable Providers and Channel Boundaries](https://github.com/GeniePod/genie-claw/milestone/2) · next

Make GenieClaw portable without weakening the 4096-token Jetson baseline.

**In scope:**

- Channel and session adapters
- Provider configuration
- Optional API-key providers behind explicit gates
- Memory and channel reliability

**Out of scope:**

- Voice-runtime internals
- Hardware variants
- Mobile apps
- OS images
- Community-growth goals

### [M3 — Home Runtime Boundary and Skill Safety](https://github.com/GeniePod/genie-claw/milestone/3) · later

Harden the smart-home agent boundary.

**In scope:**

- Home Assistant provider cleanup
- Explicit handoff to the planned genie-home-runtime
- Native skill policy
- Sandbox and audit requirements
- Final actuation safety contracts

**Out of scope:**

- Building hardware
- OS images
- Mobile apps
- Marketplace or community campaigns
- Voice and audio pipeline internals

### Active Contribution Gate

We are working M1 now. A PR that is technically correct but outside the M1
in-scope list is noise for this phase and will be closed.

Valuable contributions are the ones that help this repository become what it
is intended to be: a private, local, deterministic household agent that can
run well on NVIDIA Jetson Orin Nano 8 GB hardware. Spam-like PRs, AI-generated
issue churn, duplicate reports, unplanned bug-fix batches, or changes without
real behavior proof will be closed immediately to protect review quality.

### Accepted contribution scope

A PR is accepted **only** if it lands in one of these two buckets, with
reproducible on-device proof. Anything else will be closed.

> 💎 **Performance PRs are rewarded.** Land a performance-improvement PR that
> meets the rules below — measurable Jetson win, reproducible before→after proof —
> and you're eligible for a reward through [gittensor](https://gittensor.io/),
> the Bittensor subnet that pays out for merged open-source contributions.

1. **Performance improvement** — measurable latency / throughput / memory wins
   on Jetson Orin Nano 8 GB, with before→after numbers.
   - e.g. [genie-ai-runtime#85](https://github.com/GeniePod/genie-ai-runtime/pull/85)
     — in-memory KV prefix cache, **~13× faster prefill** (16s → ~1s per
     command); cut the BFCL eval from ~62 min to ~20 min.

2. **Tool-dispatch / real-Home-Assistant correctness** — fixes to tool routing,
   tool-call arguments, or home actuation, **measured** (BFCL) and/or
   **reproduced against a real Home Assistant**. A runnable sample HA config is
   provided at [`deploy/homeassistant/`](deploy/homeassistant/) so you can
   reproduce the failure and prove the fix.
   - **Accuracy, measured:** [#399](https://github.com/GeniePod/genie-claw/pull/399)
     — ground the predict prompt in the home device catalog: raw BFCL strict
     **20.19% → 50.96%**, grounded **72.12% → 82.69%** (Qwen3-4B @ 4096, same
     model — deterministic device-state grounding, not scale);
     [#390](https://github.com/GeniePod/genie-claw/pull/390) — action-synonym
     canonicalization + wrong-room fidelity guard;
     [#388](https://github.com/GeniePod/genie-claw/pull/388) — grounded
     entity-argument metric.
   - **Live-HA actuation:** [#400](https://github.com/GeniePod/genie-claw/pull/400)
     — canonicalize `home_control` action synonyms. *Before:* the model emits
     `"turn off"`, the runtime rejects it (*"action 'turn off' is invalid"*) and
     the light stays on. *After:* `"turn off" → "turn_off"`, and
     `light.kitchen_lights` goes `off → on`, confirmed via the HA API. Also
     [#380](https://github.com/GeniePod/genie-claw/pull/380) — stop leaking
     unparsed tool-call JSON to the user.

Every such PR needs a **Real Behavior Proof**: what you ran, on what hardware,
and what changed — for HA fixes, live-HA before/after confirmed via the API.
No reproducible proof, or outside these two buckets → closed.

## Product Quality Bar

PRs must improve the product behavior or make it easier to measure product
behavior. Low-signal generated code, demo-only routes, prompt growth without a
measured accuracy gain, and feature churn that is not tested against the agent
harness should be closed.

Every non-trivial PR should answer:

- what home-agent behavior changed
- how it affects the 4096-token harness
- which typed tools, memory retrieval paths, or deterministic device-state paths
  it improves
- what was tested locally, in CI, and, when relevant, on Jetson Orin Nano 8GB
- whether any Jetson validation gap remains

Docs-only changes can use static checks. Code that touches routing, memory,
tool calls, home state, prompt assembly, latency, or hardware behavior needs
real tests. If Jetson testing is not possible before opening a PR, state that
gap directly and keep the change small enough to review and reproduce.

## Immediate Engineering Plan

1. Run BFCL quick-router and local-LLM suites for tool routing, memory retrieval,
   and typed-tool changes.
2. Expand BFCL fixtures for home state, family memory, STT-like noise, and typed tools.
3. Score expected tool names and arguments, not just natural-language answers.
4. Add BFCL score thresholds to CI as a required regression signal.
5. Keep a Jetson Orin Nano 8GB validation path for
   latency, memory pressure, and native runtime behavior.
6. Use the scores to improve routing, memory retrieval, and typed-tool accuracy
   before expanding prompts or adding broader features.

## Agent Harness Contract

The repo now has explicit code-level contract surfaces for the new direction:

- `genie_core::agent_harness` checks prompt, tool manifest, memory hydration,
  response reserve, and optional provider context against the Jetson 4096-token
  baseline.
- `genie_core::llm::LlmRequestHints` carries session id, expected output
  length, priority, short-lived cache TTL, and stable system-prompt prefix
  cache metadata to runtimes that understand the `nvext` extension.
- `[agent]` in `geniepod.toml` selects the maintained runtime profile:
  `jetson`, `raspberry_pi`, `portable_sbc`, `laptop`, or `mac`.
- Alternate providers and profiles must keep their configured context at or
  below `[agent].context_window_tokens` unless a specific test intentionally
  proves a larger-context path without weakening the Jetson baseline.

## Install

Install the prebuilt runtime binaries (Linux **aarch64** / Jetson and **x86_64**):

```bash
curl -fsSL https://github.com/GeniePod/genie-claw/releases/latest/download/install.sh | sh
```

This installs the five runtime binaries to `/usr/local/bin` (or `~/.local/bin`)
and writes a starter config to `~/.config/geniepod/geniepod.toml`. Then:

```bash
GENIEPOD_CONFIG=~/.config/geniepod/geniepod.toml genie-core   # agent runtime + HTTP API
genie-ctl --help
```

> While GenieClaw is in the release-candidate phase, `releases/latest` has no
> stable build yet — pin the version:
> ```bash
> GENIECLAW_VERSION=v1.0.0-rc.1 \
>   sh -c "$(curl -fsSL https://github.com/GeniePod/genie-claw/releases/download/v1.0.0-rc.1/install.sh)"
> ```

The full Jetson voice / Home Assistant deployment stays in
[`GETTING_STARTED.md`](GETTING_STARTED.md) and `deploy/setup-jetson.sh`.

## Quick Start (from source)

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
| `genie-ctl` | Local CLI for chat, status, tools, BFCL scoring, health, and diagnostics |
| `genie-governor` | Resource governor and service lifecycle controller |
| `genie-health` | Local health polling and alert forwarding |
| `genie-api` | Lightweight local dashboard |
| `genie-skill-sdk` | Rust SDK for native shared-library skills |

## Documentation

- [`GETTING_STARTED.md`](GETTING_STARTED.md) - local dev, Docker, Jetson bring-up, and deploy
- [`LOW_LATENCY_HOME_AGENT.md`](LOW_LATENCY_HOME_AGENT.md) - product goal for the low-latency private home harness
- [`ARCHITECTURE.md`](ARCHITECTURE.md) - Genie ecosystem architecture
- [`doc/README.md`](doc/README.md) - documentation map
- [`doc/implementation-status.md`](doc/implementation-status.md) - implemented, partial, external, and planned work
- [`CHANGELOG.md`](CHANGELOG.md) - alpha release notes
- [`CONTRIBUTING.md`](CONTRIBUTING.md) - PR and proof requirements
- [`SECURITY.md`](SECURITY.md) - vulnerability reporting

## Contributing

Every PR needs a **Real Behavior Proof** section: what you ran, where you ran it,
which profile or hardware it represents (`jetson`, `raspberry_pi`,
`portable_sbc`, `laptop`, or `mac`), and what happened. CI/local proof is
enough for docs, harness, provider, and non-hardware work. Hardware-facing
changes should include Jetson/device proof or state the validation gap clearly.

## License

GNU Affero General Public License v3.0. See [`LICENSE`](LICENSE).
