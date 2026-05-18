#!/bin/bash
# Start the deployed GeniePod stack on Jetson.

set -euo pipefail

CONFIG_FILE="${GENIEPOD_CONFIG:-/etc/geniepod/geniepod.toml}"

if [ "$(id -u)" -eq 0 ]; then
    SYSTEMCTL=(systemctl)
    AWK=(awk)
else
    SYSTEMCTL=(sudo systemctl)
    AWK=(sudo awk)
fi

read_llm_unit() {
    "${AWK[@]}" -F'"' '
        /^\[services\.llm\]/ { in_llm = 1; next }
        /^\[/ && !/^\[services\.llm\]/ { in_llm = 0 }
        in_llm && /^systemd_unit = / { print $2; exit }
    ' "$CONFIG_FILE" 2>/dev/null || true
}

read_wakeword_script() {
    "${AWK[@]}" -F'"' '
        /^\[core\]/ { in_core = 1; next }
        /^\[/ && !/^\[core\]/ { in_core = 0 }
        in_core && /^wakeword_script = / { found = 1; print $2; exit }
        END { if (!found) print "__missing__" }
    ' "$CONFIG_FILE" 2>/dev/null || true
}

normalize_unit() {
    local unit="$1"
    case "$unit" in
        *.service) printf '%s\n' "$unit" ;;
        "") printf 'genie-ai-runtime.service\n' ;;
        *) printf '%s.service\n' "$unit" ;;
    esac
}

warmup_unit_for() {
    local unit="$1"
    case "$unit" in
        genie-ai-runtime.service) printf 'genie-ai-runtime-warmup.service\n' ;;
        genie-llm.service) printf 'genie-llm-warmup.service\n' ;;
        *) printf '\n' ;;
    esac
}

other_llm_units_for() {
    local unit="$1"
    case "$unit" in
        genie-ai-runtime.service)
            printf '%s\n' genie-llm-warmup.service genie-llm.service
            ;;
        genie-llm.service)
            printf '%s\n' genie-ai-runtime-warmup.service genie-ai-runtime.service
            ;;
    esac
}

unit_exists() {
    "${SYSTEMCTL[@]}" cat "$1" > /dev/null 2>&1
}

is_optional_unit() {
    local unit="$1"
    case "$unit" in
        genie-audio.service|genie-wakeword.service|homeassistant.service)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

is_warmup_unit() {
    local unit="$1"
    case "$unit" in
        *-warmup.service)
            return 0
            ;;
        *)
            return 1
            ;;
    esac
}

skip_disabled_unit() {
    local unit="$1"
    local reason="$2"
    echo "  Skip: $unit ($reason)"
    if unit_exists "$unit"; then
        "${SYSTEMCTL[@]}" stop "$unit" > /dev/null 2>&1 || true
        "${SYSTEMCTL[@]}" reset-failed "$unit" > /dev/null 2>&1 || true
    fi
}

start_unit() {
    local unit="$1"
    if [ -z "$unit" ]; then
        return 0
    fi

    if [ "$unit" = "genie-wakeword.service" ] && [ "$wakeword_enabled" != "1" ]; then
        skip_disabled_unit "$unit" "wakeword_script is empty; push-to-talk mode"
        return 0
    fi

    if ! unit_exists "$unit"; then
        if is_optional_unit "$unit"; then
            echo "  Skip: $unit (unit not installed)"
            return 0
        fi
        echo "  FAILED: $unit (unit not installed)"
        return 1
    fi

    if is_warmup_unit "$unit"; then
        printf "  Queuing %s ... " "$unit"
        if "${SYSTEMCTL[@]}" start --no-block "$unit"; then
            echo "OK"
        else
            echo "FAILED"
            return 1
        fi
        return 0
    fi

    printf "  Starting %s ... " "$unit"
    if ! "${SYSTEMCTL[@]}" start "$unit"; then
        echo "FAILED"
        return 1
    fi

    echo "OK"
}

raw_llm_unit="$(read_llm_unit)"
configured_llm_unit="$(normalize_unit "$raw_llm_unit")"
configured_warmup_unit="$(warmup_unit_for "$configured_llm_unit")"
wakeword_script="$(read_wakeword_script)"
wakeword_enabled=1
if [ -z "$wakeword_script" ]; then
    wakeword_enabled=0
elif [ "$wakeword_script" = "__missing__" ]; then
    # Missing key falls back to genie-core's compiled default.
    wakeword_script="/opt/geniepod/bin/genie-wake-listen.py"
fi

UNITS=(
    genie-audio.service
    "$configured_llm_unit"
    "$configured_warmup_unit"
    homeassistant.service
    genie-whisper.service
    genie-whisper-warmup.service
    genie-core.service
    genie-governor.service
    genie-health.service
    genie-api.service
    genie-mqtt.service
    genie-wakeword.service
)

echo "=== GeniePod start all ==="
echo ""
echo "Configured LLM unit: $configured_llm_unit"
if [ "$wakeword_enabled" = "1" ]; then
    echo "Wake word script: $wakeword_script"
else
    echo "Wake word: disabled (push-to-talk mode)"
fi
echo "Reloading systemd units..."
"${SYSTEMCTL[@]}" daemon-reload

while IFS= read -r other_unit; do
    [ -n "$other_unit" ] || continue
    if unit_exists "$other_unit"; then
        "${SYSTEMCTL[@]}" stop "$other_unit" > /dev/null 2>&1 || true
    fi
done < <(other_llm_units_for "$configured_llm_unit")

failed=()
for unit in "${UNITS[@]}"; do
    if ! start_unit "$unit"; then
        failed+=("$unit")
    fi
done

echo ""
if [ "${#failed[@]}" -gt 0 ]; then
    echo "Failed units: ${failed[*]}"
    exit 1
fi

echo "All available GeniePod services started."
