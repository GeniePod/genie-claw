Fixes #485
Closes #462

## Summary

Complete typed `web_search` argument validation on the voice fast-path and `POST /api/web-search` handler. Both previously used `as_u64().unwrap_or(3)` and silently coerced string limits; they now share `parse_web_search_args` with tool dispatch. Also adds `parse_web_search_fresh` so `"fresh": "true"` is rejected instead of silently becoming `false`.

## Changes

- Add `parse_web_search_fresh` and `pub(crate) parse_web_search_args` in `dispatch.rs`.
- Wire `exec_web_search`, `voice_loop` web_search fast-path, and `handle_web_search` through shared parsers.
- Unit + integration + HTTP endpoint tests for string `limit` and string `fresh`.

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

Linux x86_64 dev host:

```bash
cd genie-claw
cargo test -p genie-core web_search
cargo clippy -p genie-core -- -D warnings
cargo fmt --all -- --check
```

### What I observed

- `web_search_endpoint_rejects_string_limit` — HTTP 400 for `{"query":"rust","limit":"5"}`.
- `web_search_rejects_invalid_arguments_and_audits` — dispatch + audit rejection for string `limit` and string `fresh`.
- `parse_web_search_args_rejects_string_limit` / `web_search_fresh_must_be_boolean_when_provided` pass.
- Clippy and `cargo fmt --check` clean.

**Validation gap:** Voice fast-path not exercised in an end-to-end mic loop; uses same parser as dispatch/HTTP.

## Test plan

- `cargo test -p genie-core web_search`
- `cargo clippy -p genie-core -- -D warnings`
- `cargo fmt --all -- --check`

## Notes for reviewers

- Small, focused diff (~60 LOC production + tests) — completes open #462 via #485.
- No new provider stubs; same typed-tool boundary pattern as merged #408 / #411.
