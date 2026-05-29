//! BFCL-style scoring for local tool-call accuracy.
//!
//! The scorer is intentionally deterministic and side-effect free: it parses
//! model responses, compares ordered tool names and JSON arguments, and reports
//! exact-match rates. It does not execute tools or require a live home backend.

use crate::tools::{ToolCall, parse_tool_calls_for_eval};
use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclCase {
    pub id: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub source: Option<BfclCaseSource>,
    pub prompt: String,
    #[serde(default, alias = "expected_calls")]
    pub expected_tool_calls: Vec<ExpectedToolCall>,
    #[serde(default)]
    pub allow_extra_arguments: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclCaseSource {
    pub dataset: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub citation: Option<String>,
    #[serde(default)]
    pub derived_from: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExpectedToolCall {
    #[serde(alias = "tool")]
    pub name: String,
    #[serde(default = "empty_json_object")]
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclPrediction {
    pub id: String,
    #[serde(alias = "model_response", alias = "output", alias = "prediction")]
    pub response: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BfclCaseScore {
    pub id: String,
    pub category: Option<String>,
    pub source: Option<BfclCaseSource>,
    pub missing_prediction: bool,
    pub parse_success: bool,
    pub tool_name_match: bool,
    pub argument_match: bool,
    pub strict_match: bool,
    pub expected_tool_calls: Vec<ExpectedToolCall>,
    pub actual_tool_calls: Vec<ToolCall>,
    pub missing_tool_calls: usize,
    pub extra_tool_calls: usize,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BfclReport {
    pub total_cases: usize,
    pub parsed_cases: usize,
    pub tool_name_matches: usize,
    pub argument_matches: usize,
    pub strict_matches: usize,
    pub missing_predictions: usize,
    pub failure_count: usize,
    pub parse_accuracy: f64,
    pub tool_name_accuracy: f64,
    pub argument_accuracy: f64,
    pub strict_accuracy: f64,
    pub case_scores: Vec<BfclCaseScore>,
}

pub fn load_cases_jsonl(path: impl AsRef<Path>) -> Result<Vec<BfclCase>> {
    load_jsonl(path)
}

pub fn load_predictions_jsonl(path: impl AsRef<Path>) -> Result<Vec<BfclPrediction>> {
    load_jsonl(path)
}

pub fn score_cases(cases: &[BfclCase], predictions: &[BfclPrediction]) -> BfclReport {
    let prediction_by_id = predictions
        .iter()
        .map(|prediction| (prediction.id.as_str(), prediction))
        .collect::<HashMap<_, _>>();

    let case_scores = cases
        .iter()
        .map(|case| {
            prediction_by_id
                .get(case.id.as_str())
                .map(|prediction| score_response(case, &prediction.response))
                .unwrap_or_else(|| score_missing_prediction(case))
        })
        .collect::<Vec<_>>();

    let total_cases = case_scores.len();
    let parsed_cases = case_scores
        .iter()
        .filter(|score| score.parse_success)
        .count();
    let tool_name_matches = case_scores
        .iter()
        .filter(|score| score.tool_name_match)
        .count();
    let argument_matches = case_scores
        .iter()
        .filter(|score| score.argument_match)
        .count();
    let strict_matches = case_scores
        .iter()
        .filter(|score| score.strict_match)
        .count();
    let missing_predictions = case_scores
        .iter()
        .filter(|score| score.missing_prediction)
        .count();
    let failure_count = total_cases.saturating_sub(strict_matches);

    BfclReport {
        total_cases,
        parsed_cases,
        tool_name_matches,
        argument_matches,
        strict_matches,
        missing_predictions,
        failure_count,
        parse_accuracy: ratio(parsed_cases, total_cases),
        tool_name_accuracy: ratio(tool_name_matches, total_cases),
        argument_accuracy: ratio(argument_matches, total_cases),
        strict_accuracy: ratio(strict_matches, total_cases),
        case_scores,
    }
}

pub fn score_response(case: &BfclCase, response: &str) -> BfclCaseScore {
    let actual_tool_calls = parse_tool_calls_for_eval(response);
    score_parsed_calls(case, actual_tool_calls, false)
}

fn score_missing_prediction(case: &BfclCase) -> BfclCaseScore {
    let mut score = score_parsed_calls(case, Vec::new(), true);
    score.diagnostics.push("missing prediction".to_string());
    score
}

fn score_parsed_calls(
    case: &BfclCase,
    actual_tool_calls: Vec<ToolCall>,
    missing_prediction: bool,
) -> BfclCaseScore {
    let expected_tool_calls = case.expected_tool_calls.clone();
    let expected_len = expected_tool_calls.len();
    let actual_len = actual_tool_calls.len();
    let missing_tool_calls = expected_len.saturating_sub(actual_len);
    let extra_tool_calls = actual_len.saturating_sub(expected_len);
    let mut diagnostics = Vec::new();

    if expected_len == 0 {
        let pass = !missing_prediction && actual_tool_calls.is_empty();
        if !actual_tool_calls.is_empty() {
            diagnostics.push(format!(
                "expected no tool calls, parsed {}",
                actual_tool_calls.len()
            ));
        }

        return BfclCaseScore {
            id: case.id.clone(),
            category: case.category.clone(),
            source: case.source.clone(),
            missing_prediction,
            parse_success: pass,
            tool_name_match: pass,
            argument_match: pass,
            strict_match: pass,
            expected_tool_calls,
            actual_tool_calls,
            missing_tool_calls,
            extra_tool_calls,
            diagnostics,
        };
    }

    let parse_success = !missing_prediction && !actual_tool_calls.is_empty();
    if !parse_success {
        diagnostics.push("no parsable tool call found".to_string());
    }
    if expected_len != actual_len {
        diagnostics.push(format!(
            "tool call count mismatch: expected {expected_len}, got {actual_len}"
        ));
    }

    let mut tool_name_match = expected_len == actual_len && parse_success;
    let mut argument_match = expected_len == actual_len && parse_success;

    for (index, (expected, actual)) in expected_tool_calls
        .iter()
        .zip(actual_tool_calls.iter())
        .enumerate()
    {
        if expected.name != actual.name {
            tool_name_match = false;
            argument_match = false;
            diagnostics.push(format!(
                "tool[{index}] name mismatch: expected '{}', got '{}'",
                expected.name, actual.name
            ));
            continue;
        }

        let expected_arguments = normalize_score_arguments(&expected.arguments);
        let actual_arguments = normalize_score_arguments(&actual.arguments);
        let mut argument_diffs = Vec::new();
        compare_json_values(
            &expected_arguments,
            &actual_arguments,
            "$",
            case.allow_extra_arguments,
            &mut argument_diffs,
        );
        if !argument_diffs.is_empty() {
            argument_match = false;
            diagnostics.push(format!(
                "tool[{index}] argument mismatch: {}",
                argument_diffs.join("; ")
            ));
        }
    }

    let strict_match = parse_success && tool_name_match && argument_match;

    BfclCaseScore {
        id: case.id.clone(),
        category: case.category.clone(),
        source: case.source.clone(),
        missing_prediction,
        parse_success,
        tool_name_match,
        argument_match,
        strict_match,
        expected_tool_calls,
        actual_tool_calls,
        missing_tool_calls,
        extra_tool_calls,
        diagnostics,
    }
}

fn load_jsonl<T>(path: impl AsRef<Path>) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("open JSONL file {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut rows = Vec::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line = line.with_context(|| {
            format!(
                "read line {} from JSONL file {}",
                line_index + 1,
                path.display()
            )
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let row = serde_json::from_str::<T>(trimmed).with_context(|| {
            format!(
                "parse JSONL record at {}:{}",
                path.display(),
                line_index + 1
            )
        })?;
        rows.push(row);
    }

    Ok(rows)
}

fn compare_json_values(
    expected: &Value,
    actual: &Value,
    path: &str,
    allow_extra_arguments: bool,
    diffs: &mut Vec<String>,
) {
    if expected == actual || numbers_equal(expected, actual) {
        return;
    }

    match (expected, actual) {
        (Value::Object(expected_object), Value::Object(actual_object)) => {
            for (key, expected_value) in expected_object {
                let child_path = object_path(path, key);
                match actual_object.get(key) {
                    Some(actual_value) => compare_json_values(
                        expected_value,
                        actual_value,
                        &child_path,
                        allow_extra_arguments,
                        diffs,
                    ),
                    None => diffs.push(format!("missing {child_path}")),
                }
            }

            if !allow_extra_arguments {
                for key in actual_object.keys() {
                    if !expected_object.contains_key(key) {
                        diffs.push(format!("unexpected {}", object_path(path, key)));
                    }
                }
            }
        }
        (Value::Array(expected_array), Value::Array(actual_array)) => {
            if expected_array.len() != actual_array.len() {
                diffs.push(format!(
                    "{path} array length mismatch: expected {}, got {}",
                    expected_array.len(),
                    actual_array.len()
                ));
            }

            for (index, (expected_value, actual_value)) in
                expected_array.iter().zip(actual_array.iter()).enumerate()
            {
                compare_json_values(
                    expected_value,
                    actual_value,
                    &array_path(path, index),
                    allow_extra_arguments,
                    diffs,
                );
            }
        }
        _ => diffs.push(format!(
            "{path} expected {}, got {}",
            compact_json(expected),
            compact_json(actual)
        )),
    }
}

fn normalize_score_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::Null => empty_json_object(),
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| arguments.clone()),
        _ => arguments.clone(),
    }
}

fn numbers_equal(expected: &Value, actual: &Value) -> bool {
    let (Some(expected), Some(actual)) = (expected.as_f64(), actual.as_f64()) else {
        return false;
    };
    (expected - actual).abs() < f64::EPSILON
}

fn object_path(parent: &str, key: &str) -> String {
    if parent == "$" {
        format!("$.{key}")
    } else {
        format!("{parent}.{key}")
    }
}

fn array_path(parent: &str, index: usize) -> String {
    format!("{parent}[{index}]")
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

fn empty_json_object() -> Value {
    serde_json::json!({})
}

fn ratio(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn case(expected_tool_calls: Vec<ExpectedToolCall>) -> BfclCase {
        BfclCase {
            id: "case-1".to_string(),
            category: Some("unit".to_string()),
            source: None,
            prompt: "test prompt".to_string(),
            expected_tool_calls,
            allow_extra_arguments: false,
        }
    }

    fn expected(name: &str, arguments: Value) -> ExpectedToolCall {
        ExpectedToolCall {
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn scores_exact_tool_call() {
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"entity": "kitchen light", "action": "turn_on"}),
        )]);

        let score = score_response(
            &case,
            r#"{"tool":"home_control","arguments":{"action":"turn_on","entity":"kitchen light"}}"#,
        );

        assert!(score.parse_success);
        assert!(score.tool_name_match);
        assert!(score.argument_match);
        assert!(score.strict_match);
    }

    #[test]
    fn detects_wrong_tool_name() {
        let case = case(vec![expected(
            "set_timer",
            serde_json::json!({"seconds": 60}),
        )]);

        let score = score_response(&case, r#"{"tool":"get_time","arguments":{"seconds":60}}"#);

        assert!(!score.tool_name_match);
        assert!(!score.strict_match);
        assert!(
            score
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("name mismatch"))
        );
    }

    #[test]
    fn detects_missing_argument() {
        let case = case(vec![expected(
            "set_timer",
            serde_json::json!({"seconds": 60, "label": "cookies"}),
        )]);

        let score = score_response(&case, r#"{"tool":"set_timer","arguments":{"seconds":60}}"#);

        assert!(score.tool_name_match);
        assert!(!score.argument_match);
        assert!(
            score
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("missing $.label"))
        );
    }

    #[test]
    fn allows_extra_arguments_when_case_allows() {
        let mut case = case(vec![expected(
            "memory_recall",
            serde_json::json!({"query": "Grandma Wi-Fi"}),
        )]);
        case.allow_extra_arguments = true;

        let score = score_response(
            &case,
            r#"{"tool":"memory_recall","arguments":{"query":"Grandma Wi-Fi","limit":3}}"#,
        );

        assert!(score.argument_match);
        assert!(score.strict_match);
    }

    #[test]
    fn rejects_extra_arguments_by_default() {
        let case = case(vec![expected(
            "memory_recall",
            serde_json::json!({"query": "Grandma Wi-Fi"}),
        )]);

        let score = score_response(
            &case,
            r#"{"tool":"memory_recall","arguments":{"query":"Grandma Wi-Fi","limit":3}}"#,
        );

        assert!(!score.argument_match);
        assert!(
            score
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("unexpected $.limit"))
        );
    }

    #[test]
    fn scores_no_tool_case() {
        let case = case(Vec::new());

        let score = score_response(&case, "I can answer that without a tool.");

        assert!(score.parse_success);
        assert!(score.tool_name_match);
        assert!(score.argument_match);
        assert!(score.strict_match);
    }

    #[test]
    fn loads_jsonl_fixture_and_scores_report() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cases = load_cases_jsonl(root.join("tests/bfcl/home_tool_cases.jsonl")).unwrap();
        let predictions =
            load_predictions_jsonl(root.join("tests/bfcl/home_tool_predictions.jsonl")).unwrap();

        let report = score_cases(&cases, &predictions);

        assert_eq!(report.total_cases, 21);
        assert_eq!(report.strict_matches, 21);
        assert_eq!(report.failure_count, 0);
        assert!((report.strict_accuracy - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn loads_optional_source_metadata_for_license_audit() {
        let record = r#"{
            "id": "ha-turn-on-kitchen",
            "category": "home_control",
            "source": {
                "dataset": "Home Assistant Intents",
                "url": "https://github.com/OHF-Voice/intents",
                "license": "CC BY 4.0",
                "citation": "OHF-Voice/intents",
                "derived_from": "sentences/en/light_HassTurnOn.yaml",
                "notes": "Converted template sentence with local fixture slots."
            },
            "prompt": "turn on the kitchen light",
            "expected_tool_calls": [
                {
                    "name": "home_control",
                    "arguments": {
                        "action": "turn_on",
                        "entity": "kitchen light"
                    }
                }
            ]
        }"#;

        let case = serde_json::from_str::<BfclCase>(record).unwrap();
        let source = case.source.expect("source metadata");

        assert_eq!(source.dataset, "Home Assistant Intents");
        assert_eq!(source.license.as_deref(), Some("CC BY 4.0"));
        assert_eq!(
            source.derived_from.as_deref(),
            Some("sentences/en/light_HassTurnOn.yaml")
        );
    }

    #[test]
    fn jsonl_fixture_covers_all_static_builtin_tools() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cases = load_cases_jsonl(root.join("tests/bfcl/home_tool_cases.jsonl")).unwrap();
        let covered_tools = cases
            .iter()
            .flat_map(|case| {
                case.expected_tool_calls
                    .iter()
                    .map(|tool_call| tool_call.name.as_str())
            })
            .collect::<BTreeSet<_>>();

        let static_builtin_tools = [
            "home_control",
            "home_status",
            "home_undo",
            "action_history",
            "set_timer",
            "get_time",
            "get_weather",
            "web_search",
            "system_info",
            "calculate",
            "play_media",
            "memory_recall",
            "memory_status",
            "memory_forget",
            "memory_store",
        ];

        for tool in static_builtin_tools {
            assert!(
                covered_tools.contains(tool),
                "missing BFCL fixture for static built-in tool: {tool}"
            );
        }
    }
}
