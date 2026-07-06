Fixes #487
Closes #463

## Summary

Replace fixed temp SQLite paths in `prompt.rs` unit tests and two `genie-governor` tests with per-run unique directories (pid + counter + nanos), matching the existing `temp_memory_path` / `make_governor()` convention. Stops readonly-database flakes when WAL sidecars linger at shared paths.

## Changes

- `prompt.rs`: add `prompt_test_memory()` helper; migrate 10 tests off `prompt-test-*.db` fixed paths.
- `genie-governor`: add `test_store()` helper; migrate `night_model_swap_config` and `resolves_llm_alias_to_configured_service_unit`.

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

```bash
cd genie-claw
cargo test -p genie-core --lib prompt_
cargo test -p genie-governor night_model_swap
cargo test -p genie-governor resolves_llm
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

### What I observed

All prompt and governor tests pass. Full workspace clippy and fmt clean.

**Validation gap:** Tests-only; no runtime/Jetson behavior change.

## Test plan

- `cargo test -p genie-core --lib prompt_`
- `cargo test -p genie-governor`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo fmt --all -- --check`

## Notes for reviewers

- Tests-only; completes #463 via #487.
- No production code touched.
