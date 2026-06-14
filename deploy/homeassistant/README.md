# Sample Home Assistant — for reproducing tool-dispatch / actuation issues

The **actual mock-home Home Assistant config from the GenieClaw Jetson
deployment**, packaged to run anywhere. Point GenieClaw at it to reproduce and
prove **real home-control** fixes — the standard the [contribution
gate](../../README.md#accepted-contribution-scope) holds tool-dispatch /
Home-Assistant PRs to.

The `config/configuration.yaml` here is committed verbatim from the Jetson: the
**Demo** integration (a fully simulated house — lights, fans, covers, locks,
climate, media players) plus mock `input_boolean` / `input_number` helpers and
template sensors (front-door contact, hallway motion, outdoor temp, CO₂). It
loads from YAML, so the simulated devices appear with **no UI integration step**.

## 1. Bring up Home Assistant

```bash
docker compose -f deploy/homeassistant/docker-compose.yml up -d
# HA serves on http://<host>:8123 with the simulated house already loaded
```

This mirrors the Jetson's HA service in `/opt/geniepod/docker/docker-compose.yml`,
but bind-mounts the committed `./config` so the mock home is reproduced exactly.

## 2. Onboard + create a long-lived token

Open `http://<host>:8123`, complete first-run onboarding (create a user), then:
Profile (bottom-left) → **Security** → **Long-Lived Access Tokens** → **Create
Token** → name it `GenieClaw` → copy it (shown once). Headless alternative: see
the auth-flow steps in the main `README` / `GETTING_STARTED`.

The mock entities are already present (loaded from `configuration.yaml`), e.g.:

- `light.kitchen_lights`, `light.bed_light`, `light.ceiling_lights`
- `fan.living_room_fan`, `fan.ceiling_fan`
- `cover.kitchen_window`, `cover.garage_door`
- `lock.front_door`, `climate.hvac`, `switch.decorative_lights`
- mock sensors: `binary_sensor.front_door`, `binary_sensor.hallway_motion`,
  `sensor.outdoor_temperature`, `sensor.indoor_co2_level`

## 3. Wire it into GenieClaw

```toml
# /etc/geniepod/geniepod.toml
[services.homeassistant]
url = "http://127.0.0.1:8123/"
systemd_unit = "homeassistant.service"
```

Token via env (preferred) or config:

```bash
# genie-core.service: Environment=HA_TOKEN=<token>   (config reads HA_TOKEN when [core] ha_token is empty)
sudo systemctl daemon-reload && sudo systemctl restart genie-core
```

## 4. Reproduce + verify

```bash
genie-ctl chat "what is the state of the kitchen lights?"   # read path
genie-ctl chat "turn on the kitchen lights"                 # actuation path

# confirm against HA itself
curl -s -H "Authorization: Bearer $HA_TOKEN" \
  http://127.0.0.1:8123/api/states/light.kitchen_lights | jq .state
```

A tool-dispatch / HA PR's **Real Behavior Proof** should show the entity state
changing (e.g. `off → on`) confirmed via the HA API — exactly how
[#400](https://github.com/GeniePod/genie-claw/pull/400) (action-synonym
canonicalization) was validated against this mock home.
