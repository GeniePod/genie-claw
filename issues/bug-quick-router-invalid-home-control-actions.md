# GenieClaw Bug Issue — quick router emits home_control actions dispatch rejects

**GitHub:** https://github.com/GeniePod/genie-claw/issues/456

## Title

```
[bug] tools/quick: household routes emit invalid home_control actions (activate_until_5pm, apply_scene) rejected by dispatch (#402)
```

## Body

### Summary

`tools/quick.rs` routes multiple household scenario utterances to `home_control` with action verbs that are **not** accepted by `tools/dispatch.rs`. `HOME_CONTROL_ACTIONS` only allows `turn_on`, `turn_off`, `toggle`, `set_brightness`, `set_temperature`, `open`, `close`, `lock`, `unlock`, and `activate`. The quick router emits at least `activate_until_5pm` and `apply_scene`, which fail at execution with `home_control action '…' is invalid`.

### Steps to reproduce

1. From repo root on `main`:

   ```bash
   cd genie-claw
   cargo test -p genie-core quick::tests::routes_structured_household_questions_to_memory_recall -- --nocapture
   # (any quick router test file — no HA needed)
   ```

2. **Unit repro — routing succeeds, dispatch would fail:**

   ```rust
   // crates/genie-core/src/tools/quick.rs
   let call = route("Put the house in low power mode until five").unwrap();
   assert_eq!(call.name, "home_control");
   assert_eq!(call.arguments["action"], "activate_until_5pm"); // not in HOME_CONTROL_ACTIONS

   let call = route("Mia: Use the sleepover lights scene").unwrap();
   assert_eq!(call.arguments["action"], "apply_scene"); // not in HOME_CONTROL_ACTIONS
   ```

3. **Dispatch rejection (add failing integration test):**

   ```bash
   # ToolDispatcher::execute_with_context on the quick-routed call fails:
   # home_control action 'activate_until_5pm' is invalid; expected one of: turn_on, ...
   ```

4. Grep for all invalid emitters:

   ```bash
   rg 'activate_until_5pm|apply_scene' crates/genie-core/src/tools/quick.rs
   ```

### Expected behavior

- Quick-router `home_control` emissions use only actions accepted by `parse_home_control_args` / `canon_home_control_action`.
- Timed or scene-specific intents either map to supported verbs (e.g. `activate`) or route to a dedicated tool / explicit failure before actuation.
- BFCL household scenarios that currently route through quick path should execute (or fail with a domain-specific message), not die on an unknown action string.

### Actual behavior

| Utterance (examples) | Quick `action` | Dispatch result |
|----------------------|----------------|-----------------|
| "Put the house in low power mode until five" | `activate_until_5pm` | **invalid action** |
| "Put me in focus mode until five" | `activate_until_5pm` | **invalid action** |
| "Mia: Use the sleepover lights scene" | `apply_scene` | **invalid action** |

`quick.rs` unit tests **assert** these invalid actions as correct routing (`routes_structured_household_questions_to_memory_recall` and related), so CI stays green while runtime actuation is broken for those paths.

### Hardware

Non-Jetson (x86_64 dev / cross-build host)

### JetPack / L4T version

_No response_

### GenieClaw version / commit

`main` @ `b93fa59`

### Relevant logs

```
home_control action 'activate_until_5pm' is invalid; expected one of: turn_on, turn_off, toggle, set_brightness, set_temperature, open, close, lock, unlock, activate
```

### Additional context

**Root cause (code)**

| Location | Behavior |
|----------|----------|
| `crates/genie-core/src/tools/quick.rs` `home_control_request()` | Returns `activate_until_5pm`, `apply_scene` for household BFCL scenarios |
| `crates/genie-core/src/tools/dispatch.rs` `HOME_CONTROL_ACTIONS` | No such verbs; `canon_home_control_action` returns `None` |
| `crates/genie-core/src/tools/quick.rs` tests | Assert invalid actions as expected routing |

**Suggested fix direction**

1. Map timed-mode intents to `activate` (or add first-class `activate_until` support end-to-end in provider + policy).
2. Map `apply_scene` → `activate` (scenes already use `activate` elsewhere in quick router).
3. Add dispatch integration tests: quick-routed `home_control` calls must not fail action validation.
4. Audit `home_control_request()` for any other action strings outside `HOME_CONTROL_ACTIONS`.

Parent epic: [#402](https://github.com/GeniePod/genie-claw/issues/402). Related BFCL routing: [#379](https://github.com/GeniePod/genie-claw/issues/379). Not a duplicate of closed [#400](https://github.com/GeniePod/genie-claw/pull/400) (synonym canonicalization for valid actions).
