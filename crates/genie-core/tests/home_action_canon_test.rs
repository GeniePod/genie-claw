use genie_core::tools::home_action::{HOME_CONTROL_ACTIONS, canonicalize_household_action};
use genie_core::tools::quick::route;

const ROUTER_EMITTED_ACTIONS: &[&str] = &[
    "activate",
    "activate_until_5pm",
    "allow_10_to_10_20",
    "allow_mom_only",
    "apply_scene",
    "arm",
    "block_until_math_done",
    "check_and_alert",
    "check_and_vent",
    "clean",
    "cool_down",
    "create",
    "create_threshold_10",
    "create_until_21",
    "cut_power_and_vent",
    "enable",
    "hold",
    "lock_except",
    "mute_for_practice",
    "open",
    "pause",
    "pause_until_dinner",
    "play_low_volume",
    "privacy_20",
    "remote_start",
    "run",
    "schedule",
    "schedule_after_21",
    "schedule_gradual_blinds",
    "schedule_on_alarm",
    "schedule_on_arrival",
    "schedule_pulse",
    "send_destination",
    "set_color_blue",
    "set_for_tomorrow",
    "set_level",
    "set_preset",
    "set_volume",
    "show",
    "show_agenda",
    "show_guest_card",
    "shut_water_zone",
    "start",
    "start_video_call",
    "test",
    "turn_off",
    "turn_off_except",
    "unlock",
    "verify_and_alert",
    "warm_for_minutes",
];

#[test]
fn no_router_action_canonicalizes_to_an_invalid_verb() {
    for &action in ROUTER_EMITTED_ACTIONS {
        if let Some((canon, _)) = canonicalize_household_action(action, None) {
            assert!(
                HOME_CONTROL_ACTIONS.contains(&canon),
                "action '{action}' canonicalized to '{canon}', not a valid home_control action"
            );
        }
    }
}

#[test]
fn valid_actions_and_synonyms_pass_through() {
    assert_eq!(
        canonicalize_household_action("turn_off", None),
        Some(("turn_off", None))
    );
    assert_eq!(
        canonicalize_household_action("open", None),
        Some(("open", None))
    );
    assert_eq!(
        canonicalize_household_action("activate", None),
        Some(("activate", None))
    );
    assert_eq!(
        canonicalize_household_action("enable", None),
        Some(("turn_on", None))
    );
}

#[test]
fn level_maps_to_set_brightness_and_keeps_value() {
    assert_eq!(
        canonicalize_household_action("set_level", Some(90.0)),
        Some(("set_brightness", Some(90.0)))
    );
}

#[test]
fn exclusion_verbs_abstain_rather_than_drop_the_exception() {
    assert_eq!(canonicalize_household_action("turn_off_except", None), None);
    assert_eq!(canonicalize_household_action("lock_except", None), None);
}

#[test]
fn unknown_and_bespoke_verbs_abstain() {
    for action in [
        "apply_scene",
        "activate_until_5pm",
        "set_volume",
        "cool_down",
        "schedule",
        "run",
        "create",
        "show_agenda",
    ] {
        assert_eq!(
            canonicalize_household_action(action, Some(25.0)),
            None,
            "'{action}' should abstain, not guess an actuation"
        );
    }
}

#[test]
fn quick_router_never_emits_an_invalid_home_control_action() {
    let utterances = [
        "Put the house in low power mode until five",
        "Mia: Set my room to sleepover lights.",
        "Give me focus mode until five",
        "Stop the sprinklers, it's raining",
        "Jared: Lock everything except the back gate",
        "Jared: Turn off everything downstairs except the kitchen lights",
        "Leo: Make the stairs bright.",
        "Set the oven to 400 degrees",
        "Leo: It's too loud",
        "Warm up the car",
    ];
    for utterance in utterances {
        if let Some(call) = route(utterance)
            && call.name == "home_control"
        {
            let action = call.arguments["action"].as_str().unwrap();
            assert!(
                HOME_CONTROL_ACTIONS.contains(&action),
                "'{utterance}' routed home_control with invalid action '{action}'"
            );
        }
    }
}
