#!/bin/sh
# Builds OUT/uImage, the j49's kernel with a USB serial port: Nest's published
# 5.9.4 source (2.6.37), configured as its release build is
# (configs/j49/release_defconfig, which gives the units' /proc/config.gz)
# plus CONFIG_USB_DYNAMIC_ACM. The built-in dynamic gadget then offers ACM
# beside mass storage: ttyGS0 on the unit. Nothing else changes, so the
# symbol versions the unit's own modules (wl12xx) were built against stay
# the same.
#
# Built with Ubuntu 14.04's arm-linux-gnueabihf-gcc 4.8 (the units' kernel
# reports gcc 4.8.2) in Docker, inside a volume: the tree has file names that
# differ only in case, which a macOS checkout cannot hold. The source is
# fetched on the host into .work/j49. Also OUT/zbwake.ko, zbwake/zbwake.c
# built against it. Skipped when both are newer than this script and the
# module's source.
#
#     platform/j49/kernel.sh OUT
set -eu

[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname "$(dirname "$dir")")
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
module=$dir/zbwake/zbwake.c
if [ -f "$out/uImage" ] && [ "$out/uImage" -nt "$0" ] && [ -f "$out/zbwake.ko" ] && [ "$out/zbwake.ko" -nt "$0" ] \
    && [ "$out/zbwake.ko" -nt "$module" ]; then
    echo "$out/uImage and $out/zbwake.ko are up to date"
    exit 0
fi
command -v docker > /dev/null || { echo "The j49 kernel builds in Docker, which is missing." >&2; exit 1; }

commit=34a29f48414fe242f3fd2f77017f65b240e278be
image=ubuntu@sha256:64483f3496c1373bfd55348e88694d1c4d0c9b660dee6bfef5e12f43b9933b30
source=$root/.work/j49/linux-5.9.4
if [ "$(git -C "$source" rev-parse -q --verify HEAD 2> /dev/null)" != "$commit" ]; then
    rm -rf "$source"
    git clone -q https://nest-open-source.googlesource.com/nest-learning-thermostat/5.9.4/linux "$source" 2> /dev/null
    git -C "$source" checkout -q --detach "$commit"
fi

volume=unfr0zen-j49-kernel
docker volume create "$volume" > /dev/null
git -C "$source" archive --format=tar "$commit" |
    docker run --rm -i -v "$volume:/vol" "$image" sh -ec 'rm -rf /vol/src /vol/out; mkdir -p /vol/src /vol/out; tar -x -C /vol/src'
docker run --rm -v "$volume:/vol" -v "$out:/out" -v "$module:/zbwake/zbwake.c:ro" "$image" sh -ec '
    apt-get update -qq > /dev/null
    apt-get install -y -qq gcc-arm-linux-gnueabihf gcc make bc perl u-boot-tools > /dev/null 2>&1
    k="make -C /vol/src/linux O=/vol/out ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf-"
    sed "s/^# CONFIG_USB_DYNAMIC_ACM is not set\$/CONFIG_USB_DYNAMIC_ACM=y/" /vol/src/configs/j49/release_defconfig > /vol/out/.config
    yes "" | $k oldconfig > /vol/out/oldconfig.log
    grep -q "^CONFIG_USB_DYNAMIC_ACM=y\$" /vol/out/.config
    $k -j"$(nproc)" zImage modules > /vol/out/make.log 2>&1 || { tail -n 40 /vol/out/make.log; exit 1; }
    mkdir -p /vol/zbwake
    cp /zbwake/zbwake.c /vol/zbwake/zbwake.c
    echo "obj-m := zbwake.o" > /vol/zbwake/Kbuild
    $k M=/vol/zbwake modules > /vol/out/zbwake.log 2>&1 || { tail -n 40 /vol/out/zbwake.log; exit 1; }
    cp /vol/zbwake/zbwake.ko /out/zbwake.ko.partial
    # As the units boot0 image: loaded and entered at 0x80008000.
    mkimage -A arm -O linux -T kernel -C none -a 0x80008000 -e 0x80008000 -n Linux-2.6.37 \
        -d /vol/out/arch/arm/boot/zImage /out/uImage.partial > /dev/null
    chown '"$(id -u):$(id -g)"' /out/uImage.partial /out/zbwake.ko.partial'
mv "$out/uImage.partial" "$out/uImage"
mv "$out/zbwake.ko.partial" "$out/zbwake.ko"
echo "Built $out/uImage ($(wc -c < "$out/uImage") bytes)"
