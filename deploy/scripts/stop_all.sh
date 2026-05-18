#!/bin/bash
# Stop the deployed GeniePod stack on Jetson.

set -euo pipefail

if [ "$(id -u)" -eq 0 ]; then
    SYSTEMCTL=(systemctl)
else
    SYSTEMCTL=(sudo systemctl)
fi

UNITS=(
    genie-api.service
    genie-health.service
    genie-governor.service
    genie-core.service
    genie-wakeword.service
    genie-mqtt.service
    genie-ai-runtime-warmup.service
    genie-ai-runtime.service
    genie-llm-warmup.service
    genie-llm.service
    genie-whisper-warmup.service
    genie-whisper.service
    genie-audio.service
    homeassistant.service
)

unit_exists() {
    "${SYSTEMCTL[@]}" cat "$1" > /dev/null 2>&1
}

echo "=== GeniePod stop all ==="
echo ""

failed=()
for unit in "${UNITS[@]}"; do
    if ! unit_exists "$unit"; then
        echo "  Skip: $unit (unit not installed)"
        continue
    fi

    printf "  Stopping %s ... " "$unit"
    if "${SYSTEMCTL[@]}" stop "$unit"; then
        echo "OK"
    else
        echo "FAILED"
        failed+=("$unit")
    fi
done

echo ""
if [ "${#failed[@]}" -gt 0 ]; then
    echo "Failed units: ${failed[*]}"
    exit 1
fi

echo "All available GeniePod services stopped."
