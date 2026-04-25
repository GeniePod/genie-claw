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
- `genie-core.service` exposes the main runtime on port `3000`
- `genie-governor.service` and `genie-health.service` are active
- `genie-api.service` serves dashboard/status

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

## Alpha 4 Release Checklist

For a `1.0.0-alpha.4` deployment, validate the control-plane hardening surfaces
after binaries and config are installed:

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
