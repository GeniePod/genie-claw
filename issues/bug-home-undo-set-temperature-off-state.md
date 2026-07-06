# GenieClaw Bug Issue — home_undo cannot restore set_temperature when HVAC was off

**Type:** Bug (tool-dispatch / real-Home-Assistant correctness — [#402](https://github.com/GeniePod/genie-claw/issues/402) bucket 2)  
**Submit at:** https://github.com/GeniePod/genie-claw/issues/new?template=bug_report.yml  
**Upstream:** `main` @ `b93fa59`

> **Why file this:** [#434](https://github.com/GeniePod/genie-claw/issues/434) / [#435](https://github.com/GeniePod/genie-claw/pull/435) added `undo_restore_from_prior` for value-changing actions. `set_brightness` handles `entity.state == "off"` by restoring `turn_off`, but `set_temperature` only reads `temperature` / `target_temp` attributes and returns `None` when the climate entity was off — so undo **skips** the thermostat change (same failure class as pre-#434 brightness skip).

## Title

```
[bug] home_undo set_temperature leaves no undo_restore when HVAC was off — skips to older action (#434)
```

## Body

### Summary

After [#435](https://github.com/GeniePod/genie-claw/pull/435), `undo_restore_from_prior("set_brightness", prior)` returns `turn_off` when the light was off before dimming. The `set_temperature` arm requires `attributes.temperature` or `target_temp` and has **no off-state branch**. When the thermostat was off, `undo_restore` is `None`, `last_undoable()` skips the temperature change, and `home_undo` can reverse an unrelated earlier binary action.

### Steps to reproduce

1. Optional HA stack:

   ```bash
   cd genie-claw
   docker compose -f deploy/homeassistant/docker-compose.yml up -d
   ```

2. **Unit repro:**

   ```rust
   // prior climate entity: state "off", no temperature attribute
   let prior = HomeState { entities: vec![Entity {
       state: "off".into(),
       attributes: json!({}),
       ..
   }], .. };

   undo_restore_from_prior("set_temperature", &prior);
   // Bug: None — no undo_restore recorded

   // Contrast set_brightness same prior:
   undo_restore_from_prior("set_brightness", &prior);
   // → UndoRestore { action: "turn_off", value: None }  ✓
   ```

3. **Dispatch sequence repro (add failing test):**

   ```text
   turn_on heat (or prior action)
   set_temperature 72   # HVAC was off; may turn on + setpoint
   home_undo
   Bug: skips set_temperature, undoes turn_on instead (or wrong action)
   ```

4. **Live HA (if climate entity available):**

   ```bash
   curl -s -H "Authorization: Bearer $HA_TOKEN" \
     http://127.0.0.1:8123/api/states/climate.<entity> | jq '{state, temperature: .attributes.temperature}'

   genie-ctl chat "set the thermostat to 72"
   genie-ctl chat "undo the last home action"
   # Expect: restore prior off state or prior setpoint — not unrelated turn_off
   ```

### Expected behavior

- `set_temperature` undo captures pre-action HVAC state: if off → restore `turn_off`; if on with setpoint → restore prior `set_temperature` value (mirror brightness semantics).
- `home_undo` targets the latest thermostat change, not an older unrelated action.
- Explicit failure if restore metadata cannot be captured — no silent skip.

### Actual behavior

- `undo_restore_from_prior` `set_temperature` arm uses `?` on temperature attributes only; off entities with no temp → `None`.
- No `entity.state == "off"` branch (unlike `set_brightness`).
- Ledger entry has `undo_restore: None` → invisible or skipped by `last_undoable()`.

### Hardware

Non-Jetson (x86_64 dev / cross-build host)

### JetPack / L4T version

_No response_

### GenieClaw version / commit

`main` @ `b93fa59`

### Relevant logs

### Additional context

**Root cause (code)**

| Location | Behavior |
|----------|----------|
| `actuation.rs` `undo_restore_from_prior` `set_temperature` | No off-state handling; requires temp attribute |
| `actuation.rs` `set_brightness` arm | Has `entity.state == "off"` → `turn_off` restore |

**Suggested fix direction**

1. Add off-state branch for `set_temperature` (likely `turn_off` restore, or `set_temperature` with last known setpoint if HA exposes it while off).
2. Unit tests for off vs on prior states.
3. Dispatch test: off HVAC → `set_temperature` → `home_undo` restores off (not skip).

Follow-up to closed [#434](https://github.com/GeniePod/genie-claw/issues/434). Parent: [#402](https://github.com/GeniePod/genie-claw/issues/402). Separate from audit-hydrate restart gap (different root cause).
