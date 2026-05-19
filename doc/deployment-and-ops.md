# Deployment And Operations

## Primary Deployment Target

The repo is primarily optimized for Jetson Orin Nano 8 GB class hardware.

The practical reason is simple:

- local inference and local voice must fit in a constrained memory budget
- service count must stay low
- operational behavior has to remain understandable under pressure

## Deploy Assets In This Repo

### Config Templates

- `deploy/config/geniepod.toml`
- `deploy/config/geniepod.dev.toml`
- `deploy/config/profile.toml.example`
- `deploy/config/mosquitto.conf`

### Systemd Units

- `deploy/systemd/genie-core.service`
- `deploy/systemd/genie-api.service`
- `deploy/systemd/genie-governor.service`
- `deploy/systemd/genie-health.service`
- `deploy/systemd/genie-llm.service`
- `deploy/systemd/genie-mqtt.service`
- `deploy/systemd/genie-audio.service`
- `deploy/systemd/genie-wakeword.service`
- `deploy/systemd/homeassistant.service`
- `deploy/systemd/geniepod.target`
- `deploy/systemd/geniepod-late.target`

### Scripts

- `deploy/setup-jetson.sh`
- `deploy/scripts/genie-restart-all.sh`
- `deploy/scripts/detect-audio-device.sh`
- `deploy/scripts/genie-wake-listen.py`
- `deploy/scripts/genie-wakeword.py`

### Docker Assets

- `Dockerfile`
- `docker-compose.dev.yml`
- `deploy/docker/docker-compose.yml`

## Supported Bring-Up Styles

### Dev Machine

Use `deploy/config/geniepod.dev.toml` and point `genie-core` at a local
`llama-server`.

Main references:

- [../README.md](../README.md)
- [../GETTING_STARTED.md](../GETTING_STARTED.md)

### Docker

Useful for local service bring-up and repeatable dev environments.

Use when:

- you do not want Rust installed locally
- you want to validate API/UI surfaces quickly
- you are not debugging low-level audio or Jetson-specific behavior

### Jetson

Use `deploy/setup-jetson.sh` and the systemd units under `deploy/systemd/`.

Typical production expectations:

- `genie-llm.service` provides the local model server
- `genie-core.service` exposes the main runtime on `127.0.0.1:3000` by default
- `genie-governor.service` and `genie-health.service` are active
- `genie-api.service` serves dashboard/status

If direct LAN access to `genie-core` is required, set
`[core].bind_host = "0.0.0.0"` explicitly and put it behind a trusted network
boundary. The core API can touch memory, tools, and home actuation; localhost is
the safe default.

## Operational Commands

Common commands:

```bash
systemctl status genie-core genie-governor genie-health genie-api genie-llm
journalctl -u genie-core -n 200 --no-pager
journalctl -u genie-llm -n 200 --no-pager
curl -s http://127.0.0.1:3000/api/health
curl -s http://127.0.0.1:3000/api/tools
genie-ctl status
genie-ctl diag
genie-ctl support-bundle
```

## Current Alpha Release Checklist

For the current workspace version, validate the control-plane hardening
surfaces after binaries and config are installed:

```bash
genie-ctl version
curl -s http://127.0.0.1:3000/api/runtime/contract
curl -s http://127.0.0.1:3000/api/health
genie-ctl skill list
genie-ctl support-bundle
```

Operator decisions before enabling stricter policy:

- Pin `[core].expected_runtime_contract_hash` only after a known-good boot.
- Enable `[core.skill_policy].require_manifest` only after installed skills have valid sidecar manifests.
- Use `[core.tool_policy]` allowlists/denylists per channel when a surface should be less capable than local dashboard/API.
- Keep `unknown` out of physical actuation origins unless there is a controlled reason to allow it.

## CPU Pinning (Voice Latency Stability)

The Jetson Orin Nano has six CPU cores. The voice path (wake → STT → LLM → TTS
→ playback) is sensitive to scheduler jitter when multiple inference servers
and audio subprocesses share cores. Each `genie-*` systemd unit ships with a
`CPUAffinity=` directive that partitions the six cores into four buckets
(issue #25):

| Cores | Workload |
| --- | --- |
| 0–1 | Kernel, ALSA, MQTT broker, `genie-api`, `genie-governor`, `genie-health`, `genie-wakeword` |
| 2–3 | `whisper-server` (STT decode, two threads) |
| 4   | `llama-server` / `jetson-llm-server` (GPU-bound; one core hosts CUDA dispatch + sampler — whichever LLM backend is active) |
| 5   | `genie-core` and all audio children it spawns (`piper`, `sox`, `deep-filter`, `arecord`, `aplay`) |

`genie-wakeword` retains `SCHED_FIFO` at priority 50 so the continuous audio
loop is not preempted by best-effort work sharing cores 0–1.

Verify pinning after a deploy / restart:

```bash
# Per-service: confirm the unit and its children are on the expected cores.
for svc in genie-core genie-llm genie-ai-runtime genie-whisper genie-wakeword genie-api \
           genie-governor genie-health genie-mqtt; do
    pid=$(systemctl show -p MainPID --value "${svc}.service")
    [ "$pid" != "0" ] && printf "%-18s PID=%s  affinity=%s\n" \
        "$svc" "$pid" "$(taskset -pc "$pid" | awk -F': ' '{print $2}')"
done

# All threads of one service (useful for whisper / llama with multi-threading):
ps -L -o pid,tid,psr,comm -p "$(pidof whisper-server)"

# Live core distribution while a voice cycle runs (Jetson-specific):
sudo tegrastats --interval 250
```

Acceptance signal (issue #25): ten consecutive voice cycles should hold STT
latency within ±100 ms of the median once warmup has completed. If variance
persists after Option 1, the next step is kernel-level `isolcpus=2,3,4,5` on
the bootloader command line — that is intentionally out of scope here because
it requires a Jetson reflash / extlinux.conf edit.

## Runtime Data And State

Default production data location:

- `/opt/geniepod/data`

Expected runtime content:

- memory DB
- conversation DB
- health DB
- governor DB
- profile directory

Important runtime socket:

- `/run/geniepod/governor.sock`

## Known Operational Boundaries

These are current system realities, not bugs in the docs:

- LLM context size is constrained by Jetson memory and model choice.
- Voice mode is more sensitive to process scheduling, audio-device selection, and GPU time-sharing than plain chat mode.
- The connectivity boundary exists, but full ESP-Hosted-NG OS ownership belongs in `genie-os`, not in this runtime repo.
- The ESP32-C6 UART path is currently a health/capability boundary, not a full Thread/Matter controller implementation.
- Local speaker identity is useful for household memory routing, not security-grade authentication.
- Multilingual voice depends on installed STT/TTS models and per-language device testing.
- Vector/cuVS semantic memory is design work; the implemented memory runtime uses SQLite FTS today.
- Web search is intentionally limited to low-risk public lookups and can be disabled completely.

## Suggested Health Checks

Minimum checks after deployment:

1. Verify `llama-server` health.
2. Verify `genie-core` health and tool list.
3. Verify `genie-governor` socket and status.
4. Verify `genie-api` dashboard/status responses.
5. If enabled, verify Home Assistant connectivity.
6. If enabled, verify Telegram polling.
7. If enabled, verify audio device detection and voice round-trip.

## Where To Look During Incidents

| Symptom | First Place To Check |
| --- | --- |
| Chat UI loads but no answers | `genie-core` logs and `/api/health` |
| `llm: offline` | `genie-llm.service` and `llama-server` flags |
| Wrong or missing Home Assistant behavior | Home Assistant service config, token resolution, `ha/` boundary logs |
| Voice hears but does not answer | STT path, language selection, Piper path, audio device |
| Governor appears offline | `/run/geniepod/governor.sock` and `genie-governor.service` |
| Search is missing | `[web_search].enabled`, `/api/tools`, `/api/web-search` |
| Connectivity degraded | `/api/connectivity` and connectivity config |

## Recommended Reading

- [configuration.md](configuration.md)
- [http-and-cli.md](http-and-cli.md)
- [../GETTING_STARTED.md](../GETTING_STARTED.md)
- [../CONNECTIVITY.md](../CONNECTIVITY.md)
