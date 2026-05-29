# Getting Started With GenieClaw

This guide covers the current development and Jetson bring-up paths. The
production Jetson default is `genie-ai-runtime`; the development config still
uses any local OpenAI-compatible `llama.cpp` server on `:8080`.

OpenAI-compatible API providers and other remote/alternate providers are for
testing, development portability, and transitional validation only. The product
path remains a private, low-latency, on-device home agent with limited context,
family memory, typed tools, and local IoT/home-runtime boundaries.

## Option A: Development Machine

Prerequisites:

- Rust toolchain
- 4 GB or more free RAM for a small local test model
- an OpenAI-compatible local model server on `http://127.0.0.1:8080`

Build and test:

```bash
git clone https://github.com/GeniePod/genie-claw.git
cd genie-claw
make test
make release
```

Start a local OpenAI-compatible backend. For example, with `llama.cpp`:

```bash
mkdir -p models
# Put any small GGUF test model under ./models.

docker run --rm -p 8080:8080 -v "$(pwd)/models:/models" \
  ghcr.io/ggml-org/llama.cpp:server \
  --model /models/your-model.gguf \
  --host 0.0.0.0 \
  --port 8080 \
  --ctx-size 4096
```

Verify the backend:

```bash
curl -sf http://127.0.0.1:8080/health && echo
```

Run the core and dashboard with the dev config:

```bash
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --release --bin genie-core
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --release --bin genie-api
```

Open:

- Chat UI: `http://127.0.0.1:3000`
- Dashboard: `http://127.0.0.1:3080`

CLI examples:

```bash
cargo run --release --bin genie-ctl -- status
cargo run --release --bin genie-ctl -- chat "what time is it"
cargo run --release --bin genie-ctl -- search --limit 3 "ESP32-C6 Thread support"
cargo run --release --bin genie-ctl -- tools
cargo run --release --bin genie-ctl -- health
```

## Option B: Docker Compose

Use this path for a quick local service bring-up without installing every Rust
binary manually:

```bash
mkdir -p models
# Put a small GGUF test model under ./models and adjust docker-compose.dev.yml
# if the filename differs from the compose default.

docker compose -f docker-compose.dev.yml up --build
```

Open:

- Chat UI: `http://127.0.0.1:3000`
- Dashboard: `http://127.0.0.1:3080`

## Option C: Jetson Orin Nano

Prerequisites on the development machine:

```bash
sudo apt install gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
```

Deploy current binaries, config, systemd units, and helper scripts:

```bash
make deploy \
  JETSON_HOST=<jetson-ip> \
  JETSON_USER=<jetson-user>
```

Run first-boot setup on the Jetson. The current default backend is
`genie-ai-runtime` with the Qwen3 4B model path from `deploy/config/geniepod.toml`.

```bash
ssh <jetson-user>@<jetson-ip> 'bash /opt/geniepod/setup-jetson.sh --runtime genie-ai-runtime'
```

Start or restart the appliance stack:

```bash
ssh <jetson-user>@<jetson-ip> '/opt/geniepod/bin/genie-restart-all.sh --hard'
```

Verify services:

```bash
ssh <jetson-user>@<jetson-ip> '
  curl -sf http://127.0.0.1:8080/health && echo
  curl -sf http://127.0.0.1:3000/api/health && echo
  curl -sf http://127.0.0.1:3080/api/status && echo
  /opt/geniepod/bin/genie-ctl status
'
```

Open:

- Chat UI: `http://<jetson-ip>:3000`
- Dashboard: `http://<jetson-ip>:3080`

## Home Assistant

Home Assistant is optional and remains a transitional provider until
`genie-home-runtime` exists. If Home Assistant runs on the Jetson, enable the
managed service and complete onboarding:

```bash
ssh <jetson-user>@<jetson-ip>
sudo systemctl enable --now homeassistant
curl http://127.0.0.1:8123/
```

Set the Home Assistant token as a systemd environment value rather than storing
the token in TOML:

```bash
sudo mkdir -p /etc/systemd/system/genie-core.service.d
sudo tee /etc/systemd/system/genie-core.service.d/homeassistant.conf > /dev/null <<'EOF'
[Service]
Environment=HA_TOKEN=REPLACE_WITH_LONG_LIVED_ACCESS_TOKEN
EOF
sudo systemctl daemon-reload
sudo systemctl restart genie-core genie-health genie-governor
```

If Home Assistant runs elsewhere, update the service URL:

```toml
[services.homeassistant]
url = "http://<ha-ip>:8123/"
systemd_unit = "homeassistant.service"
```

## Configuration

Main Jetson config:

```text
/etc/geniepod/geniepod.toml
```

Source templates:

- `deploy/config/geniepod.toml`: Jetson/default appliance config
- `deploy/config/geniepod.dev.toml`: dev-machine config

The key LLM section is:

```toml
[services.llm]
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-ai-runtime.service"
backend = "genie_ai_runtime"
```

To use the legacy `llama.cpp` fallback instead, set:

```toml
[services.llm]
url = "http://127.0.0.1:8080/health"
systemd_unit = "genie-llm.service"
backend = "llama_cpp"
```

## Troubleshooting

LLM backend offline:

```bash
curl -v http://127.0.0.1:8080/health
systemctl status genie-ai-runtime genie-llm --no-pager
journalctl -u genie-ai-runtime -n 120 --no-pager
```

Core API offline:

```bash
curl -v http://127.0.0.1:3000/api/health
systemctl status genie-core --no-pager
journalctl -u genie-core -n 120 --no-pager
```

Dashboard offline:

```bash
curl -v http://127.0.0.1:3080/api/status
systemctl status genie-api --no-pager
```

Cross-compile fails:

```bash
sudo apt install gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
```
