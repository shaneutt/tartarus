#!/bin/bash
# tartarus-fstrim.sh — runs fstrim to keep overlays sparse.

set -euo pipefail

if command -v fstrim >/dev/null 2>&1; then
    fstrim -av || true
else
    echo "tartarus-fstrim: fstrim binary not present; skipping." >&2
fi
