# GenieClaw Documentation

This `doc/` directory is the entry point for the current repository
documentation. It covers the shipped surfaces in this repo and the intended
repo boundary inside the larger Genie ecosystem:

- workspace crates and binaries
- runtime services and process boundaries
- configuration and environment overrides
- HTTP APIs, CLI commands, and tool surfaces
- core subsystems such as memory, voice, security, and connectivity
- deployment assets and operational guidance
- repository layout and code ownership map
- long-term boundary with `genie-os`, `genie-home-runtime`, `genie-ai-runtime`, and app layers

Where current code is transitional, the docs call that out explicitly.

## Start Here

- [overview.md](overview.md): product purpose, runtime modes, and the main request flows
- [services-and-crates.md](services-and-crates.md): every crate, binary, and systemd service
- [configuration.md](configuration.md): config sections, fields, and environment overrides
- [http-and-cli.md](http-and-cli.md): `genie-core` HTTP API, `genie-api` dashboard API, and `genie-ctl`
- [core-subsystems.md](core-subsystems.md): LLM, prompt, tools, memory, voice, Telegram, security, and skills
- [deployment-and-ops.md](deployment-and-ops.md): local dev, Docker, Jetson deploy, systemd, and operations
- [repo-map.md](repo-map.md): top-level files, directories, and module map
- [research-agentic-ai.md](research-agentic-ai.md): research notes from current agentic AI application patterns and what GenieClaw adopts

## Runtime At A Glance

GenieClaw is a local-first home AI runtime centered on `genie-core`.

- `genie-core` is the main orchestrator.
  It serves the chat API on port `3000`, can run a local REPL on stdin, and can run the voice loop.
- `genie-api` is a separate dashboard/status service.
  It exposes dashboard HTML and system status backed by governor and health databases.
- `genie-governor` manages mode changes, memory-pressure reactions, and service lifecycle decisions.
- `genie-health` polls service endpoints and stores health history.
- `genie-ctl` is the local operator CLI.
- `llama-server` is external to this Rust workspace, but it is the default LLM backend expected by the deploy assets.

## Canonical Deep Dives Still Kept At Repo Root

The root-level documents remain useful and are still linked here instead of
being deleted or moved abruptly.

- [../README.md](../README.md): product summary and quick start
- [../GETTING_STARTED.md](../GETTING_STARTED.md): bring-up guide for dev machines and Jetson
- [../ARCHITECTURE.md](../ARCHITECTURE.md): Genie ecosystem and repo-boundary architecture
- [../CODEBASE.md](../CODEBASE.md): broader code walkthrough
- [../CONNECTIVITY.md](../CONNECTIVITY.md): ESP32-C6 boundary and split with `genie-os`
- [../VECTOR_MEMORY.md](../VECTOR_MEMORY.md): vector-memory design and rollout guidance
- Local-only `ROADMAP.md`, if present: private product and execution planning
- [../skills/SKILL-DEVELOPER-GUIDE.md](../skills/SKILL-DEVELOPER-GUIDE.md): native skill authoring

## Documentation Scope Notes

This doc set is complete for the current repository surfaces, but there are a
few intentional limits:

- Hardware behavior that depends on a specific Jetson image, kernel, or manual systemd override is documented as operational guidance, not as a stable code contract.
- `llama.cpp`, Home Assistant, Piper, Whisper, and Telegram Bot API internals are external dependencies. This repo documents how GenieClaw integrates with them, not their full upstream behavior.
- `genie-os`, `genie-home-runtime`, and `genie-ai-runtime` are documented as architectural boundaries unless code in this repo already implements a transitional adapter.
