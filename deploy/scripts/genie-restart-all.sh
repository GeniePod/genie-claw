#!/bin/bash
# Restart the deployed GeniePod stack after updating binaries/config on Jetson.

set -euo pipefail

if [ "$(id -u)" -eq 0 ]; then
    SYSTEMCTL=(systemctl)
else
    SYSTEMCTL=(sudo systemctl)
fi

UNITS=(
    genie-audio.service
    genie-mqtt.service
    genie-core.service
    genie-governor.service
    genie-health.service
    genie-api.service
    genie-wakeword.service
    homeassistant.service
)

OPTIONAL_UNITS=(
    genie-audio.service
    genie-wakeword.service
    homeassistant.service
)

is_optional_unit() {
    local unit="$1"
    local optional
    for optional in "${OPTIONAL_UNITS[@]}"; do
        if [ "$optional" = "$unit" ]; then
            return 0
        fi
    done
    return 1
}

unit_exists() {
    "${SYSTEMCTL[@]}" cat "$1" > /dev/null 2>&1
}

echo "=== GeniePod restart after update ==="
echo ""
echo "Reloading systemd units..."
"${SYSTEMCTL[@]}" daemon-reload

failed_required=()
failed_optional=()

for unit in "${UNITS[@]}"; do
    if ! unit_exists "$unit"; then
        echo "  Skip: $unit (unit not installed)"
        continue
    fi

    printf "  Restarting %s ... " "$unit"
    if "${SYSTEMCTL[@]}" restart "$unit"; then
        echo "OK"
    else
        echo "FAILED"
        if is_optional_unit "$unit"; then
            failed_optional+=("$unit")
        else
            failed_required+=("$unit")
        fi
    fi
done

echo ""
if [ "${#failed_optional[@]}" -gt 0 ]; then
    echo "Optional units failed: ${failed_optional[*]}"
fi

if [ "${#failed_required[@]}" -gt 0 ]; then
    echo "Required units failed: ${failed_required[*]}"
    exit 1
fi

echo "All required GeniePod services restarted."
