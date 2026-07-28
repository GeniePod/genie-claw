//! Failure taxonomy for BFCL scoring runs (#387).
//!
//! [`crate::eval::bfcl`] answers *how many* cases pass; it does not answer *why*
//! the rest fail. That gap matters most between two of its metrics:
//! `argument_accuracy` (raw exact string match) and `grounded_argument_accuracy`
//! (match after the runtime resolver grounds entity arguments against the
//! reference home). The delta between them is large — on-device Qwen3-4B moved
//! 20.19% raw to 72.12% grounded — but the scorer records a grounded failure as
//! a bare `false` with no diagnostic, so neither the recovered cases nor the
//! residual failures can be characterized from a scoring run.
//!
//! This module classifies every case in a [`BfclReport`] into one
//! [`FailureClass`], splitting that delta into:
//!
//! * **recoveries** — raw-fail, grounded-pass. The model named the right
//!   physical device with a different surface string. Sub-classified by *how*
//!   the string differed (entity id, snake_case, room qualifier, plural, action
//!   synonym), because each form suggests a different prompt or scorer change.
//! * **residual failures** — wrong even after grounding, split by cause. The
//!   critical split is [`FailureClass::UnresolvedEntity`] (the model named
//!   something the home does not have) versus [`FailureClass::WrongDevice`]
//!   (it resolved, but to another device). Those are indistinguishable in the
//!   scorer's pass/fail bit and call for opposite fixes.
//!
//! The analysis is derived purely from an already-computed [`BfclReport`]: it
//! re-reads the expected and actual tool calls the scorer already recorded and
//! never re-scores, so it cannot disagree with the headline numbers.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use super::bfcl::{
    BfclCaseScore, BfclReport, action_argument_keys, canonical_action, entity_argument_keys,
    resolve_reference_home_entity_ids,
};

/// One bucket in the failure taxonomy.
///
/// Ordering is meaningful for reporting: passes first, then recoveries (raw
/// fail / grounded pass), then residual failures roughly by how early in the
/// pipeline they occur.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Raw exact match — nothing to explain.
    StrictPass,
    /// Grounded-only pass: predicted an entity id (`light.kitchen`) where the
    /// gold string is a display name (`kitchen lights`).
    RecoveredEntityIdForm,
    /// Grounded-only pass: `kitchen_light` style separators instead of spaces.
    RecoveredSnakeCase,
    /// Grounded-only pass: one side carries a room/area qualifier the other
    /// omits (`lights` vs `kitchen lights`) — the dominant recovery form,
    /// because the model is faithful to the user's words while the gold string
    /// applies the home's single-device convention.
    RecoveredRoomQualifier,
    /// Grounded-only pass: singular/plural variant of the same name.
    RecoveredPlural,
    /// Grounded-only pass: action synonym the runtime executes identically
    /// (`deactivate` vs `turn_off`), entity already equal.
    RecoveredActionSynonym,
    /// Grounded-only pass whose surface delta did not match a known form.
    RecoveredOther,
    /// Entity resolves to nothing in the reference home — a named room or
    /// device the home does not have. Grounding must *not* credit these.
    UnresolvedEntity,
    /// Entity resolves, but to different ids than the gold entity.
    WrongDevice,
    /// Entity agrees; the action verb differs even after canonicalization.
    ActionMismatch,
    /// Entity and action agree; some other scalar argument differs
    /// (brightness, temperature, duration, ...).
    ScalarArgMismatch,
    /// An expected argument key is absent from the prediction.
    MissingArgument,
    /// The prediction carries an argument key the case does not expect.
    UnexpectedArgument,
    /// Right number of calls, wrong tool selected.
    ToolNameMismatch,
    /// Prediction parsed but produced a different number of tool calls.
    CallCountMismatch,
    /// No parsable tool call in the response.
    ParseFailure,
    /// No prediction row for this case id.
    MissingPrediction,
}

impl FailureClass {
    /// Stable snake_case label, matching the serde representation.
    pub fn label(self) -> &'static str {
        match self {
            Self::StrictPass => "strict_pass",
            Self::RecoveredEntityIdForm => "recovered_entity_id_form",
            Self::RecoveredSnakeCase => "recovered_snake_case",
            Self::RecoveredRoomQualifier => "recovered_room_qualifier",
            Self::RecoveredPlural => "recovered_plural",
            Self::RecoveredActionSynonym => "recovered_action_synonym",
            Self::RecoveredOther => "recovered_other",
            Self::UnresolvedEntity => "unresolved_entity",
            Self::WrongDevice => "wrong_device",
            Self::ActionMismatch => "action_mismatch",
            Self::ScalarArgMismatch => "scalar_arg_mismatch",
            Self::MissingArgument => "missing_argument",
            Self::UnexpectedArgument => "unexpected_argument",
            Self::ToolNameMismatch => "tool_name_mismatch",
            Self::CallCountMismatch => "call_count_mismatch",
            Self::ParseFailure => "parse_failure",
            Self::MissingPrediction => "missing_prediction",
        }
    }

    /// True when this bucket is a raw-fail that the grounded metric credits.
    /// The sum over these is exactly the raw-to-grounded gap.
    pub fn is_recovery(self) -> bool {
        matches!(
            self,
            Self::RecoveredEntityIdForm
                | Self::RecoveredSnakeCase
                | Self::RecoveredRoomQualifier
                | Self::RecoveredPlural
                | Self::RecoveredActionSynonym
                | Self::RecoveredOther
        )
    }

    /// True when the case fails even after grounding.
    pub fn is_residual_failure(self) -> bool {
        !matches!(self, Self::StrictPass) && !self.is_recovery()
    }
}

/// Per-case classification, carrying enough context to act on the bucket.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseAnalysis {
    pub id: String,
    pub category: Option<String>,
    pub class: FailureClass,
    /// Human-readable detail, e.g. `$.entity: expected "kitchen lights", got "lights"`.
    pub detail: String,
}

/// Aggregate taxonomy over a whole scoring run.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct AnalysisReport {
    pub total_cases: usize,
    /// Cases passing raw exact match.
    pub strict_passes: usize,
    /// Raw-fail, grounded-pass. Equals `grounded_argument_matches - argument_matches`
    /// whenever the scorer's two metrics differ only on argument grounding.
    pub grounded_recoveries: usize,
    /// Cases failing even after grounding.
    pub residual_failures: usize,
    pub argument_accuracy: f64,
    pub grounded_argument_accuracy: f64,
    /// `grounded_argument_accuracy - argument_accuracy`, the gap this taxonomy explains.
    pub grounding_gap: f64,
    /// Case count per bucket, keyed by [`FailureClass::label`].
    pub class_counts: BTreeMap<String, usize>,
    pub cases: Vec<CaseAnalysis>,
}

/// A single leaf difference between expected and actual arguments.
#[derive(Debug, Clone, PartialEq)]
struct ArgDelta {
    /// JSON-ish path, e.g. `$.entity`.
    path: String,
    /// Trailing key name, used to decide whether this is an entity/action key.
    key: String,
    expected: Option<Value>,
    actual: Option<Value>,
}

impl ArgDelta {
    fn is_entity(&self) -> bool {
        entity_argument_keys().contains(&self.key.as_str())
    }

    fn is_action(&self) -> bool {
        action_argument_keys().contains(&self.key.as_str())
    }

    fn expected_str(&self) -> Option<&str> {
        self.expected.as_ref().and_then(Value::as_str)
    }

    fn actual_str(&self) -> Option<&str> {
        self.actual.as_ref().and_then(Value::as_str)
    }

    fn describe(&self) -> String {
        match (&self.expected, &self.actual) {
            (Some(expected), Some(actual)) => format!(
                "{}: expected {}, got {}",
                self.path,
                compact(expected),
                compact(actual)
            ),
            (Some(expected), None) => {
                format!("{}: missing, expected {}", self.path, compact(expected))
            }
            (None, Some(actual)) => format!("{}: unexpected {}", self.path, compact(actual)),
            (None, None) => self.path.clone(),
        }
    }
}

/// Classify every case in `report` into exactly one [`FailureClass`].
pub fn analyze_report(report: &BfclReport) -> AnalysisReport {
    let cases: Vec<CaseAnalysis> = report.case_scores.iter().map(analyze_case).collect();

    let mut class_counts: BTreeMap<String, usize> = BTreeMap::new();
    for case in &cases {
        *class_counts
            .entry(case.class.label().to_string())
            .or_insert(0) += 1;
    }

    let strict_passes = cases
        .iter()
        .filter(|case| case.class == FailureClass::StrictPass)
        .count();
    let grounded_recoveries = cases.iter().filter(|case| case.class.is_recovery()).count();
    let residual_failures = cases
        .iter()
        .filter(|case| case.class.is_residual_failure())
        .count();

    AnalysisReport {
        total_cases: cases.len(),
        strict_passes,
        grounded_recoveries,
        residual_failures,
        argument_accuracy: report.argument_accuracy,
        grounded_argument_accuracy: report.grounded_argument_accuracy,
        grounding_gap: report.grounded_argument_accuracy - report.argument_accuracy,
        class_counts,
        cases,
    }
}

/// Classify one scored case.
fn analyze_case(score: &BfclCaseScore) -> CaseAnalysis {
    let (class, detail) = classify(score);
    CaseAnalysis {
        id: score.id.clone(),
        category: score.category.clone(),
        class,
        detail,
    }
}

fn classify(score: &BfclCaseScore) -> (FailureClass, String) {
    if score.missing_prediction {
        return (
            FailureClass::MissingPrediction,
            "no prediction row for this case id".to_string(),
        );
    }
    if !score.parse_success {
        return (
            FailureClass::ParseFailure,
            "no parsable tool call in the response".to_string(),
        );
    }
    if score.expected_tool_calls.len() != score.actual_tool_calls.len() {
        return (
            FailureClass::CallCountMismatch,
            format!(
                "expected {} tool call(s), got {}",
                score.expected_tool_calls.len(),
                score.actual_tool_calls.len()
            ),
        );
    }
    if !score.tool_name_match {
        let detail = score
            .expected_tool_calls
            .iter()
            .zip(score.actual_tool_calls.iter())
            .find(|(expected, actual)| expected.name != actual.name)
            .map(|(expected, actual)| {
                format!("expected tool '{}', got '{}'", expected.name, actual.name)
            })
            .unwrap_or_else(|| "tool name mismatch".to_string());
        return (FailureClass::ToolNameMismatch, detail);
    }
    if score.argument_match {
        return (FailureClass::StrictPass, String::new());
    }

    // Raw arguments differ. Collect the leaf deltas across all call pairs, then
    // let the grounded verdict decide whether we are explaining a recovery or a
    // residual failure.
    let deltas = collect_deltas(score);
    if deltas.is_empty() {
        // The scorer disagreed on arguments but the structural walk found no
        // leaf delta (e.g. a numeric-format-only difference). Report it rather
        // than silently mis-bucketing.
        let class = if score.grounded_argument_match {
            FailureClass::RecoveredOther
        } else {
            FailureClass::ScalarArgMismatch
        };
        return (class, "argument mismatch with no leaf delta".to_string());
    }

    let detail = deltas
        .iter()
        .map(ArgDelta::describe)
        .collect::<Vec<_>>()
        .join("; ");

    if score.grounded_argument_match {
        (classify_recovery(&deltas), detail)
    } else {
        (classify_residual_failure(&deltas), detail)
    }
}

/// Bucket a raw-fail / grounded-pass case by the shape of its surface delta.
fn classify_recovery(deltas: &[ArgDelta]) -> FailureClass {
    // Prefer explaining the entity delta: it is the axis #387 is about, and an
    // action synonym alongside it is secondary.
    if let Some(delta) = deltas.iter().find(|delta| delta.is_entity()) {
        let (Some(expected), Some(actual)) = (delta.expected_str(), delta.actual_str()) else {
            return FailureClass::RecoveredOther;
        };
        return classify_entity_surface_delta(expected, actual);
    }

    if deltas.iter().any(ArgDelta::is_action) {
        return FailureClass::RecoveredActionSynonym;
    }

    FailureClass::RecoveredOther
}

/// Classify how two entity strings that resolve to the same device differ.
fn classify_entity_surface_delta(expected: &str, actual: &str) -> FailureClass {
    let expected_lower = expected.trim().to_lowercase();
    let actual_lower = actual.trim().to_lowercase();

    // `light.kitchen` — a Home Assistant entity id rather than a display name.
    if actual_lower.contains('.') && !expected_lower.contains('.') {
        return FailureClass::RecoveredEntityIdForm;
    }
    // `kitchen_light` — right words, wrong separator.
    if actual_lower.contains('_') && !expected_lower.contains('_') {
        return FailureClass::RecoveredSnakeCase;
    }
    if is_plural_variant(&expected_lower, &actual_lower) {
        return FailureClass::RecoveredPlural;
    }
    if is_qualifier_subset(&expected_lower, &actual_lower) {
        return FailureClass::RecoveredRoomQualifier;
    }
    FailureClass::RecoveredOther
}

/// True when the two names differ only by a trailing `s`.
fn is_plural_variant(left: &str, right: &str) -> bool {
    let (shorter, longer) = if left.len() <= right.len() {
        (left, right)
    } else {
        (right, left)
    };
    longer.strip_suffix('s').is_some_and(|stem| stem == shorter)
}

/// True when one name's words are a strict subset of the other's — the
/// room-qualifier form (`lights` inside `kitchen lights`).
fn is_qualifier_subset(left: &str, right: &str) -> bool {
    let left_words: Vec<&str> = left.split_whitespace().collect();
    let right_words: Vec<&str> = right.split_whitespace().collect();
    if left_words.is_empty() || right_words.is_empty() || left_words == right_words {
        return false;
    }

    let (shorter, longer) = if left_words.len() <= right_words.len() {
        (&left_words, &right_words)
    } else {
        (&right_words, &left_words)
    };

    // Compare on singular stems so "lights" matches inside "kitchen light".
    shorter
        .iter()
        .all(|word| longer.iter().any(|other| same_stem(word, other)))
}

/// Loose singular/plural stem equality for a single word.
fn same_stem(left: &str, right: &str) -> bool {
    left == right
        || left.strip_suffix('s').is_some_and(|stem| stem == right)
        || right.strip_suffix('s').is_some_and(|stem| stem == left)
}

/// Bucket a case that fails even after grounding.
fn classify_residual_failure(deltas: &[ArgDelta]) -> FailureClass {
    if let Some(delta) = deltas.iter().find(|delta| delta.is_entity()) {
        match (delta.expected_str(), delta.actual_str()) {
            (Some(expected), Some(actual)) => {
                let actual_ids = resolve_reference_home_entity_ids(actual);
                if actual_ids.is_none() {
                    // Named something the home does not have. This is the
                    // wrong-room hallucination the fidelity guard exists to
                    // stop from earning false grounded credit.
                    return FailureClass::UnresolvedEntity;
                }
                if resolve_reference_home_entity_ids(expected) != actual_ids {
                    return FailureClass::WrongDevice;
                }
                // Entity grounds to the same device, so the residual failure is
                // on another argument; fall through to the checks below.
            }
            (Some(_), None) => return FailureClass::MissingArgument,
            (None, Some(_)) => return FailureClass::UnexpectedArgument,
            (None, None) => {}
        }
    }

    if let Some(delta) = deltas.iter().find(|delta| delta.is_action()) {
        match (delta.expected_str(), delta.actual_str()) {
            (Some(expected), Some(actual)) => {
                if canonical_action(expected) != canonical_action(actual) {
                    return FailureClass::ActionMismatch;
                }
            }
            (Some(_), None) => return FailureClass::MissingArgument,
            (None, Some(_)) => return FailureClass::UnexpectedArgument,
            (None, None) => {}
        }
    }

    if deltas.iter().any(|delta| delta.expected.is_none()) {
        return FailureClass::UnexpectedArgument;
    }
    if deltas.iter().any(|delta| delta.actual.is_none()) {
        return FailureClass::MissingArgument;
    }
    FailureClass::ScalarArgMismatch
}

/// Walk every expected/actual call pair and collect leaf argument differences.
fn collect_deltas(score: &BfclCaseScore) -> Vec<ArgDelta> {
    let mut deltas = Vec::new();
    for (expected, actual) in score
        .expected_tool_calls
        .iter()
        .zip(score.actual_tool_calls.iter())
    {
        diff_values(
            &normalize_arguments(&expected.arguments),
            &normalize_arguments(&actual.arguments),
            "$",
            "",
            &mut deltas,
        );
    }
    deltas
}

/// Mirror of the scorer's argument normalization: a JSON-encoded string of
/// arguments is decoded first so both sides compare as objects.
fn normalize_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::Null => Value::Object(serde_json::Map::new()),
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| arguments.clone()),
        other => other.clone(),
    }
}

fn diff_values(expected: &Value, actual: &Value, path: &str, key: &str, out: &mut Vec<ArgDelta>) {
    if expected == actual || numbers_equal(expected, actual) {
        return;
    }

    match (expected, actual) {
        (Value::Object(expected_object), Value::Object(actual_object)) => {
            for (child_key, expected_value) in expected_object {
                let child_path = join_path(path, child_key);
                match actual_object.get(child_key) {
                    Some(actual_value) => {
                        diff_values(expected_value, actual_value, &child_path, child_key, out)
                    }
                    None => out.push(ArgDelta {
                        path: child_path,
                        key: child_key.clone(),
                        expected: Some(expected_value.clone()),
                        actual: None,
                    }),
                }
            }
            for (child_key, actual_value) in actual_object {
                if !expected_object.contains_key(child_key) {
                    out.push(ArgDelta {
                        path: join_path(path, child_key),
                        key: child_key.clone(),
                        expected: None,
                        actual: Some(actual_value.clone()),
                    });
                }
            }
        }
        (Value::Array(expected_array), Value::Array(actual_array))
            if expected_array.len() == actual_array.len() =>
        {
            for (index, (expected_item, actual_item)) in
                expected_array.iter().zip(actual_array.iter()).enumerate()
            {
                diff_values(
                    expected_item,
                    actual_item,
                    &format!("{path}[{index}]"),
                    key,
                    out,
                );
            }
        }
        _ => out.push(ArgDelta {
            path: path.to_string(),
            key: key.to_string(),
            expected: Some(expected.clone()),
            actual: Some(actual.clone()),
        }),
    }
}

fn numbers_equal(expected: &Value, actual: &Value) -> bool {
    let (Some(expected), Some(actual)) = (expected.as_f64(), actual.as_f64()) else {
        return false;
    };
    (expected - actual).abs() < f64::EPSILON
}

fn join_path(parent: &str, key: &str) -> String {
    if parent == "$" {
        format!("$.{key}")
    } else {
        format!("{parent}.{key}")
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unserializable>".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::bfcl::{BfclCase, BfclPrediction, ExpectedToolCall, score_cases};
    use serde_json::json;

    fn case(id: &str, arguments: Value) -> BfclCase {
        BfclCase {
            id: id.to_string(),
            category: Some("home_control".to_string()),
            source: None,
            prompt: "test prompt".to_string(),
            expected_tool_calls: vec![ExpectedToolCall {
                name: "home_control".to_string(),
                arguments,
            }],
            allow_extra_arguments: false,
        }
    }

    fn prediction(id: &str, arguments: Value) -> BfclPrediction {
        BfclPrediction {
            id: id.to_string(),
            response: json!({ "tool": "home_control", "arguments": arguments }).to_string(),
        }
    }

    /// Score one expected/actual argument pair and return its bucket.
    fn class_for(expected: Value, actual: Value) -> FailureClass {
        let cases = vec![case("c1", expected)];
        let predictions = vec![prediction("c1", actual)];
        let report = score_cases(&cases, &predictions);
        let analysis = analyze_report(&report);
        analysis.cases[0].class
    }

    #[test]
    fn exact_match_is_a_strict_pass() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            FailureClass::StrictPass
        );
    }

    #[test]
    fn dropped_room_qualifier_is_a_room_qualifier_recovery() {
        // The dominant recovery form: the model is faithful to "turn on the
        // lights" while the gold string carries the home's room convention.
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "lights", "action": "turn_on" }),
            ),
            FailureClass::RecoveredRoomQualifier
        );
    }

    #[test]
    fn entity_id_form_is_its_own_recovery_bucket() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "light.kitchen", "action": "turn_on" }),
            ),
            FailureClass::RecoveredEntityIdForm
        );
    }

    #[test]
    fn snake_case_form_is_its_own_recovery_bucket() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "kitchen_lights", "action": "turn_on" }),
            ),
            FailureClass::RecoveredSnakeCase
        );
    }

    #[test]
    fn action_synonym_alone_is_an_action_synonym_recovery() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_off" }),
                json!({ "entity": "kitchen lights", "action": "deactivate" }),
            ),
            FailureClass::RecoveredActionSynonym
        );
    }

    #[test]
    fn entity_the_home_does_not_have_is_unresolved_not_wrong_device() {
        // The wrong-room hallucination: "upstairs lights" resolves to nothing,
        // so it must be separated from a real-but-wrong device.
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "upstairs lights", "action": "turn_on" }),
            ),
            FailureClass::UnresolvedEntity
        );
    }

    #[test]
    fn resolving_to_another_device_is_wrong_device() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "kitchen fan", "action": "turn_on" }),
            ),
            FailureClass::WrongDevice
        );
    }

    #[test]
    fn non_synonym_action_with_right_entity_is_an_action_mismatch() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "kitchen lights", "action": "toggle" }),
            ),
            FailureClass::ActionMismatch
        );
    }

    #[test]
    fn a_differing_scalar_argument_is_a_scalar_mismatch() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on", "brightness": 40 }),
                json!({ "entity": "kitchen lights", "action": "turn_on", "brightness": 80 }),
            ),
            FailureClass::ScalarArgMismatch
        );
    }

    #[test]
    fn an_absent_expected_key_is_a_missing_argument() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on", "brightness": 40 }),
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            FailureClass::MissingArgument
        );
    }

    #[test]
    fn an_extra_key_is_an_unexpected_argument() {
        assert_eq!(
            class_for(
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
                json!({ "entity": "kitchen lights", "action": "turn_on", "brightness": 80 }),
            ),
            FailureClass::UnexpectedArgument
        );
    }

    #[test]
    fn a_missing_prediction_row_is_its_own_bucket() {
        let cases = vec![case("c1", json!({ "entity": "kitchen lights" }))];
        let report = score_cases(&cases, &[]);
        let analysis = analyze_report(&report);
        assert_eq!(analysis.cases[0].class, FailureClass::MissingPrediction);
    }

    #[test]
    fn an_unparsable_response_is_a_parse_failure() {
        let cases = vec![case("c1", json!({ "entity": "kitchen lights" }))];
        let predictions = vec![BfclPrediction {
            id: "c1".to_string(),
            response: "I'm sorry, I can't help with that.".to_string(),
        }];
        let report = score_cases(&cases, &predictions);
        let analysis = analyze_report(&report);
        assert_eq!(analysis.cases[0].class, FailureClass::ParseFailure);
    }

    #[test]
    fn a_different_tool_is_a_tool_name_mismatch() {
        let cases = vec![case("c1", json!({ "entity": "kitchen lights" }))];
        let predictions = vec![BfclPrediction {
            id: "c1".to_string(),
            response: json!({ "tool": "get_weather", "arguments": { "location": "kitchen" } })
                .to_string(),
        }];
        let report = score_cases(&cases, &predictions);
        let analysis = analyze_report(&report);
        assert_eq!(analysis.cases[0].class, FailureClass::ToolNameMismatch);
    }

    #[test]
    fn recovery_count_equals_the_raw_to_grounded_gap() {
        // The taxonomy's core invariant: every case the grounded metric credits
        // over the raw metric lands in exactly one recovery bucket.
        let cases = vec![
            case(
                "pass",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            case(
                "recover",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            case(
                "fail",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
        ];
        let predictions = vec![
            prediction(
                "pass",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            prediction(
                "recover",
                json!({ "entity": "lights", "action": "turn_on" }),
            ),
            prediction(
                "fail",
                json!({ "entity": "upstairs lights", "action": "turn_on" }),
            ),
        ];
        let report = score_cases(&cases, &predictions);
        let analysis = analyze_report(&report);

        assert_eq!(analysis.strict_passes, 1);
        assert_eq!(analysis.grounded_recoveries, 1);
        assert_eq!(analysis.residual_failures, 1);
        assert_eq!(
            analysis.grounded_recoveries,
            report.grounded_argument_matches - report.argument_matches
        );
    }

    #[test]
    fn class_counts_cover_every_case_exactly_once() {
        let cases = vec![
            case(
                "a",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            case(
                "b",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
        ];
        let predictions = vec![
            prediction(
                "a",
                json!({ "entity": "kitchen lights", "action": "turn_on" }),
            ),
            prediction("b", json!({ "entity": "lights", "action": "turn_on" })),
        ];
        let analysis = analyze_report(&score_cases(&cases, &predictions));
        assert_eq!(analysis.total_cases, 2);
        assert_eq!(analysis.class_counts.values().sum::<usize>(), 2);
    }
}
