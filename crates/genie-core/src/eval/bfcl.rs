//! BFCL-style scoring for local tool-call accuracy.
//!
//! The scorer is intentionally deterministic and side-effect free: it parses
//! model responses, compares ordered tool names and JSON arguments, and reports
//! exact-match rates. It does not execute tools or require a live home backend.

use crate::tools::{ToolCall, parse_tool_calls_for_eval};
use anyhow::{Context, Result};
use async_trait::async_trait;
use genie_common::config::WebSearchConfig;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::Arc;

use crate::ha::{
    ActionResult, AreaRef, DeviceRef, EntityRef, HomeAction, HomeActionKind, HomeAssistantProvider,
    HomeAutomationProvider, HomeGraph, HomeState, HomeTarget, HomeTargetKind, IntegrationHealth,
    SceneRef,
};
use crate::tools::dispatch::{ToolDef, ToolDispatcher};

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
    /// Like `argument_match`, but entity-like string arguments are first
    /// canonicalized through the runtime resolver against the BFCL reference
    /// home, so that e.g. "lights" and "kitchen lights" compare equal when they
    /// resolve to the same physical entities.
    pub grounded_argument_match: bool,
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
    pub grounded_argument_matches: usize,
    pub strict_matches: usize,
    pub grounded_strict_matches: usize,
    pub missing_predictions: usize,
    pub failure_count: usize,
    pub parse_accuracy: f64,
    pub tool_name_accuracy: f64,
    pub argument_accuracy: f64,
    pub grounded_argument_accuracy: f64,
    pub strict_accuracy: f64,
    pub grounded_strict_accuracy: f64,
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
    let grounded_argument_matches = case_scores
        .iter()
        .filter(|score| score.grounded_argument_match)
        .count();
    let strict_matches = case_scores
        .iter()
        .filter(|score| score.strict_match)
        .count();
    let grounded_strict_matches = case_scores
        .iter()
        .filter(|score| {
            score.parse_success && score.tool_name_match && score.grounded_argument_match
        })
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
        grounded_argument_matches,
        strict_matches,
        grounded_strict_matches,
        missing_predictions,
        failure_count,
        parse_accuracy: ratio(parsed_cases, total_cases),
        tool_name_accuracy: ratio(tool_name_matches, total_cases),
        argument_accuracy: ratio(argument_matches, total_cases),
        grounded_argument_accuracy: ratio(grounded_argument_matches, total_cases),
        strict_accuracy: ratio(strict_matches, total_cases),
        grounded_strict_accuracy: ratio(grounded_strict_matches, total_cases),
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
    let home = bfcl_reference_home();

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
            grounded_argument_match: pass,
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
    let mut grounded_argument_match = expected_len == actual_len && parse_success;

    for (index, (expected, actual)) in expected_tool_calls
        .iter()
        .zip(actual_tool_calls.iter())
        .enumerate()
    {
        if expected.name != actual.name {
            tool_name_match = false;
            argument_match = false;
            grounded_argument_match = false;
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

        let expected_grounded = canonicalize_entity_args(&expected_arguments, &home);
        let actual_grounded = canonicalize_entity_args(&actual_arguments, &home);
        let mut grounded_diffs = Vec::new();
        compare_json_values(
            &expected_grounded,
            &actual_grounded,
            "$",
            case.allow_extra_arguments,
            &mut grounded_diffs,
        );
        if !grounded_diffs.is_empty() {
            grounded_argument_match = false;
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
        grounded_argument_match,
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

/// Object keys whose string values are treated as entity-like references and
/// canonicalized through the runtime resolver for the grounded metric.
const ENTITY_ARG_KEYS: [&str; 4] = ["entity", "name", "target", "device"];

/// Argument keys whose string value names an action verb. For the grounded
/// metric these are canonicalized through [`canon_action`] so that synonyms the
/// agent executes identically (e.g. `deactivate` == `turn_off`) are credited.
const ACTION_ARG_KEYS: [&str; 1] = ["action"];

/// Reference home that mirrors the HA-Intents reference home used by the BFCL
/// dataset. It is intentionally minimal and dataset-specific: it contains
/// exactly the four physical entities needed to unify the eight distinct gold
/// entity strings observed in the dataset ("kitchen lights", "kitchen light",
/// "all lights", "kitchen fan", "thermostat", "kitchen thermostat",
/// "living room blinds", "fan"). Because each domain has a single entity, the
/// runtime resolver maps all the area/domain spelling variants for a given
/// domain to the same `entity_ids`, which is what the grounded metric relies
/// on. Generalizing this fixture to arbitrary homes is future work.
fn bfcl_reference_home() -> HomeGraph {
    let entity = |entity_id: &str, name: &str, domain: &str, area: &str| EntityRef {
        entity_id: entity_id.to_string(),
        name: name.to_string(),
        domain: domain.to_string(),
        area: Some(area.to_string()),
        aliases: Vec::new(),
        state: "off".to_string(),
        capabilities: Vec::new(),
    };

    HomeGraph {
        areas: vec![
            AreaRef {
                id: "kitchen".to_string(),
                name: "Kitchen".to_string(),
                aliases: Vec::new(),
            },
            AreaRef {
                id: "living_room".to_string(),
                name: "Living Room".to_string(),
                aliases: Vec::new(),
            },
        ],
        devices: Vec::new(),
        entities: vec![
            entity("light.kitchen", "Kitchen Lights", "light", "Kitchen"),
            entity("fan.kitchen", "Kitchen Fan", "fan", "Kitchen"),
            entity("climate.kitchen", "Thermostat", "climate", "Kitchen"),
            entity(
                "cover.living_room",
                "Living Room Blinds",
                "cover",
                "Living Room",
            ),
        ],
        scenes: Vec::new(),
        scripts: Vec::new(),
        aliases: Vec::new(),
        domains: Vec::new(),
        capabilities: Vec::new(),
    }
}

/// Controllable device names for the BFCL reference home, lowercased to match
/// the dataset's canonical entity surface forms. A real on-device agent always
/// knows its home's devices; feeding this list into the eval prompt grounds
/// entity arguments in real device state (the GeniePod thesis) instead of
/// forcing the model to guess names it cannot see. Lowercase so the model emits
/// the exact gold strings (raw exact-match), and compact so the system prompt
/// stays inside the runtime's prefix-cache window (keeps prefill fast).
pub fn bfcl_reference_home_device_catalog() -> Vec<String> {
    bfcl_reference_home()
        .entities
        .iter()
        .map(|entity| entity.name.to_lowercase())
        .collect()
}

/// Deep-clone `args`, replacing any entity-like string argument with a stable
/// canonical token derived from the entities the runtime resolver would act on.
///
/// For object keys in [`ENTITY_ARG_KEYS`] whose value is a JSON string, the
/// string is resolved against `graph` via the shared runtime resolver. If it
/// resolves to a non-empty set of entity ids, the string is replaced with the
/// token `"ids:<sorted,comma,joined,entity_ids>"`; otherwise the original
/// string is left unchanged (so unresolved values fall back to raw comparison).
/// Nested objects and arrays are processed recursively.
fn canonicalize_entity_args(args: &Value, graph: &HomeGraph) -> Value {
    match args {
        Value::Object(object) => {
            let mut canonical = serde_json::Map::with_capacity(object.len());
            for (key, value) in object {
                let canonical_value = if ENTITY_ARG_KEYS.contains(&key.as_str()) {
                    canonicalize_entity_value(value, graph)
                } else if ACTION_ARG_KEYS.contains(&key.as_str()) {
                    match value {
                        Value::String(text) => Value::String(canon_action(text)),
                        other => canonicalize_entity_args(other, graph),
                    }
                } else {
                    canonicalize_entity_args(value, graph)
                };
                canonical.insert(key.clone(), canonical_value);
            }
            Value::Object(canonical)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| canonicalize_entity_args(item, graph))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Canonicalize a single entity-like value: if it is a string that resolves to
/// concrete entity ids, return the `"ids:..."` token; otherwise recurse (for
/// nested objects/arrays) or return the value unchanged.
fn canonicalize_entity_value(value: &Value, graph: &HomeGraph) -> Value {
    let Value::String(text) = value else {
        return canonicalize_entity_args(value, graph);
    };

    match HomeAssistantProvider::resolve_target_in_graph(graph, text, None) {
        Some(target) if !target.entity_ids.is_empty() => {
            let mut ids = target.entity_ids;
            ids.sort();
            Value::String(format!("ids:{}", ids.join(",")))
        }
        _ => value.clone(),
    }
}

/// Canonicalize an action verb to a small set of stable forms, so the grounded
/// metric credits synonyms the agent would execute identically. Unknown verbs
/// pass through normalized (lowercase, separators -> `_`) so exact matches still
/// hold. Mirrors the canonicalization the runtime tool dispatch applies in
/// `tools::dispatch::canon_home_control_action`.
///
/// `activate` is intentionally *not* folded into `turn_on`: the runtime keeps it
/// as a distinct `home_control` action for scenes/scripts (it appears separately
/// in the action enum and `canon_home_control_action` leaves it as-is), so
/// crediting `turn_on` for an expected `activate` would score a wrong actuation
/// as correct and inflate the grounded metric.
fn canon_action(text: &str) -> String {
    let normalized = text.trim().to_lowercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        "deactivate" | "disable" | "switch_off" | "power_off" | "shut_off" | "turn_off" => {
            "turn_off".to_string()
        }
        "enable" | "switch_on" | "power_on" | "turn_on" => "turn_on".to_string(),
        _ => normalized,
    }
}

struct BfclCatalogHomeStub;

#[async_trait]
impl HomeAutomationProvider for BfclCatalogHomeStub {
    async fn health(&self) -> IntegrationHealth {
        IntegrationHealth {
            connected: true,
            cached_graph: false,
            message: "bfcl-catalog".into(),
        }
    }

    async fn sync_structure(&self) -> Result<HomeGraph> {
        Ok(HomeGraph {
            areas: Vec::new(),
            devices: Vec::new(),
            entities: Vec::new(),
            scenes: Vec::new(),
            scripts: Vec::new(),
            aliases: Vec::new(),
            domains: Vec::new(),
            capabilities: Vec::new(),
        })
    }

    async fn resolve_target(
        &self,
        query: &str,
        _action_hint: Option<HomeActionKind>,
    ) -> Result<HomeTarget> {
        Ok(HomeTarget {
            kind: HomeTargetKind::Entity,
            query: query.into(),
            display_name: query.into(),
            entity_ids: vec!["light.test".into()],
            domain: Some("light".into()),
            area: Some("Kitchen".into()),
            confidence: 0.96,
            voice_safe: true,
        })
    }

    async fn get_state(&self, target: &HomeTarget) -> Result<HomeState> {
        Ok(HomeState {
            target_name: target.display_name.clone(),
            domain: target.domain.clone(),
            area: target.area.clone(),
            entities: Vec::new(),
            available: true,
            spoken_summary: format!("{} is available", target.display_name),
        })
    }

    async fn execute(&self, action: HomeAction) -> Result<ActionResult> {
        Ok(ActionResult {
            success: true,
            spoken_summary: format!("Executed {:?}", action.kind),
            affected_targets: vec![action.target.display_name],
            state_snapshot: None,
            confidence: Some(action.target.confidence),
        })
    }

    async fn list_scenes(&self, _room: Option<&str>) -> Result<Vec<SceneRef>> {
        Ok(Vec::new())
    }

    async fn list_devices(&self, _room: Option<&str>) -> Result<Vec<DeviceRef>> {
        Ok(Vec::new())
    }
}

/// Compact BFCL LLM catalog entries derived from runtime `ToolDef` schemas.
pub fn bfcl_llm_runtime_tool_catalog() -> BTreeMap<String, String> {
    let web_search = WebSearchConfig {
        enabled: true,
        ..WebSearchConfig::default()
    };
    let dispatcher =
        ToolDispatcher::new(Some(Arc::new(BfclCatalogHomeStub))).with_web_search_config(web_search);
    dispatcher
        .tool_defs()
        .into_iter()
        .map(|def| (def.name.clone(), bfcl_llm_tool_summary(&def)))
        .collect()
}

pub fn bfcl_llm_tool_summary(def: &ToolDef) -> String {
    format!(
        "{}. {}",
        bfcl_llm_tool_headline(&def.description),
        format_bfcl_arguments(&def.parameters)
    )
}

fn bfcl_llm_tool_headline(description: &str) -> String {
    description
        .split('.')
        .next()
        .unwrap_or(description)
        .trim()
        .to_string()
}

fn format_bfcl_arguments(parameters: &Value) -> String {
    let Some(properties) = parameters.get("properties").and_then(Value::as_object) else {
        return "arguments: {}".to_string();
    };
    if properties.is_empty() {
        return "arguments: {}".to_string();
    }

    let required: HashSet<&str> = parameters
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let mut parts = Vec::new();
    for (name, spec) in properties {
        if let Some(enum_values) = spec.get("enum").and_then(Value::as_array) {
            let joined = enum_values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|");
            parts.push(format!(r#""{name}": "{joined}""#));
            continue;
        }

        let value_type = spec.get("type").and_then(Value::as_str).unwrap_or("string");
        let prefix = if required.contains(name.as_str()) {
            String::new()
        } else {
            "optional ".to_string()
        };
        parts.push(format!(r#""{name}": {prefix}{value_type}"#));
    }

    format!("arguments: {{{}}}", parts.join(", "))
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
    fn reference_home_unifies_light_variants_but_separates_blinds() {
        let home = bfcl_reference_home();

        let resolve = |query: &str| -> Vec<String> {
            let mut ids = HomeAssistantProvider::resolve_target_in_graph(&home, query, None)
                .unwrap_or_else(|| panic!("query '{query}' should resolve"))
                .entity_ids;
            ids.sort();
            ids
        };

        let light = vec!["light.kitchen".to_string()];
        assert_eq!(resolve("lights"), light);
        assert_eq!(resolve("kitchen lights"), light);
        assert_eq!(resolve("kitchen light"), light);
        assert_eq!(resolve("all lights"), light);

        assert_eq!(resolve("living room blinds"), vec!["cover.living_room"]);
    }

    #[test]
    fn grounded_metric_credits_underspecified_entity() {
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"action": "turn_off", "entity": "kitchen lights"}),
        )]);

        let prediction = BfclPrediction {
            id: case.id.clone(),
            response:
                r#"{"tool":"home_control","arguments":{"action":"turn_off","entity":"lights"}}"#
                    .to_string(),
        };

        let score = score_response(&case, &prediction.response);
        assert!(score.parse_success);
        assert!(score.tool_name_match);
        assert!(
            !score.argument_match,
            "raw entity strings differ, exact match must fail"
        );
        assert!(
            score.grounded_argument_match,
            "grounded resolver should unify 'lights' and 'kitchen lights'"
        );
        assert!(!score.strict_match);

        let report = score_cases(&[case], &[prediction]);
        assert_eq!(report.argument_matches, 0);
        assert_eq!(report.grounded_argument_matches, 1);
        assert_eq!(report.strict_matches, 0);
        assert_eq!(report.grounded_strict_matches, 1);
        assert!((report.grounded_strict_accuracy - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn fidelity_guard_rejects_foreign_and_deviceless_rooms() {
        // The reference home has no upstairs, and no light in the living room.
        // A whole-home fallback must not silently credit either as the kitchen light.
        for foreign in ["upstairs lights", "upstairs_lights", "living room light"] {
            let case = case(vec![expected(
                "home_control",
                serde_json::json!({"action": "turn_off", "entity": "kitchen lights"}),
            )]);
            let response = format!(
                r#"{{"tool":"home_control","arguments":{{"action":"turn_off","entity":"{foreign}"}}}}"#
            );
            let score = score_response(&case, &response);
            assert!(
                !score.grounded_argument_match,
                "'{foreign}' names a place the home lacks and must NOT ground to the kitchen light"
            );
        }

        // Control: the legitimate room-default still grounds.
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"action": "turn_off", "entity": "kitchen lights"}),
        )]);
        let ok = score_response(
            &case,
            r#"{"tool":"home_control","arguments":{"action":"turn_off","entity":"lights"}}"#,
        );
        assert!(
            ok.grounded_argument_match,
            "bare 'lights' should still ground"
        );
    }

    #[test]
    fn grounded_metric_credits_action_synonym() {
        // Right device, but the model said "deactivate" instead of "turn_off".
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"action": "turn_off", "entity": "kitchen lights"}),
        )]);
        let prediction = BfclPrediction {
            id: case.id.clone(),
            response:
                r#"{"tool":"home_control","arguments":{"action":"deactivate","entity":"kitchen lights"}}"#
                    .to_string(),
        };

        let score = score_response(&case, &prediction.response);
        assert!(
            !score.argument_match,
            "raw action strings differ, exact match must fail"
        );
        assert!(
            score.grounded_argument_match,
            "grounded metric should treat 'deactivate' as 'turn_off'"
        );

        let report = score_cases(&[case], &[prediction]);
        assert_eq!(report.argument_matches, 0);
        assert_eq!(report.grounded_strict_matches, 1);
    }

    #[test]
    fn grounded_metric_keeps_activate_distinct_from_turn_on() {
        // `activate` is a distinct runtime action for scenes/scripts: the
        // dispatcher's canon_home_control_action leaves it as-is (covered by a
        // "distinct action, must not remap" regression test), and `home_control`
        // lists `activate` separately from `turn_on` in its action enum. The
        // grounded metric must mirror runtime dispatch, so predicting `turn_on`
        // for an expected `activate` is a *wrong actuation* (turn_on is a no-op
        // for a scene) and must NOT be credited as a grounded match.
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"action": "activate", "entity": "kitchen lights"}),
        )]);
        let prediction = BfclPrediction {
            id: case.id.clone(),
            response:
                r#"{"tool":"home_control","arguments":{"action":"turn_on","entity":"kitchen lights"}}"#
                    .to_string(),
        };

        let score = score_response(&case, &prediction.response);
        assert!(
            !score.argument_match,
            "raw action strings differ, exact match must fail"
        );
        assert!(
            !score.grounded_argument_match,
            "predicting `turn_on` for an expected `activate` is a wrong actuation \
             and must not be credited by the grounded metric"
        );
    }

    #[test]
    fn canon_action_contract() {
        // Lock the scorer's action canonicalization so a future edit can't
        // re-introduce the activate→turn_on collapse this fix removed (#458).
        for s in ["turn_on", "enable", "switch_on", "power_on"] {
            assert_eq!(canon_action(s), "turn_on", "{s} should fold to turn_on");
        }
        for s in [
            "turn_off",
            "deactivate",
            "disable",
            "switch_off",
            "power_off",
            "shut_off",
        ] {
            assert_eq!(canon_action(s), "turn_off", "{s} should fold to turn_off");
        }
        // activate stays distinct — it must NOT fold into turn_on.
        assert_eq!(canon_action("activate"), "activate");
        // Every other home_control action passes through unchanged (no cross-collapse).
        for a in [
            "toggle",
            "open",
            "close",
            "lock",
            "unlock",
            "set_brightness",
            "set_temperature",
        ] {
            assert_eq!(canon_action(a), a, "{a} must pass through unchanged");
        }
        // Normalization: case folding plus space/hyphen to underscore.
        assert_eq!(canon_action(" Turn-On "), "turn_on");
        assert_eq!(canon_action("ACTIVATE"), "activate");
        assert_eq!(canon_action("set brightness"), "set_brightness");
    }

    #[test]
    fn grounded_metric_credits_matching_activate() {
        // Positive complement to grounded_metric_keeps_activate_distinct_from_turn_on:
        // a correctly-predicted `activate` must still be credited — the fix passes
        // `activate` through, it does not drop it.
        let case = case(vec![expected(
            "home_control",
            serde_json::json!({"action": "activate", "entity": "kitchen lights"}),
        )]);
        let response = r#"{"tool":"home_control","arguments":{"action":"activate","entity":"kitchen lights"}}"#;
        let score = score_response(&case, response);
        assert!(
            score.argument_match,
            "exact activate match must be credited"
        );
        assert!(
            score.grounded_argument_match,
            "a matching activate must be credited by the grounded metric"
        );
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

    #[test]
    fn bfcl_llm_catalog_matches_runtime_home_control_schema() {
        let catalog = bfcl_llm_runtime_tool_catalog();
        let home_control = catalog
            .get("home_control")
            .expect("home_control catalog entry");
        for action in [
            "turn_on",
            "turn_off",
            "toggle",
            "set_brightness",
            "set_temperature",
            "open",
            "close",
            "lock",
            "unlock",
            "activate",
        ] {
            assert!(
                home_control.contains(action),
                "missing home_control action {action} in BFCL catalog: {home_control}"
            );
        }
    }
}
