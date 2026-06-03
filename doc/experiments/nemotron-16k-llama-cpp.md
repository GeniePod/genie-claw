# Experiment: 16k context, llama.cpp backend, Nemotron-4B Q4 (issue #376)

Evaluate whether a larger **16k context with Nemotron-4B (Q4)** on the
`llama.cpp` backend changes tool-call accuracy / behavior versus the 4096-token
Qwen baseline, on Jetson Orin Nano 8 GB.

This is an **opt-in experiment.** The product default stays the 4096-token
`genie-ai-runtime` + Qwen path. It intentionally exceeds the Jetson baseline, so
the boot harness will log `jetson_baseline_context: fail` and `/api/runtime/contract`
will report the larger context — that is **expected** for the experiment, not a
regression. Nothing in the repo defaults changes.

## Prerequisites

- `llama-server` installed at `/opt/geniepod/bin/llama-server` (the legacy backend
  path; see `setup-jetson.sh`).
- A **Nemotron-4B Q4_K_M GGUF** on the Jetson — e.g. NVIDIA *Nemotron-Mini-4B-Instruct*.
  There is no auto-download for nemotron yet, so fetch a GGUF quant and copy it:
  ```bash
  # from a dev machine
  scp nemotron-mini-4b-instruct-q4_k_m.gguf aihpc@<jetson>:/tmp/
  ssh aihpc@<jetson> 'sudo mv /tmp/nemotron-mini-4b-instruct-q4_k_m.gguf \
      /opt/geniepod/models/nemotron-mini-4b-instruct-q4_k_m.gguf'
  ```

## Steps (on the Jetson)

1. **Point llama-server at Nemotron + 16k context** via `/etc/geniepod/llm.env`
   (template: `deploy/config/llm.env.example`):
   ```ini
   GENIEPOD_LLM_MODEL=/opt/geniepod/models/nemotron-mini-4b-instruct-q4_k_m.gguf
   GENIEPOD_LLM_CTX_SIZE=16384
   ```
   `genie-llm.service` reads these via `EnvironmentFile=-/etc/geniepod/llm.env`, so
   no unit edit and no drop-in (which `setup-jetson.sh` would wipe).

2. **Switch the agent to the llama.cpp backend + a 16k budget** in
   `/etc/geniepod/geniepod.toml`:
   ```toml
   [agent]
   context_window_tokens = 16384   # intentionally above the 4096 baseline, this experiment only

   [core]
   llm_model_name = "nemotron-4b"  # selects the Nemotron prompt family

   [services.llm]
   backend = "llama_cpp"
   systemd_unit = "genie-llm.service"
   ```

3. **Restart:**
   ```bash
   sudo systemctl daemon-reload
   sudo systemctl restart genie-llm.service
   sudo systemctl restart genie-core.service
   ```

4. **Confirm:** `genie-ctl status` shows the `llama.cpp` backend; `journalctl -u
   genie-llm` shows `--ctx-size 16384` and the nemotron model loading. Watch
   `tegrastats` for memory pressure / OOM during load.

## Measure (vs the Qwen 4096 baseline)

- **BFCL tool-call accuracy** on the same fixtures:
  `genie-ctl bfcl-predict-llm` then `genie-ctl bfcl-score-llm` (baseline: 5.77%
  strict on the 208 HA-Intents quick-router cases).
- First-token + tool-response **latency**, and **unified-memory footprint** at
  16k (`tegrastats`).
- Whether nemotron-4b @ 16k **loads and serves without OOM** on the 8 GB pool —
  note that the unit comment flags a `flash-attn` + large-ctx instability on
  Tegra/aarch64 CUDA; if it crashes, retry with a smaller ctx (e.g. 8192) or
  `--flash-attn off`.

Record results on **issue #376**.

## Revert

Remove `/etc/geniepod/llm.env` (or unset the vars), restore the 4096
`genie-ai-runtime` defaults in `geniepod.toml`, and restart — or just re-run
`setup-jetson.sh`, which keeps the unit's 2048 default.
