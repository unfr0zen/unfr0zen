#!/bin/sh
# Runs on the unit before unfr0zen-apply reads the root volume mounted at
# $UNFR0ZEN_ROOT; failing, it stops the run with nothing written.
# $UNFR0ZEN_VERSION is the bundle's, $UNFR0ZEN_DRY_RUN 1 when nothing is to be written.
set -eu
echo "Kernel $(uname -r); root volume: $(df -k "$UNFR0ZEN_ROOT" | tail -n 1)"

# unfr0zen's programs and data live on the scratch partition's JFFS2, which
# the patch reaches through the root volume's own mount point for it: a
# running unit has it there, the initramfs mounts it (read-only for a dry run).
scratch=${UNFR0ZEN_ROOT%/}/media/scratch
if ! awk -v dir="$scratch" '$2 == dir { found = 1 } END { exit !found }' /proc/mounts; then
    if [ "$UNFR0ZEN_LIVE" = 1 ]; then
        echo "$scratch is not mounted" >&2
        exit 1
    fi
    [ -d "$scratch" ] || { echo "the root volume has no $scratch to mount scratch on" >&2; exit 1; }
    mtd=$(awk -F: '$2 ~ /"scratch"$/ { sub("mtd", "", $1); print $1 }' /proc/mtd)
    [ -n "$mtd" ] || { echo "no scratch partition in /proc/mtd" >&2; exit 1; }
    mode=rw
    [ "$UNFR0ZEN_DRY_RUN" = 1 ] && mode=ro
    mount -t jffs2 -o "$mode,noatime" "/dev/mtdblock$mtd" "$scratch"
    echo "Mounted mtd$mtd (scratch) at $scratch ($mode)"
elif [ "$UNFR0ZEN_LIVE" != 1 ] && [ "$UNFR0ZEN_DRY_RUN" != 1 ]; then
    # A dry run in this initramfs leaves it mounted read-only.
    mount -o remount,rw "$scratch"
fi
echo "Scratch: $(df -k "$scratch" | tail -n 1)"
