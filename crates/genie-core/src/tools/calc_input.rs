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
        let token = tokens[index];
        let prev_is_digits = out.last().is_some_and(|p| is_digits(p));
        let next_is_digits = tokens.get(index + 1).is_some_and(|n| is_digits(n));

        if token == "point" && prev_is_digits && next_is_digits {
            let prev = out.pop().expect("prev_is_digits implies a previous token");
            out.push(format!("{prev}.{}", tokens[index + 1]));
            index += 2;
        } else {
            out.push(token.to_string());
            index += 1;
        }
    }

    out.join(" ")
}

fn is_digits(token: &str) -> bool {
    !token.is_empty() && token.chars().all(|c| c.is_ascii_digit())
}
