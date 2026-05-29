# GenieClaw

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
  `genie-ctl bfcl-predict-quick`, and `genie-ctl bfcl-predict-llm`
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
