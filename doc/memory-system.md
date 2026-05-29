# Memory System Design

This document describes the memory system that is implemented in this repo
today. It is written for maintainers who need to understand how GenieClaw stores
facts, recalls household context, protects sensitive data, and routes real home
queries through structured, keyword, and semantic layers.

For future vector-backend planning, see [../VECTOR_MEMORY.md](../VECTOR_MEMORY.md).
That document covers optional neural/vector search rollout. This document covers
the runtime that currently ships in `genie-core`.

## Current Status

The implemented memory system is local-first and SQLite-backed. It includes:

- durable memory rows in `memories`
- SQLite FTS5 over generic memories
- typed household projection tables for exact recall
- FTS5 over household notes and documents
- app-only secret references that can answer where a secret lives without
  exposing the secret value
- lightweight local semantic recall using deterministic hash embeddings
- recall tracking, decay, promotion, and canonical markdown artifacts
- persisted memory policy metadata for scope, sensitivity, and spoken disclosure
- shared-room-safe filtering for voice, prompt injection, and recall
- quick routing for common memory, home status, home control, reminder, and
  media intents

The system does not currently depend on a remote embedding service, pgvector,
Qdrant, sqlite-vec, or cuVS. Semantic recall exists, but it is intentionally
small and deterministic: it uses `local-hash-home-v1` embeddings stored inside
SQLite. That gives GenieClaw fuzzy home recall without adding a second service
or GPU memory pressure to the appliance path.

## Design Goals

The memory design is optimized for a private home appliance:

- keep household memory local by default
- prefer exact structured answers for factual, permission, safety, and device
  questions
- use FTS for notes, documents, manuals, receipts, and instructions
- use semantic recall for vague comfort, routine, troubleshooting, and "I meant
  something like this" queries
- never dump all memory into the prompt
- make stored facts auditable in both SQLite and generated markdown
- keep secrets out of speakable memory
- let actuation policy decide whether a recalled fact can become a device action
- keep the default runtime small enough for Jetson-class hardware

## Main Code Locations

- `crates/genie-core/src/memory/mod.rs`
  Owns the SQLite store, schema, migrations, typed projections, generic search,
  household answers, household notes, local semantic search, promotion, and
  canonical artifact writes.
- `crates/genie-core/src/memory/embedding.rs`
  Implements the deterministic local embedding provider used for current
  semantic recall.
- `crates/genie-core/src/memory/extract.rs`
  Extracts simple identity and preference facts from user text.
- `crates/genie-core/src/memory/inject.rs`
  Selects relevant, policy-allowed memory for prompt injection.
- `crates/genie-core/src/memory/policy.rs`
  Classifies memory scope, sensitivity, and spoken disclosure policy.
- `crates/genie-core/src/memory/recall.rs`
  Adds recall-with-context filtering and promotion scoring.
- `crates/genie-core/src/tools/quick.rs`
  Routes obvious user requests directly to memory, home status, home control,
  reminders, media, timers, web search, or other tools before using the LLM.
- `crates/genie-core/src/tools/dispatch.rs`
  Executes memory tools and applies the same tool boundary used by chat, voice,
  HTTP, and CLI callers.

## Layered Model

The current system has four recall layers. They are intentionally ordered from
most precise to least precise.

```text
user query
   |
   v
quick router
   |
   +--> structured household answer
   |       exact SQL over profiles, rules, permissions, schedules, inventory,
   |       logs, device state summaries, and known typed records
   |
   +--> household note/document FTS
   |       SQLite FTS5 over manuals, notes, receipts, forms, school/project
   |       documents, recipes, contacts, storage records, and guides
   |
   +--> semantic recall
   |       deterministic local embeddings over selected memories, boosted by
   |       household semantic type labels
   |
   +--> generic memory FTS
           fallback FTS5 over all allowed memory rows
```

Home actions are not executed merely because memory recalled something. The
result still goes through the tool dispatcher and home actuation policy.

## SQLite Data Model

### Base Table

All memories start in `memories`:

- `id`
- `kind`
- `content`
- `created_ms`
- `accessed_ms`
- `recall_count`
- `max_score`
- `promoted`
- `query_hashes`
- `evergreen`
- `scope`
- `sensitivity`
- `spoken_policy`
- `display_order`

This table is the source of truth. Projection tables can be rebuilt from it.

### Generic FTS

`memories_fts` is an FTS5 virtual table over `memories.content`. It supports
general keyword search and older broad memory recall.

SQLite triggers keep it synchronized on insert, delete, and content update.

### Typed Household Projections

Typed projections make exact recall fast and auditable. They are derived from
specific `kind` values and recognizable content patterns.

Implemented projection tables include:

- `household_profiles`
  Names and roles such as father, mother, son, daughter, guest, or other
  household role labels.
- `household_profile_attributes`
  Person-specific attributes such as shoe size, locker combination references,
  preferences, and similar structured fields.
- `device_aliases`
  Human names like "playroom", "desk lamp", "garage door", or "stars" mapped to
  internal device targets.
- `household_rules`
  Rules and constraints such as allergens, screen time, homework requirements,
  appliance permissions, outdoor-play boundaries, and child permissions.
- `household_notes`
  Typed notes and documents used for FTS recall.
- `app_only_secret_references`
  Redacted references for secrets. GenieClaw can say where to find a secret in a
  secure vault without speaking the value.
- `media_profile_items`
  Playlists, media aliases, and owner-specific media targets.
- `family_calendar_events`
  Lessons, appointments, school events, rehearsals, pickup windows, travel, and
  other family schedule facts.
- `shopping_list_items`
  Pending, removed, and categorized shopping-list rows.
- `household_inventory_items`
  Pantry, storage, tools, documents, physical objects, project supplies, and
  last-known locations.
- `access_permissions`
  Who may operate locks, doors, appliances, network access, media, and other
  bounded home capabilities.
- `household_task_logs`
  Chores, hygiene checks, pet-care events, bedtime/morning check-ins, medicine
  routines, and other task completion data.
- `household_schedule_items`
  Utility bills, trash/recycling, business hours, channel guides, transport
  times, subscriptions, sunset, city services, and similar schedule records.
- `household_event_logs`
  Device, security, delivery, access, appliance, sensor, and automation events.
- `embedded_memories`
  Deterministic local semantic vectors for selected memories.

The projection functions run on write and on database open. Rebuilding on open
is intentional: projection logic can improve over time without requiring every
old memory row to be rewritten.

### Household Notes FTS

`household_notes_fts` indexes `household_notes.title` and
`household_notes.content`.

This layer is used for questions like:

- "Find the washing machine warranty."
- "Where are the spare batteries?"
- "Find Mia's debate research about school lunches."
- "How do I reset the printer Wi-Fi?"
- "Find furnace manual troubleshooting code 31."

The result is still policy-filtered before it is returned or injected.

### Local Semantic Embeddings

`embedded_memories` stores:

- `source_memory_id`
- `memory_type`
- `embedding_model`
- `dimensions`
- `embedding`
- `updated_ms`

The current provider is `local-hash-home-v1` with 64 dimensions. It is not a
neural model. It tokenizes text, expands common home concepts, buckets weighted
tokens into a stable vector, normalizes the vector, and ranks by cosine
similarity.

This gives useful fuzzy matching for home phrases like:

- "I'm cold" -> thermostat comfort preferences
- "I'm bored" -> activity suggestions
- "make my room cozy" -> room scene preferences
- "there's water under the sink" -> leak safety routine
- "why is the office internet slow" -> network diagnostics memory
- "make the laundry room not scary" -> child comfort lighting preference

The semantic layer also uses explicit household type labels. For many supported
scenarios, both the stored memory and the query get the same type prefix, such
as `basement_humidity_cause`, `guest_wifi_devices`, `green_bowl_recipe`, or
`final_safety_sweep`. Matching types receive a score boost, which makes recall
predictable even with lightweight embeddings.

## Write Path

There are several ways a memory gets written.

### Explicit Memory Tools

The user can ask GenieClaw to remember something. The quick router and tool
dispatcher can turn direct requests into `memory_store` calls.

Examples:

- "Remember that my red hoodie is in Dad's car."
- "Remember I like the green night-light better."
- "Save this lighting for art time."
- "Add batteries and poster board to my project list."

The store path:

```text
user says remember/save/add
   |
   v
quick router detects store intent
   |
   v
memory policy classifies scope/sensitivity/spoken policy
   |
   v
memories row is inserted or resolved-update replaces an older fact
   |
   v
typed projections, FTS rows, embeddings, and markdown artifacts update
```

### Automatic Fact Extraction

`memory/extract.rs` can capture simple identity and preference facts from casual
text. It is intentionally conservative and rejects question-shaped text or
high-risk sensitive content.

Examples include:

- names
- occupations
- basic likes and dislikes
- relationships
- simple location facts

### Profile And Managed Imports

Profile ingest can load TOML or text facts into memory. Managed memory updates
can refresh canonical rows and preserve display order or promoted artifacts.

### Policy First

Before a memory is stored or spoken, `memory/policy.rs` infers:

- scope: household, person, private, restricted, app-only, or similar posture
- sensitivity: normal, sensitive, high-risk secret, etc.
- spoken policy: allowed, shared-room-limited, app-only, or blocked

High-risk secrets are rejected. Some secret-like facts are kept only as app-only
references. For example, GenieClaw can answer that a Netflix password is in the
secure vault, but it should not speak the password into the room.

## Recall Path

### 1. Quick Router

The quick router handles common requests without waiting for the LLM to infer a
tool call. It recognizes:

- structured household questions
- household note and document lookup
- semantic household memory questions
- reminders and alarms
- home status requests
- home control requests
- media requests
- timers, math, weather, system status, and web search

This keeps latency low and avoids unnecessary prompt/context cost.

### 2. Structured Household Answer

`Memory::structured_household_answer()` is used when the query can be answered
from typed records.

Examples:

- "Who is the dad in this house?"
- "Is Leo allowed to use the stove?"
- "Did Mia take her allergy medicine?"
- "What devices are on guest Wi-Fi?"
- "Which automation fired the most today?"
- "Was the freezer door left open?"

This layer is preferred for facts, permissions, safety status, schedules, device
logs, and counts because it is deterministic and inspectable.

### 3. Household Notes And Documents

When the query asks to find a note, manual, receipt, form, recipe, project file,
or instruction, GenieClaw uses `household_notes_search()`.

The FTS query is normalized so natural phrases can match stored notes. If FTS
does not return a result, a LIKE fallback can still find simple keyword matches.

### 4. Semantic Search

`Memory::semantic_search()` embeds the query with the local provider and compares
it against `embedded_memories`.

The algorithm:

1. trim and reject empty queries
2. expand the query text with home-specific synonyms or scenario type labels
3. embed with `LocalHashEmbeddingProvider`
4. scan stored embeddings for the same model
5. compute cosine similarity
6. boost hits whose semantic type matches the query type
7. drop hits below `SEMANTIC_MIN_SCORE`
8. sort by score and ID
9. update recall tracking for selected hits

This is an exact SQLite scan over small local vectors. It is designed for the
current appliance memory size, not for million-document search.

### 5. Prompt Injection

`memory/inject.rs` selects a compact set of relevant memories for the LLM
prompt. It reads persisted policy metadata and filters private or restricted
facts before injection.

The goal is not to make the prompt remember everything. The goal is to inject a
small, high-signal context slice that helps the model answer or choose a tool.

## Ranking And Promotion

The memory system tracks use:

- `accessed_ms`
- `recall_count`
- `max_score`
- distinct query hashes

Recall scoring uses:

- lexical or semantic match quality
- recency
- recall frequency
- query diversity
- consolidation over repeated use
- decay from the configured half-life

Frequently and broadly recalled memories can become promotion candidates.
Promoted shared-safe memories are written to `memory/MEMORY.md`. Person-scoped or
private promoted content is not written there in full.

## Canonical Markdown Artifacts

Each memory database has a sibling `memory/` directory:

- `INDEX.md`
  Generated entry point for durable memory artifacts.
- `YYYY-MM-DD.md`
  Daily human-readable memory notes.
- `events/YYYY-MM-DD.jsonl`
  Append-only event logs.
- `MEMORY.md`
  Promoted durable entries safe for shared household disclosure.
- `namespaces/<scope>/<kind>.md`
  Namespace views for operator browsing and debugging.

These files are generated artifacts. SQLite remains the source of truth.

## Privacy And Safety Boundaries

Memory is not authorization.

Important rules:

- Speaker identity helps route household memory, but it is not security-grade
  authentication for locks, payments, or hostile-user isolation.
- Shared-room voice defaults to conservative disclosure.
- App-only secrets are referenced, not spoken.
- Memory recall can suggest an action, but home actuation still passes through
  tool policy, confirmation rules, origin policy, and the runtime safety gate.
- Camera/privacy operations are logged through privacy or security audit paths
  where implemented.
- Emergency and safety intents route to bounded safety behavior, not broad
  autonomous control.

Examples:

- "I smell gas" should escalate safety instructions and alerts, not wait for a
  general chat answer.
- "Leo: Can I open the garage door?" should use child permission rules and deny
  unsupervised operation.
- "Mia: Turn off the hallway camera while sleepover guests change" should use
  temporary privacy mode while leaving safety sensors active.

## How The 260 Household Scenarios Map

The recent household scenario batches are covered through real routing and
memory behavior, not demo-only text.

The implemented coverage falls into these groups:

- exact structured recall:
  family roles, permissions, schedules, chores, device states, logs, inventory,
  batteries, filters, utility readings, guest network devices, energy reports,
  security reports, and final safety sweeps
- FTS note and document recall:
  manuals, receipts, warranties, school forms, project notes, recipes, guides,
  health documents, paint colors, storage locations, and troubleshooting codes
- semantic recall:
  comfort scenes, mood/activity preferences, vague troubleshooting, child
  reassurance, air-quality concerns, leak/safety phrases, cooking inspiration,
  quiet modes, and contextual routines
- hybrid-style behavior:
  quick routing selects the right tool layer, memory labels improve semantic
  precision, and home control/status requests use structured targets instead of
  being treated as free-form chat
- store/update behavior:
  preferences, reminders, alarms, project list items, scene aliases, and
  automation preferences are inserted through memory-store or home-control
  routes

The tests in `crates/genie-core/src/memory/mod.rs` and
`crates/genie-core/src/tools/quick.rs` validate representative exact, FTS,
semantic, and quick-routing behavior for these scenarios.

## Example Walkthroughs

### "Sarah: What groceries are low?"

1. Quick router recognizes a structured household question.
2. Memory queries projected inventory and shopping-list facts.
3. Pantry staples can be ranked by frequency or reorder threshold when present.
4. The answer is returned without semantic search or LLM guessing.

### "Mia: Find my debate research about school lunches."

1. Quick router sends this to memory recall as a household note/document query.
2. `household_notes_search()` searches FTS for debate, school, and lunch terms.
3. The result is filtered by owner or access metadata when available.
4. The latest matching school document is returned.

### "Leo: I'm too scared to go downstairs."

1. Quick router recognizes a semantic household memory question.
2. Semantic query typing maps the phrase to a child reassurance context.
3. A matching memory or control route resolves path-light behavior and parent
   notification.
4. Any actual lighting action still goes through home-control policy.

### "Jared: Give me a privacy report for the cameras."

1. Quick router classifies this as structured home status.
2. Memory/status code targets camera access logs, privacy-mode events, and
   recording rules.
3. The response summarizes unusual access and privacy-mode changes without
   dumping raw camera internals.

### "Sarah: Find the recipe where we used the green bowl."

1. The query uses both FTS and semantic cues.
2. Meal notes and recipe embeddings can match the visual clue "green bowl".
3. Hybrid ranking boosts the recipe with a family note attached to the meal.
4. The answer returns the recipe name and relevant note.

### "Jared: Run a final safety sweep."

1. Quick router sends the request to home control/status as a named safety sweep.
2. The sweep checks lock, window, appliance, smoke, leak, and security-mode
   facts.
3. Active risks are ranked before routine warnings.
4. The response is concise and actionable.

## Operational Surfaces

Useful operator surfaces:

- `memory_status` tool:
  reports DB/FTS health, canonical artifact counts, and policy-scope counts.
- HTTP memory endpoints:
  allow dashboard update, delete, and reorder behavior.
- `genie-ctl`:
  provides local operator access to status and memory-related commands.
- Canonical `memory/` artifacts:
  help inspect durable memory state without querying SQLite directly.

## Extension Checklist

When adding a new memory-backed capability:

1. Decide the primary layer:
   structured SQL, household FTS, semantic recall, generic FTS, or home control.
2. Add or reuse a `kind` name for stored memories.
3. Ensure policy classification is correct for that kind and content.
4. Add projection support if exact recall is needed.
5. Add household note support if FTS document recall is needed.
6. Add semantic type support only when vague phrasing needs fuzzy recall.
7. Add quick-router coverage for clear user phrasing.
8. Add tests for the exact path and at least one natural query.
9. Avoid storing or speaking high-risk secret values.
10. Update docs when the behavior changes.

Do not add a route that only returns a hardcoded demo answer. The route should
connect to a real memory table, projection, FTS search, semantic memory type,
home status target, home control target, or store/update path.

## Current Limits

Known limits of the current implementation:

- Local semantic recall is deterministic and lightweight, not a neural embedding
  model.
- Semantic search scans SQLite rows and is suitable for current household memory
  sizes, not large document corpora.
- Some "hybrid" behavior is implemented as typed routing plus recall/ranking
  rather than a general query planner.
- Voice identity supports routing and personalization, not hostile-user security.
- Home Assistant is still the transitional home integration boundary.
- Future sqlite-vec, pgvector, Qdrant, or cuVS-style backends would be optional
  providers, not replacements for the structured and policy layers.

These limits are deliberate. The current system prioritizes local reliability,
inspectability, privacy, and low latency over adding a heavier vector service.
