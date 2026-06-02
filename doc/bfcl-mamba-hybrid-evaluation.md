# BFCL: Qwen @ 4096 vs Mamba-2 hybrid @ 16k (issue #376)

This document is the operator runbook for [GeniePod/genie-claw#376](https://github.com/GeniePod/genie-claw/issues/376). It compares the M1 Jetson baseline (**Qwen3-4B Q4_K_M**, 4096-token harness) against an opt-in **Nemotron-H (Mamba-2 hybrid)** profile at **16384** context. The product default in `deploy/config/geniepod.toml` stays on Qwen @ 4096 until the hybrid path wins on BFCL accuracy **and** fits the 8 GB memory/latency budget.

## Profiles (config overlays)

| Profile | Config overlay | `llm_model_name` | `context_window_tokens` |
| --- | --- | --- | --- |
| `qwen4096` | `deploy/config/geniepod.bfcl-qwen4096.jetson.toml` | `qwen` | 4096 |
| `nemotron16k` | `deploy/config/geniepod.bfcl-nemotron16k.jetson.toml` | `nemotron-4b` | 16384 |

Copy overlays to `/etc/geniepod/` on device images, or point `GENIEPOD_CONFIG` at the repo paths during development.

**Prerequisites**

- `genie-ai-runtime` serving the correct GGUF locally (no remote providers).
- Nemotron path: runtime must support the Mamba-2 hybrid architecture and 16k context within unified memory (owned by the `genie-ai-runtime` repo).
- HA-Intents BFCL cases generated locally (208 English cases is the M1 baseline size).

## 1. Generate HA-Intents cases (once per machine)

```bash
git clone --depth 1 https://github.com/OHF-Voice/intents tests/bfcl/local/ha-intents
cargo run -p genie-ctl -- bfcl-import-ha-intents \
  --source tests/bfcl/local/ha-intents \
  --out tests/bfcl/local/ha_home_cases.jsonl \
  --language en \
  --limit 1000
```

Also score the committed typed-tool fixture:

```bash
cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/home_tool_cases.jsonl \
  --predictions tests/bfcl/home_tool_predictions.jsonl
```

## 2. Baseline arm — Qwen @ 4096

Ensure `genie-ai-runtime` serves `Qwen3-4B-Q4_K_M.gguf`, then:

```bash
cargo run -p genie-ctl -- bfcl-score-llm \
  --profile qwen4096 \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_qwen4096_predictions.jsonl \
  --json > tests/bfcl/local/ha_qwen4096_report.json
```

Optional Jetson metrics sidecar (fill after measuring on device):

```json
{
  "unified_memory_mb": 6200,
  "first_token_ms": 850,
  "tool_response_ms": 2100,
  "notes": "tegrastats + single bfcl case timing"
}
```

Re-run with `--runtime-metrics /path/to/qwen-metrics.json` to embed measurements in the report JSON.

## 3. Candidate arm — Nemotron-H @ 16k

Switch the active LLM systemd unit to `nemotron-4b-q4_k_m.gguf` (see `deploy/setup-jetson.sh` and `ARCHITECTURE.md`), restart `genie-ai-runtime`, confirm 16k context loads under 8 GB, then:

```bash
cargo run -p genie-ctl -- bfcl-score-llm \
  --profile nemotron16k \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_nemotron16k_predictions.jsonl \
  --json > tests/bfcl/local/ha_nemotron16k_report.json
```

Record the same runtime metrics JSON for the hybrid run.

## 4. Head-to-head table + recommendation

```bash
cargo run -p genie-ctl -- bfcl-compare \
  --baseline-report tests/bfcl/local/ha_qwen4096_report.json \
  --candidate-report tests/bfcl/local/ha_nemotron16k_report.json
```

`bfcl-compare` prints the issue #376 deliverable table (accuracies + optional Jetson metrics) and a recommendation string. Use `--json` for machine-readable output.

### Deliverable template

| Metric | Qwen3-4B @ 4096 | Mamba-2 hybrid @ 16k |
| --- | --- | --- |
| BFCL strict accuracy | 5.77% (quick-router ref.) / measured | measured |
| tool-name / argument / parse accuracy | measured | measured |
| unified-memory footprint | measured | measured |
| first-token / tool-response latency | measured | measured |

Fill “measured” cells from the two `bfcl-score-llm --json` reports and Jetson metrics sidecars. The quick-router **5.77%** figure on 208 HA-Intents cases is the deterministic routing baseline documented in `tests/bfcl/README.md`; local-LLM numbers will differ.

## 5. Jetson measurement hints

- **Unified memory:** `tegrastats` or governor memory samples while the model is loaded at the evaluation context length.
- **First-token latency:** time one `bfcl-predict-llm --limit 1` case or a minimal `/v1/chat/completions` request.
- **Tool-response latency:** time a representative BFCL case end-to-end (same as one prediction row).

Budget guardrails used by `bfcl-compare` recommendations (conservative):

- Unified memory ≤ ~7.5 GB practical headroom on Orin Nano 8 GB.
- First token ≤ 4 s; tool-response turn ≤ 8 s.

## Automation

`deploy/scripts/bfcl-issue-376-jetson.sh` runs both profile arms and `bfcl-compare` when cases and runtime are ready. It does not switch GGUF weights; operators must point `genie-ai-runtime` at the correct model before each arm.

## Out of scope (per issue)

- Promoting 16k or the hybrid to the product default before a justified win.
- Remote/hosted LLM providers.
- `genie-ai-runtime` CUDA/Mamba serving work (separate repo).
