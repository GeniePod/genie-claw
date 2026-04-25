# Services And Crates

## Architecture Position

This workspace is the current implementation of the GenieClaw agent layer.

It includes small operational services because they are needed for the current
Jetson appliance deployment, but the long-term boundary is clear:

- `genie-core` is the agent runtime.
- `genie-api` is a lightweight local dashboard/status service, not the final product app.
- `genie-governor` and `genie-health` are appliance support services.
- Home Assistant and `llama.cpp` are transitional lower-runtime adapters.
- Future `genie-home-runtime` and `genie-ai-runtime` should replace those lower-runtime adapters.

## Workspace Crates

| Crate | Type | Responsibility |
| --- | --- | --- |
| `genie-common` | library | Shared config types, mode definitions, and tegrastats parsing |
| `genie-core` | library + binary | Main GenieClaw agent runtime |
| `genie-api` | binary | Dashboard/status API and static UI host |
| `genie-governor` | binary | Mode control, memory-pressure response, service control |
| `genie-health` | binary | Polling and health history |
| `genie-ctl` | binary | Local CLI for status, chat, search, health, skills, diagnostics |
| `genie-skill-sdk` | library | ABI and helper types for loadable native skills |

## Main Runtime Binary

### `genie-core`

Primary responsibilities:

- build the model-aware system prompt
- own the chat API and OpenAI-compatible bridge
- manage conversation persistence
- inject, extract, and recall memory
- select and execute built-in tools
- load and execute native skills
- integrate Home Assistant through a provider boundary
- run the voice loop
- expose connectivity health from the coprocessor boundary
- optionally run the Telegram adapter

Boundary rule:

`genie-core` may request model inference and physical actions, but it should not
grow into the optimized model server or the final home automation engine.

Important source roots:

- `crates/genie-core/src/main.rs`
- `crates/genie-core/src/server.rs`
- `crates/genie-core/src/repl.rs`
- `crates/genie-core/src/voice_loop.rs`

## Supporting Service Binaries

### `genie-governor`

Primary responsibilities:

- keep track of operating mode (`day`, `night_a`, `night_b`, `media`)
- sample memory and tegrastats data
- make lifecycle decisions for optional services
- expose a Unix socket control interface

Key files:

- `crates/genie-governor/src/main.rs`
- `crates/genie-governor/src/governor.rs`
- `crates/genie-governor/src/control.rs`

### `genie-health`

Primary responsibilities:

- poll configured service endpoints
- store health history
- optionally forward alerts
- support dashboard and diagnostics surfaces

Key files:

- `crates/genie-health/src/main.rs`
- `crates/genie-health/src/checker.rs`

### `genie-api`

Primary responsibilities:

- serve dashboard HTML and JavaScript
- query governor and health databases
- return current mode, live memory, service health, and tegrastats history
- proxy memory and actuation admin operations to `genie-core`

Key files:

- `crates/genie-api/src/main.rs`
- `crates/genie-api/src/routes.rs`

### `genie-ctl`

Primary responsibilities:

- operator-facing CLI for chat, web search, connectivity, skills, and diagnostics
- simple interface over `genie-core` HTTP and governor socket surfaces
- write local JSON support bundles for field debugging and incident reports

Key file:

- `crates/genie-ctl/src/main.rs`

## Native Skill Surface

### `genie-skill-sdk`

This crate is the ABI contract for loadable native skills.

It is used by the runtime loader in `genie-core` and by skill authors building
`.so` modules for the runtime skills directory.

Key files:

- `crates/genie-skill-sdk/src/lib.rs`
- `crates/genie-skill-sdk/src/args.rs`
- `crates/genie-skill-sdk/src/result.rs`
- `crates/genie-skill-sdk/src/vtable.rs`

## Process And Interface Boundaries

### Network Endpoints

- `genie-core`: `:3000`
- `llama-server`: `:8080` today; future replacement is `genie-ai-runtime`
- Home Assistant: commonly `:8123` today; future replacement is `genie-home-runtime`
- `genie-api`: separate dashboard service port, depending on deploy setup

### Local IPC

- Governor control socket: `/run/geniepod/governor.sock`

### Databases

- `memory.db`
- `conversations.db`
- `governor.db`
- `health.db`

## Default Systemd Units

Defined under `deploy/systemd/`:

- `genie-core.service`
- `genie-api.service`
- `genie-governor.service`
- `genie-health.service`
- `genie-llm.service`
- `genie-mqtt.service`
- `genie-audio.service`
- `genie-wakeword.service`
- `homeassistant.service`
- `geniepod.target`
- `geniepod-late.target`

Not every unit is always active. Some are optional or deployment-specific.

## Optional Integration Boundaries

- Home Assistant: transitional provider boundary in `crates/genie-core/src/ha/`
- Telegram: feature-gated adapter in `crates/genie-core/src/telegram.rs`
- ESP32-C6 connectivity sidecar: boundary in `crates/genie-core/src/connectivity/`
- Web search providers: DuckDuckGo default, optional local SearXNG
