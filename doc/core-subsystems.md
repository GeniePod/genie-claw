# Core Subsystems

This document maps the main `genie-core` subsystems to their source files and
their runtime role.

## LLM Client

Source:

- `crates/genie-core/src/llm/client.rs`
- `crates/genie-core/src/llm/retry.rs`
- `crates/genie-core/src/llm/mod.rs`

Responsibilities:

- OpenAI-compatible HTTP calls to the configured local model server
- health checking
- request serialization and response parsing
- retry and fallback behavior for selected request classes

## Prompt Builder And Reasoning Mode

Source:

- `crates/genie-core/src/prompt.rs`
- `crates/genie-core/src/reasoning.rs`

Responsibilities:

- detect model family from configured model name
- build the system prompt with tool and memory guidance
- adapt tool-calling instructions to model family
- choose no-think vs think-style behavior for different interaction kinds

Important current model families:

- Nemotron
- Llama
- Qwen
- Phi
- Small
- Generic

## Conversation Store

Source:

- `crates/genie-core/src/conversation.rs`
- `crates/genie-core/src/context.rs`

Responsibilities:

- create and title conversations
- append user/assistant/system messages
- export stored histories
- limit and summarize context for model prompts

## Tool System

Source:

- `crates/genie-core/src/tools/dispatch.rs`
- `crates/genie-core/src/tools/parser.rs`
- `crates/genie-core/src/tools/quick.rs`
- `crates/genie-core/src/tools/*.rs`

Responsibilities:

- define tool schemas for the model prompt
- parse tool JSON produced by the model
- execute built-in tools
- expose fast deterministic routing for repeated daily-use requests

Current notable tool modules:

- `calc.rs`
- `home.rs`
- `system.rs`
- `timer.rs`
- `weather.rs`
- `web_search.rs`

### Quick Router

The quick router exists so repeated, obvious utility requests do not depend on
the LLM choosing the correct tool.

Examples currently fast-routed:

- time
- system status
- Home Assistant connection status
- memory database diagnostics
- explicit web search
- simple timers
- simple weather requests
- simple math

## Home Assistant Boundary

Source:

- `crates/genie-core/src/ha/client.rs`
- `crates/genie-core/src/ha/provider.rs`
- `crates/genie-core/src/ha/policy.rs`

Responsibilities:

- keep Home Assistant behind a provider interface
- resolve household-facing device/entity language to HA targets
- enforce first-pass action safety policies
- enforce a final runtime actuation safety gate before physical execution
- separate "home control available" from "home control required for core usefulness"

This repo treats Home Assistant as optional integration, not as the product's
entire identity.

## Memory System

Source:

- `crates/genie-core/src/memory/mod.rs`
- `crates/genie-core/src/memory/extract.rs`
- `crates/genie-core/src/memory/inject.rs`
- `crates/genie-core/src/memory/policy.rs`
- `crates/genie-core/src/memory/recall.rs`
- `crates/genie-core/src/memory/decay.rs`

Responsibilities:

- SQLite-backed persistent memory
- FTS-backed retrieval
- canonical memory artifacts beside the DB
- explicit recall/store/forget behavior
- auto-capture from user facts
- memory-policy filtering for sensitive content
- recency/recall-aware ranking and decay

Current practical behavior:

- each memory DB now has a sibling `memory/` directory with:
  - daily notes like `YYYY-MM-DD.md`
  - append-only event logs under `events/YYYY-MM-DD.jsonl`
  - durable promoted entries in `MEMORY.md`
- each stored memory now persists policy metadata in SQLite:
  - `scope`
  - `sensitivity`
  - `spoken_policy`
- older databases are backfilled on open using the existing inference rules
- the `memory_status` tool reports both DB/FTS health and canonical artifact counts
- the `memory_status` tool also reports person/private/restricted memory counts
- casual identity facts can be auto-captured
- explicit "remember" requests can store structured facts
- high-risk secrets are blocked
- query-time memory injection reads the persisted policy metadata before adding memory to prompts
- memory recall also respects persisted policy metadata, with shared-room voice as the conservative default
- static prompt and voice bootstrap context now use the same shared-room-safe memory filtering
- promotion to `memory/MEMORY.md` is limited to memories that are safe for shared household disclosure

## Profile Ingest

Source:

- `crates/genie-core/src/profile/ingest.rs`
- `crates/genie-core/src/profile/toml_profile.rs`

Responsibilities:

- load profile data from the profile directory
- ingest TOML and text sources into memory
- normalize and deduplicate profile facts

## Voice Stack

Source:

- `crates/genie-core/src/voice_loop.rs`
- `crates/genie-core/src/voice/*.rs`

Responsibilities:

- audio recording and wake-word integration
- STT client/CLI execution
- conservative voice intent gating for shared-room wake flows
- speaker-identity provider boundary, including a future local-biometric recognizer hook, plus memory-context plumbing into voice read paths
- language detection and language-specific TTS model selection
- output formatting for speech
- streaming spoken responses
- basic DSP, gating, echo cancellation, and VAD support

Notable modules:

- `stt.rs`
- `tts.rs`
- `language.rs`
- `intent.rs`
- `format.rs`
- `identity.rs`
- `streaming.rs`
- `noise.rs`
- `dsp.rs`
- `aec.rs`
- `vad.rs`

## Security And Guardrails

Source:

- `crates/genie-core/src/security/*.rs`

Responsibilities:

- config and secret audit
- credential isolation helpers
- prompt-injection scanning
- environment sanitization before tool execution
- loop-guarding and repeated-call protection
- output sanitization and secret redaction
- taint tracking for unsafe data paths
- local-route validation and sandbox boundaries

This subsystem is intentionally spread across multiple small files because the
guardrails target different failure modes.

## Skills

Source:

- `crates/genie-core/src/skills/loader.rs`
- `crates/genie-core/src/skills/mod.rs`
- `crates/genie-skill-sdk/*`

Responsibilities:

- discover `.so` files from the runtime skills directory
- validate and load skill entrypoints
- expose loaded skills as model-callable tools
- execute native code through a narrow ABI

For author guidance, see [../skills/SKILL-DEVELOPER-GUIDE.md](../skills/SKILL-DEVELOPER-GUIDE.md).

## Connectivity Boundary

Source:

- `crates/genie-core/src/connectivity/mod.rs`

Responsibilities:

- define the health/capability boundary for an external coprocessor
- avoid embedding full Thread/Matter stack ownership in `genie-core`
- keep room for ESP32-C6 UART diagnostics/control without merging hosted-ng OS work into the runtime

The detailed architectural split is documented in
[../CONNECTIVITY.md](../CONNECTIVITY.md).

## Telegram Adapter

Source:

- `crates/genie-core/src/telegram.rs`

Responsibilities:

- long-poll Telegram Bot API
- enforce allowlist or all-chat policy
- forward inbound messages into the normal chat pipeline
- return responses back to Telegram

Telegram is enabled by config and by the crate feature set.

## OTA

Source:

- `crates/genie-core/src/ota/mod.rs`

Responsibilities:

- check release metadata and versions
- support operator-facing update checks

## Recommended Reading

- [services-and-crates.md](services-and-crates.md)
- [configuration.md](configuration.md)
- [repo-map.md](repo-map.md)
- [../ARCHITECTURE.md](../ARCHITECTURE.md)
- [../CODEBASE.md](../CODEBASE.md)
