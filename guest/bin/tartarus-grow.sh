#!/bin/bash
# tartarus-grow.sh — write a marker when disk usage crosses threshold.
# The host polls the marker via qemu-guest-agent and runs the resize.

set -euo pipefail

THRESHOLD_PCT="${TARTARUS_GROW_THRESHOLD_PCT:-85}"
MOUNTPOINT="${TARTARUS_GROW_MOUNTPOINT:-/}"
MARKER_DIR="${TARTARUS_GROW_MARKER_DIR:-/run/tartarus}"
MARKER_FILE="${MARKER_DIR}/grow-request"

mkdir -p "${MARKER_DIR}"

usage_pct=$(df --output=pcent "${MOUNTPOINT}" | tail -n 1 | tr -d ' %')

if ! [[ "${usage_pct}" =~ ^[0-9]+$ ]]; then
    echo "tartarus-grow: could not parse df output (got: ${usage_pct})" >&2
    exit 1
fi

if (( usage_pct >= THRESHOLD_PCT )); then
    printf 'requested_at=%s\nmountpoint=%s\nusage_pct=%s\nthreshold_pct=%s\n' \
        "$(date --utc +%Y-%m-%dT%H:%M:%SZ)" \
        "${MOUNTPOINT}" \
        "${usage_pct}" \
        "${THRESHOLD_PCT}" \
        > "${MARKER_FILE}"
    chmod 0644 "${MARKER_FILE}"
    logger -t tartarus-grow \
        "watermark crossed: ${MOUNTPOINT} at ${usage_pct}% (threshold ${THRESHOLD_PCT}%); marker written to ${MARKER_FILE}"
    echo "tartarus-grow: ${MOUNTPOINT} at ${usage_pct}% >= ${THRESHOLD_PCT}%; marker written" >&2
else
    if [[ -f "${MARKER_FILE}" ]]; then
        rm -f "${MARKER_FILE}"
        logger -t tartarus-grow \
            "watermark cleared: ${MOUNTPOINT} at ${usage_pct}% (threshold ${THRESHOLD_PCT}%); marker removed"
    fi
fi
