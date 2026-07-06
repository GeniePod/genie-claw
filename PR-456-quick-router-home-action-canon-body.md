Fixes #456

## Summary

Extend `canon_home_control_action` so all 46 BFCL household-scenario verbs emitted by `tools/quick.rs` map to supported `home_control` actions before actuation. The quick router could route correctly for BFCL intent scoring while dispatch rejected verbs like `activate_until_5pm`, `apply_scene`, and `schedule_on_alarm` at execution time.

## Changes

- Add `map_home_control_action_synonym` with best-effort mappings for BFCL quick-router scenario verbs → `turn_on` / `turn_off` / `activate` / `set_brightness` / `set_temperature` / `lock` / `unlock` / `close`.
- Prefix rule: `activate_until_*` → `activate`.
- Regression tests: all 46 quick-router scenario verbs canonicalize; representative quick-route utterances pass `parse_home_control_args`.

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
git checkout fix/issue-456-quick-router-home-action-canon

cargo fmt
cargo test -p genie-core home_control_canonicalizes
cargo test -p genie-core all_quick_router_home_verbs
cargo test -p genie-core quick_router_home_control_routes
cargo clippy -p genie-core -- -D warnings
```

Environment: x86_64 Linux dev host (`laptop` profile). Pure dispatch canonicalization — no Jetson or live HA required.

### What I observed

**Before (`main`):**

- `quick::route("Jared: Put the house in low-power mode until five.")` → `action: "activate_until_5pm"`.
- `parse_home_control_args` → `home_control action 'activate_until_5pm' is invalid`.
- Same for `apply_scene`, `schedule_on_alarm`, `set_level`, `arm`, etc. (46 scenario verbs).

**After (this PR):**

```text
$ cargo test -p genie-core home_control_canonicalizes
test tools::dispatch::tests::home_control_canonicalizes_action_synonyms ... ok
test tools::dispatch::tests::home_control_canonicalizes_bfcl_quick_router_verbs ... ok

$ cargo test -p genie-core all_quick_router_home_verbs
test tools::dispatch::tests::all_quick_router_home_verbs_canonicalize_for_dispatch ... ok

$ cargo test -p genie-core quick_router_home_control_routes
test tools::dispatch::tests::quick_router_home_control_routes_pass_dispatch_validation ... ok

$ cargo clippy -p genie-core -- -D warnings
(clean)
```

- `activate_until_5pm` → `activate`; `apply_scene` → `activate`; `set_level` → `set_brightness`; `arm` → `lock`.
- Quick-router tests unchanged (still assert scenario verbs at route layer); dispatch boundary now accepts them.

**Jetson validation gap:** canonicalization is platform-independent; reviewer can re-run tests above.

## Test plan

- [x] `cargo test -p genie-core home_control_canonicalizes`
- [x] `cargo test -p genie-core all_quick_router_home_verbs`
- [x] `cargo test -p genie-core quick_router_home_control_routes`
- [x] `cargo clippy -p genie-core -- -D warnings`

## Notes for reviewers

- Same pattern as [#400](https://github.com/GeniePod/genie-claw/pull/400) (action synonym canonicalization at dispatch boundary).
- Mappings are **best-effort actuation** for BFCL household scenarios — timed/scene nuance (e.g. `activate_until_5pm`) collapses to `activate` until first-class timed scenes land.
- Quick-router unit tests intentionally keep scenario verb strings for BFCL routing coverage; this PR fixes the execution gap without rewriting 200+ quick test assertions.
