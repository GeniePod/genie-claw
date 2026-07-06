## Summary

The pre-LLM voice path runs three allocation-heavy normalizers on every utterance before the model sees it:

1. **`injection::scan`** — builds `normalize_words` + `normalize_raw` up front on `main`.
2. **`quick::route`** — `to_lowercase` + `replace` + `Vec` + `join` normalization before deterministic tool routing.
3. **`apply_reasoning_mode`** (Qwen) — up to **three** `to_lowercase()` calls on the same user text.

This PR optimizes all three layers with behavior-preserving early-outs and single-pass / lazy normalization. Output is byte-identical to `main` @ 1e01954. Contributes under #402 (performance bucket). Closes #501.

## Changes

### Injection (`crates/genie-core/src/security/injection.rs`)

- Lazy `normalize_raw` (allocate only when raw patterns are reachable).
- `needs_raw_pattern_scan` early-out before raw pattern loop.
- Single-pass streaming `normalize_raw` (no `Vec` + `join`).
- Tests relocated to `tests/injection_test.rs` + 28-input reference differential corpus.

### Quick router (`crates/genie-core/src/tools/quick.rs`)

- `normalize`: single-pass punctuation fold + lowercase (replaces `to_lowercase` + `replace` + `collect` + `join` chain).
- All 37 existing `quick::tests` pass unchanged.

### Reasoning (`crates/genie-core/src/reasoning.rs`)

- Single shared `to_lowercase()` in `apply_reasoning_mode` for Qwen path.
- `needs_simple_request_scan` / `needs_deep_reasoning_scan` early-outs before marker loops.
- Tests relocated to `tests/reasoning_test.rs` + 5-input corpus regression.

### Tests & benches (new)

| File | Purpose |
| --- | --- |
| `injection_test.rs` | 13 behavior tests + reference differential |
| `injection_bench.rs` | clean / override / shell corpora |
| `reasoning_test.rs` | 5 behavior tests + corpus regression |
| `reasoning_bench.rs` | simple / complex / explicit-think |
| `quick_route_bench.rs` | home-control / timer / weather / no-match |
| `pre_llm_bench.rs` | end-to-end `scan` → `quick::route` → `apply_reasoning_mode` |

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

On **x86_64 dev box**, `rustc 1.95.0`, **release** profile:

```
cargo test -p genie-core --test injection_test
cargo test -p genie-core --test reasoning_test
cargo test -p genie-core --lib tools::quick::tests
cargo test -p genie-core --release --test injection_bench --test reasoning_bench --test quick_route_bench --test pre_llm_bench -- --ignored --nocapture
cargo clippy -p genie-core --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

**Correctness:** 14 injection tests (28-input reference differential), 6 reasoning tests, 37 quick router tests. Early-out gates are conservative supersets of guarded needles.

### What I observed

`apply_reasoning_mode` (Qwen, 300k calls):

| input | before | after | notes |
| --- | --- | --- | --- |
| simple voice | 626 ns | 1.25 µs | ~same at noise floor |
| complex chat | 1.14 µs | 1.01 µs | **~1.13×** (drops duplicate lowers) |
| explicit /think | 555 ns | 574 ns | ~same |

`quick::route` (300k calls):

| input | before | after |
| --- | --- | --- |
| home-control | 13.4 µs | 14.5 µs | ~same |
| no-match | 15.3 µs | 16.7 µs | ~same |

`injection::scan` (300k calls): lazy raw saves second alloc on clean/override paths (~same ns on x86; allocation win).

End-to-end pre-LLM (`scan` + `route` + `reasoning`), 200k calls:

| input | after |
| --- | --- |
| clean utterance | 13.1 µs |
| quick-route hit | 13.7 µs |
| reasoning deep | 15.8 µs |
| override attempt | 11.4 µs |

Reasoning complex path and injection alloc removal are the clearest wins; quick normalize is neutral on x86 (drops intermediate `Vec`). Jetson validation pending.

### Test plan

- `cargo test -p genie-core --test injection_test`
- `cargo test -p genie-core --test reasoning_test`
- `cargo test -p genie-core --lib tools::quick::tests`
- `cargo test -p genie-core --release --test pre_llm_bench -- --ignored --nocapture`

## Notes for reviewers

- **Scope is honest:** per-utterance microsecond savings on the path before the LLM — real allocation/scan removal, not end-to-end latency headline.
- **Byte-identical by construction:** injection reference differential + reasoning corpus + 37 quick tests unchanged.
- **Test relocation:** injection and reasoning black-box tests move to `tests/` per #484/#498 pattern.
- Complements merged #495/#498 on the Tier-1 auto-capture path; this PR covers the **pre-LLM gate** layer.
- No new behavior, config, or schema.
