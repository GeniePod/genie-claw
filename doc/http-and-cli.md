# HTTP And CLI Reference

## `genie-core` HTTP API

Served by `crates/genie-core/src/server.rs`.

Default bind:

- `0.0.0.0:3000`

### UI And Chat Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/` | Local chat UI |
| `POST` | `/api/chat` | Normal chat turn |
| `POST` | `/api/chat/stream` | Streaming chat turn using NDJSON events |
| `GET` | `/api/chat/history` | Messages for the current conversation |
| `POST` | `/api/chat/clear` | Start a new conversation |
| `GET` | `/api/conversations` | List all stored conversations |
| `GET` | `/api/chat/export?id=<id>` | Export one conversation as JSON |

### Tool And Status Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/api/tools` | List built-in and loaded tool definitions |
| `GET` | `/api/runtime/contract` | Deterministic runtime contract: prompt/tool/policy/hydration hashes |
| `GET` | `/api/web-search` | Web search config/cache status |
| `POST` | `/api/web-search` | Execute direct web search |
| `GET` | `/api/actuation/pending` | List pending high-risk confirmations and audit-log path |
| `GET` | `/api/actuation/actions` | List recent executed home actions for dashboard/inspection |
| `POST` | `/api/actuation/confirm` | Confirm and execute one pending action token |
| `GET` | `/api/health` | Rich runtime health |
| `GET` | `/api/connectivity` | Connectivity controller health and capabilities |

### Compatibility Endpoints

| Method | Path | Purpose |
| --- | --- | --- |
| `POST` | `/v1/chat/completions` | OpenAI-compatible local bridge |
| `GET` | `/v1/models` | Minimal model listing |

## Core Response Shapes

### `POST /api/chat`

Request:

```json
{"message":"what time is it?"}
```

Response:

```json
{
  "response": "...",
  "tool": "get_time",
  "conversation_id": "..."
}
```

`tool` is omitted or `null` when no tool was used.

### `POST /api/chat/stream`

Streaming events are newline-delimited JSON.

Current event types:

- `token`: normal streamed text
- `replace`: replace prior text with final tool-backed response
- `done`: final event with `response`, optional `tool`, and `conversation_id`

### `GET /api/health`

Current top-level fields:

- `status`
- `llm`
- `memories`
- `conversations`
- `mem_available_mb`
- `connectivity`
- `web_search`
- `runtime_contract`
- `version`

`runtime_contract` is a compact summary with the active contract hash, prompt
hash, tool schema hash, policy hash, hydration hash, model family, and tool
count. Use `GET /api/runtime/contract` for the full payload.

### `GET /api/runtime/contract`

Operational fingerprint for deterministic startup and incident response.

Current top-level fields:

- `schema_version`
- `package`
- `version`
- `model_family`
- `max_history_turns`
- `prompt_hash`
- `tool_schema_hash`
- `policy_hash`
- `hydration_hash`
- `contract_hash`
- `tool_names`
- `policy`
- `hydration`
- `validation`

Use this endpoint to verify that a deployed box booted with the expected
prompt, tool surface, policy settings, and hydrated local state.

If `[core].expected_runtime_contract_hash` is configured, `validation.status`
is `ok` or `drift`. Without a pinned hash, the status is `unpinned`.

At daemon startup, `genie-core` also appends the full contract to:

```text
<data_dir>/runtime/contracts.jsonl
```

This file is intended for support bundles and incident reconstruction.

### `POST /api/web-search`

Request:

```json
{"query":"ESP32-C6 Thread support","limit":3,"fresh":false}
```

Current response shape:

```json
{
  "tool": "web_search",
  "success": true,
  "query": "ESP32-C6 Thread support",
  "provider": "duckduckgo",
  "fresh": false,
  "cached": false,
  "blocked": false,
  "result_count": 3,
  "items": [
    {
      "title": "Example",
      "text": "Example result text",
      "url": "https://example.test"
    }
  ],
  "response": "Web search results for ..."
}
```

If a query is blocked as sensitive, the endpoint still returns `200` with
`blocked: true` and `result_count: 0`.

### `GET /api/actuation/pending`

Current response shape:

```json
{
  "pending": [
    {
      "token": "act-...",
      "entity": "front door",
      "action": "unlock",
      "value": null,
      "reason": "Front door is not marked voice-safe",
      "requested_by": "voice",
      "created_ms": 1777000000000,
      "expires_ms": 1777000600000
    }
  ],
  "audit_log_path": "/opt/geniepod/data/safety/actuation-audit.jsonl"
}
```

### `GET /api/actuation/actions`

Returns the short recent action ledger used for explanations and bounded undo.
The ledger is restored from the append-only actuation audit log when
`genie-core` starts.

Current response shape:

```json
{
  "actions": [
    {
      "id": 1,
      "entity": "living room lights",
      "action": "turn_on",
      "value": null,
      "inverse_action": "turn_off",
      "origin": "voice",
      "summary": "Turned on living room lights.",
      "confidence": 0.94,
      "executed_ms": 1777000000000
    }
  ]
}
```

### `POST /api/actuation/confirm`

Request:

```json
{"token":"act-..."}
```

Response:

```json
{"ok":true,"response":"Done."}
```

The confirmation endpoint is intended for local trusted surfaces.

### `POST /v1/chat/completions`

Supported request shape:

- OpenAI-style `messages`
- optional `model`
- optional `max_tokens`

Current implementation returns one assistant message in the `choices` array.
Token accounting fields are present but currently zero-filled.

## `genie-api` Dashboard API

Served by `crates/genie-api/src/routes.rs`.

| Method | Path | Purpose |
| --- | --- | --- |
| `GET` | `/` | Dashboard HTML |
| `GET` | `/dashboard.js` | Dashboard JavaScript |
| `GET` | `/api/status` | Governor mode, memory, uptime-oriented status |
| `GET` | `/api/tegrastats` | Recent tegrastats history from `governor.db` |
| `GET` | `/api/services` | Latest health state per service from `health.db` |
| `GET` | `/api/runtime/contract` | Runtime contract proxied from `genie-core` |
| `GET` | `/api/actuation/pending` | Pending confirmations from `genie-core` |
| `GET` | `/api/actuation/actions` | Recent executed actions from `genie-core` |
| `GET` | `/api/actuation/audit` | Recent actuation audit events |
| `POST` | `/api/actuation/confirm` | Confirm a pending actuation token |
| `GET` | `/api/memories` | List saved memories for dashboard management |
| `POST` | `/api/memories/update` | Update one memory row |
| `POST` | `/api/memories/delete` | Delete one memory row |
| `POST` | `/api/memories/reorder` | Persist memory display order |
| `POST` | `/api/mode` | Forward mode change command to governor |

## `genie-ctl` CLI

Implemented in `crates/genie-ctl/src/main.rs`.

### Main Commands

| Command | Purpose |
| --- | --- |
| `genie-ctl status` | System status summary |
| `genie-ctl mode <MODE>` | Change governor mode |
| `genie-ctl chat <MESSAGE>` | Send one chat request |
| `genie-ctl search [--fresh] [--limit N] <QUERY>` | Direct web search |
| `genie-ctl history` | Show current conversation history |
| `genie-ctl tools` | List available tools |
| `genie-ctl connectivity` | Show coprocessor boundary status |
| `genie-ctl skill ...` | Manage loadable skills |
| `genie-ctl health` | Service health report |
| `genie-ctl conversations` | List stored conversations |
| `genie-ctl update-check` | OTA check |
| `genie-ctl diag` | Diagnostics summary |
| `genie-ctl support-bundle [PATH]` | Write a JSON diagnostics bundle |
| `genie-ctl version` | Version output |

### `genie-ctl support-bundle`

Writes a local JSON support bundle. If no path is provided, the output goes to:

```text
/tmp/geniepod-support-<timestamp>.json
```

The bundle includes service reachability, governor status, core health, runtime
contract, connectivity status, actuation state, selected system files, binary
inventory, model inventory, recent runtime contracts, and recent actuation audit
events. It records config presence but does not copy config contents.

### Skill Subcommands

| Command | Purpose |
| --- | --- |
| `genie-ctl skill list` | List installed runtime skills |
| `genie-ctl skill install <SOURCE.so> [DEST_NAME]` | Validate and install a skill |
| `genie-ctl skill remove <SKILL_NAME|FILE_NAME>` | Remove a skill |
| `genie-ctl skill dir` | Print runtime skill directory |

`genie-ctl skill list` also reports optional sidecar manifest status. A skill
`hello.so` may include `hello.skill.json`; the CLI shows manifest status,
requested permissions, capabilities, review identity, and whether signature
material is present. This is audit visibility only in the current release.

## Current Built-In Tool Families

The exact tool list depends on config and loaded skills, but the built-in
surface currently includes:

- home control, home status, home undo, and action history
- time
- weather
- web search
- system info
- calculator
- media playback trigger
- memory recall, status, forget, and store
- timers

Home-control execution now has three separate safety layers:

- prompt and tool guidance for model behavior
- first-pass local action policy
- final runtime actuation gate plus append-only audit logging
- recent action ledger for "what did you do?" and bounded undo

Memory tools are policy-aware:

- memory recall defaults to shared-room-safe disclosure
- person/private/restricted memories may be withheld unless stronger read context is supplied
- memory status reports canonical artifact counts plus policy-scope counts

## Recommended Reading

- [configuration.md](configuration.md)
- [core-subsystems.md](core-subsystems.md)
- [deployment-and-ops.md](deployment-and-ops.md)
