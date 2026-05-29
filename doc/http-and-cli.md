# HTTP And CLI Reference

## `genie-core` HTTP API

Served by `crates/genie-core/src/server.rs`.

Default bind:

- `127.0.0.1:3000`

Set `[core].bind_host = "0.0.0.0"` only when `genie-core` is behind a trusted
LAN, firewall, or first-party gateway. The API includes chat, memory, tool, and
physical-actuation surfaces, so localhost is the safe default.

### Request Limits And Hardening

Both `genie-core` (`:3000`) and `genie-api` (`:3080`) read inbound requests
through the shared, bounded reader in `genie-common::http`, configured by the
`[http]` section (see `deploy/config/geniepod.toml`). This protects the
always-on daemon from an unauthenticated peer on the LAN:

- An oversized request line or header is rejected with `431` (the request body
  is capped per server — 64 KiB for `genie-core`, 4 KiB for `genie-api` — and an
  over-cap `Content-Length` is rejected with `413`).
- A connection that opens and then stalls mid-request is dropped after
  `[http].read_timeout_secs`, so half-open connections cannot wedge the
  listener.
- Concurrent connections are capped at `[http].max_connections`; transient
  `accept()` errors (e.g. `EMFILE`) are logged and the accept loop continues
  rather than terminating the process.

### UI And Chat Endpoints

First-party clients should set `X-Genie-Origin` so tool and actuation policy can
differentiate `dashboard`, `api`, `voice`, `telegram`, and other surfaces.
Requests without the header are treated as `api`, not `dashboard`.

#### Trusted Origin Resolution (issue #232)

The origin drives per-origin tool ACLs, actuation ACLs, rate limits, audit
attribution, and NLU confidence thresholds, so the client-supplied
`X-Genie-Origin` header cannot be trusted on its own — otherwise any caller
could claim `voice` to clear a higher-trust bar or rotate origins to dodge a
per-origin rate limit. genie-core only honors an origin more privileged than the
`api` baseline when the request proves it is entitled to that origin:

- from a **loopback** peer — the documented single-host trust boundary (see
  [household-security.md](household-security.md)); or
- with an **`X-Genie-Origin-Token`** that matches the secret configured for the
  claimed origin under `[core.origin_auth]`.

Anything else — a privileged claim from a non-loopback peer with no valid token,
or a mismatched token — is downgraded to `api` and logged. This means a LAN peer
reaching a `bind_host = "0.0.0.0"` deployment **cannot** assume `voice`,
`dashboard`, or `telegram` from the header alone.

Configuration:

```toml
[core.origin_auth]
# Require a token even from loopback peers (hardens multi-process same-host
# setups). Off by default so the local dashboard, CLI, and in-process adapters
# work with no setup.
require_token = false
# origin -> shared secret presented in the X-Genie-Origin-Token header.
# Prefer the GENIE_ORIGIN_TOKEN_<ORIGIN> env var and keep config files 0600.
tokens = { dashboard = "", telegram = "" }
```

Each origin's token may instead be supplied via the
`GENIE_ORIGIN_TOKEN_<ORIGIN>` environment variable (e.g.
`GENIE_ORIGIN_TOKEN_TELEGRAM`). The in-process Telegram adapter automatically
presents its configured token, so its `telegram` principal stays unforgeable by
other local processes and keeps working when `require_token = true`.

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
| `GET` | `/api/actuation/pending` | List pending high-risk confirmations and redacted audit state |
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

High-frequency home phrases are routed before model tool selection when the
intent is unambiguous. Current local-first routes include household memory
recall for school, schedule, community/business/channel-guide/subscription/TV/city-meeting hours, allowance, grocery
inventory, medical appointment, vet appointment, sunset, utility payment,
appliance state, environment/location/presence/access/waste/finance/market/fitness/health events,
receipt/manual/tool/storage/safety-equipment notes, tax and vehicle documents,
education, dictionary,
entertainment, travel planning, meal planning, guest context, fitness,
food-safety, substitutions, DIY, wardrobe, service booking, gift history,
game-night context, media preferences, hobby, podcast, language-learning,
creative/story/literature/photo recall, hiking/camping/cocktail/date-night/taco-bar planning,
contextual comfort, kid screen-time, checklists, package/garden/recipe/automation recall,
pet-care, bus pickup, snack permission, dinner-attendee, chores, leftovers,
sleepover approval, allergy-plan, stain-removal, school-note, charger/backpack/document,
key/package/laundry locations, thermostat audits, allergy medicine, dinosaur facts,
Grandma Wi-Fi notes, outdoor-play rules, wet-shoe guides, paint/project notes,
device alerts, safety-route guidance, electrical-panel maps, trash-day prep,
pest-history, homework-connectivity, plant-care, alarm-failure, side-gate-away,
guest-device, and end-of-day summaries,
wellness/anxiety, weather-report, social-logistics, and location questions; health
hydration/weight logs; app-only credential references for
Wi-Fi/password/bank-login/code/account/subscription/confirmation/spare-key questions;
media routes for focus music, morning news, and weather reports; web search for explicit
news/search/market-price requests; and integration-backed `home_control`/`home_status` calls
for explicit physical requests such as holiday lights, TV, security alarm,
nap mode, fireplace, ventilation, upstairs lights, robot mower, smoke-detector tests,
front-gate state, driveway ice status, sprinklers, lock-up/all-off/
work-from-home scenes, porch-light arrival triggers, rain-arrival and
parking-lot safety routines, locked-out unlock confirmation, phone finder,
timers, slow-cooker setup, dryer completion, baby breathing monitor, iron
state, water-heater readiness, basement/attic environment status, solar
generation, tire pressure, mailbox, car lock state, pool cleaner, printer ink,
baby-monitor state, speed-limit lookup, stove status, package status,
garage-door status, connected-car warmup/navigation, vacation/fall/smoke/working-late
protocols, movie-night/away/dinner-prep/study/cozy/reading/night-hallway scenes,
focus/quiet-porch/storm-prep/bedtime-override/piano-quiet/toddler-safe/spaceship scenes,
homework/sleepover-guest/babysitter modes, network pause, YouTube task blocking,
robot vacuum, notification mute, lock-except, contractor access, video-call hooks,
star-projector controls, quiet security, shower comfort, outlet-spill/water-leak/glass-break/gas safety,
self-cleaning oven, oven, water-pressure, sump-pump, sous-vide, camera motion,
unlocked doors/windows, rainy-pickup, toaster-smoke, pollen/allergy-day,
driveway-arrival lighting, video-call/reading-with-parent/work-call/calm-morning
scenes, guest-info displays, sunlight alarms, deferred dishwasher starts,
sleepover lights, cookie-done light alerts, garage paint ventilation,
electricity/draft/offline-device/sprinkler/fridge-door/plant/sensor-battery/bathroom-availability/end-of-day/morning-readiness status, nursery
air-quality, and freezer/freezer-door telemetry. Those physical results still
depend on the configured home provider and actuation policy.

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
  "audit_log": {
    "enabled": true,
    "storage": "local_private_file"
  }
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
| `GET` | `/api/security` | Redacted household security posture without raw config exposure |
| `GET` | `/api/runtime/contract` | Runtime contract proxied from `genie-core` |
| `GET` | `/api/actuation/pending` | Pending confirmations from `genie-core` |
| `GET` | `/api/actuation/actions` | Recent executed actions from `genie-core` |
| `GET` | `/api/actuation/audit` | Recent actuation audit events |
| `POST` | `/api/actuation/confirm` | Confirm a pending actuation token |
| `GET` | `/api/memories` | List saved memories with scope, sensitivity, disclosure class, and dashboard ordering |
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
| `genie-ctl speaker ...` | Manage local speaker identity profiles |
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
contract, connectivity status, redacted household security posture, actuation
state, selected system files, binary inventory, model inventory, recent runtime
contracts, recent tool audit events, and recent actuation audit events. It
records config presence but does not copy config contents.

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

### Speaker Subcommands

| Command | Purpose |
| --- | --- |
| `genie-ctl speaker list [--profile-dir DIR]` | List enrolled local speaker profiles |
| `genie-ctl speaker enroll <NAME> <WAV> [--profile-dir DIR]` | Enroll a WAV sample as a local speaker profile |
| `genie-ctl speaker enroll-live <NAME> [--device DEV] [--sample-rate N] [--duration SECS] [--profile-dir DIR]` | Record and enroll a speaker profile in one step |
| `genie-ctl speaker record <OUT.wav> [--device DEV] [--sample-rate N] [--duration SECS]` | Record a reusable enrollment/test WAV |
| `genie-ctl speaker identify <WAV> [--profile-dir DIR] [--min-score N]` | Match a WAV sample against enrolled profiles |
| `genie-ctl speaker remove <NAME> [--profile-dir DIR]` | Delete an enrolled speaker profile |

Speaker profiles store compact local acoustic fingerprints, not raw audio. They
are used for household memory routing in voice mode and are not a hostile-user
authentication boundary.

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
- explicit scene/routine activation phrases such as "goodnight GenieClaw"

Home-control execution now has three separate safety layers:

- prompt and tool guidance for model behavior
- first-pass local action policy
- final runtime actuation gate plus append-only audit logging
- recent action ledger for "what did you do?" and bounded undo
- tool audit events include an `action_class` such as `read_only`,
  `memory_write`, `home_actuation`, `network`, `media`, or `diagnostic`

Memory tools are policy-aware:

- memory recall defaults to shared-room-safe disclosure
- safe relationship memories maintain a local household-profile index for exact
  role recall before FTS fallback
- safe device-alias memories maintain a local alias index for exact
  Home Assistant target resolution before fuzzy matching
- safe media-profile memories maintain a local playlist index, so requests like
  "play my Morning Boost playlist" resolve a remembered provider target before
  media mode starts
- safe calendar, shopping-list, and access-permission memories maintain local
  typed indexes for questions such as "does Mia have piano today?", "what is on
  the shopping list?", and "can Leo unlock the front door?"
- safe chore/task-log and household schedule memories maintain local typed
  indexes for questions such as "did Leo feed the dog today?", "what time does
  the school bus arrive?", "when is the electricity bill due?", and "is it
  recycling week?", plus school calendar items such as parent-teacher
  conferences
- safe household event-log memories maintain a local typed index for audit-style
  questions such as "who turned off the security system?", "who opened the
  garage door?", or "is Mia home?" without repeating keypad/code details in the
  spoken answer
- safe profile attributes and household rules maintain local indexes for exact
  age, preference, allergy, homework, and screen-time recall
- safe notes, reminders, manuals, pet health, maintenance, storage, gift,
  troubleshooting, recipe, warranty, school, utility, recycling, photo,
  story, first-aid, visitor, inventory, meal-history, shopping-list, beverage,
  social, commute, pantry, comfort, location, vehicle/tax/cooking-reference,
  TV/community schedule, bank-login app-only, creative/story/literature/photo,
  trail/camping/cocktail/date-night/taco-bar, school checklists, appliance
  filters/manuals, tablet chargers, kid routines, school/project forms,
  debate/poem/project documents, water-heater receipts, camera/privacy reports,
  guest-network device reports, filter-change notes, garage-change reports,
  plumber shutoff notes, and emergency/safety-equipment locations,
  and watch notes maintain a typed
  local FTS index for direct questions such as "find my note about...", "what
  did the vet say...", "what color did we paint...", "how do I clean...", or
  "where are..."
- password/code questions can resolve to app-only secret references without
  exposing the value in shared-room chat
- selected safe preferences, notes, shopping, activity, troubleshooting, recipe,
  media, maintenance, routine, wellness, education, first-aid, story, travel,
  diet, visitor, music-profile, beverage, social, commute, pantry, comfort,
  location, pizza, arrival, and manual memories also maintain local
  embeddings for fuzzy household recall such as comfort, lunchbox, bored-child,
  printer, car-noise, recipe, date-night, movie, science-fair, headache,
  zoo-trip, diet-meal, washing-machine, visitor, watch-history, focus-music,
  scary-movie, hydration, brightness, loneliness, commute, tacos, humidity,
  first-aid, key-location, outdoor-sound, pizza, remote-start, arrival,
  painting, stomach-care, magic, manicure, charity, French learning, podcast,
  motivation, wardrobe-shoe, thirst, yoga, sunbathing, guys-night, Thai-food,
  fever, snow, homework-check, weather-report, anxiety, Roman-history,
  mood-music, washer-leak, bike-security, garden watering, chickpea recipes,
  hallway-light troubleshooting, sleep comfort, night-hallway safety, outlet-spill
  safety, after-dinner cleanup, board-game setup, glare/reading comfort,
  post-bath comfort, quiet drawing, workshop dust control, school-night reset,
  guest breakfast, low-power mode, family dinner screens, appliance-noise causes,
  air-quality/pollen causes, camera privacy, freezer/laundry safety automations,
  and taco-bar planning
  questions when exact words are missing
- live answers still require live tools: garage/lock/thermostat state comes from
  Home Assistant, weather comes from the weather tool, media playback comes from
  the local media path, and sensors such as baby monitors, cameras, face
  recognition, smart-fridge inventories, purchase carts, car remote start,
  traffic, Bluetooth trackers, microphones, or location history are not
  simulated by memory recall; scenario routes for current lights, guest Wi-Fi
  clients, noisy appliances, water pressure, camera privacy, and final safety
  sweeps therefore go through live tool/status boundaries when configured
- person/private/restricted memories may be withheld unless stronger read context is supplied
- memory status reports canonical artifact counts plus policy-scope counts

## Recommended Reading

- [configuration.md](configuration.md)
- [core-subsystems.md](core-subsystems.md)
- [deployment-and-ops.md](deployment-and-ops.md)
