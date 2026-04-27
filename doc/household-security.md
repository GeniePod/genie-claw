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
- dashboard memory management should show editable saved memories, not raw
  database rows or config files

Speaker recognition improves routing; it is not a hard security boundary unless
the deployment explicitly adds biometric enrollment, local profile storage,
fallback policy, and user-visible review.

## Practical Rules

- Keep `genie-core` and `genie-api` on localhost unless a trusted gateway owns
  authentication.
- Prefer environment variables for tokens.
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
