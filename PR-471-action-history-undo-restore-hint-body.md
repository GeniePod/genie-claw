Fixes #471

## Summary

`action_history` now mirrors `last_undoable`: value-changing home actions (`set_brightness`, `set_temperature`, `toggle`) show an `undo:` hint derived from `undo_restore` instead of **not undoable** when `inverse_action` is absent. Closes the UX gap left after #434 / #435 where `home_undo` worked in-session but history misled the model and dashboard users.

## Changes

- Add `RecordedAction::action_history_undo_hint()` in `actuation.rs` — prefers `inverse_action`, then formats `undo_restore` (e.g. `undo: set_brightness 100`, `undo: turn_off`).
- `exec_action_history` delegates to that helper instead of only checking `inverse_action`.
- Unit tests for hint formatting plus `action_history_shows_undo_restore_hint_after_dim` integration test (`RecordingHomeProvider` stub).

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

Linux x86_64 dev host (`main` base + this branch), no live Home Assistant:

```bash
cd genie-claw
cargo test -p genie-core action_history -- --nocapture
cargo clippy -p genie-core -- -D warnings
```

### What I observed

All `action_history`-related tests pass. The new integration test exercises the full dispatch path: `home_control` turn_on → `set_brightness` 30 → `action_history`. Output includes `set_brightness kitchen light` with `undo: set_brightness 100` (prior brightness 255 → 100%) and does **not** contain `not undoable` for that entry. Existing `turn_on` hydrate test still shows `undo: turn_off`. Clippy clean.

**Validation gap:** Not exercised on Jetson or against a live HA instance. This change is display-only in `exec_action_history`; undo execution logic is unchanged and already covered by `home_undo_restores_prior_brightness_after_dim`. Real HA would surface the same ledger fields; restart/hydrate undo hints remain #472.

## Test plan

- `cargo test -p genie-core action_history`
- `cargo clippy -p genie-core -- -D warnings`
- Optional on hardware with HA: dim a light, ask "what did you do?" / call `action_history`, confirm undo hint matches what `home_undo` would restore.

## Notes for reviewers

- Scoped to in-session ledger entries that already carry `undo_restore`. Audit-hydrated actions after restart still lack `undo_restore` (#472) — out of scope here.
- Hint format mirrors `undo_home_control_args` args (`action` + optional `value`), not a new schema.
