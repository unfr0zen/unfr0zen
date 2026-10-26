#!/bin/sh
# Builds Doom (doomgeneric with doomgeneric_nest.c) for the gen 3 and the
# gen 2 as one static ARMv7 binary, as nestlocal is: build/doom-armv7.
# scripts/build-firmware.sh runs it with the firmware; the app embeds the
# binary and installs it for --doom. The WAD is the app's
# (crates/unfr0zen-host/build.rs).
#
#     sh platform/common/doom/build.sh OUTPUT
set -eu
dir=platform/common/doom
root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
cd "$root"
out=${1:-build/doom-armv7}
src=$dir/doomgeneric/doomgeneric
[ -f "$src/doomgeneric.c" ] || { echo "$dir/doomgeneric is empty: git submodule update --init $dir/doomgeneric" >&2; exit 1; }

# The game's sources, as doomgeneric's Makefile lists them, with this
# platform in place of its X11 one.
objs=$(sed -n 's/^SRC_DOOM *= *//p' "$src/Makefile" | sed 's/doomgeneric_xlib\.o//')
sources=$(for o in $objs; do printf '%s ' "$src/${o%.o}.c"; done)
mkdir -p "$(dirname "$out")"
# Plain ARMv7-A, which both the gen 3's Cortex-A9 and the gen 2's A8 run.
cc() {
    if command -v zig > /dev/null 2>&1; then
        zig cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@" -fno-sanitize=all
    else
        python3 -m ziglang cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@" -fno-sanitize=all
    fi
}
# shellcheck disable=SC2086
cc -O2 -static -s -w -I"$src" -I"$dir/compat" \
    -DNORMALUNIX -DLINUX -D_DEFAULT_SOURCE -DFEATURE_SOUND -DDOOMGENERIC_RESX=320 -DDOOMGENERIC_RESY=200 \
    $sources "$dir/doomgeneric_nest.c" -o "$out" -lm -lpthread
