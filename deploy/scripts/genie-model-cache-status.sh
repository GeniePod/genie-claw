#!/bin/bash
# Report how much of the active LLM GGUF is resident in Linux page cache.

set -euo pipefail

CONFIG_FILE="${GENIEPOD_CONFIG:-/etc/geniepod/geniepod.toml}"
DEFAULT_MODEL="/opt/geniepod/models/Qwen3-4B-Q4_K_M.gguf"

usage() {
    echo "Usage: $0 [MODEL_PATH]"
    echo ""
    echo "With no MODEL_PATH, reads [core].llm_model_path from $CONFIG_FILE"
    echo "and falls back to $DEFAULT_MODEL."
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
    usage
    exit 0
fi

if [ $# -gt 1 ]; then
    usage >&2
    exit 2
fi

if [ "$(id -u)" -eq 0 ]; then
    AWK=(awk)
else
    AWK=(sudo awk)
fi

read_configured_model() {
    "${AWK[@]}" -F'"' '
        /^\[core\]/ { in_core = 1; next }
        /^\[/ && !/^\[core\]/ { in_core = 0 }
        in_core && /^llm_model_path = / { print $2; exit }
    ' "$CONFIG_FILE" 2>/dev/null || true
}

MODEL_PATH="${1:-}"
if [ -z "$MODEL_PATH" ]; then
    MODEL_PATH="$(read_configured_model)"
fi
if [ -z "$MODEL_PATH" ]; then
    MODEL_PATH="$DEFAULT_MODEL"
fi

if [ ! -f "$MODEL_PATH" ]; then
    echo "ERROR: model file not found: $MODEL_PATH" >&2
    exit 1
fi

if ! command -v python3 > /dev/null 2>&1; then
    echo "ERROR: python3 is required to call mincore without extra packages" >&2
    exit 1
fi

python3 - "$MODEL_PATH" <<'PY'
import ctypes
import mmap
import os
import sys

path = sys.argv[1]
page_size = os.sysconf("SC_PAGE_SIZE")
size = os.path.getsize(path)

if size == 0:
    print(f"Model: {path}")
    print("Size: 0 MB")
    print("Resident: 0 / 0 MB (0.0%) cold")
    sys.exit(0)

fd = os.open(path, os.O_RDONLY)
try:
    mm = mmap.mmap(fd, size, access=mmap.ACCESS_COPY)
finally:
    os.close(fd)

buf = None
try:
    buf = (ctypes.c_char * 1).from_buffer(mm)
    addr = ctypes.addressof(buf)
    pages = (size + page_size - 1) // page_size
    vec = (ctypes.c_ubyte * pages)()

    libc = ctypes.CDLL(None, use_errno=True)
    mincore = libc.mincore
    mincore.argtypes = [ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_ubyte)]
    mincore.restype = ctypes.c_int

    if mincore(ctypes.c_void_p(addr), ctypes.c_size_t(size), vec) != 0:
        errno = ctypes.get_errno()
        raise OSError(errno, os.strerror(errno))

    resident_pages = sum(1 for value in vec if value & 1)
    resident_bytes = min(resident_pages * page_size, size)
    pct = (resident_bytes / size) * 100.0
    size_mb = size / 1048576.0
    resident_mb = resident_bytes / 1048576.0

    if pct >= 95.0:
        state = "warm"
    elif pct <= 1.0:
        state = "cold"
    else:
        state = "partial"

    print(f"Model: {path}")
    print(f"Size: {size_mb:.0f} MB")
    print(f"Resident: {resident_mb:.0f} / {size_mb:.0f} MB ({pct:.1f}%) {state}")
finally:
    if buf is not None:
        del buf
    mm.close()
PY
