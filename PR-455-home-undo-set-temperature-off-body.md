Fixes #455

## Summary

Mirror the `set_brightness` off-state handling in `undo_restore_from_prior` for `set_temperature`. When the HVAC was `off` before a setpoint change, `home_undo` now records `turn_off` as the restore action instead of leaving `undo_restore` empty (which skipped the thermostat change and reversed an unrelated earlier action).

## Changes

- `undo_restore_from_prior("set_temperature")`: prior `state == "off"` → `UndoRestore { action: "turn_off" }`.
- Unit test: off vs heating prior states.
- Dispatch test: off thermostat → `set_temperature(72)` → `home_undo` → `turn_off` (climate back off).
- Extend `RecordingHomeProvider` stub with climate domain for the integration test.

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
git checkout fix/issue-455-home-undo-set-temperature-off

cargo fmt
cargo test -p genie-core undo_restore_from_prior_set_temperature
cargo test -p genie-core home_undo_restores_off_state_after_set_temperature
cargo test -p genie-core home_undo
cargo clippy -p genie-core -- -D warnings
```

Environment: x86_64 Linux dev host (`laptop` profile). Stub provider test + unit tests; optional live HA repro below.

Optional live HA repro:

```bash
docker compose -f deploy/homeassistant/docker-compose.yml up -d
# Ensure climate entity is off with no active setpoint exposed
genie-ctl chat "set the thermostat to 72"
genie-ctl chat "undo the last home action"
# Expect: HVAC returns to off, not an unrelated earlier action
```

### What I observed

**Before (`main`):**

- `undo_restore_from_prior("set_temperature", prior_off)` → `None`.
- `home_undo` after heating an off thermostat could skip the temperature change.

**After (this PR):**

```text
$ cargo test -p genie-core undo_restore_from_prior_set_temperature
test tools::actuation::tests::undo_restore_from_prior_set_temperature_when_off ... ok

$ cargo test -p genie-core home_undo_restores_off_state_after_set_temperature
test tools::dispatch::tests::home_undo_restores_off_state_after_set_temperature ... ok

$ cargo clippy -p genie-core -- -D warnings
(clean)
```

- Off prior → undo issues `turn_off`; heating prior with `temperature: 68` → undo restores `set_temperature(68)`.

**Repro caveat:** Some HA climate entities expose `temperature` while `state` is `off`; this fix targets the `state == "off"` path aligned with #435 brightness handling.

## Test plan

- [x] `cargo test -p genie-core undo_restore_from_prior_set_temperature`
- [x] `cargo test -p genie-core home_undo_restores_off_state_after_set_temperature`
- [x] `cargo test -p genie-core home_undo`
- [x] `cargo clippy -p genie-core -- -D warnings`
- [ ] Live HA: off climate → set temp → undo → off (optional)

## Notes for reviewers

- Follow-up to [#435](https://github.com/GeniePod/genie-claw/pull/435) / closed [#434](https://github.com/GeniePod/genie-claw/issues/434).
- Parent epic: [#402](https://github.com/GeniePod/genie-claw/issues/402).
