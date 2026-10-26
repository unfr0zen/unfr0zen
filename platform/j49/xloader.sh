#!/bin/sh
# Builds OUT/x-load.bin, the x-loader the selected OMAP board's ROM takes over USB:
# Nest's published 5.9.4 x-loader, configured as BOARD-usb-loader, which
# brings up the clocks and SDRAM, then loads files into SDRAM over USB and
# jumps to them (drivers/usb/usb.c).
#
# Built as kernel.sh builds the kernel, with Ubuntu 14.04's
# arm-linux-gnueabihf-gcc 4.8 in Docker, in ARM state: the units' own
# x-loader is ARM code, and the board's assembly (platform.S) is called with
# plain bl, which from Thumb code would run it in the wrong state. Skipped
# when OUT/x-load.bin is newer than this script.
#
#     platform/j49/xloader.sh OUT [j49|diamond]
set -eu

[ $# -ge 1 ] && [ $# -le 2 ] || { echo "usage: $0 OUT [j49|diamond]" >&2; exit 2; }
board=${2:-j49}
case "$board" in j49|diamond) ;; *) echo "unknown board: $board" >&2; exit 2;; esac
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname "$(dirname "$dir")")
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
if [ -f "$out/x-load.bin" ] && [ "$out/x-load.bin" -nt "$0" ]; then
    echo "$out/x-load.bin is up to date"
    exit 0
fi
command -v docker > /dev/null || { echo "The OMAP x-loader builds in Docker, which is missing." >&2; exit 1; }

commit=9b7bb7281b715544e6f5201390d8dac416a4a494
image=ubuntu@sha256:64483f3496c1373bfd55348e88694d1c4d0c9b660dee6bfef5e12f43b9933b30
source=$root/.work/j49/x-loader
if [ "$(git -C "$source" rev-parse -q --verify HEAD 2> /dev/null)" != "$commit" ]; then
    rm -rf "$source"
    git clone -q https://nest-open-source.googlesource.com/nest-learning-thermostat/5.9.4/x-loader "$source" 2> /dev/null
    git -C "$source" checkout -q --detach "$commit"
fi

git -C "$source" archive --format=tar "$commit" x-loader | docker run --rm -i -e BOARD="$board" -v "$out:/out" "$image" sh -ec '
    apt-get update -qq > /dev/null
    apt-get install -y -qq gcc-arm-linux-gnueabihf gcc make > /dev/null 2>&1
    mkdir /src
    tar -x -C /src
    cd /src/x-loader
    m="make ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf-"
    $m CC="arm-linux-gnueabihf-gcc -marm" "$BOARD-usb-loader_config" > /dev/null
    $m CC="arm-linux-gnueabihf-gcc -marm" > make.log 2>&1 || { tail -n 40 make.log; exit 1; }
    cp x-load.bin /out/x-load.bin.partial
    chown '"$(id -u):$(id -g)"' /out/x-load.bin.partial'
mv "$out/x-load.bin.partial" "$out/x-load.bin"
echo "Built $out/x-load.bin ($(wc -c < "$out/x-load.bin") bytes)"
