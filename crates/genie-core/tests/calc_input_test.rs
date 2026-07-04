use genie_core::tools::quick::route;

fn expression(text: &str) -> String {
    let call = route(text).unwrap_or_else(|| panic!("'{text}' should route to a tool"));
    assert_eq!(call.name, "calculate", "'{text}' should route to calculate");
    call.arguments["expression"]
        .as_str()
        .unwrap_or_else(|| panic!("'{text}' expression should be a string"))
        .to_string()
}

#[test]
fn decimal_arithmetic_is_preserved() {
    assert_eq!(expression("what is 3.5 plus 2.5"), "3.5 + 2.5");
    assert_eq!(expression("what is 10.5 divided by 2"), "10.5 / 2");
    assert_eq!(expression("calculate 0.1 plus 0.2"), "0.1 + 0.2");
}

#[test]
fn spoken_point_becomes_decimal() {
    assert_eq!(expression("what is 3 point 5 plus 2 point 5"), "3.5 + 2.5");
}

#[test]
fn spoken_word_decimals_fold_before_calculate() {
    assert_eq!(
        expression("what is three point five plus two point five"),
        "3.5 + 2.5"
    );
    assert_eq!(
        expression("what is twelve point five percent of 80"),
        "80 * 12.5 / 100"
    );
    assert_eq!(
        expression("convert ninety eight point six f to celsius"),
        "(98.6 - 32) * 5 / 9"
    );
}

#[test]
fn mixed_digit_and_spoken_fraction_decimals() {
    assert_eq!(expression("what is 3 point five plus 2.5"), "3.5 + 2.5");
}

#[test]
fn decimal_percentage_and_temperature() {
    assert_eq!(expression("what is 12.5 percent of 80"), "80 * 12.5 / 100");
    assert_eq!(
        expression("convert 98.6f to celsius"),
        "(98.6 - 32) * 5 / 9"
    );
}

#[test]
fn integer_math_is_unchanged() {
    assert_eq!(expression("what is 2 plus 2"), "2 + 2");
    assert_eq!(expression("what is 20 percent of 80"), "80 * 20 / 100");
}

#[test]
fn spoken_cardinals_in_percentage_and_temperature() {
    assert_eq!(expression("what is twenty percent of 80"), "80 * 20 / 100");
    assert_eq!(expression("what is 20 percent of eighty"), "80 * 20 / 100");
    assert_eq!(
        expression("what is twenty five percent of eighty"),
        "80 * 25 / 100"
    );
    assert_eq!(
        expression("convert ninety eight f to celsius"),
        "(98 - 32) * 5 / 9"
    );
    assert_eq!(
        expression("convert 350 degrees to celsius"),
        "(350 - 32) * 5 / 9"
    );
}

#[test]
fn non_math_does_not_route_to_calculate() {
    assert!(
        route("what time is it")
            .map(|c| c.name != "calculate")
            .unwrap_or(true)
    );
}

#[test]
fn percent_symbol_routes_like_the_spelled_out_word() {
    // The "%" symbol is the common way to type a percentage. It must reach the
    // same calculator expression as the spelled-out "percent" word, which
    // already works (`spoken_cardinals_in_percentage_and_temperature`).
    assert_eq!(expression("what is 15% of 200"), "200 * 15 / 100");
    assert_eq!(expression("what is 20% of 80"), "80 * 20 / 100");
    // A spaced "%" and a decimal base both fold to the same expression.
    assert_eq!(expression("what is 20 % of 80"), "80 * 20 / 100");
    assert_eq!(expression("what is 12.5% of 80"), "80 * 12.5 / 100");
    // Apostrophe prefix ("what's") must not block the percent-symbol path.
    assert_eq!(expression("what's 50% of 80"), "80 * 50 / 100");
}
