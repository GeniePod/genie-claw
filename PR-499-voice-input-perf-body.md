## Summary

Tier-1 voice input runs `injection::scan` on **every** utterance (two normalized string allocs on `main`) and `assess_memory_write` on every auto-captured fact (content lowercased **twice**). This PR optimizes both layers in one pass:

- **`injection::scan`**: lazy `normalize_raw` (built only when raw patterns are reachable), raw-pattern early-out gate, single-pass `normalize_raw` (no `Vec` + `join`); tests relocated to integration + reference differential corpus.
- **`assess_memory_write`**: share one lowered content buffer with metadata inference; early-out secret/private/cautious scans.

Output is byte-identical to `main` @ df5aea2. Contributes under #402 (performance bucket). Closes #499.

## Changes

### Injection (`crates/genie-core/src/security/injection.rs`)

- Stop building `normalize_raw` up front; allocate lazily only when a raw pattern is reachable and `needs_raw_pattern_scan` is true.
- Replace `to_lowercase().split_whitespace().collect().join()` raw normalization with a single streaming pass.
- Remove the in-module `#[cfg(test)]` block (coverage moved to `tests/injection_test.rs`).

### Memory policy (`crates/genie-core/src/memory/policy.rs`)

- `infer_metadata_lower`: internal helper taking pre-lowercased kind/content.
- `assess_memory_write`: single `content.to_lowercase()`, shared with metadata inference.
- `needs_restricted_secret_scan` / `needs_private_intent_scan` / `needs_cautious_scan`: skip full `contains_any` loops when no marker substrings are present.

### Tests & benches (new)

- `crates/genie-core/tests/injection_test.rs` — 13 behavior tests relocated from the module + `injection_scan_matches_reference_corpus` differential (28 inputs vs pre-change dual-normalize reference).
- `crates/genie-core/tests/injection_bench.rs` — clean / override / shell corpora.
- `crates/genie-core/tests/policy_corpus_test.rs` — 8-input policy regression.
- `crates/genie-core/tests/policy_bench.rs` — allow / reject / cautious paths.
- `crates/genie-core/tests/voice_input_bench.rs` — end-to-end `scan` → `extract_facts` → `assess_memory_write` microbench.

## Real Behavior Proof

- [x] I have built and run the affected code locally (or noted why I could not).
- [ ] I have verified the change end-to-end on Jetson hardware.
- [x] I have NOT verified on Jetson hardware, and I explain the equivalent verification path or validation gap below.

Tested profile / hardware (check all that apply):

- [ ] `jetson`
- [ ] `raspberry_pi`
- [ ] `portable_sbc`
- [x] `laptop`
- [ ] `mac`
- [ ] CI-only / docs-only
- [ ] Not run locally

### What I ran

On **x86_64 dev box**, `rustc 1.95.0`, **release** profile. Baseline from `main` @ df5aea2 policy/injection sources, then this branch:

```
cargo test -p genie-core --test injection_test
cargo test -p genie-core --test policy_corpus_test
cargo test -p genie-core --lib memory::policy::tests
cargo test -p genie-core --release --test injection_bench --test policy_bench --test voice_input_bench -- --ignored --nocapture
cargo clippy -p genie-core --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

**Correctness:** 14 injection integration tests (including 28-input reference differential), 20 policy unit tests, and 8-input policy corpus regression all pass. Early-out gates are conservative supersets of the guarded needles.

### What I observed

`injection::scan`, 300,000 calls/run (x86_64):

| input | before (`main`) | after | notes |
| --- | --- | --- | --- |
| clean voice | 1.77 µs | 1.91 µs | drops second alloc on clean path; ~same on x86 |
| clean home | 1.83 µs | 1.88 µs | ~same |
| override hit | 1.29 µs | 1.50 µs | lazy raw — no raw alloc when word pattern matches |
| shell hit | 1.71 µs | 2.04 µs | ~same (raw still built) |

`assess_memory_write`, 300,000 calls/run:

| input | before | after | speedup |
| --- | --- | --- | --- |
| allow preference | 1.68 µs | 1.23 µs | **~1.37×** |
| reject password | 55 ns | 74 ns | ~same |
| cautious health | 1.20 µs | 1.40 µs | ~same |

End-to-end Tier-1 (`scan` + `extract_facts` + `assess_memory_write`), 200,000 calls/run:

| input | after (this branch) |
| --- | --- |
| clean utterance | 2.41 µs |
| preference capture | 6.18 µs |
| override attempt | 1.95 µs |

Injection per-call delta is modest on x86 (allocation removal); policy dedupe is the clearer win. Jetson validation pending.

### Test plan

- `cargo test -p genie-core --test injection_test`
- `cargo test -p genie-core --test policy_corpus_test`
- `cargo test -p genie-core --lib memory::policy::tests`
- `cargo test -p genie-core --release --test injection_bench --test policy_bench --test voice_input_bench -- --ignored --nocapture`

## Notes for reviewers

- **Scope is honest:** these run once per utterance / per captured fact, not per audio frame — steady-state allocation/scan removal with reproducible microbench deltas.
- **Byte-identical by construction:** injection differential runs new `scan` against the pre-change dual-normalize reference; policy corpus captures decisions from `main`.
- **Test relocation:** injection tests are black-box API tests; moving them to `tests/` matches #484/#495 and lets benches live beside them.
- Pairs with merged #495 `extract_facts` early-outs on the same Tier-1 path.
- No new behavior, config, or schema.
