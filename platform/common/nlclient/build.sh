#!/bin/sh
# Builds OUT/custom-screen.o, the UNFR0ZEN screen as one relocatable ARMv7
# object for every firmware build: the app links it at the addresses and
# layouts discovered in the unit's nlclient, so no compiler or linker is
# needed when the finished app patches a unit. Needs clang.
#
#     sh platform/common/nlclient/build.sh OUT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
clang --target=arm-linux-gnueabihf -march=armv7-a -marm -mfpu=vfpv3-d16 -mfloat-abi=hard \
    -std=c11 -O2 -fPIE -ffreestanding -fno-builtin -fno-stack-protector -fno-unwind-tables \
    -fno-asynchronous-unwind-tables -Wall -Wextra -Werror -c "$dir/src/custom_screen.c" -o "$1/custom-screen.o"
