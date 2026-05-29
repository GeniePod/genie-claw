# Core Subsystems

This document maps the main `genie-core` subsystems to their source files and
their runtime role.

## LLM Client

Source:

- `crates/genie-core/src/llm/mod.rs`
- `crates/genie-core/src/llm/openai_compat.rs`
- `crates/genie-core/src/llm/genie_ai_runtime.rs`
- `crates/genie-core/src/llm/llama_cpp.rs`
- `crates/genie-core/src/llm/openai_compatible.rs`
- `crates/genie-core/src/llm/provider.rs`

Responsibilities:

- OpenAI-compatible HTTP calls to the configured model server
- Jetson-default `genie-ai-runtime` request shaping and hint metadata
- legacy/development `llama.cpp` fallback support
- optional OpenAI-compatible provider auth/readiness planning for development,
  testing, and transitional validation only
- health checking
- request serialization and response parsing
- bounded connect/read/request timeouts for blocking and streaming calls

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
- append privacy-preserving tool audit events to `<data_dir>/runtime/tool-audit.jsonl`

The tool audit log records tool name, request origin, success, duration,
argument keys, and output length. It intentionally does not record argument
values or tool outputs.

`[core.tool_policy]` can apply origin-specific allowlists and denylists before
any tool runs. This is separate from the physical actuation gate: it can restrict
web search, media, memory, or skill calls by channel even when no home device is
being controlled.

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
- home undo requests
- recent action/history requests

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
- enforce a configurable channel allowlist before physical execution
- enforce per-origin physical actuation rate limits
- keep a recent action ledger for explanations and bounded undo
- expose action history to tools, HTTP, and the dashboard
- hydrate recent action history from the append-only actuation audit log on startup
- separate "home control available" from "home control required for core usefulness"

This repo treats Home Assistant as optional transitional integration, not as the
product's entire identity. The target is a native local device graph and IoT
boundary owned below GenieClaw.

## Memory System

Deep dive: [memory-system.md](memory-system.md).

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
- low-token, high-signal context injection for the home harness

Current practical behavior:

- each memory DB now has a sibling `memory/` directory with:
  - `INDEX.md` as the generated durable-memory entry point
  - daily notes like `YYYY-MM-DD.md`
  - append-only event logs under `events/YYYY-MM-DD.jsonl`
  - durable promoted entries in `MEMORY.md`
  - namespace notes under `namespaces/<scope>/<kind>.md`
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
- query-time memory injection should select only the relevant family/household
  facts for the current turn rather than dumping all memory
- memory recall also respects persisted policy metadata, with shared-room voice as the conservative default
- static prompt and voice bootstrap context now use the same shared-room-safe memory filtering
- promotion to `memory/MEMORY.md` is limited to memories that are safe for shared household disclosure
- promoted memories are projected into a Dendron-style local namespace tree for operator browsing without turning the runtime into a PKM product
- non-shared-safe promoted memories stay represented in namespace notes, but their content is redacted in markdown by default

## Profile Ingest

Source:

- `crates/genie-core/src/profile/ingest.rs`
- `crates/genie-core/src/profile/toml_profile.rs`

Responsibilities:

- load profile data from the profile directory
- ingest TOML and text sources into memory
- normalize and deduplicate profile facts

## Voice Runtime Boundary

Current transitional source:

- `crates/genie-core/src/voice_loop.rs`
- `crates/genie-core/src/voice/*.rs`

Long-term owner:

- external voice boundary

GenieClaw should own:

- spoken agent behavior
- transcript-to-agent routing
- response text generation
- memory/tool/home-intent policy for voice-origin requests
- shared-room safety policy

The external voice boundary should own:

- wake word
- VAD
- STT
- TTS
- capture/playback device handling
- denoise and acoustic echo control
- voice session streaming events

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

These modules remain a Jetson alpha bring-up path until the external voice
boundary is production-ready. New voice-pipeline implementation should target
that boundary unless it is strictly an agent-layer behavior change.

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

## Runtime Contract

Source:

- `crates/genie-core/src/runtime_contract.rs`
- `crates/genie-core/src/server.rs`

Responsibilities:

- expose prompt, tool schema, policy, and hydration fingerprints
- provide a deterministic startup contract for operations and incident response
- make tool/policy drift visible without inspecting logs
- append boot contracts to `<data_dir>/runtime/contracts.jsonl`
- expose a compact contract summary through `/api/health`
- report `ok`, `drift`, or `unpinned` when `[core].expected_runtime_contract_hash` is configured
- support persistent local-agent patterns where boot state must be reproducible

Current HTTP surface:

- `GET /api/runtime/contract`
- `GET /api/health` includes `runtime_contract`

The contract hash is an operational fingerprint, not a security signature.
It should be used to compare deployments, debug field issues, and confirm that
the expected prompt/tool/policy bundle is active.

## Skills

Source:

- `crates/genie-core/src/skills/loader.rs`
- `crates/genie-core/src/skills/mod.rs`
- `crates/genie-skill-sdk/*`

Responsibilities:

- discover `.so` files from the runtime skills directory
- validate and load skill entrypoints
- discover optional sidecar manifests such as `hello.skill.json`
- expose manifest status, permissions, capabilities, review, and signing presence for audit
- expose loaded skills as model-callable tools
- execute native code through a narrow ABI

Manifest status is visibility-only by default. Operators can enable
`[core.skill_policy]` to reject missing/mismatched manifests, require signature
material, or deny specific manifest permission labels. Signature checking is
presence-only today; cryptographic verification belongs in later signed-skill
platform work.

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
