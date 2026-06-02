#!/usr/bin/env bash
# Issue #376: run Qwen@4096 vs Nemotron-H@16k BFCL arms and print bfcl-compare.
# Requires genie-ai-runtime on :8080 and HA cases at CASES_JSONL.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CASES_JSONL="${CASES_JSONL:-$ROOT/tests/bfcl/local/ha_home_cases.jsonl}"
OUT_DIR="${OUT_DIR:-$ROOT/tests/bfcl/local}"
GENIE_CTL="${GENIE_CTL:-cargo run -q -p genie-ctl --}"

if [[ ! -f "$CASES_JSONL" ]]; then
  echo "Missing cases: $CASES_JSONL" >&2
  echo "Generate with bfcl-import-ha-intents (see doc/bfcl-mamba-hybrid-evaluation.md)." >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

echo "== BFCL arm: qwen4096 (ensure Qwen3-4B is loaded in genie-ai-runtime) =="
read -r -p "Press Enter when the Qwen runtime is ready..."
$GENIE_CTL bfcl-score-llm \
  --profile qwen4096 \
  --cases "$CASES_JSONL" \
  --out "$OUT_DIR/ha_qwen4096_predictions.jsonl" \
  ${QWEN_METRICS:+--runtime-metrics "$QWEN_METRICS"} \
  --json >"$OUT_DIR/ha_qwen4096_report.json"

echo "== BFCL arm: nemotron16k (switch runtime to Nemotron-H 4B @ 16k) =="
read -r -p "Press Enter when the Nemotron hybrid runtime is ready..."
$GENIE_CTL bfcl-score-llm \
  --profile nemotron16k \
  --cases "$CASES_JSONL" \
  --out "$OUT_DIR/ha_nemotron16k_predictions.jsonl" \
  ${NEMOTRON_METRICS:+--runtime-metrics "$NEMOTRON_METRICS"} \
  --json >"$OUT_DIR/ha_nemotron16k_report.json"

echo "== Head-to-head =="
$GENIE_CTL bfcl-compare \
  --baseline-report "$OUT_DIR/ha_qwen4096_report.json" \
  --candidate-report "$OUT_DIR/ha_nemotron16k_report.json"
