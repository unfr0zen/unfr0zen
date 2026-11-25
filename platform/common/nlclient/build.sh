#!/bin/sh
# Builds unfr0zen's settings screens and optional j49 RCS screen as relocatable ARMv7
# objects: the app links them against the unit's nlclient, so no compiler or
# linker is needed when the finished app sets up a unit. Needs clang.
#
#     sh platform/common/nlclient/build.sh OUT
set -eu
[ $# -eq 1 ] || {
    echo "usage: $0 OUT" >&2
    exit 2
}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
for variant in custom_screen custom_screen_legacy rcs_screen; do
    flags=
    case "$variant" in
        custom_screen)
            name=custom_screen
            output=custom-screen.o
            ;;
        custom_screen_legacy)
            name=custom_screen
            output=custom-screen-legacy.o
            flags=-DCUSTOM_LEGACY_ABI
            ;;
        rcs_screen)
            name=rcs_screen
            output=j49-rcs-screen.o
            ;;
    esac
    clang --target=arm-linux-gnueabihf -march=armv7-a -marm -mfpu=vfpv3-d16 -mfloat-abi=hard \
        -std=c11 -O2 -fPIE -ffreestanding -fno-builtin -fno-stack-protector -fno-unwind-tables \
        -fno-asynchronous-unwind-tables -Wall -Wextra -Werror $flags -c "$dir/src/$name.c" -o "$1/$output"
done
