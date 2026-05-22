# Changelog

## Unreleased

- **Real streaming TTS** (#26): the voice loop now detects sentence
  boundaries inside the LLM streaming callback and forwards completed
  sentences to a concurrent TTS task immediately, instead of waiting
  for the full response before speaking. First sentence reaches the
  speaker as soon as the LLM finishes emitting it (typically 1-3 s
  after wake) rather than after the full response (~5-17 s). New
  `SentenceStreamer` strips inline markdown / URLs / list markers /
  fenced code blocks per-sentence, merges sub-8-char openers with the
  next clause, and caps spoken output at 3 sentences to match
  `format::for_voice`. `TtsEngine` is shared across the producer and
  consumer halves via `Arc<TtsEngine>`. The first-reply latency banner
  (#19) now reports the true LLM-until-first-sentence figure instead
  of the full-response figure.
- **Voice-cycle integration test + mockable surface** (#21). Extracts
  `voice_loop::process_transcript` from `voice_cycle` so the
  post-record orchestration (intent gate, speaker identity, memory
  recall, quick-tool fast path, LLM streaming + TTS, tool dispatch,
  conversation persistence, latency banner, memory extract) can be
  driven end-to-end with mocks. Adds `LlmClient::mock(replies)` (new
  `MockLlmBackend` implementation of the existing `LlmBackendClient`
  trait, replays scripted replies on both `chat` and `chat_stream`,
  optional `with_fallback("…")`), `SttEngine::mock(transcripts)` (new
  `SttMode::Mock` variant with a `MockTranscript` queue;
  `transcribe_file` pops the next scripted transcript), and
  `TtsEngine::silent()` (new `TtsMode::Silent` variant; `speak`,
  `synthesize`, `synthesize_to_file`, `start`, and `stop` all no-op).
  `TtsEngine::snapshot()` returns a config-only copy so a borrowed
  `&TtsEngine` can be wrapped in `Arc<TtsEngine>` for
  `streaming::stream_and_speak`. New integration test at
  `crates/genie-core/tests/voice_loop_integration.rs` drives
  `process_transcript` with these mocks and asserts on transcript
  flow, the conversation store, and the dispatcher's tool-audit
  JSONL.
- **Parallel-safe SQLite test paths** (#21). The memory test helper
  now gives every test a `${tmpdir}/geniepod-mem-${label}-${pid}-${id}-${nanos}/`
  parent directory; the DB lives at `<dir>/memory.db` and
  `Memory::open` derives `canonical_dir = <dir>/memory`, so the
  markdown promotion pipeline (`MEMORY.md`, `namespaces/*/preference.md`,
  `events/*.jsonl`) is per-test instead of shared. Fixes the
  `promotion_redacts_person_memory_in_namespace_note` flake the
  issue calls out. The two bespoke-path memory tests
  (`evergreen_memories_dont_decay`,
  `open_backfills_policy_columns_for_existing_rows`) flow through
  the same `temp_memory_path("label")` helper.
- **`tools::parser` Linux-only test gate** (#21). The
  `try_tool_call_executes_single_key_system_info_shape` test asserts
  the rendered `system_info` tool output contains `Memory available:`,
  which the production tool only emits on Linux (the line comes from
  `tegrastats::mem_available_mb()` reading `/proc/meminfo`). Gated
  behind `#[cfg(target_os = "linux")]` so macOS CI no longer flags a
  false negative.

### Changed

- `deploy/scripts/genie-restart-all.sh` rewritten as a full hard-reset:
  delegates to `stop_all.sh` for the systemd stops, best-effort
  `pkill -x` reaps known LLM/STT/TTS/audio subprocess names that may
  have survived the cgroup stop (piper, whisper-server, whisper-cli,
  jetson-llm-server, jetson-llm, llama-server, deep-filter, sox,
  ffmpeg), `sync; echo 3 > /proc/sys/vm/drop_caches` releases page
  cache, `swapoff -a; swapon -a` flushes the swap file to a clean
  baseline, then delegates to `start_all.sh` to bring the stack back
  up. Deliberately gives back the warm Qwen3-4B page-cache residency
  PR #70 preserves across plain `systemctl restart` — the script
  exists for the post-`make deploy` case where binaries / config /
  model path may have changed and the prior warm cache is stale.
  Pass `--soft` to skip the cache + swap reset for a service-only
  refresh that preserves the warm LLM cache. `swapoff` failure
  (no swap, or not enough free RAM to absorb the swap contents) is
  logged and skipped rather than fatal so the script never wedges
  the box mid-restart. New regression test
  `genie_restart_all_hard_mode_performs_full_memory_reset` pins
  the five-step shape (stop → reap → drop_caches → swapoff/swapon
  → start) and the ordering, so future edits can't quietly drop a
  step without failing CI.

### Added

- `CONTRIBUTING.md`, `SECURITY.md`, `.github/PULL_REQUEST_TEMPLATE.md`,
  and `.github/workflows/contribution.yml` — formal contribution guide
  + private-disclosure security policy + PR template +
  `Contribution / PR body checklist` CI job (triggered via
  `pull_request_target` so the check runs from the base branch's
  workflow definition — fires on every PR regardless of whether the
  PR head pre-dates the workflow). Checklist also blocks PR bodies
  that include AI-attribution footers like `🤖 Generated with Claude
  Code` (case-insensitive, matches the bracketed-link form as well)
  to keep PR attribution with the human contributor; same spirit as
  the existing no-`Co-Authored-By: Claude` commit-trailer rule. Quality / engineering /
  bug-fix contributions are explicitly welcomed; every PR must include
  a `## Real Behavior Proof` section in the body (CI enforces structure,
  reviewer reads the content) so reviewers can see what was actually
  run and where, not just what CI checked. Security disclosures go to
  <contact@genieclaw.org> privately rather than the public issue tracker;
  scope, in-scope/out-of-scope categories, and response timeline are
  documented in `SECURITY.md`. Dependabot / Renovate / release PRs
  are exempt from the proof requirement via a title-prefix allowlist
  in the checklist workflow. README's bottom-of-file gets a brief
  "Contributing" + "Security" pair of sections pointing at the
  canonical docs.

## 1.0.0-alpha.9 - 2026-05-18

Alpha 9 is the **CI / supply-chain hardening + voice-frontend maturation**
release. It absorbs every change that landed on `main` between the
`v1.0.0-alpha.5` tag and today — the never-tagged narrative milestones
referenced as "alpha.6" (GPU contention / LLM warmup fixes), "alpha.7"
(DeepFilterNet capture, half-duplex post-TTS gate, first-reply latency
banner) and "alpha.8" (LLM backend abstraction, telegram voice, voice
optional) all roll up here.

Headlines:

- **Verified voice cycle**: ~4 s first reply (285 ms STT + 3679 ms LLM →
  first audio) on Orin Nano + LyraT V4.3 + Phi-4-mini Q4_K_M, with a
  first-reply latency banner that prints the 5-phase breakdown so
  regressions are visible at a glance.
- **LLM backend is now a config-driven facade** (#32, #35, #38, #39, #40,
  #43) — `[llm.backend]` selects `llama-cpp` or `genie-ai-runtime`. The
  v1.0.0 `genie-ai-runtime` install pipeline ships in #56, and the
  Jetson deploy default flipped to `genie-ai-runtime` + Qwen3-4B Q4_K_M
  in #55 (closes #52). `llama.cpp` + Phi-4-mini remain the one-line
  fallback via `[services.llm].backend = "llama_cpp"`.
- **Voice is now an opt-out Cargo feature** (#41, #57). Default builds
  are byte-identical for Jetson; `--no-default-features` produces a
  chat-only binary that compiles on macOS / Windows without ALSA.
- **Telegram voice ingestion** (#42, #53) — bot users can send voice
  notes and get a spoken (or text) reply on the same conversation
  path as the mic-array loop.
- **CI pipeline** (#34): fmt + clippy + test (#37), aarch64 Jetson
  cross-compile (#49), cargo-audit + cargo-deny supply-chain (#50),
  shellcheck + ruff for shell/Python (#51). All green on `main`.
- **Qwen3-4B Q4_K_M** is now the Jetson default model (#44, #46, #55);
  Phi-4-mini Q4_K_M remains as the explicit fallback via
  `setup-jetson.sh --model phi-4-mini`.

Workspace version bumped `1.0.0-alpha.5` → `1.0.0-alpha.9` across all
seven workspace crates.

### Added

- `.github/workflows/audit.yml` and `deny.toml` — supply-chain audit
  workflow for issue #34. Runs `rustsec/audit-check` plus
  `EmbarkStudios/cargo-deny-action` on every `Cargo.{toml,lock}` /
  `deny.toml` / workflow change and on a Monday-06:00-UTC cron.
  `deny.toml` codifies the project's license policy
  (`AGPL-3.0-only` for genie-claw itself; the standard permissive
  set plus `CDLA-Permissive-2.0` for webpki-roots and `OpenSSL` for
  ring on the dependency side), pins all packages to crates.io as the
  only allowed source so a future git dep requires an explicit policy
  update, and currently runs `wildcards = "allow"` because workspace
  path-deps would otherwise trip the public-crate exemption logic.
  Audit badge added at the top of the README.
- `.github/workflows/cross.yml` — aarch64 Jetson cross-compile workflow
  for issue #34. Installs `gcc-aarch64-linux-gnu` /
  `g++-aarch64-linux-gnu`, adds the `aarch64-unknown-linux-gnu` Rust
  target, then runs the same two-step `cargo build --release --locked
  --target aarch64-unknown-linux-gnu` recipe `make jetson` uses (with
  `CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc` and the matching
  `AR_` override) to produce `genie-core`, `genie-ctl`, `genie-governor`,
  `genie-health`, and `genie-api`. A post-build verify step asserts
  each binary is in fact an ELF tagged `ARM aarch64` before uploading
  them as the `genie-jetson-aarch64-${{ github.sha }}` artifact with a
  14-day retention. Catches the cross-compile breakage that previously
  only surfaced at `make jetson` / `make deploy` time. Jetson badge added
  at the top of the README.
- Opt-in Qwen3-4B model download in `setup-jetson.sh` (issue #44, Phase 1).
  `deploy/setup-jetson.sh --model qwen3-4b` fetches
  `Qwen3-4B-Q4_K_M.gguf` from `Qwen/Qwen3-4B-GGUF` into
  `/opt/geniepod/models/`. `--model phi-4-mini` is also accepted as an
  explicit form of today's default. The flag only changes the download
  target; it does not rewrite `llm_model_path` in
  `/etc/geniepod/geniepod.toml`, so existing Phi-4-mini deployments stay
  on Phi-4-mini until the operator flips the config line by hand. The
  recommended pairing is Qwen3-4B + `genie-ai-runtime` once both are
  installed — see the new "Recommended LLM Pairing" section in README.
  `geniepod.toml` carries commented examples for both
  `llm_model_name = "qwen"` and the matching `llm_model_path`. Regression
  tests in `prompt.rs` lock the `Qwen3-4B-Q4_K_M.gguf` filename to
  `ModelFamily::Qwen` so a future detector refactor cannot silently drop
  it into the small-model prompt shape. The default flip ships in Phase 2
  alongside the genie-ai-runtime default flip in issue #33.
- `.github/workflows/ci.yml` — the fmt + clippy + test daily loop for
  issue #34 (PR #37). Runs `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`, and
  `cargo test --workspace --locked` (unit, integration, and doc tests) on
  every push to `main` and every pull request. Each job uses
  `Swatinem/rust-cache` with a per-job shared key so cached runs stay
  short. Concurrency group cancels superseded runs on the same ref. CI
  badge added at the top of the README. Bundles a `rustls-webpki`
  0.103.12 → 0.103.13 lockfile bump for [RUSTSEC-2026-0104](https://rustsec.org/advisories/RUSTSEC-2026-0104)
  (reachable panic in CRL parsing on the transitive HTTPS path via
  `reqwest → hyper-rustls → rustls → rustls-webpki`), plus
  `temp_memory` / `make_governor` test-isolation fixes
  (`genie-core/src/memory/mod.rs`, `genie-governor/src/governor.rs`)
  required for the new `cargo test --workspace` job to be stable under
  parallel execution.
- `voice` Cargo feature on `genie-core` and `genie-ctl` (issue #41).
  Default-on so `cargo build` produces today's Jetson-targeted binary
  unchanged. `cargo build -p genie-core --no-default-features` (and the
  matching `-p genie-ctl`) produces a chat-only binary that drops the
  STT/TTS/AEC/wakeword pipeline, the `VoiceOrchestrator`, the
  `voice_loop::run` dispatcher, and `genie-ctl`'s `speaker` subcommand:
    - `pub mod voice;` and `pub mod voice_loop;` in
      `crates/genie-core/src/lib.rs` are now `#[cfg(feature = "voice")]`.
    - The voice-mode branch in `crates/genie-core/src/main.rs` is gated;
      when voice is requested (`--voice` / `GENIEPOD_VOICE=1` /
      `core.voice_enabled = true`) on a chat-only build, the runtime
      logs one warning and falls through to the existing chat / HTTP
      path so deploying an unchanged `geniepod.toml` is a non-event.
    - `genie-ctl`'s `genie-core` dependency uses
      `default-features = false`; `genie-ctl`'s own new `voice` feature
      forwards to `genie-core/voice`. The `speaker` subcommand,
      `speaker` help line, all `cmd_speaker_*` helpers, and the two
      `parse_speaker_options` unit tests are `#[cfg(feature = "voice")]`.
      Invoking `genie-ctl speaker …` on a chat-only build exits with a
      clear "rebuild with --features voice" message instead of crashing.
  Knock-on cleanups so `cargo clippy -- -D warnings` is green on both
  variants: `local_http_host` and its two unit tests in
  `crates/genie-core/src/main.rs` are now `#[cfg(feature = "telegram")]`
  (they are only used by the Telegram adapter and were latent dead code
  on no-telegram builds); the `std::process::Command` import in
  `crates/genie-ctl/src/main.rs` is `#[cfg(feature = "voice")]` because
  the only `std::process::Command::new` call lives in `record_speaker_wav`.
  Release binary on x86_64-linux drops from 4.8 MB to 4.6 MB without
  voice; the bigger payoff is unblocking macOS / Windows hosts that
  previously could not compile the ALSA-coupled voice modules.
  CI matrix coverage (acceptance criterion #8) will be added on top of
  issue #34's `ci.yml` workflow once that lands; the
  `cargo build / clippy / test` invocations to add are
  `-p genie-core -p genie-ctl --no-default-features` next to the
  existing `--workspace` ones.
- `.github/workflows/scripts.yml` and `ruff.toml` — shellcheck + ruff
  workflow for the stretch slice of issue #34. Discovers all
  tracked `*.sh` and `*.py` files via `git ls-files`, then runs
  `shellcheck --severity=warning` and `ruff check
  --output-format=github`. `ruff.toml` pins `target-version = "py310"`
  (Jetson Ubuntu 22.04) and ignores E402, with an inline comment
  explaining why: `deploy/scripts/genie-wake-listen.py` and
  `genie-wakeword.py` legitimately import after redirecting ALSA stderr
  to `/dev/null` so the C-level diagnostic noise from PyAudio doesn't
  leak into the protocol stdout. Trigger paths are scoped to `**.sh` /
  `**.py` / the workflow file itself so Rust-only changes don't spin up
  this job.
- First-voice-reply latency banner (issue #19). On the first completed voice
  cycle of a `genie-core` run, the loop prints a one-shot 5-phase breakdown
  from end-of-user-speech to first audible audio:
    - `preprocess (DFN+sox)`
    - `STT`
    - `LLM until first sentence`
    - `TTS first synth`
    - `speech end -> first audio` (total)
  Lets an operator see exactly which phase dominates a slow first reply —
  18 seconds of "STT done -> first audio" is almost always LLM cold-start,
  not Piper. Reference points are stamped by markers in `stt.rs`
  (`audio_captured_at`, after `arecord` finishes) and `tts.rs`
  (`first_speak_called_at` on the first speak entry, `first_audio_at`
  immediately before the first PCM byte hits `aplay`'s stdin).
- `genie-whisper-warmup.service` (issue #17) — oneshot systemd unit ordered
  `After=genie-whisper.service` that polls the whisper-server port and POSTs
  one second of synthesized silence to `/inference`. Forces the ggml-small
  weights and CUDA kernels into iGPU memory before the first user-visible
  voice cycle, eliminating the 60-90 s first-STT cold path observed on
  Orin Nano. Mirrors the existing `genie-llm-warmup.service` design from
  PR #7. Wired into `setup-jetson.sh`'s enable loop; failures are non-fatal
  (`|| true`) so a broken whisper does not block boot. Skips cleanly on
  hosts without `sox` or `whisper-server`.
- Half-duplex post-TTS gate to suppress speaker→mic acoustic echo (issue #15).
  `TtsEngine::speak()` now sleeps `post_tts_silence_ms` milliseconds after
  `aplay` exits. The ALSA hardware playback buffer continues draining for
  some time after `aplay` returns, and the room itself takes time to decay
  below the whisper-server no-speech threshold. Without the gate, the next
  cycle's mic capture picked up the assistant's own TTS and whisper happily
  transcribed it as the next user utterance — confirmed in the #14 chase
  on Jetson + LyraT + speakers in a shared room. Default 1500 ms; settable
  via `[core].post_tts_silence_ms` in `geniepod.toml`. Headphone / headset
  installs can drop it to 0.
- `aec::process_aec` now discards stale echo references — any TTS reference
  older than `TTS_duration + MAX_ECHO_TAIL_MS` (1.5 s) is dropped before
  NLMS runs. Push-to-talk recordings that happen after the room reverb has
  decayed should not be processed against an aged TTS reference; the
  previous behavior would convolve fresh user speech with old TTS PCM and
  introduce phantom artifacts.
- DeepFilterNet capture-side denoiser as the alpha.7 default (issue #12).
  `record_audio` now branches on a new `audio_denoiser` config knob with
  three backends: `"deepfilternet"` (neural; new default), `"sox"` (the
  alpha.6 baseline of spectral subtraction + compand), and `"none"`
  (bandpass + compand + normalize). The DFN chain runs as
  `sox(channels 1, highpass 100, lowpass 7000) → deep-filter --atten-lim N
  → sox(gain -n -3)`: bandpass first so DFN's STFT doesn't spend capacity
  on rumble/hiss bands whisper can't use, then DFN denoise (handles
  non-stationary noise — fans, typing, background voices — without
  needing a captured noise profile), then peak-normalize. Compand is
  intentionally dropped from the DFN chain because DFN's implicit gating
  preserves quiet phonemes better than a hard `-65 dBFS` compand gate.
  Any subprocess failure (binary missing, DFN crash, intermediate file
  empty) falls back to the sox chain at runtime, so a host without the
  binary still records cleanly via the alpha.6 path. New config fields
  `audio_denoiser`, `deep_filter_path`, `deep_filter_atten_lim_db`.
- `setup-jetson.sh` now installs the prebuilt
  `deep-filter-0.5.6-aarch64-unknown-linux-gnu` binary (~39 MB, MIT/Apache
  dual-licensed, DFN3 model statically linked via tract) into
  `/opt/geniepod/bin/deep-filter` when `audio_denoiser = "deepfilternet"`.
  The download is best-effort — failures leave the runtime fallback path
  in place rather than aborting setup. Skipped when the operator has
  pinned `audio_denoiser` to `"sox"` or `"none"`.

### Changed

- `voice_loop` now runs `stt::flush_mic_buffer` BEFORE printing the
  "Recording N seconds — speak now!" / "Listening for follow-up" prompt,
  rather than inside `record_audio` after the prompt. The flush is a 1 s
  throwaway capture that drains stale samples (TTS residue, DMA carry-over)
  between cycles. With the old ordering, the throwaway ran AFTER the user
  saw the prompt, so the first ~1 s of speech went into the discarded
  flush WAV — operators reported the opening of their commands being
  chopped off. New ordering: flush is silent during the brief gap between
  cycles, then prompt appears the instant arecord actually starts. Both
  the push-to-talk path and the continuous follow-up path are fixed.
- `record_audio`'s sox preprocessing chain now does dynamic-range
  compression with `compand 0.02,0.20 -50,-50,-25,-12,-5,-5 -2` before
  the final `gain -n -3` peak-normalize. The previous pipeline applied
  a single linear gain to satisfy peak-normalize: if a user's loudest
  syllable was at -5 dBFS and the quietest at -25 dBFS, BOTH got the
  same scalar boost and the quiet syllables stayed buried under whisper's
  hallucination threshold. With compand, quiet-speech input around
  -25 dBFS now maps to -12 dBFS (+13 dB lift) while loud peaks stay
  at -5 dBFS and the noise floor below -50 dBFS is NOT amplified.
  Compand attack/release of 20 ms / 200 ms matches speech syllable
  timing. Net effect on STT: whisper-small reaches whole-utterance
  intelligibility on quieter LyraT captures that previously
  produced assistant-stock hallucinations ("I'm here to help",
  "feel free to ask"). Closes #6.

### Added

- `genie-llm-warmup.service` — a oneshot systemd unit ordered `After=genie-llm.service`
  that polls `/health` and sends one tiny `/completion` request to force
  Phi-4-mini into iGPU memory before the first user-visible voice cycle.
  Without this the first voice interaction after boot would either block
  on the ~30-60 s cold model load or time out with `503: Loading model`.
  Wired into `setup-jetson.sh`'s enable loop so a fresh `make deploy` +
  reboot ends with the LLM already hot. Closes #3.

### Changed

- `genie-llm.service` now launches `llama-server` with a tighter context
  window (`--ctx-size 2048`, down from 4096). On Orin Nano's 7.6 GB iGPU
  this halves the KV-cache footprint and eases the eviction pressure that
  was pushing `whisper-server`'s model out of GPU memory during long LLM
  responses. Net effect: STT latency stops jumping from ~270 ms to ~3.6 s
  across consecutive voice cycles. 2048-token context is comfortable for
  command-style voice interactions (typical conversation history is well
  under 1k tokens). Closes #2.

  Quantized KV cache (`--cache-type-k q4_0 --cache-type-v q4_0`) was
  intended as an additional ~570 MB win but currently crashes
  `llama-server` with `GGML_ASSERT(ggml_is_contiguous(a)) failed` in
  `ggml_reshape_2d` when combined with `--flash-attn on` and the Phi-3/
  Phi-4 attention graph on aarch64 CUDA. Documented inline in the
  service unit; tracked upstream in llama.cpp.

### Changed (Jetson default-backend flip — PR #55, closes #52)

- Jetson deploy defaults flipped to `genie-ai-runtime v1.0.0` + Qwen3-4B
  Q4_K_M. `deploy/config/geniepod.toml` now ships with
  `[services.llm].backend = "genie_ai_runtime"`,
  `systemd_unit = "genie-ai-runtime.service"`, `llm_model_name = "qwen"`,
  and `llm_model_path = /opt/geniepod/models/Qwen3-4B-Q4_K_M.gguf`.
  `deploy/config/geniepod.dev.toml` stays on the `llama_cpp` path for
  local x86/macOS development (explicit `backend = "llama_cpp"` instead
  of relying on the workspace default). The
  `LlmBackendKind::default` accordingly flips from `LlamaCpp` to
  `GenieAiRuntime` in `crates/genie-common/src/config.rs`, and
  `Governor::llm_service_unit`'s fallback flips to
  `genie-ai-runtime.service` (`crates/genie-governor/src/governor.rs`).
- `deploy/setup-jetson.sh` now picks the LLM units to enable from the
  `[services.llm].backend` line in `geniepod.toml`: defaults to
  `genie-ai-runtime` + `genie-ai-runtime-warmup`, falls back to
  `genie-llm` + `genie-llm-warmup` when `backend = "llama_cpp"` (or
  `"llama-cpp"`). The "Start services" footer prints the matching unit.
  The Qwen3-4B Q4_K_M download is now the default model when invoked
  without `--model`; `--model phi-4-mini` selects the prior default
  as the explicit fallback. The cutover NOTE block from PR #46 still
  surfaces on `--model phi-4-mini` re-runs (predicate flipped from
  "not phi-4-mini" to "not qwen3-4b") with prompt-template /
  systemd-unit guidance updated for the new default. Runtime install
  remains opt-in via `--runtime genie-ai-runtime` (issue #54 / PR #56);
  the setup script now points operators at that flag when
  `jetson-llm-server` is missing instead of duplicating the build logic.
- `deploy/systemd/genie-core.service` now orders
  `After=… genie-ai-runtime.service genie-llm.service` with
  `Wants=… genie-ai-runtime.service`, so `genie-core` waits for whichever
  LLM unit the operator has enabled (the two units `Conflicts=` each
  other, so only one is active at a time and the unused `After=` entry
  is a no-op).
- `deploy/systemd/genie-governor.service` adds
  `/etc/systemd/system/genie-ai-runtime.service.d` to `ReadWritePaths=`
  so the governor's drop-in writer can land context-size adjustments
  on the new unit's config dir as well as the legacy
  `genie-llm.service.d`.
- `README.md` and `ARCHITECTURE.md` flip the "default vs selectable"
  wording for the LLM backend (genie-ai-runtime is the Jetson default;
  llama.cpp is the selectable fallback).

## 1.0.0-alpha.5 - 2026-05-11

Alpha 5 is the voice-frontend release. It takes GenieClaw from a chat/HTTP
appliance to a working **push-to-talk loop on Jetson hardware**: ESP32-LyraT
V4.3 microphones in over I2S, USB headphone out for TTS, `whisper.cpp` on
CUDA for sub-second STT, `llama.cpp` for the LLM, Piper for synthesis. The
deploy story is hermetic — one `make deploy` cross-compiles the aarch64
binaries on a Linux build host, ships everything via SSH to the Jetson, and
`setup-jetson.sh` audits voice prerequisites before enabling services. The
LyraT-on-Jetson install slice lives in `doc/lyrat-jetson-audio.md`; full
hardware bring-up (firmware, wiring, Jetson-IO overlay, byte-exact
verification) is in the companion `ai-hardware-engineer-roadmap` guide.

### Added

- Jetson APE/I2S2 audio frontend support. `genie-audio.service` now runs
  `/opt/geniepod/bin/genie-audio-init` at boot to configure the Tegra AHUB
  route (`ADMAIF1 Mux = I2S2`, I2S2 codec master mode, framing, channel,
  and bit-format controls) so an external I2S source on the Jetson 40-pin
  header — e.g. ESP32-LyraT V4.3 via JP4 — is surfaced through ALSA as
  `plughw:APE,0`. The script is idempotent, waits up to 30 s for the APE
  card to enumerate, and exits cleanly on hosts without the I2S2 overlay.
- `detect-audio-device.sh` now prefers `plughw:APE,0` when `ADMAIF1 Mux` is
  routed to `I2S2`, falling back to USB audio and then card 0.
- `genie-core::detect_audio_device` delegates to the deploy script when
  installed, so `audio_device = "auto"` works for both LyraT and USB users
  without touching `/etc/geniepod/geniepod.toml`.
- `doc/lyrat-jetson-audio.md` — GenieClaw-side install slice for the
  LyraT-on-Jetson audio frontend. Hardware bring-up (firmware, wiring,
  Jetson-IO overlay, byte-exact verification) lives in the
  `ai-hardware-engineer-roadmap` LyraT-Jetson guide; this page covers only
  the genie-claw integration, reboot persistence, and known limitations.
- `genie-whisper.service` — long-running `whisper-server` daemon so the
  Whisper model stays loaded in GPU memory across utterances. Per-call STT
  cost drops from ~1.5 s CUDA cold-start + inference to ~50 ms HTTP POST +
  inference. `genie-core` switches between CLI and server mode based on
  the new `whisper_port` field in `[core]` config (default `8178`, set to
  `0` to fall back to CLI mode). The `whisper-server` binary is built from
  `whisper.cpp` (build with `-DGGML_CUDA=ON -DCMAKE_CUDA_ARCHITECTURES=87`
  on Orin Nano) and lives at `/opt/geniepod/bin/whisper-server`.
- `setup-jetson.sh` now audits voice-runtime prerequisites (`whisper-cli`,
  `whisper-server`, whisper model, `piper`, piper voice + `.onnx.json`
  sidecar) against the paths in `[core]` config. Voice prereqs are not auto-downloaded — too
  large and license-sensitive — but the install script now surfaces what
  is missing with concrete install pointers instead of letting the first
  voice-loop invocation fail mysteriously. The `geniepod.target` symlink
  is also created so every `WantedBy=geniepod.target` service auto-starts
  on boot.
- New `[core].audio_output_device` config field. `genie-core` now uses
  separate ALSA devices for capture (`audio_device`) and playback
  (`audio_output_device`), so a LyraT-on-I2S2 input can pair with a USB
  headphone output. Both default to `"auto"` and resolve through
  `detect-audio-device.sh`; the helper now accepts an `--output` flag and
  uses different priority orders for each side.
- `genie-core` now passes the `language` form field to whisper-server in
  server mode (`SttEngine::transcribe_via_server`), so `stt_language` in
  the config actually reaches the server-mode decoder instead of being
  silently dropped. Combined with the new English-only default
  (`stt_language = "en"`), this measurably improved transcription accuracy.
- `record_audio` now captures stereo (`-c 2`) and downmixes to mono in the
  sox preprocessing stage. Works around a Tegra ALSA `plughw` timing bug
  where `-c 1` returns the requested mono frame count in roughly half the
  wall-clock time (interpreting stereo frames as paired mono samples
  instead of downmixing them), causing the recorder to capture only a
  fraction of the requested duration of real-world audio.
- `record_audio`'s sox chain now does `channels 1 -> highpass 100 -> lowpass
  7000 -> gain -n -3`. Band-passing the speech band before peak-normalize
  prevents the ES8388 ADC's high-frequency noise floor from dominating the
  spectrum after gain stage. `gain -n -3` then peak-normalizes the cleaned
  signal to -3 dBFS so whisper sees nominal speech-level audio. Also
  saves `/tmp/geniepod-last-rec.wav` as a fixed-path debug copy so
  operators can `aplay` a recent capture without chasing PID-keyed paths.
- `flush_mic_buffer` is now actually called from `record_audio` (it was
  defined but unused). Drains the ALSA DMA capture queue and re-opens
  the I2S device between cycles so consecutive push-to-talk cycles produce
  independent captures instead of one polluting the next with TTS bleed
  or kernel-side carry-over.
- `transcribe_via_server` also sends `temperature = 0.0` and an explicit
  empty `prompt` form field, defending the decoder against any future
  whisper.cpp server version that might cache prior context.
- `make deploy-config` now force-overwrites `/etc/geniepod/geniepod.toml`
  on the target (was `cp -n`). The repo config is now the single source
  of truth for layout/path/policy knobs; secrets stay in env vars
  (`HA_TOKEN`, `TELEGRAM_BOT_TOKEN`) as documented inline.

### Changed

- `genie-core` now binds to `127.0.0.1` by default through
  `[core].bind_host`, reducing accidental LAN exposure of chat, memory, tool,
  and actuation APIs.
- First-party dashboard and CLI chat requests now send `X-Genie-Origin`; chat
  requests without an origin header are treated as `api` instead of
  `dashboard`.
- Voice speaker identity now receives the captured WAV before cleanup, keeping
  the local biometric recognizer boundary viable for the next alpha.
- Local speaker identification now supports offline WAV-derived profile
  enrollment and matching through `genie-ctl speaker`.
- Speaker profile management now supports live microphone enrollment, WAV
  recording, and profile removal from `genie-ctl`.
- Default `[core]` knobs flipped for the alpha.5 voice-on narrative:
  `voice_enabled` `false -> true`, `wakeword_script` `<wake-listener>
  -> ""` (push-to-talk default), `whisper_port` `0 -> 8178` (server
  mode), `whisper_model` `ggml-small.bin` (with whisper-server keeping it
  GPU-resident, the per-call cost is amortized away), `audio_sample_rate`
  `48000 -> 24000` (match the LyraT I2S2 wire LRCK), `voice_record_secs`
  `5 -> 3` (most household commands fit in 3 s), `stt_language` `"auto"
  -> "en"` (English-only decoder is noticeably more accurate than
  multilingual for English speech).
- Tool-summary system prompt in `voice_loop.rs` rewritten from
  "Summarize the tool result in one natural sentence for voice." to a
  prompt that demands 5-10-word conversational replies and explicitly
  forbids machine phrases ("tool result", "the tool indicates"), shaving
  ~5 s off the typical voice-cycle TTS playback.

### Known issues / tracked for alpha.6

- LyraT firmware (`espressif/esp-adf` `examples/recorder/lyrat_jp4_passthrough/`)
  is configured for 48 kHz I2S but emits 24 kHz LRCK on the JP4 wire on
  the ESP32-LyraT V4.3. Workaround in alpha.5 is to set the Jetson AHUB
  `I2S2 Sample Rate` to match (24 kHz, done by `genie-audio-init`).
  Root cause is likely an APLL/MCLK divider or slot-width fallback in
  the ESP-IDF I2S clock generator.
- STT latency on `whisper-server` (ggml-small) varies between ~270 ms
  (cold path, no LLM activity) and ~3.6 s (after a heavy LLM
  generation) on Orin Nano's shared 7.6 GB iGPU. Suspected cause:
  `llama-server`'s KV-cache growth evicts whisper's model pages.
  Mitigation options (not yet implemented): pin whisper memory, force
  whisper to CPU (slower but consistent), or schedule STT and LLM
  inferences sequentially.
- LLM cold-start on first `genie-llm.service` request takes ~30-60 s
  while Phi-4-mini-Q4_K_M loads into iGPU memory. After that the model
  stays hot. A pre-warm step (one `curl /completion` after service
  start) is documented but not yet automated.
- `record_audio` wall-clock duration is ~6.8 s for a `-d 3` request due
  to Tegra ALSA arecord init/teardown overhead. Captured data is
  byte-exact and timing-correct (3 s of 24 kHz stereo); only the user-
  perceived "ready for next cycle" latency is affected.

## 1.0.0-alpha.4 - 2026-04-25

Alpha 4 is a control-plane hardening release. It moves GenieClaw closer to a
safe local physical agent by making runtime state, tool use, actuation, and
native skills observable and policy-controlled.

### Added

- Runtime contract endpoint and boot log for prompt, tool, policy, and
  hydration fingerprints.
- Optional runtime contract drift detection through
  `[core].expected_runtime_contract_hash`.
- `genie-ctl support-bundle` for local field diagnostics.
- Privacy-preserving tool audit log at `<data_dir>/runtime/tool-audit.jsonl`.
- Actuation channel allowlist and per-origin physical-action rate limits.
- Origin-aware tool policy through `[core.tool_policy]`.
- Native skill sidecar manifest audit metadata.
- Configurable native skill load policy through `[core.skill_policy]`.
- Support-bundle tails for runtime contract, tool audit, and actuation audit logs.

### Changed

- Skill listing now reports manifest status, permissions, capabilities, review
  identity, and signing-material presence.
- Runtime policy status now exposes tool policy, tool audit status, actuation
  limits, skill policy, and loaded skill manifest metadata.
- Documentation now separates current implementation from later work such as
  cryptographic skill signatures and stronger native skill sandboxing.

### Notes

- Skill signature checking is presence-only in this alpha; cryptographic
  verification is still future signed-skill-platform work.
- Tool audit intentionally records argument keys and output length, not argument
  values or outputs.
- Defaults preserve current behavior unless an operator enables stricter
  `skill_policy`, `tool_policy`, or actuation origin/rate settings.
