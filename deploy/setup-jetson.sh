#!/bin/bash
# GeniePod — Jetson first-time setup script
# Run this on the Jetson after make deploy:
#   ssh geniepod@<jetson-ip> 'bash /opt/geniepod/setup-jetson.sh'
#
# Flags:
#   --model phi-4-mini           Explicit form of today's default
#                                (Phi-4-mini Q4_K_M).
#   --model qwen3-4b             Download Qwen3-4B Q4_K_M instead
#                                (issue #44). Recommended pairing with
#                                genie-ai-runtime once both are installed.
#                                The flag only changes the download target;
#                                it does NOT rewrite llm_model_path in
#                                /etc/geniepod/geniepod.toml — flip that
#                                line by hand once the new model is on disk.
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

# Phi-4-mini Q4_K_M — the current default. Pinned to lmstudio-community's
# GGUF mirror because that conversion has been verified end-to-end on this
# repo's Tegra/aarch64 + llama.cpp + flash-attn stack.
PHI_MODEL_FILENAME="phi-4-mini-instruct-q4_k_m.gguf"
PHI_MODEL_URL="https://huggingface.co/lmstudio-community/Phi-4-mini-instruct-GGUF/resolve/main/Phi-4-mini-instruct-Q4_K_M.gguf"
PHI_MODEL_LABEL="Phi-4-mini Q4_K_M (~2.4 GB)"

# Qwen3-4B Q4_K_M — opt-in alternative (issue #44). Sourced from upstream
# Qwen GGUF release. Stronger reasoning / multilingual / JSON tool-call
# behavior than Phi-4-mini; per-token decode is slower, which is what
# genie-ai-runtime is meant to address downstream.
QWEN3_MODEL_FILENAME="Qwen3-4B-Q4_K_M.gguf"
QWEN3_MODEL_URL="https://huggingface.co/Qwen/Qwen3-4B-GGUF/resolve/main/Qwen3-4B-Q4_K_M.gguf"
QWEN3_MODEL_LABEL="Qwen3-4B Q4_K_M (~2.5 GB)"

# ── Argument parsing ────────────────────────────────────────────
MODEL_CHOICE=""
RUNTIME_TO_INSTALL=""

while [ $# -gt 0 ]; do
    case "$1" in
        --model)
            if [ $# -lt 2 ]; then
                echo "ERROR: --model requires a value (phi-4-mini | qwen3-4b)" >&2
                exit 2
            fi
            MODEL_CHOICE="$2"
            shift 2
            ;;
        --model=*)
            MODEL_CHOICE="${1#--model=}"
            shift
            ;;
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
            sed -n '2,21p' "$0"
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            echo "Usage: $0 [--model phi-4-mini|qwen3-4b] [--runtime genie-ai-runtime]" >&2
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

# --runtime is install-only: do the build/install and exit before the
# rest of the Jetson setup runs. Validation happens here so an unknown
# value fails fast, before we resolve any model paths.
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

case "$MODEL_CHOICE" in
    ""|qwen3-4b)
        MODEL_FLAG_FILENAME="$QWEN3_MODEL_FILENAME"
        MODEL_FLAG_URL="$QWEN3_MODEL_URL"
        MODEL_FLAG_LABEL="$QWEN3_MODEL_LABEL"
        ;;
    phi-4-mini)
        MODEL_FLAG_FILENAME="$PHI_MODEL_FILENAME"
        MODEL_FLAG_URL="$PHI_MODEL_URL"
        MODEL_FLAG_LABEL="$PHI_MODEL_LABEL"
        ;;
    *)
        echo "ERROR: unknown --model '$MODEL_CHOICE'. Supported: phi-4-mini, qwen3-4b" >&2
        exit 2
        ;;
esac

echo "=== GeniePod Jetson Setup ==="
if [ -n "$MODEL_CHOICE" ]; then
    echo "Model selection: --model $MODEL_CHOICE ($MODEL_FLAG_LABEL)"
fi
echo ""

# 1. Create directories.
echo "[1/6] Creating directories..."
sudo mkdir -p "$GENIEPOD_DIR/bin" "$GENIEPOD_DIR/docker" "$MODEL_DIR" "$DATA_DIR" /run/geniepod
sudo mkdir -p /etc/systemd/system/genie-llm.service.d /etc/systemd/system/genie-ai-runtime.service.d
sudo chown -R "$(whoami):$(whoami)" "$GENIEPOD_DIR" /run/geniepod

# Clean up stale systemd drop-ins that legacy installs may have left behind.
# These override the canonical ExecStart in /etc/systemd/system/genie-llm.service
# and silently mask new flags (--cache-type-k, --ctx-size, etc.) from PR-deployed
# unit files. The repo unit IS the source of truth; per-host customizations should
# live in geniepod.toml, not in systemd overrides.
for unit in genie-llm.service genie-ai-runtime.service; do
    for drop_in in ctx.conf model.conf; do
        if [ -f "/etc/systemd/system/${unit}.d/$drop_in" ]; then
            echo "  Removing stale systemd drop-in: ${unit}.d/$drop_in"
            sudo rm -f "/etc/systemd/system/${unit}.d/$drop_in"
        fi
    done
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
# Selection rules (issue #44):
#   - Without --model: honor llm_model_path in geniepod.toml if set, else
#     fall back to the Phi-4-mini default path. Auto-download only when
#     the resolved path matches the default for the active model choice.
#   - With --model <name>: download <name>'s canonical artifact to
#     $MODEL_DIR/<filename>. Does NOT rewrite llm_model_path — operator
#     flips that line by hand to switch the running LLM.
echo "[4/6] Checking LLM model..."
DEFAULT_MODEL_PATH="$MODEL_DIR/$MODEL_FLAG_FILENAME"
if [ -n "$MODEL_CHOICE" ]; then
    GGUF="$DEFAULT_MODEL_PATH"
else
    CONFIGURED_MODEL_PATH="$(awk -F'"' '/^llm_model_path = / {print $2; exit}' "$CONFIG_DIR/geniepod.toml" 2>/dev/null || true)"
    GGUF="${CONFIGURED_MODEL_PATH:-$DEFAULT_MODEL_PATH}"
fi
sudo mkdir -p "$(dirname "$GGUF")"

if [ -f "$GGUF" ]; then
    echo "  OK: $(basename "$GGUF") ($(du -h "$GGUF" | cut -f1))"
else
    if [ "$GGUF" = "$DEFAULT_MODEL_PATH" ]; then
        echo "  Downloading $MODEL_FLAG_LABEL..."
        if wget -q --show-progress -O "$GGUF" "$MODEL_FLAG_URL"; then
            echo "  OK: downloaded $(du -h "$GGUF" | cut -f1)"
        else
            rm -f "$GGUF"
            echo "  FAILED: could not download $MODEL_FLAG_LABEL automatically"
            echo "    Try manually from a dev machine:"
            echo "      wget -O $MODEL_FLAG_FILENAME '$MODEL_FLAG_URL'"
            echo "      scp $MODEL_FLAG_FILENAME $(whoami)@$(hostname -I | awk '{print $1}'):/tmp/"
            echo "      sudo mv /tmp/$MODEL_FLAG_FILENAME $GGUF"
            exit 1
        fi
    else
        echo "  MISSING: configured model $(basename "$GGUF")"
        echo "    Copy the model to: $GGUF"
        exit 1
    fi
fi

# Cutover guidance for non-default --model selections (issue #44 review,
# PR #46). Must run independent of the download branch above so that
# re-runs against an already-on-disk model still surface the four manual
# steps the operator needs to take. Suppressed once geniepod.toml's
# llm_model_path already points at the downloaded model, on the
# assumption that the operator has completed the cutover.
if [ -n "$MODEL_CHOICE" ] && [ "$MODEL_CHOICE" != "qwen3-4b" ]; then
    CUTOVER_CONFIGURED_PATH="$(awk -F'"' '/^llm_model_path = / {print $2; exit}' "$CONFIG_DIR/geniepod.toml" 2>/dev/null || true)"
    if [ "$GGUF" != "$CUTOVER_CONFIGURED_PATH" ]; then
        echo ""
        echo "  NOTE: $CONFIG_DIR/geniepod.toml was not modified."
        echo "        To run with this model, set:"
        echo "          llm_model_path = \"$GGUF\""
        echo "          llm_model_name = \"phi\"    # selects the Phi prompt template"
        echo "        update GENIEPOD_LLM_MODEL in the active LLM systemd unit"
        echo "        (genie-ai-runtime.service for the alpha.9 default, or"
        echo "        genie-llm.service for the llama.cpp fallback), then:"
        echo "          sudo systemctl restart <active-llm-unit> genie-core"
    fi
fi

# 5. Check LLM runtimes and resolve the *effective* backend.
#
# The configured default in geniepod.toml is genie_ai_runtime (issue #52 / PR
# #55). On a fresh deploy without `--runtime genie-ai-runtime` the binary it
# needs (`jetson-llm-server`) isn't on disk yet, which used to leave step
# [6/6] enabling units that couldn't start (issue #60).
#
# Resolution policy (issue #60, Option A):
#   - configured backend's binary present → use that backend.
#   - configured = genie_ai_runtime, binary missing, llama-server present
#       → auto-fallback to llama_cpp; patch geniepod.toml so the runtime,
#         systemd, and the operator's eyes all agree; print a loud NOTE.
#   - configured = llama_cpp, binary missing, jetson-llm-server present
#       → symmetric auto-fallback to genie_ai_runtime.
#   - neither binary present → don't enable any LLM unit in [6/6]; print
#     a remediation block and let the rest of setup continue (whisper/
#     piper still install cleanly, operator fixes LLM and re-runs).
echo "[5/6] Checking LLM runtimes..."

HAVE_JETSON_LLM=false
[ -f "$GENIEPOD_DIR/bin/jetson-llm-server" ] && HAVE_JETSON_LLM=true
HAVE_LLAMA=false
[ -f "$GENIEPOD_DIR/bin/llama-server" ] && HAVE_LLAMA=true

if [ "$HAVE_JETSON_LLM" = "true" ]; then
    echo "  OK: jetson-llm-server"
else
    echo "  NOT FOUND: jetson-llm-server (default backend binary)"
fi
if [ "$HAVE_LLAMA" = "true" ]; then
    echo "  OK: llama-server (legacy fallback backend)"
else
    echo "  NOT FOUND: llama-server (legacy fallback backend)"
fi

CONFIGURED_BACKEND="$(awk -F'"' '/^backend = / {print $2; exit}' "$CONFIG_DIR/geniepod.toml" 2>/dev/null || true)"
[ -z "$CONFIGURED_BACKEND" ] && CONFIGURED_BACKEND="genie_ai_runtime"
# Normalize the hyphenated alias documented next to backend = in the toml.
case "$CONFIGURED_BACKEND" in
    llama-cpp)         CONFIGURED_BACKEND="llama_cpp" ;;
    genie-ai-runtime)  CONFIGURED_BACKEND="genie_ai_runtime" ;;
esac

EFFECTIVE_BACKEND="$CONFIGURED_BACKEND"
SKIP_LLM_UNITS=false

patch_services_llm_backend() {
    # Rewrite the [services.llm] section's `backend = ...` and
    # `systemd_unit = ...` values to the requested target, in place,
    # without touching surrounding comments or other sections. awk is
    # used instead of sed so that "systemd_unit =" in [services.core]
    # right above can't be hit by accident.
    local new_backend="$1"
    local new_unit="$2"
    local cfg="$CONFIG_DIR/geniepod.toml"
    local tmp
    tmp="$(mktemp)"
    awk -v nb="$new_backend" -v nu="$new_unit" '
        BEGIN { in_llm = 0 }
        /^\[services\.llm\]/   { in_llm = 1; print; next }
        /^\[/ && !/^\[services\.llm\]/ { in_llm = 0 }
        in_llm && /^backend = "[^"]*"/ {
            sub(/^backend = "[^"]*"/, "backend = \"" nb "\"")
            print
            next
        }
        in_llm && /^systemd_unit = "[^"]*"/ {
            sub(/^systemd_unit = "[^"]*"/, "systemd_unit = \"" nu "\"")
            print
            next
        }
        { print }
    ' "$cfg" > "$tmp" && sudo install -m 600 "$tmp" "$cfg"
    rm -f "$tmp"
}

if [ "$CONFIGURED_BACKEND" = "genie_ai_runtime" ] && [ "$HAVE_JETSON_LLM" = "false" ]; then
    if [ "$HAVE_LLAMA" = "true" ]; then
        EFFECTIVE_BACKEND="llama_cpp"
        echo ""
        echo "  NOTE: configured backend (genie_ai_runtime) is not installed."
        echo "        Auto-falling back to llama.cpp so this box leaves setup"
        echo "        in a working state (issue #60)."
        echo "        Patching $CONFIG_DIR/geniepod.toml:"
        echo "            backend      = \"llama_cpp\""
        echo "            systemd_unit = \"genie-llm.service\""
        echo "        To run the default backend instead, install it via:"
        echo "            bash $0 --runtime genie-ai-runtime"
        echo "        then re-run setup-jetson.sh (or hand-edit geniepod.toml back)."
        patch_services_llm_backend "llama_cpp" "genie-llm.service"
    else
        echo ""
        echo "  ERROR: configured backend (genie_ai_runtime) is not installed"
        echo "         and no llama.cpp fallback is available either."
        echo "         Step [6/6] will NOT enable any LLM systemd unit."
        echo "         Remediate with one of:"
        echo "           bash $0 --runtime genie-ai-runtime    # install the default"
        echo "         OR install llama.cpp's llama-server to $GENIEPOD_DIR/bin/"
        echo "         then re-run this script."
        SKIP_LLM_UNITS=true
    fi
elif [ "$CONFIGURED_BACKEND" = "llama_cpp" ] && [ "$HAVE_LLAMA" = "false" ]; then
    if [ "$HAVE_JETSON_LLM" = "true" ]; then
        EFFECTIVE_BACKEND="genie_ai_runtime"
        echo ""
        echo "  NOTE: configured backend (llama_cpp) is not installed."
        echo "        Auto-falling back to genie_ai_runtime so this box leaves"
        echo "        setup in a working state (issue #60)."
        echo "        Patching $CONFIG_DIR/geniepod.toml:"
        echo "            backend      = \"genie_ai_runtime\""
        echo "            systemd_unit = \"genie-ai-runtime.service\""
        patch_services_llm_backend "genie_ai_runtime" "genie-ai-runtime.service"
    else
        echo ""
        echo "  ERROR: configured backend (llama_cpp) is not installed and no"
        echo "         genie-ai-runtime fallback is available either."
        echo "         Step [6/6] will NOT enable any LLM systemd unit."
        echo "         Install llama.cpp's llama-server to $GENIEPOD_DIR/bin/"
        echo "         OR run: bash $0 --runtime genie-ai-runtime"
        echo "         then re-run this script."
        SKIP_LLM_UNITS=true
    fi
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
#
# LLM service selection consumes the EFFECTIVE_BACKEND resolved in step [5/6]
# above (issue #60). That variable already accounts for the auto-fallback
# from genie_ai_runtime → llama_cpp when jetson-llm-server isn't installed,
# so reading geniepod.toml again here would re-introduce the bug.
if [ "$SKIP_LLM_UNITS" = "true" ]; then
    echo "  Skipping LLM units — no backend binary is installed (see step [5/6])."
    LLM_SERVICES=""
elif [ "$EFFECTIVE_BACKEND" = "llama_cpp" ]; then
    LLM_SERVICES="genie-llm genie-llm-warmup"
else
    LLM_SERVICES="genie-ai-runtime genie-ai-runtime-warmup"
fi

for svc in homeassistant genie-audio genie-whisper genie-whisper-warmup $LLM_SERVICES genie-core genie-governor genie-health genie-api genie-mqtt; do
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
if [ "$SKIP_LLM_UNITS" = "true" ]; then
    echo "  (skip the LLM start line — no backend is installed yet; see step [5/6])"
elif [ "$EFFECTIVE_BACKEND" = "llama_cpp" ]; then
    echo "  sudo systemctl start genie-llm    # LLM server (wait ~10s for model load)"
else
    echo "  sudo systemctl start genie-ai-runtime    # LLM server (wait ~10s for model load)"
fi
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
