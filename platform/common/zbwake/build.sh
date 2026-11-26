#!/bin/sh
# Builds zbwake.ko, the kernel module that makes the radio's nHOST_INT a wake
# source, for the supported boards' firmware releases. The board's zbwake
# directory names the kernel source, output prefix and GPIO configuration.
# The kernel is configured with each board's defconfig and release
# fragment, which give the units' /proc/config.gz; the gen3's 5.6 predates
# linux-imx-4.1.15 and builds from its linux-imx, with hung-task detection
# disabled as on those units. The kernel
# checks the CRCs of the symbols a module uses; they come from genksyms over
# the objects that export them, so no kernel is built. Needs git, make, bc, a
# host gcc and arm-none-eabi-gcc (the build image has them); ZBWAKE_JOBS
# releases at a time (default 4).
#
#     platform/common/zbwake/build.sh OUT RELEASE...   (board-prefixed when required)
#     platform/common/zbwake/build.sh OUT              every listed release whose
#                                       module is missing or older than
#                                       the files in this directory
set -eu

[ $# -ge 1 ] || {
    echo "usage: $0 OUT [RELEASE...]" >&2
    exit 2
}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
platform=$(CDPATH= cd -- "$dir/../.." && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
shift
board_dir() {
    case "$1" in
        *-*) board=${1%%-*} ;;
        *) board=diamond3 ;; # The original, unprefixed module names.
    esac
    printf '%s/%s/zbwake\n' "$platform" "$board"
}
if [ $# -eq 0 ]; then
    # shellcheck disable=SC2013 # the releases are words
    for release in $(
        for config in "$platform"/*/zbwake/board.conf; do
            prefix=
            # shellcheck source=/dev/null
            . "$config"
            sed "s/^/$prefix/" "$(dirname "$config")/releases"
        done
    ); do
        ko=$out/$release.ko
        [ -f "$ko" ] && [ -z "$(find "$dir" "$(board_dir "$release")" -newer "$ko")" ] || set -- "$@" "$release"
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
    config_dir=$(board_dir "$release")
    board=$(basename "$(dirname "$config_dir")")
    prefix=
    source=
    # shellcheck source=/dev/null
    . "$config_dir/board.conf"
    version=${release#"$prefix"}
    # GCC 14 diagnoses annotations and host-tool patterns in these kernels.
    hostcc="gcc -fcommon -Wno-dangling-pointer"
    cflags="-Wno-attributes -Wno-missing-attributes -Wno-attribute-alias"
    compiler=5
    symbols="gpio_to_desc|gpiod_to_irq|$common_symbols"
    crcs=11
    if [ "$board" = diamond3 ] && [ "$version" = 5.6 ]; then
        source=linux-imx
        compiler=4
        symbols="__gpio_to_irq|$common_symbols"
        crcs=10
        hostcc="$hostcc -Wno-unused-function"
    fi
    git clone -q --depth 1 "https://nest-open-source.googlesource.com/nest-learning-thermostat/$version/$source" "$tree"
    cd "$tree"
    # These kernels select a header for the compiler's major version; newer
    # GCC versions use the latest header available in each tree. The host
    # tools predate -fno-common being the default.
    gcc_version=$(arm-none-eabi-gcc -dumpversion | cut -d. -f1)
    if [ -f "include/linux/compiler-gcc$compiler.h" ] && [ ! -f "include/linux/compiler-gcc$gcc_version.h" ]; then
        cp "include/linux/compiler-gcc$compiler.h" "include/linux/compiler-gcc$gcc_version.h"
    fi
    # With module versioning the kernel skips the release in vermagic, so
    # releases with the same CRCs get the same module.
    m() { make -s CROSS_COMPILE=arm-none-eabi- HOSTCC="$hostcc" KCFLAGS="$cflags" LOCALVERSION= "$@"; }
    scripts/kconfig/merge_config.sh -m "arch/arm/configs/${board}_defconfig" \
        "arch/arm/configs/${board}_release_fragment" >/dev/null
    [ "$source" != linux-imx ] || scripts/config --disable DETECT_HUNG_TASK
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
    cp "$dir/zbwake.c" "$dir/Kbuild" "$config_dir/board.h" "$tree-module/"
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
