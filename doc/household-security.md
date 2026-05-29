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
- password/code memories may be represented as app-only references, but the
  shared-room assistant should point users to the local dashboard or credential
  store instead of speaking values
- account numbers and travel confirmation numbers are also app-only references;
  they are not indexed as speakable household notes

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
  answer low-latency questions about age, shoe sizes, allergies, homework, and
  screen-time constraints before FTS fallback; safe calendar, access,
  chore/task-log, schedule/channel-guide/subscription/TV/community-meeting,
  appliance-state, environment,
  location/presence, access, waste-collection, finance/market/payment-event,
  fitness/health-reading, security-event, pet-care, school-transport, device-audit,
  device-health, laundry, and pantry inventory memories answer local exact-match
  questions before fuzzy fallback;
  safe household notes, reminders, manuals, warranties, receipts, utility notes,
  first-aid notes, education notes, dictionary notes, story/media notes,
  inventory notes, storage notes, meal history, commute notes, pantry notes,
  travel notes, contact/contractor notes, delivery notes, location notes,
  shopping notes, tool notes, DIY notes, fitness notes, food-safety notes,
  safety-equipment notes, school documents, recipe notes, gift history, tax and
  vehicle documents, cooking references, TV manuals, network-device notes,
  hobby, podcast, language-learning, creative/story/literature/photo recall,
  wardrobe/DIY/service, hiking/camping/cocktail/date-night/taco-bar planning,
  school checklists, appliance filters/manuals, contextual comfort, garden
  watering, recipe feedback, routine steps, automation troubleshooting,
  sleep/night-safety, porch quieting, stain/laundry guides, leftovers,
  backpack/key/package/document locations, allergy action plans, school-task
  state, project/electrical-panel notes, trash-day prep, pest-history,
  homework-connectivity, plant-care, alarm-failure, side-gate-away, guest-device,
  end-of-day summaries, school/project forms, debate/poem/project documents,
  filter changes, camera/privacy reports, guest-network device reports,
  garage-change reports, air-quality causes, appliance-noise causes, and final
  safety sweeps, guest/babysitter notes, outlet/gas/water-leak/glass-break
  safety, wellness/anxiety, weather-report, and social-logistics context, and
  protected account, lock-code, or key-location references are indexed in
  typed local tables for direct note recall or app-only disclosure
- classification layer: each memory is scoped and tagged by sensitivity before
  it is injected, spoken, or shown; policy decisions expose a stable disclosure
  class such as household, person, sensitive, private, or restricted
- policy layer: the current origin, room context, speaker confidence, and
  memory metadata decide whether disclosure is allowed, confirm-required,
  app-only, or denied
- action layer: device control, media, purchases, security, network, phone
  finder, sprinklers, locks/gates, freezer telemetry, and other side effects
  pass through tool policy and actuation safety even if memory retrieval found
  the right target; work-from-home/all-off scenes, fireplace/ventilation,
  tire-pressure, mailbox, robot-mower, pool-cleaner, upstairs-light,
  smoke-detector, TV/alarm, nap-mode, baby-monitor, printer-ink, speed-limit,
  connected-car warmup/navigation, vacation-mode, fall-alert, smoke-ventilation,
  working-late, movie-night/away/dinner-prep/study/focus/cozy/reading/night-hallway
  scenes, network-pause, bedtime reading overrides, quiet porch/piano modes,
  toddler-safe kitchen, robot vacuum, lock-except rules, homework mode, rainy pickup,
  sleepover guest mode, babysitter mode, work-call quiet, pollen/allergy-day modes,
  after-dinner cleanup, board-game, quiet-drawing, school-night reset, guest-breakfast,
  low-power, workshop-dust, video-call and reading-with-parent scenes,
  driveway-arrival lighting, dishwasher deferred start, guest-info display,
  sunlight/rainy-day alarm scheduling, contractor/temporary-family garage or lock
  access, YouTube task blocking, family-dinner screen pauses, quiet security,
  shower comfort, star projector, mirror-light, stairwell, grilling-light and
  sleepover-light scenes, cookie-done/cookie-cooling alerts, garage paint ventilation,
  freezer-threshold and laundry-leak automations, hallway-camera privacy, outlet-spill,
  toaster-smoke, dripping/water-leak, glass-break, and gas safety,
  self-cleaning-oven, water-pressure, sump-pump, sous-vide, camera-motion,
  unlocked-door/window, electricity, draft/offline-device, sprinkler-history,
  fridge-door, plant, sensor-battery, bathroom-availability, privacy, safety-sweep,
  end-of-day, and morning-readiness,
  nursery-air-quality, iron, water-heater, garage-door, and
  locked-out flows still resolve through this action layer;
  tool results and audit events carry an action class such as
  `read_only`, `memory_write`, `home_actuation`, `network`, `media`, `timer`,
  or `diagnostic`
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
- Keep fuzzy household recall local-first: embedded-memory rows are metadata in
  the local SQLite store, not a remote vector service dependency.
- Keep household indexes typed and inspectable. Calendar events, shopping-list
  items, access permissions, chore/task logs, household schedules, event logs,
  finance/allowance/payment logs, device aliases, and media targets are local
  SQLite rows derived from safe memory records, not remote profile data.
- Store playlists and media preferences as provider targets instead of secrets.
  Credential, bank-login, subscription, account, and access-code memories should
  remain app-only references and are not spoken in shared-room chat.
- Do not store router, Wi-Fi, access-code, lock-combination, account-number,
  confirmation-number, spare-key, or sensitive document-location values as
  speakable notes. They are either rejected by memory-write policy or reduced
  to app-only references for local-dashboard viewing.

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
