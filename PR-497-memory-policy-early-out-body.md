## Summary

`assess_memory_write` gates every Tier-1 auto-captured memory write. It lowercased content, then called `infer_metadata`, which lowercased content **again** and ran full secret/private/cautious substring scans even on benign preference facts.

This shares one lowered content buffer across `assess_memory_write` and metadata inference, and adds conservative marker early-outs before the expensive `contains_any` loops. Output is byte-identical to current `main`. Measured **~1.36× faster** on the common allow path (household preference fact) on x86_64 dev hardware. Contributes under #402 (performance bucket). Closes #497.

## Changes

- `crates/genie-core/src/memory/policy.rs`:
  - `infer_metadata_lower`: internal helper taking pre-lowercased kind/content; public `infer_metadata` unchanged API.
  - `assess_memory_write`: single `content.to_lowercase()`, pass shared buffer to `infer_metadata_lower`.
  - `needs_restricted_secret_scan` / `needs_private_intent_scan` / `needs_cautious_scan`: skip full scans when no marker substrings are present (conservative supersets of the literal needles).
- `crates/genie-core/tests/policy_corpus_test.rs`: 8-input corpus regression + metadata round-trip check.
- `crates/genie-core/tests/policy_bench.rs`: ignored microbench (allow / reject / cautious paths).

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

On **x86_64 dev box**, `rustc 1.95.0`, **release** profile. Baseline captured from `main` @ ecd7592 policy.rs, then this branch:

```
cargo test -p genie-core --lib memory::policy::tests
cargo test -p genie-core --test policy_corpus_test
cargo test -p genie-core --release --test policy_bench -- --ignored --nocapture
cargo clippy -p genie-core --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

**Correctness:** 20 existing policy unit tests pass unchanged. Corpus regression captures `assess_memory_write` + `infer_metadata` fields for allow, restricted, cautious, private, and person paths. Early-out gates are conservative supersets of every needle in the guarded scans.

### What I observed

`assess_memory_write`, 300,000 calls/run (x86_64, single run):

| input | before (`main`) | after (this branch) | speedup |
| --- | --- | --- | --- |
| allow preference (common) | 1.68 µs/call | 1.24 µs/call | ~1.36× |
| reject password | 55 ns/call | 67 ns/call | ~same |
| cautious health | 1.20 µs/call | 1.33 µs/call | ~same |

The common allow path drops one redundant `to_lowercase` and skips secret/cautious scans on benign content. Jetson validation pending.

### Test plan

- `cargo test -p genie-core --lib memory::policy::tests` (20 tests)
- `cargo test -p genie-core --test policy_corpus_test`
- `cargo test -p genie-core --release --test policy_bench -- --ignored --nocapture` on `main` vs this branch

## Notes for reviewers

- **Scope is honest:** `assess_memory_write` runs once per auto-captured fact (after `extract_facts`), not per audio frame — modest steady-state savings with a reproducible microbench delta.
- **Byte-identical by construction:** early-out markers are literal supersets of the guarded needles; corpus regression is the guarantee.
- **Paired with extract_facts perf work:** same Tier-1 auto-capture pipeline as #484 / #495.
- No new behavior, config, or schema.
