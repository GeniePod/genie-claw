use crate::ha::HomeActionKind;
use anyhow::Result;

pub const HOME_CONTROL_ACTIONS: &[&str] = &[
    "turn_on",
    "turn_off",
    "toggle",
    "set_brightness",
    "set_temperature",
    "open",
    "close",
    "lock",
    "unlock",
    "activate",
];

/// Actions that actuate a numeric setpoint and therefore require a `value`.
/// Every other action (turn_on, turn_off, toggle, open, close, lock, unlock,
/// activate) is a no-op for `value` and leaves it `None`.
pub(crate) fn action_requires_value(action: &str) -> bool {
    matches!(action, "set_brightness" | "set_temperature")
}

pub(crate) fn home_action_kind(action: &str) -> Result<HomeActionKind> {
    match action {
        "turn_on" => Ok(HomeActionKind::TurnOn),
        "turn_off" => Ok(HomeActionKind::TurnOff),
        "toggle" => Ok(HomeActionKind::Toggle),
        "set_brightness" => Ok(HomeActionKind::SetBrightness),
        "set_temperature" => Ok(HomeActionKind::SetTemperature),
        "open" => Ok(HomeActionKind::Open),
        "close" => Ok(HomeActionKind::Close),
        "lock" => Ok(HomeActionKind::Lock),
        "unlock" => Ok(HomeActionKind::Unlock),
        "activate" | "activate_scene" => Ok(HomeActionKind::Activate),
        other => anyhow::bail!("unknown home action: {other}"),
    }
}

/// Canonicalize a model-emitted action verb to one of [`HOME_CONTROL_ACTIONS`].
///
/// Small models routinely emit the natural-language form ("turn off"),
/// hyphenated/cased variants ("Turn-Off"), or a synonym ("deactivate") rather
/// than the exact enum value `turn_off`. Rejecting those means a correct intent
/// silently fails to actuate. Normalize separators + case, map a few
/// unambiguous synonyms, and accept the result only if it lands on a real
/// action. `activate` is left as-is (it is its own action for scenes/scripts).
pub(crate) fn canon_home_control_action(raw: &str) -> Option<&'static str> {
    let normalized = raw.trim().to_lowercase().replace([' ', '-'], "_");
    let mapped: &str = match normalized.as_str() {
        "deactivate" | "disable" | "switch_off" | "power_off" | "shut_off" => "turn_off",
        "enable" | "switch_on" | "power_on" => "turn_on",
        other => other,
    };
    HOME_CONTROL_ACTIONS.iter().copied().find(|&a| a == mapped)
}

/// Canonicalize a quick-router household action to a valid `home_control` verb,
/// or `None` to abstain. Only unambiguous rewrites are emitted: an action that
/// already canonicalizes through [`canon_home_control_action`] (a real verb or a
/// safe synonym), and `set_level` -> `set_brightness` (the numeric level is a
/// brightness). Every other household verb returns `None` so the deterministic
/// path defers to the LLM rather than guessing a concrete actuation. In
/// particular the `*_except` exclusion verbs are *not* collapsed to their base
/// verb, which would actuate the entity the user asked to exclude.
pub fn canonicalize_household_action(
    action: &str,
    value: Option<f64>,
) -> Option<(&'static str, Option<f64>)> {
    if let Some(valid) = canon_home_control_action(action) {
        return Some((valid, value));
    }
    match action {
        "set_level" => Some(("set_brightness", value)),
        _ => None,
    }
}
