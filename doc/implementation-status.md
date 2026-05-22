# Implementation Status

Last reconciled against the repository code: 2026-05-07.

This page is the source of truth for what is implemented in this repository
versus what is architecture, transitional integration, or future ecosystem work.

Status labels:

- **Implemented:** code exists in this repo and is wired into a runtime, CLI, API, or deploy surface.
- **Partial:** a useful boundary or first implementation exists, but production completeness depends on external services, hardware, or later work.
- **External:** this repo integrates with the component, but does not implement it.
- **Planned:** documented architecture only; not implemented here.

## Implemented In This Repo

| Area | Status | Evidence | Notes |
| --- | --- | --- | --- |
| Rust workspace split | Implemented | `crates/genie-core`, `genie-api`, `genie-common`, `genie-governor`, `genie-health`, `genie-ctl`, `genie-skill-sdk` | Current repo is the GenieClaw agent/runtime workspace. |
| Chat HTTP server | Implemented | `crates/genie-core/src/server.rs` | Includes local chat UI, `/api/chat`, `/api/chat/stream`, conversation history/export, `/api/health`, `/api/runtime/contract`, and OpenAI-compatible `/v1/chat/completions`. |
| Operator CLI | Implemented | `crates/genie-ctl/src/main.rs` | Supports status, chat, search, tools, skills, speaker profiles, connectivity, health, conversations, diagnostics, support bundles, and version. |
| Dashboard/status service | Implemented | `crates/genie-api/src/routes.rs` | Lightweight local dashboard/status API, not the final product app. |
| Conversation persistence | Implemented | `crates/genie-core/src/conversation.rs` | SQLite conversation store with recent history and export flow. |
| Household memory | Implemented | `crates/genie-core/src/memory/*` | SQLite + FTS memory, extraction, recall, injection, decay, policy metadata, canonical markdown artifacts, namespace projection, dashboard edit/delete/reorder endpoints. |
| Prompt and reasoning policy | Implemented | `crates/genie-core/src/prompt.rs`, `crates/genie-core/src/reasoning.rs` | Model-family prompt selection and no-think/think-mode application by interaction kind. |
| Built-in tool dispatcher | Implemented | `crates/genie-core/src/tools/*` | Time, calculator, weather, system info, timers, memory tools, media hook, optional home tools, optional web search, quick router, policy checks, and audit logging. |
| Web search tool | Implemented | `crates/genie-core/src/tools/web_search.rs` | No-key DuckDuckGo provider and optional SearXNG provider with cache and sensitive-query blocking. It is a lightweight search tool, not a full browser/research crawler. |
| Home action safety in agent layer | Implemented | `crates/genie-core/src/tools/actuation.rs`, `crates/genie-core/src/ha/policy.rs` | Origin allowlist, target confidence thresholds, sensitive-action confirmation tokens, rate limits, action ledger, bounded undo, and append-only audit log. |
| Runtime contract | Implemented | `crates/genie-core/src/runtime_contract.rs`, `crates/genie-core/src/server.rs` | Prompt/tool/policy/hydration fingerprints, optional drift detection, and boot contract log. |
| Household security posture | Implemented | `crates/genie-api/src/routes.rs`, `crates/genie-core/src/security/*` | Dashboard exposes redacted posture instead of raw config. Security helpers include audit, credential isolation, env sanitization, injection scanning, loop guard, sandbox helpers, and taint tracking. |
| Voice pipeline modules | Transitional | `crates/genie-core/src/voice_loop.rs`, `crates/genie-core/src/voice/*` | Current Jetson alpha bring-up path. Long-term ownership belongs in `genie-voice-runtime`; GenieClaw should consume transcripts and issue speak commands rather than own wake/VAD/STT/TTS/audio. |
| Optional local speaker identity | Implemented | `crates/genie-core/src/voice/identity.rs`, `crates/genie-ctl/src/main.rs` | Local WAV-derived profile enrollment/matching and voice memory-context routing. This is household routing, not hostile-user authentication. |
| Native skill loading | Implemented | `crates/genie-core/src/skills/*`, `crates/genie-skill-sdk/*` | Loads native `.so` skills through a narrow ABI, exposes skills as tools, and audits sidecar manifest metadata. |
| Skill policy | Implemented | `crates/genie-common/src/config.rs`, `crates/genie-core/src/skills/loader.rs` | Can require manifests, require signature material presence, and deny permission labels. Cryptographic signature verification is not implemented. |
| Telegram channel | Implemented | `crates/genie-core/src/telegram.rs` | Long-poll Telegram adapter gated by feature/config and chat ID policy. |
| Governor service | Implemented | `crates/genie-governor/src/*` | Mode control, pressure checks, tegrastats/memory sampling, and Unix socket control. |
| Health service | Implemented | `crates/genie-health/src/*` | Polls configured endpoints and stores service health history. |
| Deploy assets | Implemented | `deploy/` | Jetson setup script, configs, systemd units, Docker assets, wake/audio helper scripts. |

## Partial Or Transitional

| Area | Status | What Exists | What Is Still Missing |
| --- | --- | --- | --- |
| Home Assistant integration | Partial / transitional | Provider boundary, status/control/history/undo, local action safety, HA token/config path | Home Assistant is not reimplemented in Rust here. Final device graph, automations, and deterministic physical safety belong in `genie-home-runtime`. |
| LLM runtime | External / transitional | OpenAI-compatible client points at local `llama-server`; deploy units configure `llama.cpp` | Jetson-only optimized C++ runtime, CUDA kernels, memory planner, and model-serving replacement belong in `genie-ai-runtime`. |
| Voice multilingual support | Partial | STT language hint/auto mode, language detection, and optional per-language Piper model selection | Full quality for Chinese, Spanish, German, etc. depends on installed Whisper/Piper models and device testing. It is not a certified full-language product yet. |
| Speaker recognition | Partial | Local acoustic fingerprints from WAV profiles and runtime matching | Not robust biometric authentication, anti-spoofing, enrollment UX, or security-grade identity. |
| ESP32-C6 connectivity | Partial boundary | Config, status endpoint, capability model, UART path validation, Thread/Matter capability intent | No real UART protocol controller, no Thread/Matter stack, no ESP-Hosted-NG implementation in this repo. ESP-Hosted-NG belongs in `genie-os`; protocol ownership belongs in `genie-home-runtime`/connectivity services. |
| Native skill security | Partial | ABI boundary, manifest audit, configurable load policy, signature material presence check | No cryptographic signature verification, process isolation, syscall sandbox, marketplace, or full permission broker. |
| Memory dashboard | Partial / implemented admin surface | HTTP endpoints and dashboard proxy for list/update/delete/reorder | Product-grade UI/UX, conflict resolution, and multi-device sync are app-layer work. |
| Web/mobile application layer | Partial | Lightweight local dashboard and chat UI | Full installer, setup, mobile app, push notifications, and polished memory manager are not implemented here. |
| OTA/update flow | Partial | Update-check module and CLI command | Full signed OTA channels, rollback, fleet management, and image-level update ownership belong to `genie-os`. |

## Not Implemented In This Repo

| Area | Status | Correct Owner |
| --- | --- | --- |
| `genie-home-runtime` | Planned / separate repo | Rust AI-native home automation engine, device graph, automations, MCP server, deterministic final physical safety layer. |
| `genie-ai-runtime` | Planned / separate repo | Jetson-only C++ inference runtime customized from `llama.cpp`, optimized CUDA kernels, and memory planner. |
| `genie-voice-runtime` | Initial / separate repo | External voice runtime for wake, VAD, STT, TTS, audio streaming, and voice session protocol. |
| `genie-os` | Planned / separate repo | Custom L4T image, board bring-up, drivers, OTA base image, service supervision, ESP-Hosted-NG OS integration. |
| Full Matter/Thread/Zigbee/BLE production stack | Planned outside this repo | Lower connectivity/home-runtime layers. |
| Full vector/cuVS semantic memory backend | Planned design only | `VECTOR_MEMORY.md` describes the rollout. Current runtime uses SQLite FTS, not embeddings/vector search. |
| Cryptographic skill signing and sandboxed marketplace | Planned | Later signed skill platform. Current code only audits manifests and signature material presence. |
| Security-grade biometric authorization | Planned / not a goal for current provider | Current local speaker identity is for household memory routing, not locks/payments/hostile-user isolation. |
| Cloud account system or multi-tenant hosted gateway | Not planned for this repo | GenieClaw is local-first appliance software. |

## Current Alpha Truth

The workspace version is currently `1.0.0-alpha.4`.

The `Unreleased` changelog contains next-alpha work already implemented in the
codebase, including safer localhost defaults, origin-aware first-party requests,
and local speaker-profile management. Those features are present in source, but
the crate version has not yet been bumped to a new alpha release.

## How To Keep This Page Honest

When adding a feature:

1. Add or update code/tests.
2. Update the relevant subsystem doc.
3. Update this status page with `Implemented`, `Partial`, `External`, or `Planned`.
4. Avoid saying “fully supported” unless the repo contains the runtime code,
   tests, deploy path, and operational guidance for that claim.
