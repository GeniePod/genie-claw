# Configuration Reference

## Config Files

Primary config files in this repo:

- production template: `deploy/config/geniepod.toml`
- development template: `deploy/config/geniepod.dev.toml`
- profile example: `deploy/config/profile.toml.example`

Runtime load path:

- default: `/etc/geniepod/geniepod.toml`
- override: `GENIEPOD_CONFIG=/path/to/file.toml`

## Top-Level Sections

| Section | Purpose |
| --- | --- |
| `data_dir` | Root directory for runtime databases and profile data |
| `[core]` | `genie-core` runtime behavior |
| `[governor]` | governor polling and day/night behavior |
| `[governor.pressure]` | memory-pressure thresholds |
| `[health]` | service polling and alert forwarding |
| `[services.*]` | local service endpoints and systemd unit names |
| `[telegram]` | Telegram long-poll adapter |
| `[web_search]` | public web search tool behavior |
| `[connectivity]` | coprocessor boundary enablement |
| `[connectivity.esp32c6_uart]` | UART transport settings for ESP32-C6 |

## `[core]`

| Key | Purpose |
| --- | --- |
| `port` | HTTP port for `genie-core` |
| `ha_token` | Home Assistant token when not supplied by env |
| `llm_model_name` | Logical model family name for prompt optimization |
| `whisper_model` | Whisper model path |
| `whisper_port` | Whisper server port, `0` means CLI mode |
| `whisper_cli_path` | Path to `whisper-cli` |
| `stt_language` | STT language hint, `"auto"` enables detection |
| `piper_model` | Default Piper voice model path |
| `piper_path` | Path to Piper binary |
| `piper_pipe_mode` | Keep Piper hot for lower latency |
| `voice_tts_models` | Optional per-language Piper voices |
| `max_history_turns` | Max conversation history included per turn |
| `expected_runtime_contract_hash` | Optional pinned contract hash for runtime drift detection |
| `audio_device` | ALSA device or `"auto"` |
| `audio_sample_rate` | Capture sample rate |
| `voice_enabled` | Enable voice mode by config |
| `voice_record_secs` | Recording duration per turn |
| `voice_continuous` | Auto-listen for follow-up after speaking |
| `voice_continuous_secs` | Shorter recording length in continuous mode |
| `llm_model_path` | Model path used by voice mode time-sharing logic |
| `wakeword_script` | Wake-word listener helper path |
| `speaker_identity.*` | Optional voice speaker-identity provider settings |
| `actuation_safety.*` | Final home-actuation safety gate settings |

### `[core.speaker_identity]`

| Key | Purpose |
| --- | --- |
| `enabled` | Turn speaker-identity enrichment on for voice flows |
| `provider` | `none`, `fixed`, or `local_biometric` |
| `fixed_name` | Speaker label used by the `fixed` provider |
| `fixed_confidence` | `low`, `medium`, or `high` confidence reported by the `fixed` provider |
| `local_profile_dir` | Local enrolled speaker profile directory for the biometric provider |
| `local_min_score` | Minimum score required to accept a biometric match |

Behavior notes:

- `none` is the default and behaves like anonymous shared-room voice.
- `fixed` is mainly for single-user boxes, testing, and plumbing validation.
- `local_biometric` is the optional local-recognizer boundary. It is scaffolded now so a real on-device recognizer can be dropped in later without changing config or voice-loop wiring.
- Today, only `fixed` returns an identity immediately. `local_biometric` is a placeholder boundary and currently falls back to unknown identity until the recognizer implementation lands.
- This only affects memory read context in voice mode today; it does not yet add completed biometric recognition by itself.

### `[core.actuation_safety]`

| Key | Purpose |
| --- | --- |
| `enabled` | Turn the final actuation safety gate on or off |
| `min_target_confidence` | Minimum target-match confidence for ordinary actions |
| `min_sensitive_confidence` | Minimum target-match confidence for medium/high-risk actions |
| `deny_multi_target_sensitive` | Block medium/high-risk actions that fan out to multiple entities |
| `require_available_state` | Require a successful current-state check before executing non-scene/script actions |
| `allowed_origins` | Request origins allowed to execute physical actuation |
| `max_actions_per_minute` | Default per-origin actuation rate limit |
| `max_actions_per_minute_by_origin` | Optional per-origin rate-limit overrides |

Behavior notes:

- This gate runs after target resolution and before the actual Home Assistant service call.
- It is separate from prompt rules and separate from the first local policy check.
- Default behavior is fail-closed for ambiguous or degraded physical actions.
- `unknown` is not in the default `allowed_origins`; direct tool execution without a channel context cannot actuate the home.
- Valid origin keys are `voice`, `dashboard`, `api`, `telegram`, `repl`, and `confirmation`.
- Rate limits apply before physical execution and are tracked per origin over a 60-second window.

### Runtime Contract Pinning

After a known-good deployment, read the active hash:

```bash
curl -s http://127.0.0.1:3000/api/health | jq -r '.runtime_contract.contract_hash'
```

Then set:

```toml
[core]
expected_runtime_contract_hash = "<known-good-contract-hash>"
```

If prompt, tool schema, policy, or hydrated boot state changes, `/api/health`
and `/api/runtime/contract` report `validation.status = "drift"`, and
`genie-core` logs a warning on startup.

## `[governor]`

| Key | Purpose |
| --- | --- |
| `poll_interval_ms` | Sampling interval |
| `night_start_hour` | When night mode begins |
| `day_start_hour` | When day mode resumes |
| `night_model_swap` | Optional larger-model-at-night behavior |

### `[governor.pressure]`

| Key | Purpose |
| --- | --- |
| `stop_optins_mb` | Stop optional services below this free-memory threshold |
| `reduce_context_mb` | Trigger smaller LLM context behavior |
| `swap_stt_mb` | Trigger lower-cost STT behavior |
| `zram_mb` | Enable last-resort zram behavior |

## `[health]`

| Key | Purpose |
| --- | --- |
| `interval_secs` | Health polling interval |
| `alert_enabled` | Enable alert forwarding |
| `alert_webhook_url` | Local alert receiver base URL |

## `[services.*]`

Required service blocks:

- `[services.core]`
- `[services.llm]`

Optional service blocks:

- `[services.homeassistant]`
- `[services.nextcloud]`
- `[services.jellyfin]`

Each service block has:

| Key | Purpose |
| --- | --- |
| `url` | Health or base URL |
| `systemd_unit` | Associated systemd unit name |

## `[telegram]`

| Key | Purpose |
| --- | --- |
| `enabled` | Enable Telegram adapter |
| `bot_token` | Bot token when not supplied by env |
| `api_base` | Telegram Bot API base URL |
| `poll_timeout_secs` | Long-poll timeout |
| `allowed_chat_ids` | Allowlist of chat IDs |
| `allow_all_chats` | Disable allowlist enforcement |

Telegram is also gated at build/runtime by the `telegram` feature in
`crates/genie-core/Cargo.toml`.

## `[web_search]`

| Key | Purpose |
| --- | --- |
| `enabled` | Enable the `web_search` tool and quick router |
| `provider` | `duckduckgo` or `searxng` |
| `base_url` | SearXNG base URL when using `searxng` |
| `allow_remote_base_url` | Permit non-loopback SearXNG URLs |
| `timeout_secs` | Request timeout |
| `max_results` | Upper bound on returned items |
| `cache_enabled` | Enable in-process cache |
| `cache_ttl_secs` | Cache freshness window |
| `cache_max_entries` | Cache size cap |

Behavior notes:

- DuckDuckGo is the default and requires no key.
- SearXNG is treated as local-first by default.
- Queries that look like secrets or local credentials are blocked before network use.

## `[connectivity]`

| Key | Purpose |
| --- | --- |
| `enabled` | Turn the coprocessor path on |
| `transport` | Current supported value: `esp32c6_uart` |
| `device` | Logical device name, currently descriptive only |

### `[connectivity.esp32c6_uart]`

| Key | Purpose |
| --- | --- |
| `device_path` | Linux serial device path |
| `baud_rate` | UART baud rate |
| `reset_gpio` | Optional ESP32-C6 reset GPIO |
| `hardware_flow_control` | RTS/CTS support |
| `mtu_bytes` | Max frame size |
| `response_timeout_ms` | UART response timeout |

Legacy alias support exists for `esp32c6_spi`, but the current boundary is
UART-oriented and the detailed SPI hosted work belongs in `genie-os`.

## Environment Overrides And Related Runtime Variables

| Variable | Purpose |
| --- | --- |
| `GENIEPOD_CONFIG` | Override config path |
| `HA_TOKEN` | Home Assistant token fallback |
| `TELEGRAM_BOT_TOKEN` | Telegram token fallback |
| `GENIEPOD_WEB_SEARCH_BASE_URL` | Override SearXNG base URL |
| `GENIEPOD_VOICE` | Force voice mode when set to `1` |
| `RUST_LOG` | Logging level/filter |

Operational variables used by systemd/deploy surfaces outside the Rust config:

| Variable | Purpose |
| --- | --- |
| `GENIEPOD_LLM_MODEL` | Model path used by `genie-llm.service` / `llama-server` |

## Config Resolution Rules

- `Config::load()` reads `GENIEPOD_CONFIG` first, else `/etc/geniepod/geniepod.toml`.
- Home Assistant token resolution prefers `[core].ha_token`, then `HA_TOKEN`.
- Telegram bot token resolution prefers `[telegram].bot_token`, then `TELEGRAM_BOT_TOKEN`.
- SearXNG base URL resolution prefers `GENIEPOD_WEB_SEARCH_BASE_URL` when set, else `[web_search].base_url`.

## Recommended Reading

- [overview.md](overview.md)
- [services-and-crates.md](services-and-crates.md)
- [http-and-cli.md](http-and-cli.md)
- [../GETTING_STARTED.md](../GETTING_STARTED.md)
