use genie_core::tools::quick::route;

fn routed_temperature(text: &str) -> (String, f64) {
    let call = route(text).unwrap_or_else(|| panic!("'{text}' should route to a tool"));
    assert_eq!(call.name, "home_control", "{text}");
    assert_eq!(call.arguments["action"], "set_temperature", "{text}");
    let entity = call.arguments["entity"].as_str().unwrap().to_string();
    let value = call.arguments["value"].as_f64().unwrap();
    (entity, value)
}

#[test]
fn digit_temperature_still_routes() {
    let (entity, value) = routed_temperature("Set the oven to 400 degrees");
    assert_eq!(entity, "oven");
    assert_eq!(value, 400.0);
}

#[test]
fn spoken_whole_number_temperature_routes() {
    let (entity, value) = routed_temperature("Set the oven to four hundred degrees");
    assert_eq!(entity, "oven");
    assert_eq!(value, 400.0);
}

#[test]
fn spoken_compound_temperature_routes() {
    let (_, value) = routed_temperature("Set the thermostat to seventy two degrees");
    assert_eq!(value, 72.0);
}

#[test]
fn spoken_temperature_with_connector_routes() {
    let (_, value) = routed_temperature("Set the thermostat to one hundred and five degrees");
    assert_eq!(value, 105.0);
}

#[test]
fn directional_adverb_is_not_part_of_the_entity() {
    // "set the thermostat down/up/back to N" — the adverb describes the setpoint
    // change, not the device. The entity must stay the named device, not
    // "thermostat down". Value is already correct; only the entity was garbled.
    for (utterance, entity, value) in [
        ("Set the thermostat down to 68", "thermostat", 68.0),
        ("Set the thermostat up to 72", "thermostat", 72.0),
        (
            "Set the bedroom thermostat back to 70",
            "bedroom thermostat",
            70.0,
        ),
    ] {
        let (got_entity, got_value) = routed_temperature(utterance);
        assert_eq!(got_entity, entity, "{utterance}");
        assert_eq!(got_value, value, "{utterance}");
    }
}
