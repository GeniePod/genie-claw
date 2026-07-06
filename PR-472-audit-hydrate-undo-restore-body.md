Fixes #472

## Summary

Persist `undo_restore` on executed actuation audit lines and hydrate it back into `RecordedAction` on startup. `home_undo` for value-changing actions (`set_brightness`, `set_temperature`, `toggle`) now works after a process restart within the audit retention window, not only for binary actions where `inverse_action` can be recomputed.

## Changes

- Add optional `undo_restore` field to `AuditEvent` (serde-defaulted for backward-compatible reads of legacy JSONL).
- Write `recorded.undo_restore` when appending executed audit events in `exec_home_control_inner`.
- `audit_event_to_recorded_action` copies `event.undo_restore` instead of hardcoding `None`.
- Unit tests: hydrate round-trip, legacy line without field, integration test simulating dispatcher restart → `home_undo` restores brightness.

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

Linux x86_64 dev host, no live Home Assistant (`RecordingHomeProvider` stub):

```bash
cd genie-claw
cargo test -p genie-core home_undo_restores_brightness_after_audit -- --nocapture
cargo test -p genie-core audit_logger_hydrates -- --nocapture
cargo clippy -p genie-core -- -D warnings
```

### What I observed

`home_undo_restores_brightness_after_audit_hydrate_restart` passes: first dispatcher turns on kitchen light and dims to 30%; a **new** `ToolDispatcher` loaded from the same audit JSONL successfully `home_undo`s and issues `set_brightness` restore to 100% (brightness 255 on stub). `audit_logger_hydrates_undo_restore_from_executed_event` and `audit_logger_hydrates_legacy_lines_without_undo_restore_field` pass — new field round-trips and old audit lines without `undo_restore` still hydrate with `inverse_action` only. Clippy clean.

**Validation gap:** Not run on Jetson or live HA. Restart path is exercised via `with_actuation_audit_path` + fresh dispatcher, matching production hydrate-on-startup. Real HA would use the same audit ledger fields.

## Test plan

- `cargo test -p genie-core home_undo_restores_brightness_after_audit`
- `cargo test -p genie-core audit_logger_hydrates`
- `cargo test -p genie-core action_history_hydrates_from_audit_log` (regression: `turn_on` still shows `undo: turn_off`)
- `cargo clippy -p genie-core -- -D warnings`

## Notes for reviewers

- `undo_restore` is omitted from JSONL when `None` (`skip_serializing_if`) — no size change for `turn_on`/`turn_off` lines.
- Complements #471 (action_history hints) and merged #434/#435; independent branch from `main`.
- Undo-of-undo (`undo_of` entries) still write `undo_restore: None` at execution time (unchanged).
