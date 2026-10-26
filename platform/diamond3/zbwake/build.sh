#!/bin/sh
# Builds zbwake.ko, the kernel module that makes the radio's nHOST_INT a wake
# source, for gen3 firmware releases: OUT/RELEASE.ko from Nest's published
# kernel source for each (nest-learning-thermostat/RELEASE/linux-imx-4.1.15),
# configured as Nest's release builds are (diamond3_defconfig and
# diamond3_release_fragment, which give the units' /proc/config.gz). The kernel
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

[ $# -ge 1 ] || { echo "usage: $0 OUT [RELEASE...]" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
shift
if [ $# -eq 0 ]; then
    for release in $(cat "$dir/releases"); do
        ko=$out/$release.ko
        [ -f "$ko" ] && [ -z "$(find "$dir" -newer "$ko")" ] || set -- "$@" "$release"
    done
    [ $# -gt 0 ] || { echo "zbwake.ko is up to date for every release"; exit 0; }
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export ARCH=arm
symbols="gpio_to_desc|gpiod_to_irq|irq_set_irq_wake|printk|module_layout|__aeabi_unwind_cpp_pr0|request_threaded_irq|free_irq|wakeup_source_register|wakeup_source_unregister|__pm_wakeup_event"

build() {  # build RELEASE
    release=$1
    tree=$work/$release
    git clone -q --depth 1 "https://nest-open-source.googlesource.com/nest-learning-thermostat/$release/linux-imx-4.1.15" "$tree"
    cd "$tree"
    # 4.1 includes the header for the compiler's major version and has them
    # up to GCC 5; later ones build it as 5 does. The host tools predate
    # -fno-common being the default.
    gcc_version=$(arm-none-eabi-gcc -dumpversion | cut -d. -f1)
    [ -f "include/linux/compiler-gcc$gcc_version.h" ] \
        || cp include/linux/compiler-gcc5.h "include/linux/compiler-gcc$gcc_version.h"
    # vermagic says 4.1.15 for every release: with module versioning the
    # kernel skips the release in it, so releases with the same CRCs get the
    # same module.
    m() { make -s CROSS_COMPILE=arm-none-eabi- HOSTCC="gcc -fcommon" LOCALVERSION= "$@"; }
    scripts/kconfig/merge_config.sh -m arch/arm/configs/diamond3_defconfig \
        arch/arm/configs/diamond3_release_fragment > /dev/null
    m olddefconfig
    : > .scmversion
    m modules_prepare
    exporters=$(grep -lrE "EXPORT_SYMBOL(_GPL)?\(($symbols)\)" kernel drivers/gpio drivers/base/power arch/arm/kernel | sed 's/\.c$/.o/')
    m $exporters
    for o in $exporters; do arm-none-eabi-nm "$o"; done |
        sed -nE "s/^([0-9a-f]+) . __crc_($symbols)\$/0x\1\t\2\tvmlinux\tEXPORT_SYMBOL/p" > Module.symvers
    [ "$(wc -l < Module.symvers)" -eq 11 ] || { echo "expected 11 CRCs:"; cat Module.symvers; return 1; }
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
        wait "${job%%:*}" || { echo "zbwake.ko for ${job#*:} failed:"; tail -n 30 "$work/${job#*:}.log"; exit 1; }
        echo "built zbwake.ko for ${job#*:}"
    done
    pids=
}
for release in "$@"; do
    (build "$release") > "$work/$release.log" 2>&1 &
    pids="$pids $!:$release"
    [ "$(echo $pids | wc -w)" -lt "${ZBWAKE_JOBS:-4}" ] || reap
done
reap
