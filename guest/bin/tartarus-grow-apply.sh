#!/bin/bash
# tartarus-grow-apply.sh — in-guest finisher for online disk grow.
# Called by the host after qemu-img resize + virDomainBlockResize.

set -euo pipefail

MOUNTPOINT="${TARTARUS_GROW_MOUNTPOINT:-/}"
MARKER_FILE="${TARTARUS_GROW_MARKER_FILE:-/run/tartarus/grow-request}"

root_device=$(findmnt -no SOURCE "${MOUNTPOINT}")
fs_type=$(findmnt -no FSTYPE "${MOUNTPOINT}")

if [[ -z "${root_device}" || -z "${fs_type}" ]]; then
    echo "tartarus-grow-apply: could not resolve device/fstype for ${MOUNTPOINT}" >&2
    exit 1
fi

parent_device=$(lsblk -no PKNAME "${root_device}")
if [[ -z "${parent_device}" ]]; then
    echo "tartarus-grow-apply: could not resolve parent disk for ${root_device}" >&2
    exit 1
fi

partno=$(lsblk -no PARTN "${root_device}")
if ! [[ "${partno}" =~ ^[0-9]+$ ]]; then
    echo "tartarus-grow-apply: could not parse partition number from ${root_device}" >&2
    exit 1
fi

echo "tartarus-grow-apply: growing /dev/${parent_device} partition ${partno} (${fs_type} on ${root_device})" >&2

set +e
growpart "/dev/${parent_device}" "${partno}"
gp_rc=$?
set -e
if (( gp_rc != 0 && gp_rc != 1 )); then
    echo "tartarus-grow-apply: growpart failed with exit ${gp_rc}" >&2
    exit "${gp_rc}"
fi

case "${fs_type}" in
    ext2|ext3|ext4)
        resize2fs "${root_device}"
        ;;
    xfs)
        xfs_growfs "${MOUNTPOINT}"
        ;;
    btrfs)
        btrfs filesystem resize max "${MOUNTPOINT}"
        ;;
    *)
        echo "tartarus-grow-apply: unsupported root filesystem type: ${fs_type}" >&2
        exit 2
        ;;
esac

rm -f "${MARKER_FILE}"

echo "tartarus-grow-apply: ${MOUNTPOINT} (${fs_type}) extended to fill ${root_device}" >&2
