#!/bin/sh
# Builds the UNFR0ZEN screen and optional j49 RCS screen as relocatable ARMv7
# objects: the app links them against the unit's nlclient, so no compiler or
# linker is needed when the finished app patches a unit. Needs clang.
#
#     sh platform/common/nlclient/build.sh OUT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
for name in custom_screen rcs_screen; do
    case "$name" in custom_screen) output=custom-screen.o ;; rcs_screen) output=j49-rcs-screen.o ;; esac
    clang --target=arm-linux-gnueabihf -march=armv7-a -marm -mfpu=vfpv3-d16 -mfloat-abi=hard \
        -std=c11 -O2 -fPIE -ffreestanding -fno-builtin -fno-stack-protector -fno-unwind-tables \
        -fno-asynchronous-unwind-tables -Wall -Wextra -Werror -c "$dir/src/$name.c" -o "$1/$output"
done
