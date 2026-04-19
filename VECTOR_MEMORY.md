# Vector Memory Design

Design note for adding semantic retrieval to GenieClaw without breaking the current Jetson-first architecture.

This document answers a specific question:

> Can GenieClaw borrow the good ideas from `cuVS` without importing the whole RAPIDS stack into the core runtime?

The answer is yes, but only if we borrow architecture and API shape first, and keep any future code reuse narrow, isolated, and optional.

## Decision Summary

GenieClaw should not adopt `cuVS` as a hard dependency in `genie-core` today.

GenieClaw should instead:

1. keep the current SQLite `FTS5` memory path as the default
2. add a provider boundary for semantic retrieval
3. treat GPU vector search as an optional backend
4. prefer borrowing ideas and interface patterns before copying code
5. only reuse code in small, auditable pieces with preserved license notices

This keeps the appliance path stable on Jetson while still allowing higher-end vector retrieval later.

## Current State

Today, GenieClaw memory is:

- SQLite-backed
- `FTS5` keyword search
- BM25 ranking
- time decay
- promotion/recall tracking

That means the memory system is already useful, but it is not yet semantic retrieval. There is no embedding pipeline, no vector index, and no hybrid scoring between keyword and vector similarity.

Relevant files:

- `crates/genie-core/src/memory/mod.rs`
- `crates/genie-core/src/memory/extract.rs`
- `crates/genie-core/src/memory/inject.rs`
- `crates/genie-core/src/memory/recall.rs`

## What To Borrow From `cuVS`

The valuable part of `cuVS` for GenieClaw is not “GPU fast” in the abstract. It is the way the problem is split.

Borrow these ideas:

- explicit resource ownership
  - one object owns CUDA streams, handles, scratch allocation, and setup cost
- clear separation between index construction and search
  - build once, query many
- explicit `index params` versus `search params`
  - build-time choices are not mixed with per-query tuning
- multiple index families behind one conceptual surface
  - brute force, graph ANN, IVF-style ANN, and future variants
- batch-first APIs
  - search one query or many without changing the overall model
- host/device boundary made visible
  - copying and memory placement are part of the design, not hidden magic

Those ideas are valuable even if GenieClaw never links against `cuVS`.

## What Not To Borrow

Do not import these assumptions into the core appliance path:

- RAPIDS as a required dependency chain for `genie-core`
- a CUDA-only memory architecture
- server-GPU assumptions about available VRAM
- ANN everywhere
- vector retrieval as the default for every query and every device

GenieClaw’s current product target is Jetson Orin Nano 8 GB. That device already runs close to the edge when the local LLM is active. Competing for the same GPU memory with a large vector index is the wrong default.

## Proposed Architecture

The right shape is:

1. keyword retrieval remains the baseline
2. semantic retrieval becomes a provider
3. fusion stays in GenieClaw
4. GPU acceleration is optional and off the critical boot path

### Layering

```text
user query
   |
   v
memory query planner
   |
   +--> keyword retriever (SQLite FTS5)
   |
   +--> semantic retriever (optional provider)
   |
   v
fusion / rerank / decay / trust policy
   |
   v
prompt context injection
```

### Minimal Trait Boundary

The core should own a narrow Rust interface like this:

```rust
pub struct SemanticHit {
    pub id: String,
    pub score: f32,
    pub metadata: serde_json::Value,
}

pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>>;
}

pub trait SemanticIndex: Send + Sync {
    fn upsert(&self, id: &str, vector: &[f32], metadata: &serde_json::Value) -> anyhow::Result<()>;
    fn delete(&self, id: &str) -> anyhow::Result<()>;
    fn search(&self, vector: &[f32], limit: usize) -> anyhow::Result<Vec<SemanticHit>>;
}
```

That is the important line. `genie-core` should depend on this boundary, not on `cuVS` directly.

## Recommended Rollout

### Phase 0: Keep Current Memory As Default

Do nothing to the appliance path by default.

Keep:

- SQLite `FTS5`
- BM25
- time decay
- promotion logic

This stays the fallback and the default even after semantic memory exists.

### Phase 1: Add Embeddings Without ANN

First add embeddings and persistence without introducing a GPU index.

Why:

- it tests the product value before the systems cost
- it keeps debugging simple
- it lets us build hybrid retrieval logic before optimizing it

Suggested shape:

- add an `embeddings` table in SQLite
- store:
  - `memory_id`
  - `model`
  - `dim`
  - `vector_blob`
  - `updated_ms`
- only embed selected records:
  - promoted memories
  - imported profile/doc facts
  - high-signal notes

Do not embed every turn by default.

### Phase 2: Add Hybrid Retrieval

Once vectors exist, add a hybrid query path:

1. run FTS retrieval
2. run semantic retrieval if embeddings are available
3. merge hits by stable `memory_id`
4. compute a fused score

Suggested score:

```text
final = 0.45 * keyword_score
      + 0.35 * semantic_score
      + 0.10 * recency_score
      + 0.10 * recall_score
```

This keeps semantic search important but not dominant.

### Phase 3: Add Optional Semantic Backends

At this point, add backends behind the trait:

- `None`
  - no semantic retrieval
- `SqliteFlat`
  - brute-force vector scan from SQLite blobs
- `CpuAnn`
  - optional CPU ANN backend if needed later
- `CuvsSidecar`
  - optional GPU backend on supported hardware

This is the right point to integrate `cuVS`, not before.

### Phase 4: Add Query Planning

The retriever should not pay semantic costs on every query.

Use semantic retrieval for:

- “what do you know about me”
- “what did I tell you about X”
- profile-based preference recall
- long-form note/doc search

Do not use semantic retrieval for:

- time
- weather
- timers
- direct tool routing
- obvious exact-match identity facts when keyword search already works

## Why A Sidecar Is Better Than Direct Linking

If GenieClaw ever uses `cuVS`, the safest shape is a sidecar process, not a direct dependency inside `genie-core`.

Reasons:

- avoids dragging RAPIDS/CUDA complexity into the main core binary
- failure isolation
  - index crashes do not kill chat
- easier fallback
  - if the sidecar is missing, GenieClaw still works
- easier hardware targeting
  - the sidecar can be absent on unsupported Jetson installs
- cleaner operational model
  - one service can own GPU vector memory and index lifecycle

Suggested service:

- `genie-vector`

Suggested responsibilities:

- own the embedding model or embedding ingestion path
- build and persist optional vector indexes
- answer vector search RPC requests locally
- expose health and index statistics

## Jetson-Safe Rules

If semantic memory is added on Jetson, these rules should stay in force:

- the default deployment must not require a vector backend
- semantic indexing must be opt-in
- embeddings should be computed selectively, not for every utterance
- vector memory must not reduce LLM reliability
- if GPU memory is tight, semantic search must disable itself automatically

Practical rule:

- the LLM gets the GPU first
- vector search gets leftover budget, or moves to CPU, or stays off

## Data Model Proposal

Add these tables next to current memory tables:

```text
embeddings
- memory_id        TEXT PRIMARY KEY
- model            TEXT NOT NULL
- dim              INTEGER NOT NULL
- vector_blob      BLOB NOT NULL
- updated_ms       INTEGER NOT NULL

embedding_jobs
- memory_id        TEXT PRIMARY KEY
- state            TEXT NOT NULL
- error            TEXT
- updated_ms       INTEGER NOT NULL
```

Why separate tables:

- keeps current memory schema stable
- allows model migration
- allows partial backfill
- makes opt-in indexing straightforward

## Query Pipeline Proposal

For a semantic-enabled memory query:

1. normalize the query
2. run FTS search
3. decide whether semantic search is justified
4. if yes, embed the query and search the semantic backend
5. merge hits by `memory_id`
6. apply:
   - keyword score
   - semantic score
   - age decay
   - recall/promotion features
7. take top `k`
8. inject only the short, high-signal results into prompt context

The core design point is that vector search returns candidates. GenieClaw still owns the final ranking.

## Code Reuse Policy

This is the important governance rule.

GenieClaw may borrow:

- API ideas
- index/search separation
- parameter shapes
- lifecycle patterns

GenieClaw should avoid copying:

- large algorithm implementations
- GPU kernels
- broad wrapper layers
- generated bindings without a real need

If code is ever reused directly from an Apache-2.0 project like `cuVS`, do it only under these conditions:

1. the copied code is small and isolated
2. provenance is recorded in the source file
3. the original license header and notices are preserved where required
4. the reused code lives behind a narrow internal boundary
5. a local test proves it is worth keeping

If those conditions are not met, prefer:

- a sidecar integration
- FFI boundary
- or reimplementation from the public interface and published behavior

## Concrete Recommendation

For GenieClaw, the next correct step is not “integrate `cuVS`”.

The next correct step is:

1. add a semantic-memory trait boundary
2. add optional embedding storage
3. add hybrid retrieval logic
4. keep SQLite `FTS5` as the default
5. evaluate a `genie-vector` sidecar later

That gives GenieClaw the important ideas from `cuVS` without taking on the full cost today.

## Suggested Next Implementation Steps

If this design is accepted, the next engineering tasks should be:

1. create `crates/genie-core/src/memory/semantic.rs`
   - trait definitions only
2. add config for semantic retrieval mode
   - `none`, `sqlite_flat`, `sidecar`
3. add embedding persistence tables
4. add a simple `SqliteFlat` brute-force backend for development
5. add hybrid ranking in the memory query path
6. defer `cuVS` evaluation until the semantic path proves product value

That sequence is the lowest-risk path from current GenieClaw to future semantic memory.
