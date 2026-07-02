pub(crate) fn prepare(text: &str) -> String {
    let lowered = text.to_lowercase();
    let chars: Vec<char> = lowered.chars().collect();
    let mut cleaned = String::with_capacity(chars.len());

    for (index, &current) in chars.iter().enumerate() {
        if current.is_alphanumeric() || current.is_whitespace() {
            cleaned.push(current);
        } else if current == '.' && flanked_by_digits(&chars, index) {
            cleaned.push('.');
        } else {
            cleaned.push(' ');
        }
    }

    fold_spoken_decimals(&cleaned)
}

fn flanked_by_digits(chars: &[char], index: usize) -> bool {
    let before = index
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .is_some_and(|c| c.is_ascii_digit());
    let after = chars.get(index + 1).is_some_and(|c| c.is_ascii_digit());
    before && after
}

fn fold_spoken_decimals(text: &str) -> String {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut index = 0;

    while index < tokens.len() {
        if let Some((consumed, decimal)) = match_decimal_fold_at(&tokens, index) {
            out.push(decimal);
            index += consumed;
            continue;
        }

        out.push(tokens[index].to_string());
        index += 1;
    }

    out.join(" ")
}

/// Fold `<int> point <frac>` into a single decimal token.
///
/// Handles digit forms (`3 point 5` → `3.5`, landed in #504) and spoken forms
/// (`three point five` → `3.5`, `ninety eight point six` → `98.6`). The
/// fractional part is a single digit word (`zero`–`nine`) or digit token.
fn match_decimal_fold_at(tokens: &[&str], start: usize) -> Option<(usize, String)> {
    let point_rel = tokens[start..].iter().position(|token| *token == "point")?;
    if point_rel == 0 {
        return None;
    }

    let point_idx = start + point_rel;
    let frac_token = tokens.get(point_idx + 1)?;
    let frac_digit = parse_single_digit_word(frac_token).or_else(|| {
        is_digits(frac_token)
            .then(|| frac_token.parse::<u64>().ok())
            .flatten()
    })?;

    let int_val = if point_idx == start + 1 && is_digits(tokens[start]) {
        tokens[start].parse().ok()?
    } else if let Some((value, end)) = super::number_words::parse_spoken_number(tokens, start)
        && end == point_idx
    {
        value
    } else {
        return None;
    };

    let consumed = point_idx + 2 - start;
    Some((consumed, format!("{int_val}.{frac_digit}")))
}

fn parse_single_digit_word(token: &str) -> Option<u64> {
    match token {
        "zero" => Some(0),
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        _ => None,
    }
}

fn is_digits(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit())
}
