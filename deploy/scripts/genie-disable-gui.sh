#!/bin/bash
# GeniePod — disable the desktop GUI on a Jetson (L4T / Ubuntu)
#
# The desktop session (display manager + X/Wayland + GNOME shell) holds a few
# hundred MB of the Orin Nano's 8 GB *unified* memory — the same pool the local
# LLM/KV-cache competes for. On a headless appliance the GUI is dead weight, so
# this drops to a console-only boot and frees that memory for the agent.
#
# Usage (run on the Jetson, as root):
#   sudo bash /opt/geniepod/bin/genie-disable-gui.sh          # disable GUI
#   sudo bash /opt/geniepod/bin/genie-disable-gui.sh --enable # restore GUI
#
# It is idempotent and reversible. Best run over SSH — it tears down the local
# desktop session immediately, so a monitor/keyboard session would be dropped
# to a text console (SSH connections survive).

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "This script must run as root. Re-run with: sudo $0 $*" >&2
  exit 1
fi

# Name of the active display manager unit (e.g. gdm3.service / lightdm.service),
# resolved via the standard display-manager.service alias symlink. Empty on a
# truly headless image with no DM installed.
dm_unit=""
if [[ -e /etc/systemd/system/display-manager.service ]]; then
  dm_unit="$(basename "$(readlink -f /etc/systemd/system/display-manager.service)")"
fi

avail_mb() { free -m | awk '/^Mem:/ {print $7}'; }

# ---- re-enable path -------------------------------------------------------
if [[ "${1:-}" == "--enable" || "${1:-}" == "--on" ]]; then
  systemctl set-default graphical.target
  if [[ -n "$dm_unit" ]]; then
    systemctl enable "$dm_unit" >/dev/null 2>&1 || true
  fi
  echo "GUI re-enabled (default target: $(systemctl get-default))."
  echo "Start it now without rebooting:  sudo systemctl isolate graphical.target"
  exit 0
fi

# ---- disable path ---------------------------------------------------------
before="$(avail_mb)"

# 1. Persist across reboots: boot into the console (multi-user) target, which
#    never pulls in graphical.target / the display manager.
systemctl set-default multi-user.target

# 2. Stop the GUI now. `display-manager.service` is the distro-agnostic alias,
#    so this works whether the image ships gdm3, lightdm, or sddm.
if [[ -n "$dm_unit" ]]; then
  echo "Display manager: ${dm_unit} — disabling and stopping"
  systemctl disable "$dm_unit" >/dev/null 2>&1 || true
  systemctl stop display-manager.service 2>/dev/null || true
else
  echo "No display manager detected (already headless?) — only setting the default target."
fi

# 3. Drop the current graphical session immediately so the memory is freed
#    without waiting for a reboot. SSH sessions are unaffected.
systemctl isolate multi-user.target 2>/dev/null || true

sleep 2
after="$(avail_mb)"

echo
echo "GUI disabled."
echo "  default target : $(systemctl get-default)"
echo "  available RAM  : ${before} MB -> ${after} MB"
echo
echo "Re-enable with:  sudo $0 --enable   (then reboot, or: sudo systemctl isolate graphical.target)"
