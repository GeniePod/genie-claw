#!/bin/bash
# GeniePod — Jetson first-time setup script
# Run this on the Jetson after make deploy:
#   ssh geniepod@<jetson-ip> 'bash /opt/geniepod/setup-jetson.sh'
#
# Flags:
#   --runtime genie-ai-runtime   Build + install genie-ai-runtime v1.0.0
#                                alongside the existing llama.cpp backend.
#                                Does NOT modify /etc/geniepod/geniepod.toml
#                                and does NOT stop any running service —
#                                operator does the cutover by hand per the
#                                instructions printed at the end. (issue #54)

set -euo pipefail

GENIEPOD_DIR="/opt/geniepod"
CONFIG_DIR="/etc/geniepod"
MODEL_DIR="$GENIEPOD_DIR/models"
DATA_DIR="$GENIEPOD_DIR/data"

# ── Argument parsing ────────────────────────────────────────────
RUNTIME_TO_INSTALL=""

while [ $# -gt 0 ]; do
    case "$1" in
        --runtime)
            shift
            if [ $# -eq 0 ]; then
                echo "ERROR: --runtime requires an argument (e.g. genie-ai-runtime)" >&2
                exit 2
            fi
            RUNTIME_TO_INSTALL="$1"
            shift
            ;;
        --runtime=*)
            RUNTIME_TO_INSTALL="${1#--runtime=}"
            shift
            ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            echo "Usage: $0 [--runtime genie-ai-runtime]" >&2
            exit 2
            ;;
    esac
done

# ── --runtime mode: install an alternate LLM backend only ───────
install_genie_ai_runtime() {
    local build_dir="$GENIEPOD_DIR/src/genie-ai-runtime"
    local repo_url="https://github.com/GeniePod/genie-ai-runtime.git"
    local tag="v1.0.0"

    echo "=== GeniePod: install genie-ai-runtime $tag ==="
    echo ""

    # 1. Verify prerequisites.
    echo "[1/4] Checking build prerequisites..."
    for pkg in cmake g++; do
        if ! command -v "$pkg" > /dev/null 2>&1; then
            echo "  Installing $pkg via apt..."
            sudo apt-get update -qq
            sudo apt-get install -y "$pkg"
        fi
        echo "  OK: $pkg ($("$pkg" --version 2>/dev/null | head -1))"
    done
    if ! command -v nvcc > /dev/null 2>&1 && [ ! -d /usr/local/cuda/include ]; then
        echo "  WARN: CUDA toolkit not detected — JetPack normally ships it."
        echo "        If the build fails on missing cuBLAS / cuda_runtime.h, install:"
        echo "          sudo apt-get install -y nvidia-cuda-toolkit"
    fi

    # 2. Clone the pinned release.
    echo "[2/4] Fetching $repo_url @ $tag ..."
    sudo mkdir -p "$(dirname "$build_dir")"
    sudo chown -R "$(whoami):$(whoami)" "$(dirname "$build_dir")"
    if [ -d "$build_dir/.git" ]; then
        echo "  Existing checkout found — fetching $tag and resetting."
        git -C "$build_dir" fetch --tags --depth 1 origin "$tag"
        git -C "$build_dir" checkout --quiet "tags/$tag"
        git -C "$build_dir" clean -fdx
    else
        rm -rf "$build_dir"
        git clone --branch "$tag" --depth 1 "$repo_url" "$build_dir"
    fi

    # 3. Build (10-20 min on Orin Nano).
    echo "[3/4] Building (Release, $(nproc) jobs — this takes 10-20 min on Orin Nano)..."
    cd "$build_dir"
    cmake -B build -DCMAKE_BUILD_TYPE=Release
    cmake --build build -j"$(nproc)"

    # 4. Install binaries. Refuse to overwrite if something looks wrong.
    echo "[4/4] Installing binaries to $GENIEPOD_DIR/bin/ ..."
    for bin in jetson-llm-server jetson-llm; do
        if [ ! -f "build/$bin" ]; then
            echo "  ERROR: build/$bin not produced — build output unexpected." >&2
            exit 1
        fi
        sudo install -Dm755 "build/$bin" "$GENIEPOD_DIR/bin/$bin"
        echo "  OK: $bin ($(du -h "$GENIEPOD_DIR/bin/$bin" | cut -f1))"
    done

    echo ""
    echo "=== genie-ai-runtime $tag installed ==="
    echo ""
    echo "NOTE: jetson-llm-server installed but not yet selected as the LLM backend."
    echo "      Your existing llama.cpp setup is unchanged."
    echo ""
    echo "To run genie-ai-runtime instead of llama.cpp:"
    echo "  1. Stop the current llama.cpp backend:"
    echo "       sudo systemctl stop genie-llm"
    echo "  2. Edit /etc/geniepod/geniepod.toml:"
    echo "       [services.llm]"
    echo "       backend      = \"genie_ai_runtime\""
    echo "       systemd_unit = \"genie-ai-runtime.service\""
    echo "  3. Start the new backend:"
    echo "       sudo systemctl daemon-reload"
    echo "       sudo systemctl enable --now genie-ai-runtime.service"
    echo "       sudo systemctl enable --now genie-ai-runtime-warmup.service"
    echo "  4. Restart genie-core to pick up the config change:"
    echo "       sudo systemctl restart genie-core"
    echo ""
    echo "To roll back to llama.cpp:"
    echo "  1. sudo systemctl stop genie-ai-runtime genie-ai-runtime-warmup"
    echo "  2. Edit /etc/geniepod/geniepod.toml:"
    echo "       [services.llm]"
    echo "       backend      = \"llama_cpp\""
    echo "       systemd_unit = \"genie-llm.service\""
    echo "  3. sudo systemctl start genie-llm"
    echo "  4. sudo systemctl restart genie-core"
    echo ""
    echo "Verify:"
    echo "  genie-ctl status            # should report llm_backend"
    echo "  systemctl status genie-ai-runtime.service"
}

if [ -n "$RUNTIME_TO_INSTALL" ]; then
    case "$RUNTIME_TO_INSTALL" in
        genie-ai-runtime)
            install_genie_ai_runtime
            exit 0
            ;;
        *)
            echo "ERROR: unknown runtime: $RUNTIME_TO_INSTALL" >&2
            echo "Supported: genie-ai-runtime" >&2
            exit 2
            ;;
    esac
fi

echo "=== GeniePod Jetson Setup ==="
echo ""

# 1. Create directories.
echo "[1/6] Creating directories..."
sudo mkdir -p "$GENIEPOD_DIR/bin" "$GENIEPOD_DIR/docker" "$MODEL_DIR" "$DATA_DIR" /run/geniepod
sudo mkdir -p /etc/systemd/system/genie-llm.service.d
sudo chown -R "$(whoami):$(whoami)" "$GENIEPOD_DIR" /run/geniepod

# Clean up stale systemd drop-ins that legacy installs may have left behind.
# These override the canonical ExecStart in /etc/systemd/system/genie-llm.service
# and silently mask new flags (--cache-type-k, --ctx-size, etc.) from PR-deployed
# unit files. The repo unit IS the source of truth; per-host customizations should
# live in geniepod.toml, not in systemd overrides.
for drop_in in ctx.conf model.conf; do
    if [ -f "/etc/systemd/system/genie-llm.service.d/$drop_in" ]; then
        echo "  Removing stale systemd drop-in: $drop_in"
        sudo rm -f "/etc/systemd/system/genie-llm.service.d/$drop_in"
    fi
done

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

# 5e. Check voice runtime prerequisites (Whisper STT + Piper TTS).
# These are not auto-downloaded — too large + license-sensitive — but we
# surface missing pieces here so the first voice-loop invocation does not
# fail mysteriously. Paths are read from /etc/geniepod/geniepod.toml so
# user-customized layouts are respected.
echo "[5e/6] Checking voice runtime prerequisites..."

read_toml_string() {
    # Tolerate read failure (e.g. /etc/geniepod/geniepod.toml is chmod 600 and
    # this script is being run without sudo). On failure we just use the
    # documented defaults below.
    awk -F'"' "/^$1 = / {print \$2; exit}" "$CONFIG_DIR/geniepod.toml" 2>/dev/null || true
}

WHISPER_CLI="$(read_toml_string whisper_cli_path)"
WHISPER_CLI="${WHISPER_CLI:-$GENIEPOD_DIR/bin/whisper-cli}"
WHISPER_MODEL="$(read_toml_string whisper_model)"
WHISPER_MODEL="${WHISPER_MODEL:-$MODEL_DIR/ggml-small.bin}"
PIPER_BIN="$(read_toml_string piper_path)"
PIPER_BIN="${PIPER_BIN:-$GENIEPOD_DIR/piper/piper}"
PIPER_VOICE="$(read_toml_string piper_model)"
PIPER_VOICE="${PIPER_VOICE:-$GENIEPOD_DIR/voices/en_US-amy-medium.onnx}"

VOICE_MISSING=0

if [ -x "$WHISPER_CLI" ]; then
    echo "  OK: whisper-cli ($(du -h "$WHISPER_CLI" | cut -f1)) at $WHISPER_CLI"
else
    echo "  MISSING: whisper-cli at $WHISPER_CLI"
    VOICE_MISSING=1
fi

# alpha.5: record_audio peak-normalizes captures with `sox gain -n` so
# weak mic signals reach whisper at nominal level. Not strictly required
# (genie-core falls back to raw recording with a warning), but strongly
# recommended for accuracy.
if command -v sox > /dev/null 2>&1; then
    echo "  OK: sox ($(sox --version 2>/dev/null | head -1 | sed 's/^.*: //'))"
else
    echo "  RECOMMEND: sox not installed — install with: sudo apt install -y sox"
    echo "             (genie-core falls back to raw audio, but STT accuracy suffers on quiet captures)"
fi

# alpha.5: whisper-server is preferred for STT (long-running, model stays in
# GPU memory). Optional in dev hosts where whisper_port = 0 forces CLI mode.
WHISPER_SERVER="$GENIEPOD_DIR/bin/whisper-server"
WHISPER_PORT="$(read_toml_string whisper_port)"
WHISPER_PORT="${WHISPER_PORT:-0}"
if [ -x "$WHISPER_SERVER" ]; then
    echo "  OK: whisper-server ($(du -h "$WHISPER_SERVER" | cut -f1)) at $WHISPER_SERVER"
elif [ "$WHISPER_PORT" != "0" ]; then
    echo "  MISSING: whisper-server at $WHISPER_SERVER (whisper_port=$WHISPER_PORT — server mode is configured but binary is absent)"
    VOICE_MISSING=1
else
    echo "  (whisper-server not installed; whisper_port=0 so CLI fallback is fine)"
fi

if [ -f "$WHISPER_MODEL" ]; then
    echo "  OK: $(basename "$WHISPER_MODEL") ($(du -h "$WHISPER_MODEL" | cut -f1))"
else
    echo "  MISSING: whisper model at $WHISPER_MODEL"
    VOICE_MISSING=1
fi

if [ -x "$PIPER_BIN" ]; then
    echo "  OK: piper ($(du -h "$PIPER_BIN" | cut -f1)) at $PIPER_BIN"
else
    echo "  MISSING: piper at $PIPER_BIN"
    VOICE_MISSING=1
fi

if [ -f "$PIPER_VOICE" ]; then
    echo "  OK: $(basename "$PIPER_VOICE") ($(du -h "$PIPER_VOICE" | cut -f1))"
    if [ ! -f "${PIPER_VOICE}.json" ]; then
        echo "  WARN: ${PIPER_VOICE}.json sidecar missing — piper will fail to load this voice"
        VOICE_MISSING=1
    fi
else
    echo "  MISSING: piper voice at $PIPER_VOICE"
    VOICE_MISSING=1
fi

if [ "$VOICE_MISSING" -eq 1 ]; then
    echo ""
    echo "  Voice prerequisites are not auto-downloaded. To install:"
    echo "    Whisper.cpp:  https://github.com/ggml-org/whisper.cpp"
    echo "                  (build with -DGGML_CUDA=on on Jetson, then copy"
    echo "                   build/bin/whisper-cli to $GENIEPOD_DIR/bin/)"
    echo "    Whisper model: cd whisper.cpp && bash models/download-ggml-model.sh small"
    echo "                   mv models/ggml-small.bin $MODEL_DIR/"
    echo "    Piper TTS:    https://github.com/rhasspy/piper/releases"
    echo "                  (linux_aarch64.tar.gz — extract into $GENIEPOD_DIR/piper/)"
    echo "    Piper voices: https://huggingface.co/rhasspy/piper-voices"
    echo "                  (need both .onnx and .onnx.json, place in $GENIEPOD_DIR/voices/)"
    echo "  Until installed, keep voice_enabled = false in $CONFIG_DIR/geniepod.toml."
fi

# 5f. Install DeepFilterNet `deep-filter` binary (alpha.7, issue #12).
# Used by record_audio when audio_denoiser = "deepfilternet". The binary is
# self-contained — DFN3 model is statically linked via tract. License: MIT/
# Apache-2.0 dual (the project explicitly clarifies AGPL compatibility).
# Falls back to sox-chain at runtime if this step is skipped, so the install
# is non-fatal.
echo "[5f/6] Checking DeepFilterNet binary..."
AUDIO_DENOISER="$(read_toml_string audio_denoiser)"
AUDIO_DENOISER="${AUDIO_DENOISER:-deepfilternet}"
DEEP_FILTER_BIN="$(read_toml_string deep_filter_path)"
DEEP_FILTER_BIN="${DEEP_FILTER_BIN:-$GENIEPOD_DIR/bin/deep-filter}"
DEEP_FILTER_VER="0.5.6"
DEEP_FILTER_URL="https://github.com/Rikorose/DeepFilterNet/releases/download/v${DEEP_FILTER_VER}/deep-filter-${DEEP_FILTER_VER}-aarch64-unknown-linux-gnu"

if [ "$AUDIO_DENOISER" != "deepfilternet" ]; then
    echo "  SKIP: audio_denoiser=\"$AUDIO_DENOISER\" — deep-filter not required"
elif [ -x "$DEEP_FILTER_BIN" ]; then
    echo "  OK: deep-filter ($(du -h "$DEEP_FILTER_BIN" | cut -f1)) at $DEEP_FILTER_BIN"
else
    echo "  Downloading deep-filter v${DEEP_FILTER_VER} (~39 MB)..."
    TMP_DOWNLOAD="$(mktemp /tmp/deep-filter.XXXXXX)"
    if wget -q --show-progress -O "$TMP_DOWNLOAD" "$DEEP_FILTER_URL"; then
        # The release asset is a Linux ELF executable; reject anything else.
        if file "$TMP_DOWNLOAD" 2>/dev/null | grep -q "ELF.*aarch64"; then
            sudo install -m 0755 "$TMP_DOWNLOAD" "$DEEP_FILTER_BIN"
            rm -f "$TMP_DOWNLOAD"
            echo "  OK: installed $DEEP_FILTER_BIN ($(du -h "$DEEP_FILTER_BIN" | cut -f1))"
        else
            rm -f "$TMP_DOWNLOAD"
            echo "  WARN: downloaded file is not an aarch64 ELF — leaving deep-filter unset"
            echo "        Capture pipeline will fall back to sox chain at runtime."
        fi
    else
        rm -f "$TMP_DOWNLOAD"
        echo "  WARN: could not download deep-filter from $DEEP_FILTER_URL"
        echo "        Capture pipeline will fall back to sox chain at runtime."
    fi
fi

# 6. Enable systemd services.
echo "[6/6] Enabling systemd services..."
sudo systemctl daemon-reload

# Enable the umbrella geniepod.target first. Every genie-* service is
# WantedBy=geniepod.target, so without enabling the target itself none
# of them auto-start on reboot — even though `systemctl enable <svc>`
# returns success (it only creates the .wants symlink under the target).
if sudo systemctl enable geniepod.target 2>/dev/null; then
    echo "  Enabled: geniepod.target"
else
    echo "  WARN: geniepod.target unit not found — services will not auto-start"
fi

# Enable core services. genie-audio runs the I2S/AHUB route setup at boot
# (no-op if /opt/geniepod/bin/genie-audio-init is missing, see ConditionPathExists).
for svc in homeassistant genie-audio genie-whisper genie-whisper-warmup genie-llm genie-llm-warmup genie-core genie-governor genie-health genie-api genie-mqtt; do
    if sudo systemctl enable "$svc.service" 2>/dev/null; then
        echo "  Enabled: $svc"
    else
        echo "  Skipped: $svc (unit not found)"
    fi
done

# Run audio init immediately so the current session also has the route set up
# without requiring a reboot. Safe to run any time, idempotent.
if [ -x "$GENIEPOD_DIR/bin/genie-audio-init" ]; then
    "$GENIEPOD_DIR/bin/genie-audio-init" || echo "  audio init returned non-zero (non-fatal)"
fi

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
