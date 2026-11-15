#!/bin/sh
# Runs on the unit after unfr0zen-apply's writes to $UNFR0ZEN_ROOT are
# verified on flash; failing, it fails the run. $UNFR0ZEN_CHANGES is how
# many changes the setup made.
set -eu
echo "unfr0zen $UNFR0ZEN_VERSION: $UNFR0ZEN_CHANGES changes"

root=${UNFR0ZEN_ROOT%/}
# Directories the files moved to scratch leave empty on the root volume,
# which the initramfs has read-only again by now.
for dir in "$root/usr/share/doom" "$root/usr/share/nestlocal"; do
    [ -d "$dir" ] && [ -z "$(ls -A "$dir")" ] || continue
    if [ "$UNFR0ZEN_LIVE" = 1 ]; then
        rmdir "$dir"
    else
        # Read-only again whether or not the directory went.
        mount -o remount,rw "$root/"
        removed=0
        rmdir "$dir" || removed=$?
        sync
        mount -o remount,ro "$root/"
        [ "$removed" = 0 ] || exit "$removed"
    fi
    echo "Removed the empty ${dir#"$root"}"
done

sync
# The initramfs's own scratch mount; a running unit keeps its own.
if [ "$UNFR0ZEN_LIVE" != 1 ] && awk -v dir="$root/media/scratch" '$2 == dir { found = 1 } END { exit !found }' /proc/mounts; then
    umount "$root/media/scratch"
fi
