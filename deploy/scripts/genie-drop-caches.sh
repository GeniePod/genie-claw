#!/bin/bash
# GeniePod — drop kernel caches to reclaim RAM on the Jetson.
#
# The Linux page cache, dentries, and inode caches are reclaimable memory, but
# on the Orin Nano's tight 8 GB *unified* pool they can crowd out a large model
# load or inflate memory-pressure readings. This drops them on demand — useful
# before loading the LLM, between soak/test runs, or when checking the true
# free-memory headroom. It is safe (caches just re-warm from disk), so the only
# cost is a brief slowdown as hot files are re-read.
#
# Usage (on the Jetson, as root):
#   sudo bash /opt/geniepod/bin/genie-drop-caches.sh        # drop everything (3)
#   sudo bash /opt/geniepod/bin/genie-drop-caches.sh 1      # page cache only
#   sudo bash /opt/geniepod/bin/genie-drop-caches.sh 2      # dentries + inodes
#
# Levels follow /proc/sys/vm/drop_caches: 1=pagecache, 2=slab (dentries/inodes),
# 3=both (default).

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "This script must run as root. Re-run with: sudo $0 $*" >&2
  exit 1
fi

level="${1:-3}"
case "$level" in
  1 | 2 | 3) ;;
  *)
    echo "usage: $0 [1|2|3]   (1=pagecache, 2=slab, 3=both; default 3)" >&2
    exit 1
    ;;
esac

avail_mb() { free -m | awk '/^Mem:/ {print $7}'; }

before="$(avail_mb)"

# Flush dirty pages first so they are reclaimable, then drop the requested caches.
sync
echo "$level" > /proc/sys/vm/drop_caches

# Best-effort: compact free memory so large contiguous allocations (model load /
# KV cache) are easier to satisfy. Not present on every kernel — ignore failures.
echo 1 > /proc/sys/vm/compact_memory 2>/dev/null || true

after="$(avail_mb)"

echo "drop_caches=${level}  available RAM: ${before} MB -> ${after} MB"
