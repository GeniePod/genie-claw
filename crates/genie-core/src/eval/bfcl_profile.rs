//! BFCL evaluation profiles for Jetson head-to-head model comparisons (issue #376).
//!
//! Profiles are opt-in overlays loaded via `GENIEPOD_CONFIG`. They do not change the
//! production default in `deploy/config/geniepod.toml`.

use super::bfcl::BfclReport;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Named Jetson BFCL evaluation profiles shipped in `deploy/config/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BfclEvalProfileId {
    /// Qwen3-4B Q4_K_M @ 4096 — M1 product baseline.
    Qwen4096,
    /// Mamba-2 hybrid (Nemotron-H 4B) @ 16384 — larger-context experiment.
    Nemotron16k,
}

impl BfclEvalProfileId {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "qwen4096" | "qwen-4096" | "qwen" => Some(Self::Qwen4096),
            "nemotron16k" | "nemotron-16k" | "nemotron" | "mamba16k" | "hybrid16k" => {
                Some(Self::Nemotron16k)
            }
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Qwen4096 => "qwen3-4b-4096",
            Self::Nemotron16k => "nemotron-h-4b-16k",
        }
    }

    pub fn config_relative_path(self) -> &'static str {
        match self {
            Self::Qwen4096 => "deploy/config/geniepod.bfcl-qwen4096.jetson.toml",
            Self::Nemotron16k => "deploy/config/geniepod.bfcl-nemotron16k.jetson.toml",
        }
    }

    pub fn expected_llm_model_name(self) -> &'static str {
        match self {
            Self::Qwen4096 => "qwen",
            Self::Nemotron16k => "nemotron-4b",
        }
    }

    pub fn expected_context_window_tokens(self) -> u32 {
        match self {
            Self::Qwen4096 => 4096,
            Self::Nemotron16k => 16384,
        }
    }

    pub fn architecture_note(self) -> &'static str {
        match self {
            Self::Qwen4096 => "dense attention (KV grows with context)",
            Self::Nemotron16k => "Mamba-2 hybrid (near-constant SSM state vs context)",
        }
    }

    /// Resolve the profile config against `repo_root` (workspace root).
    pub fn resolve_config_path(self, repo_root: impl AsRef<Path>) -> PathBuf {
        repo_root
            .as_ref()
            .join(self.config_relative_path())
    }

    pub fn all() -> &'static [Self] {
        &[Self::Qwen4096, Self::Nemotron16k]
    }
}

/// Optional Jetson runtime measurements captured outside BFCL scoring.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BfclJetsonRuntimeMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_memory_mb: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Metadata bundled with a `bfcl-score-llm --json` run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclEvalRunRecord {
    pub profile_label: String,
    pub llm_model_name: String,
    pub context_window_tokens: u32,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture_note: Option<String>,
    pub generated_predictions: usize,
    pub tool_calls: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predictions_out: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_metrics: Option<BfclJetsonRuntimeMetrics>,
    pub report: BfclReport,
}

/// Side-by-side comparison used for issue #376 deliverables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclHeadToHeadRow {
    pub metric: String,
    pub baseline: String,
    pub candidate: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BfclHeadToHeadComparison {
    pub baseline_label: String,
    pub candidate_label: String,
    pub rows: Vec<BfclHeadToHeadRow>,
    pub recommendation: String,
    pub baseline_strict_accuracy: f64,
    pub candidate_strict_accuracy: f64,
}

pub fn build_eval_run_record(
    profile_label: impl Into<String>,
    llm_model_name: impl Into<String>,
    context_window_tokens: u32,
    backend: impl Into<String>,
    config_path: Option<PathBuf>,
    architecture_note: Option<&str>,
    generated_predictions: usize,
    tool_calls: usize,
    predictions_out: Option<PathBuf>,
    runtime_metrics: Option<BfclJetsonRuntimeMetrics>,
    report: BfclReport,
) -> BfclEvalRunRecord {
    BfclEvalRunRecord {
        profile_label: profile_label.into(),
        llm_model_name: llm_model_name.into(),
        context_window_tokens,
        backend: backend.into(),
        config_path: config_path.map(|path| path.display().to_string()),
        architecture_note: architecture_note.map(str::to_string),
        generated_predictions,
        tool_calls,
        predictions_out: predictions_out.map(|path| path.display().to_string()),
        runtime_metrics,
        report,
    }
}

pub fn compare_eval_runs(
    baseline: &BfclEvalRunRecord,
    candidate: &BfclEvalRunRecord,
) -> BfclHeadToHeadComparison {
    let baseline_metrics = baseline.runtime_metrics.as_ref();
    let candidate_metrics = candidate.runtime_metrics.as_ref();

    let rows = vec![
        metric_row(
            "BFCL strict accuracy",
            &format_accuracy(baseline.report.strict_accuracy),
            &format_accuracy(candidate.report.strict_accuracy),
        ),
        metric_row(
            "tool-name accuracy",
            &format_accuracy(baseline.report.tool_name_accuracy),
            &format_accuracy(candidate.report.tool_name_accuracy),
        ),
        metric_row(
            "argument accuracy",
            &format_accuracy(baseline.report.argument_accuracy),
            &format_accuracy(candidate.report.argument_accuracy),
        ),
        metric_row(
            "parse accuracy",
            &format_accuracy(baseline.report.parse_accuracy),
            &format_accuracy(candidate.report.parse_accuracy),
        ),
        metric_row(
            "unified-memory footprint (MB)",
            &format_optional_f64(baseline_metrics.and_then(|m| m.unified_memory_mb)),
            &format_optional_f64(candidate_metrics.and_then(|m| m.unified_memory_mb)),
        ),
        metric_row(
            "first-token latency (ms)",
            &format_optional_f64(baseline_metrics.and_then(|m| m.first_token_ms)),
            &format_optional_f64(candidate_metrics.and_then(|m| m.first_token_ms)),
        ),
        metric_row(
            "tool-response latency (ms)",
            &format_optional_f64(baseline_metrics.and_then(|m| m.tool_response_ms)),
            &format_optional_f64(candidate_metrics.and_then(|m| m.tool_response_ms)),
        ),
    ];

    let recommendation = recommend_hybrid_vs_qwen(baseline, candidate);

    BfclHeadToHeadComparison {
        baseline_label: baseline.profile_label.clone(),
        candidate_label: candidate.profile_label.clone(),
        rows,
        recommendation,
        baseline_strict_accuracy: baseline.report.strict_accuracy,
        candidate_strict_accuracy: candidate.report.strict_accuracy,
    }
}

/// Product recommendation for issue #376: accuracy must improve; memory/latency must fit Jetson.
pub fn recommend_hybrid_vs_qwen(
    baseline: &BfclEvalRunRecord,
    candidate: &BfclEvalRunRecord,
) -> String {
    let baseline_strict = baseline.report.strict_accuracy;
    let candidate_strict = candidate.report.strict_accuracy;
    let accuracy_delta = candidate_strict - baseline_strict;

    if candidate_strict <= baseline_strict {
        return format!(
            "Keep Qwen3-4B @ 4096 as the Jetson default. The hybrid @ 16k did not beat the \
             baseline on strict BFCL accuracy ({:.2}% vs {:.2}%).",
            candidate_strict * 100.0,
            baseline_strict * 100.0
        );
    }

    if let Some(reason) = jetson_budget_blocker(candidate.runtime_metrics.as_ref()) {
        return format!(
            "Hybrid @ 16k scored higher on strict BFCL ({:.2}% vs {:.2}%, +{:.2} pp) but does \
             not fit the Jetson memory/latency budget: {}. Keep Qwen @ 4096 until runtime \
             metrics are within budget.",
            candidate_strict * 100.0,
            baseline_strict * 100.0,
            accuracy_delta * 100.0,
            reason
        );
    }

    if baseline.runtime_metrics.is_none() || candidate.runtime_metrics.is_none() {
        return format!(
            "Hybrid @ 16k leads on strict BFCL ({:.2}% vs {:.2}%, +{:.2} pp). Record Jetson \
             unified-memory and latency metrics on both profiles, then re-run \
             `genie-ctl bfcl-compare` before changing the product default.",
            candidate_strict * 100.0,
            baseline_strict * 100.0,
            accuracy_delta * 100.0
        );
    }

    format!(
        "Hybrid @ 16k wins on strict BFCL ({:.2}% vs {:.2}%, +{:.2} pp) and recorded Jetson \
         metrics are within the documented budget. Consider promoting the hybrid path only after \
         `genie-ai-runtime` serving validation and a full HA-Intents + typed-tool suite pass; \
         keep Qwen @ 4096 until that promotion is explicit.",
        candidate_strict * 100.0,
        baseline_strict * 100.0,
        accuracy_delta * 100.0
    )
}

/// Conservative Jetson Orin 8GB guardrails for the 16k hybrid experiment.
fn jetson_budget_blocker(metrics: Option<&BfclJetsonRuntimeMetrics>) -> Option<String> {
    let metrics = metrics?;
    if let Some(memory_mb) = metrics.unified_memory_mb
        && memory_mb > 7500.0
    {
        return Some(format!(
            "unified memory {memory_mb:.0} MB exceeds ~7.5 GB practical headroom"
        ));
    }
    if let Some(first_token_ms) = metrics.first_token_ms
        && first_token_ms > 4000.0
    {
        return Some(format!(
            "first-token latency {first_token_ms:.0} ms exceeds 4 s interactive budget"
        ));
    }
    if let Some(tool_response_ms) = metrics.tool_response_ms
        && tool_response_ms > 8000.0
    {
        return Some(format!(
            "tool-response latency {tool_response_ms:.0} ms exceeds 8 s tool-turn budget"
        ));
    }
    None
}

pub fn format_head_to_head_table(comparison: &BfclHeadToHeadComparison) -> String {
    let baseline_col = format!("{} (baseline)", comparison.baseline_label);
    let candidate_col = comparison.candidate_label.clone();
    let metric_width = comparison
        .rows
        .iter()
        .map(|row| row.metric.len())
        .max()
        .unwrap_or(20)
        .max(20);
    let baseline_width = baseline_col.len().max(12);
    let candidate_width = candidate_col.len().max(12);

    let mut out = String::new();
    out.push_str(&format!(
        "{:<metric_width$} | {:<baseline_width$} | {:<candidate_width$}\n",
        "Metric", baseline_col, candidate_col,
        metric_width = metric_width,
        baseline_width = baseline_width,
        candidate_width = candidate_width
    ));
    out.push_str(&format!(
        "{:-<metric_width$}-+-{:-<baseline_width$}-+-{:-<candidate_width$}-\n",
        "", "", "",
        metric_width = metric_width,
        baseline_width = baseline_width,
        candidate_width = candidate_width
    ));
    for row in &comparison.rows {
        out.push_str(&format!(
            "{:<metric_width$} | {:<baseline_width$} | {:<candidate_width$}\n",
            row.metric, row.baseline, row.candidate,
            metric_width = metric_width,
            baseline_width = baseline_width,
            candidate_width = candidate_width
        ));
    }
    out.push('\n');
    out.push_str(&format!("Recommendation: {}\n", comparison.recommendation));
    out
}

fn metric_row(metric: &str, baseline: &str, candidate: &str) -> BfclHeadToHeadRow {
    BfclHeadToHeadRow {
        metric: metric.to_string(),
        baseline: baseline.to_string(),
        candidate: candidate.to_string(),
    }
}

fn format_accuracy(rate: f64) -> String {
    format!("{:.2}%", rate * 100.0)
}

fn format_optional_f64(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}"))
        .unwrap_or_else(|| "— (record on Jetson)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::bfcl::{BfclCase, score_cases};

    fn sample_report(strict_matches: usize, total: usize) -> BfclReport {
        let cases = (0..total)
            .map(|idx| BfclCase {
                id: format!("case-{idx}"),
                category: None,
                source: None,
                prompt: "turn on kitchen light".to_string(),
                expected_tool_calls: vec![],
                allow_extra_arguments: false,
            })
            .collect::<Vec<_>>();
        let predictions = cases
            .iter()
            .enumerate()
            .map(|(idx, case)| super::super::bfcl::BfclPrediction {
                id: case.id.clone(),
                response: if idx < strict_matches {
                    "No tool is required for this utterance.".to_string()
                } else {
                    r#"{"tool":"home_control","arguments":{}}"#.to_string()
                },
            })
            .collect::<Vec<_>>();
        score_cases(&cases, &predictions)
    }

    fn sample_record(label: &str, strict_matches: usize, total: usize) -> BfclEvalRunRecord {
        build_eval_run_record(
            label,
            "qwen",
            4096,
            "mock",
            None,
            None,
            total,
            strict_matches,
            None,
            None,
            sample_report(strict_matches, total),
        )
    }

    #[test]
    fn recommends_qwen_when_candidate_does_not_beat_baseline() {
        let baseline = sample_record("qwen3-4b-4096", 2, 10);
        let candidate = sample_record("nemotron-h-4b-16k", 1, 10);
        let text = recommend_hybrid_vs_qwen(&baseline, &candidate);
        assert!(text.contains("Keep Qwen3-4B @ 4096"));
    }

    #[test]
    fn compare_table_includes_accuracy_rows() {
        let baseline = sample_record("qwen3-4b-4096", 2, 10);
        let candidate = sample_record("nemotron-h-4b-16k", 4, 10);
        let comparison = compare_eval_runs(&baseline, &candidate);
        let table = format_head_to_head_table(&comparison);
        assert!(table.contains("BFCL strict accuracy"));
        assert!(table.contains("40.00%"));
        assert!(table.contains("Recommendation:"));
    }

    #[test]
    fn profile_ids_parse_aliases() {
        assert_eq!(
            BfclEvalProfileId::parse("nemotron-16k"),
            Some(BfclEvalProfileId::Nemotron16k)
        );
        assert_eq!(
            BfclEvalProfileId::parse("qwen"),
            Some(BfclEvalProfileId::Qwen4096)
        );
    }
}
