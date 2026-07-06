# GenieClaw Bug Issue — audit hydrate drops undo_restore

**GitHub:** https://github.com/GeniePod/genie-claw/issues/472

## Title

```
[bug] home_undo loses value-changing undo after restart — audit hydrate drops undo_restore (#402)
```

## Body

### Summary

In-process `home_undo` for value-changing actions depends on `RecordedAction.undo_restore` captured before actuation (#434). That snapshot is **not persisted** in actuation audit JSONL: `AuditEvent` has no `undo_restore` field, and `audit_event_to_recorded_action` always sets `undo_restore: None`. After a process restart (or any dispatcher rebuilt with `with_actuation_audit_path`), hydrated history can list `set_brightness` / `toggle` actions but `home_undo` cannot find an undoable entry.

`turn_on` / `turn_off` still undo after restart because `inverse_action` is recomputed from the action name alone.

### Steps to reproduce

1. From repo root on `main` (`685a7a7` or later):

   ```bash
   cd genie-claw
   cargo test -p genie-core action_history_hydrates_from_audit_log -- --nocapture
   ```

   Existing test only covers `turn_on` → `undo: turn_off` after hydrate (inverse-action path).

2. **Restart regression repro (add failing test):**

   - Create `ToolDispatcher` with `with_actuation_audit_path(path)`.
   - Execute `home_control` `set_brightness` (stub `RecordingHomeProvider` with prior brightness 80 → set 20).
   - Assert `home_undo` works in same process.
   - Build a **new** `ToolDispatcher` with the same audit path (simulates restart).
   - `action_history` may still list the dim action.
   - `home_undo` returns no undoable action / fails to restore brightness.

3. **Code paths:**

   - Audit write: `exec_home_control_inner` appends `AuditEvent { action, value, action_id, … }` but **no** `undo_restore` (`dispatch.rs` ~1180–1192).
   - Hydrate: `audit_event_to_recorded_action` sets `undo_restore: None` (`actuation.rs` ~557–574).
   - `last_undoable` requires `undo_restore.is_some() || inverse_action.is_some()` (~272).

### Expected behavior

- Actuation audit records enough data to rebuild `undo_restore` after restart, **or** documents that value-changing undo is intentionally session-scoped (and `action_history` should not imply otherwise).
- Preferred: persist `undo_restore` (action + optional value) on executed audit lines; hydrate it into `RecordedAction` so `home_undo` works across restarts within the audit retention window.

### Actual behavior

| Action | Survives restart in history? | `inverse_action` after hydrate | `undo_restore` after hydrate | `home_undo` after restart |
|--------|------------------------------|-------------------------------|-----------------------------|---------------------------|
| `turn_on` | yes | `turn_off` | None | works |
| `set_brightness` | yes | None | **None** | **broken** |
| `toggle` | yes | None | **None** | **broken** |
| `set_temperature` | yes | None | **None** | **broken** |

### Hardware

Non-Jetson (x86_64 dev / cross-build host). Reproducible with `RecordingHomeProvider`; real HA optional for Real Behavior Proof in PR.

### JetPack / L4T version

_No response_

### GenieClaw version / commit

`main` @ `685a7a7`

### Relevant logs

_No response_

### Contribution fit (#402)

Real-HA tool-dispatch persistence fix in `actuation.rs` (`AuditEvent`, hydrate) + `dispatch.rs` (audit append). Natural follow-up to merged #434 / #435; complements misleading `action_history` hints (separate issue). Include unit test: dim → restart dispatcher → `home_undo` restores brightness.
