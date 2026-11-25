#!/bin/sh
# Builds zbwake.ko, the kernel module that makes the radio's nHOST_INT a wake
# source, for gen3 firmware releases: OUT/RELEASE.ko from Nest's published
# kernel source for each (nest-learning-thermostat/RELEASE/linux-imx for 5.6,
# linux-imx-4.1.15 for later releases), configured as Nest's release builds are
# (diamond3_defconfig and diamond3_release_fragment, with hung-task detection
# disabled on 5.6, matching the units' /proc/config.gz). The kernel
# checks the CRCs of the symbols a module uses; they come from genksyms over
# the objects that export them, so no kernel is built. Needs git, make, bc, a
# host gcc and arm-none-eabi-gcc (the build image has them); ZBWAKE_JOBS
# releases at a time (default 4).
#
#     platform/diamond3/zbwake/build.sh OUT RELEASE...    (the releases: releases)
#     platform/diamond3/zbwake/build.sh OUT               every release of releases whose
#                                       OUT/RELEASE.ko is missing or older than
#                                       the files in this directory
set -eu

[ $# -ge 1 ] || {
    echo "usage: $0 OUT [RELEASE...]" >&2
    exit 2
}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
shift
if [ $# -eq 0 ]; then
    # shellcheck disable=SC2013 # the releases are words
    for release in $(cat "$dir/releases"); do
        ko=$out/$release.ko
        [ -f "$ko" ] && [ -z "$(find "$dir" -newer "$ko")" ] || set -- "$@" "$release"
    done
    [ $# -gt 0 ] || {
        echo "zbwake.ko is up to date for every release"
        exit 0
    }
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export ARCH=arm
common_symbols="irq_set_irq_wake|printk|module_layout|__aeabi_unwind_cpp_pr0|request_threaded_irq|free_irq|wakeup_source_register|wakeup_source_unregister|__pm_wakeup_event"

build() { # build RELEASE
    release=$1
    tree=$work/$release
    # GCC 14 diagnoses annotations and host-tool patterns in these kernels.
    hostcc="gcc -fcommon -Wno-dangling-pointer"
    cflags="-Wno-attributes -Wno-missing-attributes -Wno-attribute-alias"
    case "$release" in
        5.6)
            repo=linux-imx
            compiler=4
            symbols="__gpio_to_irq|$common_symbols"
            crcs=10
            hostcc="$hostcc -Wno-unused-function"
            ;;
        *)
            repo=linux-imx-4.1.15
            compiler=5
            symbols="gpio_to_desc|gpiod_to_irq|$common_symbols"
            crcs=11
            ;;
    esac
    git clone -q --depth 1 "https://nest-open-source.googlesource.com/nest-learning-thermostat/$release/$repo" "$tree"
    cd "$tree"
    # These kernels select a header for the compiler's major version; newer
    # GCC versions use the latest header available in each tree. The host
    # tools predate -fno-common being the default.
    gcc_version=$(arm-none-eabi-gcc -dumpversion | cut -d. -f1)
    [ -f "include/linux/compiler-gcc$gcc_version.h" ] ||
        cp "include/linux/compiler-gcc$compiler.h" "include/linux/compiler-gcc$gcc_version.h"
    # With module versioning the kernel skips the release in vermagic, so
    # releases with the same CRCs get the same module.
    m() { make -s CROSS_COMPILE=arm-none-eabi- HOSTCC="$hostcc" KCFLAGS="$cflags" LOCALVERSION= "$@"; }
    scripts/kconfig/merge_config.sh -m arch/arm/configs/diamond3_defconfig \
        arch/arm/configs/diamond3_release_fragment >/dev/null
    [ "$release" != 5.6 ] || scripts/config --disable DETECT_HUNG_TASK
    m olddefconfig
    : >.scmversion
    m modules_prepare
    exporters=$(grep -lrE "EXPORT_SYMBOL(_GPL)?\(($symbols)\)" kernel drivers/gpio drivers/base/power arch/arm/kernel | sed 's/\.c$/.o/')
    # shellcheck disable=SC2086 # one object per word
    m $exporters
    for o in $exporters; do arm-none-eabi-nm "$o"; done |
        sed -nE "s/^([0-9a-f]+) . __crc_($symbols)\$/0x\1\t\2\tvmlinux\tEXPORT_SYMBOL/p" >Module.symvers
    [ "$(wc -l <Module.symvers)" -eq "$crcs" ] || {
        echo "expected $crcs CRCs:"
        cat Module.symvers
        return 1
    }
    mkdir "$tree-module"
    cp "$dir/zbwake.c" "$dir/Kbuild" "$tree-module/"
    m M="$tree-module" modules
    cp "$tree-module/zbwake.ko" "$out/$release.ko"
    cd /
    rm -rf "$tree" "$tree-module"
}

pids=
reap() {
    for job in $pids; do
        wait "${job%%:*}" || {
            echo "zbwake.ko for ${job#*:} failed:"
            tail -n 30 "$work/${job#*:}.log"
            exit 1
        }
        echo "built zbwake.ko for ${job#*:}"
    done
    pids=
}
for release in "$@"; do
    (build "$release") >"$work/$release.log" 2>&1 &
    pids="$pids $!:$release"
    # shellcheck disable=SC2086 # one job per word
    [ "$(echo $pids | wc -w)" -lt "${ZBWAKE_JOBS:-4}" ] || reap
done
reap
