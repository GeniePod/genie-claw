#!/bin/bash
# GeniePod — Jetson first-time setup script
# Run this on the Jetson after make deploy:
#   ssh geniepod@<jetson-ip> 'bash /opt/geniepod/setup-jetson.sh'

set -euo pipefail

GENIEPOD_DIR="/opt/geniepod"
CONFIG_DIR="/etc/geniepod"
MODEL_DIR="$GENIEPOD_DIR/models"
DATA_DIR="$GENIEPOD_DIR/data"

echo "=== GeniePod Jetson Setup ==="
echo ""

# 1. Create directories.
echo "[1/6] Creating directories..."
sudo mkdir -p "$GENIEPOD_DIR/bin" "$GENIEPOD_DIR/docker" "$MODEL_DIR" "$DATA_DIR" /run/geniepod
sudo mkdir -p /etc/systemd/system/genie-llm.service.d
sudo chown -R "$(whoami):$(whoami)" "$GENIEPOD_DIR" /run/geniepod

# 2. Check binaries.
echo "[2/6] Checking binaries..."
for bin in genie-core genie-governor genie-health genie-api genie-ctl; do
    if [ -f "$GENIEPOD_DIR/bin/$bin" ]; then
        echo "  OK: $bin ($(du -h "$GENIEPOD_DIR/bin/$bin" | cut -f1))"
    else
        echo "  MISSING: $bin — run 'make deploy' from your dev machine"
        exit 1
    fi
done

if [ -f "$GENIEPOD_DIR/bin/genie-audio-init" ]; then
    echo "  OK: genie-audio-init ($(du -h "$GENIEPOD_DIR/bin/genie-audio-init" | cut -f1))"
else
    echo "  WARN: genie-audio-init missing — genie-audio.service will be skipped"
fi

# 3. Check config.
echo "[3/6] Checking config..."
if [ -f "$CONFIG_DIR/geniepod.toml" ]; then
    echo "  OK: $CONFIG_DIR/geniepod.toml"
    sudo chmod 600 "$CONFIG_DIR/geniepod.toml"
    [ -f "$CONFIG_DIR/mosquitto.conf" ] && sudo chmod 600 "$CONFIG_DIR/mosquitto.conf"
    echo "  Secured config permissions"
else
    echo "  MISSING: config — run 'make deploy' from your dev machine"
    exit 1
fi

# 4. Ensure the configured LLM model exists.
echo "[4/6] Checking LLM model..."
CONFIGURED_MODEL_PATH="$(awk -F'"' '/^llm_model_path = / {print $2; exit}' "$CONFIG_DIR/geniepod.toml" 2>/dev/null || true)"
DEFAULT_PHI_MODEL="$MODEL_DIR/phi-4-mini-instruct-q4_k_m.gguf"
GGUF="${CONFIGURED_MODEL_PATH:-$DEFAULT_PHI_MODEL}"
sudo mkdir -p "$(dirname "$GGUF")"

if [ -f "$GGUF" ]; then
    echo "  OK: $(basename "$GGUF") ($(du -h "$GGUF" | cut -f1))"
else
    if [ "$GGUF" = "$DEFAULT_PHI_MODEL" ]; then
        echo "  Downloading Phi-4-mini Q4_K_M (~2.4 GB)..."
        if wget -q --show-progress -O "$GGUF" \
            "https://huggingface.co/lmstudio-community/Phi-4-mini-instruct-GGUF/resolve/main/Phi-4-mini-instruct-Q4_K_M.gguf"
        then
            echo "  OK: downloaded $(du -h "$GGUF" | cut -f1)"
        else
            rm -f "$GGUF"
            echo "  FAILED: could not download Phi-4-mini automatically"
            echo "    Try manually from a dev machine:"
            echo "      hf download lmstudio-community/Phi-4-mini-instruct-GGUF --include 'Phi-4-mini-instruct-Q4_K_M.gguf' --local-dir ."
            echo "      scp Phi-4-mini-instruct-Q4_K_M.gguf $(whoami)@$(hostname -I | awk '{print $1}'):/tmp/"
            echo "      sudo mv /tmp/Phi-4-mini-instruct-Q4_K_M.gguf $GGUF"
            exit 1
        fi
    else
        echo "  MISSING: configured model $(basename "$GGUF")"
        echo "    Copy the model to: $GGUF"
        exit 1
    fi
fi

# 5. Check llama.cpp.
echo "[5/6] Checking llama.cpp..."
if [ -f "$GENIEPOD_DIR/bin/llama-server" ]; then
    echo "  OK: llama-server"
else
    echo "  NOT FOUND: llama-server"
    echo ""
    echo "  Build and install llama.cpp with CUDA:"
    echo "    git clone https://github.com/ggml-org/llama.cpp.git"
    echo "    cd llama.cpp"
    echo "    cmake -B build -DGGML_CUDA=ON"
    echo "    cmake --build build -j\$(nproc)"
    echo "    sudo cp build/bin/llama-server $GENIEPOD_DIR/bin/"
    echo ""
fi

if command -v docker > /dev/null 2>&1 && docker compose version > /dev/null 2>&1; then
    echo "  OK: docker compose"
else
    echo "  NOT FOUND: Docker Engine with compose plugin"
    echo "    Required for Home Assistant container on this Ubuntu-based install"
fi

# 5b. Set Jetson power/performance mode.
echo "[5b/6] Setting Jetson performance mode..."
if sudo nvpmodel -m 1 2>/dev/null; then
    echo "  Set nvpmodel to mode 1 (25W / max speed)"
elif sudo nvpmodel -m 0 2>/dev/null; then
    echo "  Fallback: set nvpmodel to mode 0"
else
    echo "  nvpmodel not available"
fi
sudo jetson_clocks 2>/dev/null && echo "  Clocks locked to max" || echo "  jetson_clocks not available"

# 5c. Apply memory optimizations.
echo "[5c/6] Applying memory optimizations..."
if [ ! -f /etc/sysctl.d/99-geniepod.conf ]; then
    sudo tee /etc/sysctl.d/99-geniepod.conf > /dev/null << 'SYSCTL'
# GeniePod memory optimization for Jetson Orin Nano 8GB
vm.min_free_kbytes = 32768
vm.watermark_boost_factor = 0
vm.swappiness = 10
vm.vfs_cache_pressure = 200
vm.dirty_ratio = 5
vm.dirty_background_ratio = 2
vm.dirty_writeback_centisecs = 50
vm.overcommit_memory = 1
vm.oom_kill_allocating_task = 1
SYSCTL
    sudo sysctl --system > /dev/null 2>&1
    echo "  sysctl optimizations applied"
else
    echo "  sysctl already configured"
fi

# 5d. Reduce CMA if not already done.
if ! grep -q "cma=256M" /proc/cmdline 2>/dev/null; then
    echo "  NOTE: CMA not yet reduced. Add cma=256M to boot args for +256 MB free RAM:"
    echo "    sudo sed -i 's/\\(APPEND.*\\)/\\1 cma=256M/' /boot/extlinux/extlinux.conf"
    echo "    sudo reboot"
fi

# 6. Enable systemd services.
echo "[6/6] Enabling systemd services..."
sudo systemctl daemon-reload

# Enable core services.
for svc in homeassistant genie-llm genie-core genie-governor genie-health genie-api genie-mqtt; do
    if sudo systemctl enable "$svc.service" 2>/dev/null; then
        echo "  Enabled: $svc"
    else
        echo "  Skipped: $svc (unit not found)"
    fi
done

echo ""
echo "=== Setup complete ==="
echo ""
echo "Start services:"
echo "  sudo systemctl start genie-llm    # LLM server (wait ~10s for model load)"
echo "  sudo systemctl start genie-core   # Voice AI + chat API on :3000"
echo "  sudo systemctl start genie-api    # System dashboard on :3080"
echo "  sudo systemctl start genie-governor"
echo "  sudo systemctl start genie-health"
echo ""
echo "Or start all at once:"
echo "  sudo systemctl start geniepod.target"
echo ""
echo "Check status:"
echo "  genie-ctl status"
echo "  genie-ctl health"
echo ""
echo "After future updates:"
echo "  /opt/geniepod/bin/genie-restart-all.sh"
echo ""
echo "Open in browser:"
echo "  http://$(hostname -I | awk '{print $1}'):3000   (chat UI)"
echo "  http://$(hostname -I | awk '{print $1}'):3080   (system dashboard)"
echo ""
echo "Measure RAM:"
echo "  free -h"
echo "  tegrastats --interval 5000"
