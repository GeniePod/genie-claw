Fixes #480

## Summary

`undo_restore_from_prior` no longer derives group undo metadata from `prior.entities.first()` alone. For multi-entity group targets, every member must produce the same restore snapshot; heterogeneous member states skip `undo_restore` capture so `home_undo` cannot issue a wrong group-wide `turn_off` (or other action) based on arbitrary HA state ordering.

## Changes

- Extract per-entity restore logic into `undo_restore_from_entity`.
- `undo_restore_from_prior` requires unanimous restore across all group members; returns `None` when members disagree.
- Add `PartialEq` to `UndoRestore` for snapshot comparison.
- Unit tests: mixed group → `None`; unanimous group → shared `set_brightness` restore.
- Integration test: heterogeneous group dim is recorded as not undoable; `home_undo` fails safely instead of executing a bogus restore.

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

Linux x86_64 dev host, no live Home Assistant (`MixedGroupHomeProvider` stub with `HomeTargetKind::Group`):

```bash
cd genie-claw
cargo test -p genie-core --lib undo_restore_from_prior
cargo test -p genie-core --lib home_undo_skips_heterogeneous
cargo clippy -p genie-core -- -D warnings
```

### What I observed

- `undo_restore_from_prior_mixed_group_brightness_returns_none` — mixed off/on members → no `undo_restore` for `set_brightness` or `toggle`.
- `undo_restore_from_prior_unanimous_group_brightness_succeeds` — both members on @ 204 → `set_brightness ~80%` restore.
- `home_undo_skips_heterogeneous_group_dim_without_wrong_turn_off` — dim on heterogeneous group records `not undoable` in `action_history`; `home_undo` returns "No recent reversible" with only one `SetBrightness` executed (no erroneous `TurnOff`).
- Clippy clean.

**Validation gap:** Not run on Jetson or live HA area group. Stub exercises the same `get_state` → `undo_restore_from_prior` → ledger → `home_undo` path as production group targets.

## Test plan

- `cargo test -p genie-core --lib undo_restore_from_prior`
- `cargo test -p genie-core --lib home_undo_skips_heterogeneous`
- `cargo test -p genie-core --lib home_undo_restores_prior_brightness_after_dim` (regression: single entity unchanged)
- `cargo clippy -p genie-core -- -D warnings`

## Notes for reviewers

- Conservative fix: heterogeneous groups are not undoable via single `home_control` restore (same limitation as before, but no longer **wrong**). Per-member undo would need a follow-up schema change.
- Single-entity behavior unchanged.
