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

/// Maximum nested parenthesis depth the parser will accept.
///
/// The parser is recursive-descent: each `(` adds three stack frames
/// (`parse_expr` → `parse_term` → `parse_factor`) before the recursion
/// deepens again. Tokio's default worker thread has a 2 MiB stack, which
/// empirically aborts somewhere around ~1500-3000 nested parens. 64 levels
/// leaves a roughly 30x safety margin for the worst-case frame size while
/// still admitting any realistic arithmetic expression. The check exists so
/// a hostile chat message (or LLM-forwarded prompt-injected expression) can
/// never abort the daemon via stack overflow.
const MAX_PAREN_DEPTH: usize = 64;

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
//
// `depth` counts the number of unbalanced `(` already on the stack above
// this call. Only `parse_factor` deepens the recursion (the `LParen` arm
// below), so the cap is enforced here. `parse_expr` and `parse_term` pass
// the same `depth` through unchanged.
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
            if depth >= MAX_PAREN_DEPTH {
                return Err(format!(
                    "nested parentheses too deep (max {})",
                    MAX_PAREN_DEPTH
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

    /// A realistically-nested expression — way below the cap — still parses
    /// and produces the correct numeric result. Guards against the cap being
    /// set too aggressively or the depth counter incrementing in the wrong
    /// branch.
    #[test]
    fn realistic_complex_expression_well_below_limit_still_works() {
        let result = evaluate("(((1 + 2) * 3) - ((4 + 5) / 6))").unwrap();
        // (((3) * 3) - (9 / 6)) = 9 - 1.5 = 7.5
        assert!((result - 7.5).abs() < 1e-9, "got {}", result);
    }

    /// `MAX_PAREN_DEPTH` levels of parens around a single literal must parse.
    /// This locks the cap in: bumping it down would break this; the bug fix
    /// keeps every realistic expression alive.
    #[test]
    fn parens_at_the_documented_max_depth_succeed() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_PAREN_DEPTH),
            ")".repeat(MAX_PAREN_DEPTH)
        );
        let result = evaluate(&expr).unwrap();
        assert_eq!(result, 1.0);
    }

    /// One level past the cap must return a safe `Err`, NOT a stack-overflow
    /// abort. The error message names the cap so an operator can diagnose
    /// from logs without reading source.
    #[test]
    fn parens_one_past_max_depth_return_a_safe_error() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_PAREN_DEPTH + 1),
            ")".repeat(MAX_PAREN_DEPTH + 1)
        );
        let result = evaluate(&expr);
        assert!(result.is_err(), "expected error, got {:?}", result);
        let msg = result.unwrap_err();
        assert!(
            msg.contains("nested parentheses too deep"),
            "unexpected error message: {}",
            msg
        );
        assert!(
            msg.contains(&MAX_PAREN_DEPTH.to_string()),
            "error should name the cap, got: {}",
            msg
        );
    }

    /// Non-paren recursion (a long `+` chain) must NOT be rejected by the
    /// depth cap — `parse_expr` and `parse_term` iterate with `while`, they
    /// don't deepen the call stack. Guards against an over-eager depth
    /// check that misclassifies long flat chains.
    #[test]
    fn non_paren_recursion_is_unaffected() {
        // 5000 terms — well past anything the paren branch would tolerate,
        // but legitimately parseable because the operator loop is iterative.
        let mut expr = String::from("1");
        for _ in 0..5000 {
            expr.push_str(" + 1");
        }
        let result = evaluate(&expr).unwrap();
        assert_eq!(result, 5001.0);
    }

    /// The hostile payload the bug report cites — `(` ×5000 + `1` + `)` ×5000
    /// — used to abort the genie-core process via stack overflow (SIGABRT,
    /// not catchable as a panic). After the fix it must return `Err`. The
    /// test runs inside a 2 MiB stacked thread to match Tokio's worker
    /// default; on `main` (pre-fix) this test would abort the whole test
    /// process and take CI with it.
    #[test]
    fn attacker_payload_does_not_overflow_the_stack() {
        let result = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024) // matches tokio's default worker stack
            .spawn(|| {
                let expr = format!("{}1{}", "(".repeat(5000), ")".repeat(5000));
                evaluate(&expr)
            })
            .expect("spawning a fixed-stack-size thread must succeed")
            .join()
            .expect("the parser must NOT abort the process on a hostile depth");
        assert!(
            result.is_err(),
            "expected an Err, got {:?} (pre-fix this whole test process aborted)",
            result
        );
        assert!(
            result.unwrap_err().contains("nested parentheses too deep"),
            "expected the depth-cap error message"
        );
    }
}
