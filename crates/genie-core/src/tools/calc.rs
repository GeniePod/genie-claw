/// Simple expression calculator.
///
/// Evaluates basic math: +, -, *, /, parentheses, decimals.
/// No dependencies — hand-written recursive descent parser.
pub fn evaluate(expr: &str) -> Result<f64, String> {
    let tokens = tokenize(expr)?;
    let mut pos = 0;
    let result = parse_expr(&tokens, &mut pos, 0)?;

    if pos < tokens.len() {
        return Err(format!("unexpected token: {:?}", tokens[pos]));
    }

    Ok(result)
}

/// Maximum nesting depth for parentheses. Prevents stack overflow from
/// deeply nested expressions like "(((...)))" that would abort the process.
const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone)]
enum Token {
    Number(f64),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' => {
                chars.next();
            }
            '0'..='9' | '.' => {
                let mut num_str = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() || c == '.' {
                        num_str.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let num: f64 = num_str
                    .parse()
                    .map_err(|_| format!("invalid number: {}", num_str))?;
                tokens.push(Token::Number(num));
            }
            '+' => {
                tokens.push(Token::Plus);
                chars.next();
            }
            '-' => {
                // Handle unary minus.
                if tokens.is_empty()
                    || matches!(
                        tokens.last(),
                        Some(
                            Token::Plus | Token::Minus | Token::Star | Token::Slash | Token::LParen
                        )
                    )
                {
                    chars.next();
                    let mut num_str = String::from("-");
                    while let Some(&c) = chars.peek() {
                        if c.is_ascii_digit() || c == '.' {
                            num_str.push(c);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if num_str == "-" {
                        tokens.push(Token::Minus);
                    } else {
                        let num: f64 = num_str
                            .parse()
                            .map_err(|_| format!("invalid number: {}", num_str))?;
                        tokens.push(Token::Number(num));
                    }
                } else {
                    tokens.push(Token::Minus);
                    chars.next();
                }
            }
            '*' => {
                tokens.push(Token::Star);
                chars.next();
            }
            '/' => {
                tokens.push(Token::Slash);
                chars.next();
            }
            '(' => {
                tokens.push(Token::LParen);
                chars.next();
            }
            ')' => {
                tokens.push(Token::RParen);
                chars.next();
            }
            _ => return Err(format!("unexpected character: '{}'", ch)),
        }
    }

    Ok(tokens)
}

// Recursive descent: expr → term ((+|-) term)*
fn parse_expr(tokens: &[Token], pos: &mut usize, depth: usize) -> Result<f64, String> {
    let mut result = parse_term(tokens, pos, depth)?;

    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::Plus => {
                *pos += 1;
                result += parse_term(tokens, pos, depth)?;
            }
            Token::Minus => {
                *pos += 1;
                result -= parse_term(tokens, pos, depth)?;
            }
            _ => break,
        }
    }

    Ok(result)
}

// term → factor ((*|/) factor)*
fn parse_term(tokens: &[Token], pos: &mut usize, depth: usize) -> Result<f64, String> {
    let mut result = parse_factor(tokens, pos, depth)?;

    while *pos < tokens.len() {
        match tokens[*pos] {
            Token::Star => {
                *pos += 1;
                result *= parse_factor(tokens, pos, depth)?;
            }
            Token::Slash => {
                *pos += 1;
                let divisor = parse_factor(tokens, pos, depth)?;
                if divisor == 0.0 {
                    return Err("division by zero".to_string());
                }
                result /= divisor;
            }
            _ => break,
        }
    }

    Ok(result)
}

// factor → NUMBER | '(' expr ')'
fn parse_factor(tokens: &[Token], pos: &mut usize, depth: usize) -> Result<f64, String> {
    if *pos >= tokens.len() {
        return Err("unexpected end of expression".to_string());
    }

    match &tokens[*pos] {
        Token::Number(n) => {
            let val = *n;
            *pos += 1;
            Ok(val)
        }
        Token::LParen => {
            if depth >= MAX_DEPTH {
                return Err(format!(
                    "expression nesting too deep (max {} levels)",
                    MAX_DEPTH
                ));
            }
            *pos += 1;
            let result = parse_expr(tokens, pos, depth + 1)?;
            if *pos >= tokens.len() || !matches!(tokens[*pos], Token::RParen) {
                return Err("missing closing parenthesis".to_string());
            }
            *pos += 1;
            Ok(result)
        }
        other => Err(format!("unexpected token: {:?}", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_arithmetic() {
        assert_eq!(evaluate("2 + 3").unwrap(), 5.0);
        assert_eq!(evaluate("10 - 4").unwrap(), 6.0);
        assert_eq!(evaluate("3 * 7").unwrap(), 21.0);
        assert_eq!(evaluate("15 / 3").unwrap(), 5.0);
    }

    #[test]
    fn order_of_operations() {
        assert_eq!(evaluate("2 + 3 * 4").unwrap(), 14.0);
        assert_eq!(evaluate("(2 + 3) * 4").unwrap(), 20.0);
    }

    #[test]
    fn decimals() {
        let result = evaluate("2.5 * 4").unwrap();
        assert!((result - 10.0).abs() < 0.001);
    }

    #[test]
    fn negative_numbers() {
        assert_eq!(evaluate("-5 + 3").unwrap(), -2.0);
        assert_eq!(evaluate("10 + -3").unwrap(), 7.0);
    }

    #[test]
    fn nested_parens() {
        assert_eq!(evaluate("((2 + 3) * (4 - 1))").unwrap(), 15.0);
    }

    #[test]
    fn division_by_zero() {
        assert!(evaluate("5 / 0").is_err());
    }

    #[test]
    fn complex_expression() {
        let result = evaluate("(100 - 32) * 5 / 9").unwrap();
        assert!((result - 37.778).abs() < 0.01); // Fahrenheit to Celsius
    }

    #[test]
    fn deeply_nested_parens_returns_error() {
        // This would overflow the stack without the depth limit
        let deeply_nested = "(".repeat(5000) + "1" + &")".repeat(5000);
        let result = evaluate(&deeply_nested);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("nesting too deep"));
    }

    #[test]
    fn max_depth_boundary() {
        // Exactly at MAX_DEPTH should work
        let at_limit = "(".repeat(MAX_DEPTH) + "1" + &")".repeat(MAX_DEPTH);
        assert!(evaluate(&at_limit).is_ok());

        // One over should fail
        let over_limit = "(".repeat(MAX_DEPTH + 1) + "1" + &")".repeat(MAX_DEPTH + 1);
        assert!(evaluate(&over_limit).is_err());
    }
}