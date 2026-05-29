# Repository Map

## Top Level

| Path | Purpose |
| --- | --- |
| `README.md` | Product summary and quick start |
| `GETTING_STARTED.md` | Bring-up guide |
| `LOW_LATENCY_HOME_AGENT.md` | Canonical low-latency private home-agent goal |
| `ARCHITECTURE.md` | Genie ecosystem and repo-boundary architecture |
| `CODEBASE.md` | Narrative code walkthrough |
| `CONNECTIVITY.md` | ESP32-C6 boundary and ownership split |
| `VECTOR_MEMORY.md` | Vector-memory design document |
| Local-only `ROADMAP.md` | Private product roadmap, ignored by git when present |
| `doc/` | Current documentation entry point |
| `doc/implementation-status.md` | Source of truth for implemented, partial, external, and planned work |
| `doc/memory-system.md` | Implemented memory storage, recall, policy, and extension design |
| `crates/` | Workspace crates |
| `deploy/` | Configs, scripts, systemd units, Docker assets |
| `skills/` | Native skill examples and guide |

## Workspace Crates

| Path | Purpose |
| --- | --- |
| `crates/genie-common` | shared config, mode, tegrastats |
| `crates/genie-core` | GenieClaw agent runtime |
| `crates/genie-api` | dashboard/status service |
| `crates/genie-governor` | mode and pressure manager |
| `crates/genie-health` | health polling |
| `crates/genie-ctl` | local CLI |
| `crates/genie-skill-sdk` | loadable skill ABI |

## `crates/genie-core/src`

### Entrypoints

- `main.rs`
- `lib.rs`
- `server.rs`
- `repl.rs`
- `voice_loop.rs`

### Runtime Context And Conversation

- `context.rs`
- `conversation.rs`

### LLM

- `llm/mod.rs`
- `llm/openai_compat.rs`
- `llm/genie_ai_runtime.rs`
- `llm/llama_cpp.rs`
- `llm/openai_compatible.rs`
- `llm/provider.rs`

This is the LLM backend facade. Jetson deploys default to the external
`genie-ai-runtime`; `llama.cpp` remains selectable as a legacy fallback and
development backend. Optional OpenAI-compatible providers are disabled by
default and exist only for development, testing, and transitional validation.

### Prompt And Reasoning

- `prompt.rs`
- `reasoning.rs`

### Home Assistant

- `ha/mod.rs`
- `ha/client.rs`
- `ha/provider.rs`
- `ha/policy.rs`

This is the current home-runtime adapter. It points at Home Assistant today and
should point at the external home boundary later.

### Tools

- `tools/mod.rs`
- `tools/dispatch.rs`
- `tools/parser.rs`
- `tools/quick.rs`
- `tools/system.rs`
- `tools/home.rs`
- `tools/timer.rs`
- `tools/calc.rs`
- `tools/weather.rs`
- `tools/web_search.rs`

### Evaluation

- `eval/mod.rs`
- `eval/bfcl.rs`
- `tests/bfcl/*`

Side-effect-free tool-call scoring for BFCL-style JSONL fixtures.

### Memory

- `memory/mod.rs`
- `memory/extract.rs`
- `memory/inject.rs`
- `memory/policy.rs`
- `memory/recall.rs`
- `memory/decay.rs`

### Profile

- `profile/mod.rs`
- `profile/ingest.rs`
- `profile/toml_profile.rs`

### Security

- `security/mod.rs`
- `security/audit.rs`
- `security/credentials.rs`
- `security/env_sanitize.rs`
- `security/injection.rs`
- `security/loop_guard.rs`
- `security/sandbox.rs`
- `security/taint.rs`

### Voice

- `voice/mod.rs`
- `voice/aec.rs`
- `voice/dsp.rs`
- `voice/format.rs`
- `voice/language.rs`
- `voice/noise.rs`
- `voice/pipeline.rs`
- `voice/streaming.rs`
- `voice/stt.rs`
- `voice/tts.rs`
- `voice/vad.rs`

### Other Runtime Surfaces

- `connectivity/mod.rs`
- `skills/mod.rs`
- `skills/loader.rs`
- `ota/mod.rs`
- `telegram.rs`

## Tests

Current integration-style tests outside `src/`:

- `crates/genie-core/tests/tool_dispatch_test.rs`
- `crates/genie-core/tests/tools_test.rs`
- `crates/genie-core/tests/memory_recall.rs`
- `crates/genie-core/tests/prompt_sha_test.rs`
- `crates/genie-core/tests/tool_gate_integration_test.rs`
- `crates/genie-core/tests/voice_loop_integration.rs`

Most other tests are colocated unit tests inside the module files.

## Deploy Tree

### Config

- `deploy/config/geniepod.toml`
- `deploy/config/geniepod.dev.toml`
- `deploy/config/profile.toml.example`

### Systemd

- `deploy/systemd/*.service`
- `deploy/systemd/*.target`

### Scripts

- `deploy/setup-jetson.sh`
- `deploy/scripts/*`

### Docker

- `Dockerfile`
- `docker-compose.dev.yml`
- `deploy/docker/docker-compose.yml`

## Skills Tree

Current important files:

- `skills/SKILL-DEVELOPER-GUIDE.md`

Runtime-loaded skill binaries are not stored under `skills/`; they are loaded
from the runtime skills directory used by `genie-core`.

## Which File To Open First For Common Tasks

| Task | Start Here |
| --- | --- |
| Chat/API behavior | `crates/genie-core/src/server.rs` |
| Prompt/tool selection | `crates/genie-core/src/prompt.rs` and `tools/dispatch.rs` |
| BFCL tool-call scoring | `crates/genie-core/src/eval/bfcl.rs` and `tests/bfcl/` |
| Memory bugs | `crates/genie-core/src/memory/mod.rs` |
| Voice bugs | `crates/genie-core/src/voice_loop.rs` and `voice/` |
| Home Assistant behavior | `crates/genie-core/src/ha/provider.rs` |
| Search behavior | `crates/genie-core/src/tools/web_search.rs` |
| CLI behavior | `crates/genie-ctl/src/main.rs` |
| Governor behavior | `crates/genie-governor/src/governor.rs` |
| Dashboard behavior | `crates/genie-api/src/routes.rs` |

## Recommended Reading

- [overview.md](overview.md)
- [implementation-status.md](implementation-status.md)
- [services-and-crates.md](services-and-crates.md)
- [core-subsystems.md](core-subsystems.md)
- [../CODEBASE.md](../CODEBASE.md)
