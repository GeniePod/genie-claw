# BFCL Tool-Call Fixtures

These JSONL fixtures are a small GenieClaw-specific BFCL-style suite for local
tool-call accuracy. They are designed to run on the NVIDIA Jetson Orin 8GB path
without a live home backend: the scorer parses model responses and checks tool
names plus JSON arguments, but never executes tools.

This is the current focus. BFCL is the measurable gate for quick-router and
local-LLM tool-call accuracy. Everything else is noise until GenieClaw reliably
chooses the right typed tool with the right compact arguments inside the Jetson
4096-token constraint.

## Committed Fixture Smoke Test

Run the committed fixture and prediction files:

```bash
cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/home_tool_cases.jsonl \
  --predictions tests/bfcl/home_tool_predictions.jsonl
```

## Generate Home Assistant Intents Cases

To generate a local Home Assistant Intents-derived suite without committing raw
public data:

```bash
git clone --depth 1 https://github.com/OHF-Voice/intents tests/bfcl/local/ha-intents
cargo run -p genie-ctl -- bfcl-import-ha-intents \
  --source tests/bfcl/local/ha-intents \
  --out tests/bfcl/local/ha_home_cases.jsonl \
  --language en \
  --limit 1000
```

Use `--language all` for a larger multilingual suite.

## Quick-Router BFCL

The quick-router path is deterministic and does not call a model. This isolates
GenieClaw's rule-based fast path for common home commands.

```bash
cargo run -p genie-ctl -- bfcl-predict-quick \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_home_predictions.jsonl
```

Then score it:

```bash
cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --predictions tests/bfcl/local/ha_home_predictions.jsonl
```

## Local-LLM BFCL

The local-LLM path calls the configured `[services.llm]` backend directly and
writes raw model responses as BFCL predictions. It does not execute tools and
does not require a live home backend. Use it to measure how the local model,
prompt, runtime, and typed-tool schema behave together.

Start or verify the local LLM runtime first, then run:

```bash
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml \
cargo run -p genie-ctl -- bfcl-score-llm \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_home_llm_predictions.jsonl \
  --max-tokens 160
```

This generates local-model predictions, scores them immediately, and optionally
writes the raw prediction JSONL through `--out`.

Use the older two-step path when you already have predictions or want to inspect
raw model output before scoring:

```bash
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml \
cargo run -p genie-ctl -- bfcl-predict-llm \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_home_llm_predictions.jsonl \
  --max-tokens 160

cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --predictions tests/bfcl/local/ha_home_llm_predictions.jsonl
```

For a short Jetson smoke test before a longer run:

```bash
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml \
cargo run -p genie-ctl -- bfcl-score-llm \
  --cases tests/bfcl/local/ha_home_cases.jsonl \
  --out tests/bfcl/local/ha_home_llm_smoke_predictions.jsonl \
  --limit 25 \
  --max-tokens 160
```

`bfcl-score-llm` and `bfcl-predict-llm` default to JSON response mode. If a
local runtime rejects OpenAI-compatible `response_format`, add `--no-json-mode`
and score the raw responses anyway.

## Current Progress Baseline

Latest operator-reported local run on 2026-05-29:

```text
generated 208 BFCL cases from 475 Home Assistant Intents sentence templates
generated 208 quick-router BFCL predictions
tool calls: 35

BFCL tool-call score
cases:               208
parse_accuracy:      16.83%
tool_name_accuracy:  16.35%
argument_accuracy:   5.77%
strict_accuracy:     5.77%
missing_predictions: 0
failures:            196
```

Interpretation: the scorer/importer path is working, prediction coverage is
complete, and the current deterministic quick-router baseline is intentionally
low on the broader Home Assistant Intents suite. The next progress target is
not larger prompts; it is improving deterministic intent routing, entity/slot
normalization, and typed-tool argument accuracy for high-signal home commands.

The fixture format is intentionally plain:

- `home_tool_cases.jsonl`: one case per line with `id`, `prompt`,
  `expected_tool_calls`, and optional `allow_extra_arguments`.
- `home_tool_predictions.jsonl`: one model response per line with matching
  `id` and `response`.
- Public-dataset-derived cases may also include `source` metadata with
  `dataset`, `url`, `license`, `citation`, `derived_from`, and `notes`.

The first suite covers every static built-in tool name from `ToolDispatcher`,
including home/device calls, memory read/write/diagnostic tools, timers,
weather/search, calculations, media, no-tool responses, multi-tool responses,
and OpenAI-compatible `tool_calls` output. Dynamic native skill tools are loaded
at runtime, so each installed skill should add its own BFCL fixture.

For local stress testing, put large generated suites under `tests/bfcl/local/`.
That directory is gitignored on purpose. A useful local run shape is:

```bash
cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/local/long_home_tool_cases.jsonl \
  --predictions tests/bfcl/local/long_home_tool_predictions.jsonl
```

Public data imports must follow [doc/evaluation-data.md](../../doc/evaluation-data.md).
Do not commit raw public datasets, noncommercial-only audio, private household
facts, secrets, or large generated suites. Committed public-derived fixtures
need license and attribution metadata.
