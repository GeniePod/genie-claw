# Household Security Model

GenieClaw is built for a household appliance, not a hostile multi-tenant cloud
service.

The default trust boundary is one home, one trusted operator group, and multiple
family members using the same local assistant. That is different from a
single-user developer bot and different from an adversarial shared server.

## Trust Boundary

Supported default:

- one GeniePod host per home or trusted household boundary
- one local operator group that can administer the device
- multiple household members with shared-room voice access
- shared household memory, filtered before voice or dashboard disclosure
- local dashboard/API bound to localhost or a trusted first-party gateway

Not supported as one shared instance:

- mutually untrusted tenants
- adversarial users sharing one tool-enabled assistant
- per-person OS authorization inside one `genie-core` process
- exposing raw config files as an API or dashboard surface

If mixed-trust operation is required, split the boundary: separate GeniePod
host, separate OS user, separate credentials, and separate data directory.

## Config Files Are Operator Artifacts

Raw config files such as `/etc/geniepod/geniepod.toml` may contain local paths,
model choices, service URLs, and sometimes secrets. They are not a user-facing
control surface.

Allowed surfaces:

- redacted security posture from `/api/security`
- explicit dashboard controls for memories and pending actions
- typed config summaries that report presence or policy, not raw values
- local support bundles that redact secrets and avoid dumping TOML

Disallowed surfaces:

- dashboard views that print raw config files
- API endpoints that serialize the full `Config` struct
- chat/tool responses that reveal tokens, config contents, or private file paths
- memory recall that exposes private member-scoped facts in shared-room mode

## Family And Shared Memory

The shared-memory default is household-safe recall. GenieClaw should treat
memory as product data with visibility rules:

- household facts can be recalled in shared spaces
- personal facts need speaker/profile context before disclosure
- sensitive facts should not be volunteered by voice
- secrets, access codes, credentials, and sensitive document/key locations are
  not valid shared-room memory
- dashboard memory management should show editable saved memories, not raw
  database rows or config files

Speaker recognition improves routing; it is not a hard security boundary unless
the deployment explicitly adds biometric enrollment, local profile storage,
fallback policy, and user-visible review.

## Memory Retrieval Is Not Authorization

Recall answers the question "what candidate facts might be relevant?" It does
not answer "may this fact be revealed?" or "may this action execute?"

GenieClaw keeps these decisions separate:

- recall layer: structured records, SQLite `FTS5`, and optional semantic
  retrieval can find candidate memories; safe relationship memories also feed a
  local household-profile index for exact role questions such as "who is the
  dad?", and safe device-alias memories feed exact Home Assistant target
  resolution before fuzzy matching; safe profile attributes and household rules
  answer low-latency questions about age, allergies, homework, and screen-time
  constraints before FTS fallback; safe household notes, reminders, manuals,
  and watch notes are indexed in a typed local FTS table for direct note recall
- classification layer: each memory is scoped and tagged by sensitivity before
  it is injected, spoken, or shown; policy decisions expose a stable disclosure
  class such as household, person, sensitive, private, or restricted
- policy layer: the current origin, room context, speaker confidence, and
  memory metadata decide whether disclosure is allowed, confirm-required,
  app-only, or denied
- action layer: device control, media, purchases, security, network, and other
  side effects pass through tool policy and actuation safety even if memory
  retrieval found the right target; tool results and audit events carry an
  action class such as `read_only`, `memory_write`, `home_actuation`,
  `network`, `media`, `timer`, or `diagnostic`
- audit layer: tool execution records the tool name, origin, success state, and
  argument keys without logging secret values

This means a memory hit can still produce a refusal or confirmation request.
For example, a remembered allergy may be safe to reveal to a caregiver, while a
gate code, Wi-Fi password, safe location, or purchase request must not be
treated as ordinary recall.

## Practical Rules

- Keep `genie-core` and `genie-api` on localhost unless a trusted gateway owns
  authentication.
- Prefer environment variables for tokens.
- The request origin (`voice`/`dashboard`/`telegram`/…) is a trust input, not a
  client-asserted label: genie-core only honors a privileged `X-Genie-Origin`
  from a loopback peer or with a matching `[core.origin_auth]` token, and
  downgrades everything else to `api`. When binding beyond localhost, configure
  origin tokens (or set `require_token`) so a network peer cannot forge a
  higher-trust channel.
- Keep config files `0600` and data directories `0700`.
- Do not use Telegram `allow_all_chats` for a real home.
- Keep tool policy and actuation safety enabled.
- Require confirmation for high-risk home actions.
- Use separate hosts or OS users for people who should not share authority.

## Runtime Contract

`/api/security` exists for dashboards and support tooling. It reports:

- household trust model
- whether raw config exposure is disabled
- shared-memory posture
- local control-surface posture
- secret presence without secret values
- risk flags for common footguns

The endpoint must never return raw TOML, tokens, full filesystem paths, speaker
labels, or memory database internals.
