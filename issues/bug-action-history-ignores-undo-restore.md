# GenieClaw Bug Issue — action_history ignores undo_restore

**GitHub:** https://github.com/GeniePod/genie-claw/issues/471

## Title

```
[bug] action_history labels set_brightness/toggle as not undoable despite undo_restore (#402)
```

## Body

### Summary

`home_undo` can restore value-changing actions (`set_brightness`, `set_temperature`, `toggle`) via the `undo_restore` snapshot captured at execution time (#434). `action_history` only consults `inverse_action` when formatting undo hints, so those same ledger entries are reported as **not undoable** even though `home_undo` would succeed in the same process.

This misleads the model and dashboard users who ask “what can I undo?” before calling `home_undo`.

### Steps to reproduce

1. From repo root on `main` (`685a7a7` or later):

   ```bash
   cd genie-claw
   cargo test -p genie-core home_undo_restores_prior_brightness_after_dim -- --nocapture
   ```

2. **In-process repro (no HA required — use `RecordingHomeProvider` stub):**

   - Execute `home_control` with `action: "set_brightness"`, `value: 20` on an entity that was at 80%.
   - Call `action_history`.
   - Observe the line for that action ends with `; not undoable`.
   - Call `home_undo` in the same session — it **succeeds** and restores prior brightness.

3. **Code path:**

   - `exec_action_history` (`dispatch.rs` ~1286–1290) only maps `inverse_action` to `undo: …`; otherwise prints `not undoable`.
   - `ActionLedger::last_undoable` (~272) correctly treats `undo_restore.is_some()` as undoable.
   - `inverse_action("set_brightness")` and `inverse_action("toggle")` return `None` (`actuation.rs` ~312–321).

### Expected behavior

- `action_history` undo hints align with `home_undo` / `last_undoable`: when `undo_restore` is present, show a human-readable restore hint (e.g. `undo: set_brightness 80` or `undo: turn_off` when prior state was off).
- Same-session UX: “recent actions” and “undo last action” must not contradict each other.

### Actual behavior

| Action recorded | `undo_restore` | `inverse_action` | `action_history` hint | `home_undo` same session |
|-----------------|----------------|------------------|----------------------|--------------------------|
| `set_brightness` | Some | None | **not undoable** | **works** |
| `set_temperature` | Some | None | **not undoable** | **works** |
| `toggle` | Some | None | **not undoable** | **works** |
| `turn_on` | None | Some(`turn_off`) | `undo: turn_off` | works |

### Hardware

Non-Jetson (x86_64 dev / cross-build host)

### JetPack / L4T version

_No response_

### GenieClaw version / commit

`main` @ `685a7a7`

### Relevant logs

_No response_

### Contribution fit (#402)

Real-HA / tool-dispatch UX fix in `crates/genie-core/src/tools/dispatch.rs` (`exec_action_history`), with unit test mirroring `home_undo_restores_prior_brightness_after_dim`. Small, reviewable follow-up to merged #434 / #435.
