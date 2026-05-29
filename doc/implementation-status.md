# Implementation Status

Last reconciled against the repository code: 2026-05-29.

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
| Household memory | Implemented | `crates/genie-core/src/memory/*` | SQLite + FTS memory, local semantic recall, typed indexes for profiles, profile attributes such as shoe sizes, device aliases, rules, notes, media profiles, calendar events including lessons/medical/vet appointments, shopping-list items, pantry inventory, access permissions, chore/task logs including hygiene and pet-care checks, household schedules including school transport/sunset/community/business/channel-guide/subscription/TV/community-meeting hours, appliance/environment/location/presence/waste/access/finance/market/fitness/health/security/payment event logs, app-only sensitive references, contextual comfort/routine/safety/report semantic recall, extraction, recall, injection, decay, policy metadata, canonical markdown artifacts, namespace projection, dashboard edit/delete/reorder endpoints. |
| Prompt and reasoning policy | Implemented | `crates/genie-core/src/prompt.rs`, `crates/genie-core/src/reasoning.rs` | Model-family prompt selection and no-think/think-mode application by interaction kind. |
| Built-in tool dispatcher | Implemented | `crates/genie-core/src/tools/*` | Time, calculator, weather, system info, timers, memory tools, media hook, optional home tools, optional web search, quick router, policy checks, and audit logging. |
| Web search tool | Implemented | `crates/genie-core/src/tools/web_search.rs` | No-key DuckDuckGo provider and optional SearXNG provider with cache and sensitive-query blocking. It is a lightweight search tool, not a full browser/research crawler. |
| Home action safety in agent layer | Implemented | `crates/genie-core/src/tools/actuation.rs`, `crates/genie-core/src/ha/policy.rs` | Origin allowlist, target confidence thresholds, sensitive-action confirmation tokens, rate limits, action ledger, bounded undo, and append-only audit log. |
| Runtime contract | Implemented | `crates/genie-core/src/runtime_contract.rs`, `crates/genie-core/src/server.rs` | Prompt/tool/policy/hydration fingerprints, optional drift detection, and boot contract log. |
| Household security posture | Implemented | `crates/genie-api/src/routes.rs`, `crates/genie-core/src/security/*` | Dashboard exposes redacted posture instead of raw config. Security helpers include audit, credential isolation, env sanitization, injection scanning, loop guard, sandbox helpers, and taint tracking. |
| Voice pipeline modules | Transitional | `crates/genie-core/src/voice_loop.rs`, `crates/genie-core/src/voice/*` | Current Jetson alpha bring-up path. Long-term ownership belongs behind an external voice boundary; GenieClaw should consume transcripts and issue speak commands rather than own wake/VAD/STT/TTS/audio. |
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
| Home Assistant integration | Partial / transitional | Provider boundary, status/control/history/undo, local action safety, HA token/config path | Home Assistant is not reimplemented in Rust here. Seasonal/device actions such as driveway ice checks, front-gate state, holiday lights, sprinklers, robot mower, porch-light arrival triggers, rain-arrival, vacation-mode, storm-prep, smoke/gas/outlet-spill safety, working-late, fall-alert, and parking-lot safety routines, movie-night/away-mode/dinner-prep/study/focus/cozy/reading/night-hallway/work-from-home/all-off/nap/bedtime-override scenes, quiet porch/piano/toddler-safe modes, robot-vacuum and lock-except controls, TV/alarm/network-pause controls, fireplace/ventilation/upstairs-light controls, smoke-detector tests, lock-up/locked-out confirmation flows, phone finder, connected-car warmup/navigation, baby-monitor/iron/water-heater/status, mailbox/freezer/freezer-door telemetry, tire-pressure/dryer/car/stove/oven/package/garage-door/sous-vide/sump-pump/water-pressure/nursery-air-quality/self-cleaning-oven/camera-motion/unlocked-door/window/electricity/sprinkler-history/morning-readiness status, solar generation status, basement/attic environment status, and slow-cooker control route through this boundary when configured. Final device graph, automations, and deterministic physical safety belong behind an external home boundary. |
| LLM runtime | External / integrated | `crates/genie-core/src/llm/*`, `deploy/systemd/genie-ai-runtime.service`, `[services.llm].backend = "genie_ai_runtime"` | Jetson deploys default to the external `genie-ai-runtime` on `:8080`; `llama.cpp` remains a selectable fallback/development backend. OpenAI-compatible/API/OAuth providers are transitional development and testing adapters only. |
| Voice multilingual support | Partial | STT language hint/auto mode, language detection, and optional per-language Piper model selection | Full quality for Chinese, Spanish, German, etc. depends on installed Whisper/Piper models and device testing. It is not a certified full-language product yet. |
| Speaker recognition | Partial | Local acoustic fingerprints from WAV profiles and runtime matching | Not robust biometric authentication, anti-spoofing, enrollment UX, or security-grade identity. |
| ESP32-C6 connectivity | Partial boundary | Config, status endpoint, capability model, UART path validation, Thread/Matter capability intent | No real UART protocol controller, no Thread/Matter stack, no ESP-Hosted-NG implementation in this repo. ESP-Hosted-NG belongs in the platform/OS layer; protocol ownership belongs in external home/connectivity services. |
| Native skill security | Partial | ABI boundary, manifest audit, configurable load policy, signature material presence check | No cryptographic signature verification, process isolation, syscall sandbox, marketplace, or full permission broker. |
| Memory dashboard | Partial / implemented admin surface | HTTP endpoints and dashboard proxy for list/update/delete/reorder | Product-grade UI/UX, conflict resolution, and multi-device sync are app-layer work. |
| Web/mobile application layer | Partial | Lightweight local dashboard and chat UI | Full installer, setup, mobile app, push notifications, and polished memory manager are not implemented here. |
| OTA/update flow | Partial | Update-check module and CLI command | Full signed OTA channels, rollback, fleet management, and image-level update ownership belong to the platform/OS layer. |

## Not Implemented In This Repo

| Area | Status | Correct Owner |
| --- | --- | --- |
| External home boundary | Planned | AI-native home automation engine, device graph, automations, MCP/server API, deterministic final physical safety layer. |
| `genie-ai-runtime` | External / separate repo | Jetson-only inference runtime, CUDA kernels, memory planner, and OpenAI-compatible serving surface. GenieClaw owns the client contract, not the runtime implementation. |
| External voice boundary | Initial / planned | External voice runtime for wake, VAD, STT, TTS, audio streaming, and voice session protocol. |
| Platform/OS layer | Planned | Custom L4T image, board bring-up, drivers, OTA base image, service supervision, ESP-Hosted-NG OS integration. |
| Full Matter/Thread/Zigbee/BLE production stack | Planned outside this repo | Lower connectivity/home-runtime layers. |
| Full vector/cuVS semantic memory backend | Planned design only | `VECTOR_MEMORY.md` describes the rollout. Current runtime has lightweight local semantic recall stored in SQLite; remote/vector-service and cuVS-scale backends are not implemented here. |
| Cryptographic skill signing and sandboxed marketplace | Planned | Later signed skill platform. Current code only audits manifests and signature material presence. |
| Security-grade biometric authorization | Planned / not a goal for current provider | Current local speaker identity is for household memory routing, not locks/payments/hostile-user isolation. |
| Cloud account system or multi-tenant hosted gateway | Not planned for this repo | GenieClaw is local-first appliance software. |

## Current Alpha Truth

The workspace version is currently `1.0.0-alpha.9`.

The current alpha line defaults Jetson deployments to `genie-ai-runtime`,
preserves the 4096-token agent harness, and treats optional remote/API providers
as development/testing paths behind explicit config, credential-env, remote-URL
opt-in, and context-budget checks.

The product direction is low-latency private home AI: keep context small,
retrieve only high-signal family memory and device state, and use typed local
tools/home-runtime boundaries instead of relying on larger remote prompts.

## How To Keep This Page Honest

When adding a feature:

1. Add or update code/tests.
2. Update the relevant subsystem doc.
3. Update this status page with `Implemented`, `Partial`, `External`, or `Planned`.
4. Avoid saying “fully supported” unless the repo contains the runtime code,
   tests, deploy path, and operational guidance for that claim.
