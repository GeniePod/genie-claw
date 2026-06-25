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
