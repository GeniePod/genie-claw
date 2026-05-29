# BFCL Tool-Call Fixtures

These JSONL fixtures are a small GenieClaw-specific BFCL-style suite for local
tool-call accuracy. They are designed to run on the NVIDIA Jetson Orin 8GB path
without a live home backend: the scorer parses model responses and checks tool
names plus JSON arguments, but never executes tools.

Run:

```bash
cargo run -p genie-ctl -- bfcl-score \
  --cases tests/bfcl/home_tool_cases.jsonl \
  --predictions tests/bfcl/home_tool_predictions.jsonl
```

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

To create a deterministic baseline prediction file from GenieClaw's current
quick router:

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
