use std::str::FromStr;

enum CardinalWord {
    Value(u64),
    Hundred,
    Thousand,
}

pub(crate) fn parse_spoken_number(tokens: &[&str], start: usize) -> Option<(u64, usize)> {
    if let Some(Ok(value)) = tokens.get(start).map(|token| token.parse::<u64>()) {
        return Some((value, start + 1));
    }

    let mut total: u64 = 0;
    let mut group: u64 = 0;
    let mut index = start;
    let mut matched = false;

    while let Some(&token) = tokens.get(index) {
        if matched
            && token == "and"
            && tokens
                .get(index + 1)
                .is_some_and(|next| cardinal_word(next).is_some())
        {
            index += 1;
            continue;
        }
        let Some(word) = cardinal_word(token) else {
            break;
        };
        match word {
            CardinalWord::Value(value) => group = group.saturating_add(value),
            CardinalWord::Hundred => group = group.max(1).saturating_mul(100),
            CardinalWord::Thousand => {
                total = total.saturating_add(group.max(1).saturating_mul(1000));
                group = 0;
            }
        }
        matched = true;
        index += 1;
    }

    matched.then(|| (total.saturating_add(group), index))
}

pub(crate) fn parse_amount(text: &str) -> Option<f64> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if let Some(token) = tokens
        .iter()
        .find(|token| token.chars().any(|ch| ch.is_ascii_digit()))
    {
        let cleaned: String = token
            .chars()
            .filter(|ch| ch.is_ascii_digit() || *ch == '.')
            .collect();
        if let Ok(value) = f64::from_str(&cleaned)
            && value.is_finite()
        {
            return Some(value);
        }
    }
    for start in 0..tokens.len() {
        if let Some((value, _)) = parse_spoken_number(&tokens, start) {
            let value = value as f64;
            if value.is_finite() {
                return Some(value);
            }
        }
    }
    None
}

fn cardinal_word(token: &str) -> Option<CardinalWord> {
    use CardinalWord::{Hundred, Thousand, Value};
    Some(match token {
        "zero" => Value(0),
        "one" | "a" | "an" => Value(1),
        "two" => Value(2),
        "three" => Value(3),
        "four" => Value(4),
        "five" => Value(5),
        "six" => Value(6),
        "seven" => Value(7),
        "eight" => Value(8),
        "nine" => Value(9),
        "ten" => Value(10),
        "eleven" => Value(11),
        "twelve" => Value(12),
        "thirteen" => Value(13),
        "fourteen" => Value(14),
        "fifteen" => Value(15),
        "sixteen" => Value(16),
        "seventeen" => Value(17),
        "eighteen" => Value(18),
        "nineteen" => Value(19),
        "twenty" => Value(20),
        "thirty" => Value(30),
        "forty" => Value(40),
        "fifty" => Value(50),
        "sixty" => Value(60),
        "seventy" => Value(70),
        "eighty" => Value(80),
        "ninety" => Value(90),
        "hundred" => Hundred,
        "thousand" => Thousand,
        _ => return None,
    })
}
