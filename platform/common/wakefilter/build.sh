#!/bin/sh
# Builds libwakefilter.so, the library preloaded into nlsleepd (wakefilter.c),
# into OUT: one for every unit, so for the gen 2's Cortex-A8 and with nothing
# of glibc newer than its 2.19. Needs zig, on PATH or as python3's ziglang
# module (the build image has the module).
#
#     platform/common/wakefilter/build.sh OUT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
if command -v zig > /dev/null 2>&1; then zig=zig; else zig="python3 -m ziglang"; fi
$zig cc -target arm-linux-gnueabihf.2.28 -mcpu=cortex_a8 -Os -Wall -Wextra -Werror \
    -fno-stack-protector -shared -Wl,-z,lazy -Wl,-soname,libwakefilter.so -o "$1/libwakefilter.so" "$dir/wakefilter.c" -ldl
