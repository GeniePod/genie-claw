# Changelog

## Unreleased

### Added

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
