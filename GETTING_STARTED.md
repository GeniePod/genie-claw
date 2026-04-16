# Getting Started with genie-core

Step-by-step guide from zero to a working local GeniePod Home runtime.

---

## Option A: Quick demo on your dev machine (no Jetson needed)

### Prerequisites

- Rust 1.75+ (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
- 4 GB free RAM (for the LLM model)

### 1. Clone and build

```bash
git clone https://github.com/GeniePod/genie-claw.git
cd genie-claw
make test     # 45 tests should pass
make release  # builds optimized binaries
```

### 2. Download a model

Any GGUF model works. TinyLlama is small enough for testing:

```bash
mkdir -p models
wget -O models/tinyllama.gguf \
  "https://huggingface.co/TheBloke/TinyLlama-1.1B-Chat-v1.0-GGUF/resolve/main/tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf"
```

For better quality (needs ~3 GB RAM):

```bash
wget -O models/nemotron-4b.gguf \
  "https://huggingface.co/nvidia/Nemotron-Mini-4B-Instruct-GGUF/resolve/main/Nemotron-Mini-4B-Instruct-Q4_K_M.gguf"
```

### 3. Start llama.cpp server

Download llama.cpp or use Docker:

```bash
# Option A: Docker (easiest)
docker run -p 8080:8080 -v $(pwd)/models:/models \
  ghcr.io/ggml-org/llama.cpp:server \
  --model /models/tinyllama.gguf --host 0.0.0.0 --port 8080 --ctx-size 2048

# Option B: Build from source
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp && cmake -B build && cmake --build build -j
./build/bin/llama-server --model ../models/tinyllama.gguf --host 127.0.0.1 --port 8080
```

Verify it's running:

```bash
curl http://127.0.0.1:8080/health
# Should return: {"status":"ok"}
```

### 4. Start genie-core

```bash
cd genie-claw
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --release --bin genie-core
```

You should see:

```
INFO GeniePod core starting
INFO memory loaded, memories=0
INFO conversation store loaded, conversations=0
INFO genie-core HTTP server listening, addr=127.0.0.1:3000
```

### 5. Chat!

**Browser:** Open http://localhost:3000

**CLI:**

```bash
# In another terminal:
cargo run --release --bin genie-ctl -- chat "what time is it"
cargo run --release --bin genie-ctl -- chat "what is the weather in Denver"
cargo run --release --bin genie-ctl -- chat "set a timer for 5 minutes"
cargo run --release --bin genie-ctl -- tools
cargo run --release --bin genie-ctl -- health
```

**curl:**

```bash
curl -X POST http://127.0.0.1:3000/api/chat \
  -H "Content-Type: application/json" \
  -d '{"message": "hello, what can you do?"}'
```

### 6. (Optional) Start the system dashboard

```bash
GENIEPOD_CONFIG=deploy/config/geniepod.dev.toml cargo run --release --bin genie-api
# Open http://localhost:3080
```

---

## Option B: Docker compose (no Rust needed)

```bash
cd genie-claw

# Download a model
mkdir -p models
wget -O models/tinyllama.gguf \
  "https://huggingface.co/TheBloke/TinyLlama-1.1B-Chat-v1.0-GGUF/resolve/main/tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf"

# Start everything (builds from Dockerfile, ~5 min first time)
docker compose -f docker-compose.dev.yml up --build

# Open http://localhost:3000 (chat UI)
# Open http://localhost:3080 (system dashboard)
```

---

## Option C: Deploy to Jetson Orin Nano

### Prerequisites

- Jetson Orin Nano Super devkit ($249) with JetPack 6.x flashed
- SSH access to the Jetson (`ssh geniepod@<jetson-ip>`)
- Cross-compiler installed on your dev machine: `sudo apt install gcc-aarch64-linux-gnu`

### 1. Cross-compile

```bash
cd genie-claw
make jetson
```

This builds all 5 binaries for aarch64. Output in `target/aarch64-unknown-linux-gnu/release/`.

### 2. Download models on the Jetson

```bash
ssh geniepod@<jetson-ip>
sudo mkdir -p /opt/geniepod/models
cd /opt/geniepod/models

# Nemotron 4B — the primary model (2.8 GB, ~18 tok/s on Orin Nano)
wget "https://huggingface.co/nvidia/Nemotron-Mini-4B-Instruct-GGUF/resolve/main/Nemotron-Mini-4B-Instruct-Q4_K_M.gguf" \
  -O nemotron-4b-q4_k_m.gguf

# Whisper small model for STT (future use)
# wget "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin" -O whisper-small.bin
```

If you already have the Nemotron model under an older OpenClaw path, move it instead of downloading again:

```bash
sudo mkdir -p /opt/geniepod/models
sudo mv /opt/orinclaw/models/nemotron-4b-q4_k_m.gguf /opt/geniepod/models/
ls -lh /opt/geniepod/models/nemotron-4b-q4_k_m.gguf
```

### 3. Install llama.cpp on the Jetson

```bash
ssh geniepod@<jetson-ip>
git clone https://github.com/ggml-org/llama.cpp.git
cd llama.cpp
cmake -B build -DGGML_CUDA=ON
cmake --build build -j$(nproc)
sudo cp build/bin/llama-server /opt/geniepod/bin/
```

### 4. Deploy GeniePod binaries + config

```bash
# From your dev machine:
cd genie-claw
make deploy JETSON_HOST=<jetson-ip> JETSON_USER=geniepod
```

This copies:
- Binaries to `/opt/geniepod/bin/`
- Config to `/etc/geniepod/` (won't overwrite existing)
- systemd units to `/etc/systemd/system/`
- Docker compose config to `/opt/geniepod/docker/`

Run the first-boot setup on the Jetson before starting `genie-core`. This fixes directory ownership for `/opt/geniepod/data`, secures config permissions, verifies the model path, and enables the systemd units.

```bash
ssh geniepod@<jetson-ip> 'bash /opt/geniepod/setup-jetson.sh'
```

If you already tried starting `genie-core` and saw `unable to open database file: /opt/geniepod/data/memory.db`, fix the deployed permissions and retry:

```bash
ssh geniepod@<jetson-ip>
sudo chown -R $(whoami):$(whoami) /opt/geniepod /run/geniepod
sudo chmod 600 /etc/geniepod/geniepod.toml
```

### 5. Install and start Home Assistant on the Jetson

For Ubuntu-based Jetson installs, use Home Assistant Container. Install Docker Engine and the Docker Compose plugin using Docker's official Ubuntu instructions:

- https://docs.docker.com/engine/install/ubuntu/
- https://docs.docker.com/compose/install/linux/

Once Docker is installed, start the managed Home Assistant service:

```bash
ssh geniepod@<jetson-ip>
sudo systemctl enable --now homeassistant
sudo systemctl status homeassistant --no-pager
curl http://127.0.0.1:8123/
```

Complete the Home Assistant onboarding flow in your browser:

```bash
http://<jetson-ip>:8123
```

Then create a long-lived access token in Home Assistant and wire it into `genie-core` with a systemd drop-in:

```bash
ssh geniepod@<jetson-ip>
sudo mkdir -p /etc/systemd/system/genie-core.service.d
sudo tee /etc/systemd/system/genie-core.service.d/homeassistant.conf > /dev/null <<'EOF'
[Service]
Environment=HA_TOKEN=REPLACE_WITH_LONG_LIVED_ACCESS_TOKEN
EOF
sudo systemctl daemon-reload
sudo systemctl restart genie-core genie-health genie-governor
```

If Home Assistant is running on another box in the home, point GeniePod at that URL instead:

```toml
[services.homeassistant]
url = "http://<ha-ip>:8123/"
systemd_unit = "homeassistant.service"
```

### 6. Start services

```bash
ssh geniepod@<jetson-ip>

# Start llama.cpp manually first to verify:
/opt/geniepod/bin/llama-server \
  --model /opt/geniepod/models/nemotron-4b-q4_k_m.gguf \
  --host 127.0.0.1 \
  --port 8080 \
  --ctx-size 2048 \
  --n-gpu-layers 999 \
  --flash-attn on \
  --cache-type-k q8_0 \
  --cache-type-v q8_0 \
  --threads 4

# In another SSH session, start genie-core:
/opt/geniepod/bin/genie-core

# Open http://<jetson-ip>:3000 in your browser
```

### 7. Enable systemd services (persistent)

```bash
ssh geniepod@<jetson-ip>
sudo systemctl daemon-reload
sudo systemctl enable --now genie-llm.service
sudo systemctl enable --now genie-core.service
sudo systemctl enable --now genie-governor.service
sudo systemctl enable --now genie-health.service

# Check status:
sudo systemctl status genie-core
genie-ctl status
genie-ctl health
```

### 7. Measure RAM

```bash
# After all services are running:
genie-ctl status
free -h
tegrastats --interval 5000  # watch GPU + RAM in real time
```

Expected day mode: ~4.8-5.8 GB used, 2.2-3.2 GB free.

---

## Configuration

Main config: `/etc/geniepod/geniepod.toml`

```toml
[core]
port = 3000                     # Chat API port
ha_token = ""                   # Home Assistant token (or set HA_TOKEN env)
max_history_turns = 20          # Conversation context window

[governor]
poll_interval_ms = 5000
night_start_hour = 23
day_start_hour = 6
night_model_swap = false        # Set true to use 9B model at night

[governor.pressure]
stop_optins_mb = 500            # Stop Nextcloud/Jellyfin below this
```

See `deploy/config/geniepod.toml` for all options.

---

## Troubleshooting

### genie-core can't connect to LLM

```bash
curl http://127.0.0.1:8080/health
# If this fails, llama.cpp isn't running.
```

### Chat responses are empty

The LLM model may be too small. Try a larger model (Nemotron 4B instead of TinyLlama).

### Governor offline

```bash
genie-ctl status
# If governor is offline, start it:
sudo systemctl start genie-governor
```

### Cross-compile fails

```bash
# Install the cross-compiler:
sudo apt install gcc-aarch64-linux-gnu
rustup target add aarch64-unknown-linux-gnu
```

---

## What's next

After you have the basic demo running:

1. **Connect Home Assistant** — set `HA_TOKEN` in config, try "turn on the lights"
2. **Try voice mode** — enable `voice_enabled` or launch `genie-core --voice`
3. **Add household context** — place profile files under `/opt/geniepod/data/profile/`
4. **Test governor modes** — `genie-ctl mode media`, `genie-ctl mode night_b`
