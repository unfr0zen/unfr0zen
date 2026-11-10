#!/bin/sh
# Builds libwakefilter.so, the library preloaded into the gen 3's nlsleepd
# (wakefilter.c), into OUT, against the glibc the units run. Needs zig, as
# python3's ziglang module (the build image has it).
#
#     platform/diamond3/wakefilter/build.sh OUT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
python3 -m ziglang cc -target arm-linux-gnueabihf.2.28 -mcpu=cortex_a9 -Os -Wall -Wextra -Werror \
    -fno-stack-protector -shared -Wl,-soname,libwakefilter.so -o "$1/libwakefilter.so" "$dir/wakefilter.c" -ldl
