#!/bin/bash
# Hard restart of the deployed GeniePod stack on Jetson.
#
# Default (--hard) order of operations:
#   1. Stop every GeniePod systemd unit (delegates to stop_all.sh).
#   2. Best-effort kill of any subprocess that survived the stop. systemctl
#      stop kills the cgroup so this is rare, but genie-core spawns
#      piper / whisper / sox / ffmpeg / jetson-llm-server / deep-filter,
#      and a crashed parent may leave them behind.
#   3. `sync; echo 3 > /proc/sys/vm/drop_caches` — release page cache.
#   4. `swapoff -a; swapon -a` — flush the swap file back to free state.
#   5. Start every GeniePod systemd unit (delegates to start_all.sh).
#
# Trade-off — important for issue #69 / PR #70 readers:
#   Step 3 evicts the Qwen3-4B GGUF from page cache, so the first LLM
#   request after restart hits the ~30 s cold-load path instead of the
#   ~1.3 s warm path PR #70 preserved across plain `systemctl restart
#   genie-ai-runtime`. That's the intended behavior here — this script
#   exists to give back a clean memory state after `make deploy`, where
#   the binaries / config / model path may have changed and the prior
#   warm cache is stale anyway. If you only want a soft service refresh
#   (and want to keep the warm LLM cache), pass `--soft` instead.
#
# Usage:
#   genie-restart-all.sh          # full hard reset (cache + swap)
#   genie-restart-all.sh --hard   # explicit form of the default
#   genie-restart-all.sh --soft   # services only; preserve page cache + swap

set -uo pipefail

MODE="hard"
while [ $# -gt 0 ]; do
    case "$1" in
        --soft) MODE="soft" ; shift ;;
        --hard) MODE="hard" ; shift ;;
        -h|--help)
            sed -n '2,30p' "$0"
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            echo "Usage: $0 [--hard|--soft]" >&2
            exit 2
            ;;
    esac
done

if [ "$(id -u)" -eq 0 ]; then
    SUDO=()
else
    SUDO=(sudo)
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
STOP_ALL="$SCRIPT_DIR/stop_all.sh"
START_ALL="$SCRIPT_DIR/start_all.sh"

# Process names that GeniePod units typically spawn as subprocesses. If a
# parent crashed or a previous shutdown left descendants behind, reap them
# before we reclaim memory in step 3-4. `pkill -x` matches exact basenames
# only, so unrelated processes that happen to embed these strings (e.g.
# "ffmpeg-tutorial-screenshot.png" in a file viewer) are not at risk.
ORPHAN_CMDS=(
    piper
    whisper-server
    whisper-cli
    jetson-llm-server
    jetson-llm
    llama-server
    deep-filter
    sox
    ffmpeg
)

echo "=== GeniePod restart (mode=$MODE) ==="
echo ""

# 1. Stop all systemd units.
if [ -x "$STOP_ALL" ]; then
    "$STOP_ALL"
else
    echo "WARN: $STOP_ALL not executable; skipping stop step" >&2
fi

# 2. Reap orphaned subprocesses.
echo ""
echo "Reaping orphaned subprocesses..."
reaped=0
for cmd in "${ORPHAN_CMDS[@]}"; do
    if "${SUDO[@]}" pkill -x "$cmd" 2>/dev/null; then
        echo "  Killed orphan: $cmd"
        reaped=$((reaped + 1))
    fi
done
if [ "$reaped" -eq 0 ]; then
    echo "  None — systemctl stop reaped the cgroups cleanly."
fi

if [ "$MODE" = "hard" ]; then
    # 3. Drop page cache. Loses the warm Qwen3-4B GGUF residency from
    # PR #70 by design — see header comment for the trade-off.
    echo ""
    echo "Dropping page cache (sync + drop_caches=3)..."
    if "${SUDO[@]}" sh -c 'sync && echo 3 > /proc/sys/vm/drop_caches' 2>/dev/null; then
        echo "  OK"
    else
        echo "  WARN: drop_caches write failed (need root). Continuing."
    fi

    # 4. Free swap. swapoff returns the swap file's pages to RAM (or
    # discards them if backed by clean files), then swapon re-enables it
    # at a clean baseline. If swap was full and not enough free RAM
    # exists to absorb it, swapoff fails — we don't want to wedge the
    # box, so fall through without retrying.
    echo "Freeing swap (swapoff -a && swapon -a)..."
    if "${SUDO[@]}" swapoff -a 2>/dev/null; then
        echo "  swapoff: OK"
        if "${SUDO[@]}" swapon -a 2>/dev/null; then
            echo "  swapon:  OK"
        else
            echo "  swapon:  no swap entries in /etc/fstab (continuing)"
        fi
    else
        echo "  swapoff: not configured or not enough free RAM to absorb (continuing)"
    fi
fi

# 5. Start all systemd units.
echo ""
if [ -x "$START_ALL" ]; then
    "$START_ALL"
else
    echo "WARN: $START_ALL not executable; skipping start step" >&2
    exit 1
fi

echo ""
echo "=== GeniePod restart complete (mode=$MODE) ==="
