# Agentic AI Research Notes

This note captures the relevant technical takeaways from current agentic AI
application patterns and maps them to GenieClaw implementation decisions.

The point is not to copy Claude Code, OpenClaw, LangGraph, or cloud agent
frameworks. GenieClaw is a local physical home agent. We adopt only patterns
that improve daily usefulness, safety, observability, and deterministic local
operation on Jetson-class hardware.

## High-Signal Trends

### 1. Persistent Control Planes Beat One-Off Chat

Modern useful agents are long-running systems with sessions, tools, memory,
channels, and event handling.

GenieClaw relevance:

- `genie-core` should remain a persistent local agent runtime.
- Channels such as web, CLI, REPL, voice, Telegram, and future mobile surfaces
  should route through the same memory/tool/policy layer.
- Local sessions and audit history matter as much as raw model response quality.

Current implementation:

- persistent conversation store
- local memory store
- channel-specific request origins
- dashboard/CLI surfaces
- actuation audit and recent action ledger

Next work:

- explicit channel/session registry
- per-channel permissions and rate limits
- more visible routing diagnostics

### 2. Tools, Skills, Hooks, And Protocols Are The Product Surface

The model is not the whole application. The useful surface is the controlled
set of tools, permissions, schemas, skills, hooks, and runtime contracts around
the model.

GenieClaw relevance:

- tool schemas must be stable, inspectable, and versioned
- skills must be permissioned, auditable, and removable
- risky physical actions must not rely on prompt compliance

Current implementation:

- model-visible tool definitions
- deterministic quick router for common daily requests
- native skill loader baseline
- actuation safety gate, confirmations, audit log, and bounded undo

Next work:

- signed skill manifests
- per-skill permissions
- hook points for safe pre/post action checks
- compatibility tests for the tool surface

### 3. Deterministic Startup Is A Production Requirement

Persistent assistants need reproducible boot state. Operators need to know what
prompt, tools, policy, model family, and hydrated state are active.

GenieClaw relevance:

- every field unit should expose a startup/runtime fingerprint
- prompt/tool/policy drift should be visible without SSH debugging
- memory and action hydration should be explicit

Current implementation:

- `GET /api/runtime/contract`
- prompt hash
- tool schema hash
- policy hash
- hydration hash
- full contract hash
- policy and hydrated-state JSON payloads
- compact contract summary in `/api/health`
- boot contract JSONL log at `<data_dir>/runtime/contracts.jsonl`
- dashboard runtime contract card
- `genie-ctl support-bundle` includes the active contract and recent contract log tail
- optional `[core].expected_runtime_contract_hash` drift detection

Next work:

- integrate contract drift with OTA/update windows and alert forwarding

### 4. Security And Permission Boundaries Are Core Features

Agentic systems fail dangerously when tools, memory, and external input share
one unbounded trust zone.

GenieClaw relevance:

- all inbound text is untrusted
- retrieved content and web results are untrusted
- memory recall must respect shared-room privacy
- home control must be fail-closed

Current implementation:

- prompt-injection checks
- output sanitization and secret redaction
- environment sanitization for tools
- memory write/read policy
- actuation confirmation and runtime safety gate
- actuation channel allowlist by request origin
- per-origin physical actuation rate limits
- web-search sensitive-query blocking

Next work:

- per-tool capability policy
- per-channel permission policy
- stronger skill sandboxing
- RAG/document-ingest prompt-injection screening before vector memory rollout

### 5. Multi-Agent Patterns Are Useful Only Behind Strong Isolation

Multi-agent systems are useful for coding and research workflows, but a home
physical agent should not spawn unconstrained workers for actuation.

GenieClaw relevance:

- use multi-agent patterns for offline research, planning, diagnostics, and
  long-running non-physical jobs
- keep physical home control single-authority and deterministic

Current implementation:

- no general multi-agent execution loop in the home-control path

Next work:

- background diagnostic workers with no actuation permission
- isolated workspaces for future developer/operator tasks
- explicit routing rules before any multi-agent feature reaches production

### 6. RAG And Vector Search Belong In Memory, But Must Be Treated As Untrusted

RAG increases usefulness, but retrieved chunks can carry indirect prompt
injection. Vector search also creates real edge-inference and GPU acceleration
workloads.

GenieClaw relevance:

- vector memory can improve recall quality
- cuVS/FAISS-style acceleration belongs later, likely in `genie-ai-runtime` or
  a lower memory/index service
- document ingestion must screen and label untrusted content

Current implementation:

- SQLite FTS memory
- memory policy metadata
- vector memory design documented separately

Next work:

- local embedding provider abstraction
- untrusted-document labels
- retrieval-time injection screening
- evaluation set for memory recall quality

## Adopt Now

- deterministic runtime contract
- stricter tool/policy visibility
- action audit and undo surfaces
- memory manager and policy-aware recall
- channel origin tracking
- local-first search with local SearXNG option

## Adopt Later

- MCP server/client interfaces
- signed skill marketplace
- multi-agent workers for non-physical background jobs
- GPU vector search acceleration
- richer gateway/node model across app, mobile, and device surfaces

## Do Not Adopt Blindly

- broad cloud-agent frameworks that increase context and memory pressure
- unbounded multi-agent orchestration for physical control
- plugins without permissions, review, and audit
- RAG over arbitrary documents without prompt-injection defenses
- giant context windows as a substitute for better memory and routing

## Product Rule

For GenieClaw, a research idea is relevant only if it improves at least one of:

- repeated daily usefulness
- local-first reliability
- physical safety
- privacy in shared spaces
- operator observability
- Jetson memory/runtime efficiency

If it does not, it belongs outside the near-term implementation path.
