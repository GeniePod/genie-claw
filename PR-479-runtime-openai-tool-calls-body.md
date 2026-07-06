Fixes #479

## Summary

Runtime tool dispatch (`try_tool_call_with_context` → `parse_tool_call_value`) now accepts the same OpenAI-compatible `tool_calls` / `function_call` wrappers and top-level JSON arrays that BFCL eval parsing already handled. The first call is dispatched at runtime; malformed wrappers fall through to the `#378` unparsed fallback instead of leaking raw JSON to the user.

## Changes

- `parse_tool_call_value` delegates to `parse_tool_call_value_for_eval` after native and single-key compact shapes.
- `is_unparsed_tool_call` recognizes `tool_calls` / `function_call` / array wrappers; flags malformed wrappers that parse as JSON but yield no tool call.
- Runtime tests mirror eval coverage for OpenAI wrapper and JSON-array first-call dispatch.

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

Linux x86_64 dev host, stub `ToolDispatcher` (no HA / network):

```bash
cd genie-claw
cargo test -p genie-core --lib openai_tool_calls
cargo test -p genie-core --lib try_tool_call_accepts
cargo test -p genie-core --lib unparsed_tool_call
cargo clippy -p genie-core -- -D warnings
```

### What I observed

- `try_tool_call_accepts_openai_tool_calls_wrapper` — `{"tool_calls":[...calculate...]}` executes `calculate` and returns `4` for `2+2`.
- `try_tool_call_accepts_json_array_first_call_only` — array dispatches first `get_time` call.
- `openai_tool_calls_wrapper_is_not_flagged_as_unparsed` — valid wrapper no longer treated as normal assistant text path.
- `malformed_openai_tool_calls_wrapper_is_unparsed` — empty tool name triggers `#378` fallback class.
- Existing `#378` unparsed tests still pass. Clippy clean.

**Validation gap:** Not run on Jetson or through live chat/voice loop. Same `try_tool_call_with_context` entry point as `server.rs` / `voice_loop.rs`.

## Test plan

- `cargo test -p genie-core --lib openai_tool_calls`
- `cargo test -p genie-core --lib try_tool_call_accepts`
- `cargo test -p genie-core --lib unparsed_tool_call`
- `cargo test -p genie-core --lib try_tool_call_recovers_scalar_single_key_calculate` (regression: compact shape unchanged)
- `cargo clippy -p genie-core -- -D warnings`

## Notes for reviewers

- Runtime still dispatches **first** call only from arrays / `tool_calls` (same as eval's first-element semantics for BFCL).
- Single-key compact path still validates against `tool_defs` before eval fallback, preserving unknown-tool rejection for `{"unknown_tool": ...}`.
