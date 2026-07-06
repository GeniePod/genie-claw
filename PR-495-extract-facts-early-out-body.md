## Summary

After #484, `extract_facts` still walked every identity/preference `extract_pattern` scan and invoked `extract_favorite` on **all** utterances — even when no matching phrase exists. `extract_favorite` also allocated via chained `.replace`, and `extract_remember` re-allocated `to_lowercase()` despite the caller already having the original text.

This adds substring early-outs for the identity and preference blocks, gates `extract_favorite` on `"favo"`, replaces the favorite `.replace` chain with `strip_prefix`, and drops the redundant `to_lowercase` in `extract_remember`. Output is byte-identical to current `main`. Measured **~5.3× faster on the common no-match utterance** on x86_64 dev hardware. Contributes under #402 (performance bucket). Closes #495.

## Changes

- `crates/genie-core/src/memory/extract.rs`:
  - `needs_identity_scan` / `needs_preference_scan`: skip entire pattern blocks when no marker substring is present (safe because all patterns in each block require one of these markers in the already-lowercased text).
  - `extract_favorite`: only called when `"favo"` is present; use `strip_prefix` instead of two `.replace` allocations.
  - `extract_remember`: ASCII case-insensitive `"remember"` prefix via `eq_ignore_ascii_case`; no second `to_lowercase`.
- `crates/genie-core/tests/extract_test.rs`: add `extract_facts_corpus_regression` (25 fixed inputs captured from `main` @ 72b0b28).
- `crates/genie-core/tests/extract_bench.rs`: extend ignored microbench with `identity-hit` and `preference-hit` corpora; rename common case to `no-match`.

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

On **x86_64 dev box**, `rustc 1.95.0`, **release** profile. Baseline captured on `main` @ 72b0b28, then this branch:

```
cargo test -p genie-core --test extract_test
cargo test -p genie-core --release --test extract_bench -- --ignored --nocapture
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

**Correctness:** the 25 existing integration tests pass unchanged, plus a new 25-input corpus regression test with expected `(category, content)` pairs captured from `main`. Early-out gates are conservative supersets of the literal prefixes each block searches for, so skipping cannot hide a match.

### What I observed

`extract_facts`, 300,000 calls/run (x86_64, single run):

| input | before (`main`) | after (this branch) | speedup |
| --- | --- | --- | --- |
| no-match (common) | 1.6 µs/call | 0.30 µs/call | ~5.3× |
| `"my "`, no relationship | 5.71 µs/call | 5.10 µs/call | ~1.12× |
| real relationship | 5.69 µs/call | 5.16 µs/call | ~1.10× |
| identity phrases | — | 3.75 µs/call | (new bench) |
| preference phrases | — | 3.53 µs/call | (new bench) |

The common case (most utterances match nothing) drops from ~1.6 µs to ~0.30 µs — identity/preference prefix scans are skipped entirely after `to_lowercase`. Jetson validation pending; expect a similar relative gain on the no-match path given #484's pattern.

### Test plan

- `cargo test -p genie-core --test extract_test` (25 existing + 1 corpus regression)
- `cargo test -p genie-core --release --test extract_bench -- --ignored --nocapture` on `main` vs this branch to reproduce the table.

## Notes for reviewers

- **Scope is honest:** `extract_facts` runs once per utterance (Tier-1 auto-capture), not per audio frame — this removes redundant scans/allocs with a reproducible microbench delta, not a user-perceptible latency change.
- **Byte-identical by construction:** early-out markers are literal supersets of the fixed prefixes in each block; the corpus regression test guards output.
- **Intentionally unchanged:** `to_lowercase` at the top (required for case-insensitive matching) and the #484 relationship const table + `"my "` early-out.
- No new behavior, config, or schema.
