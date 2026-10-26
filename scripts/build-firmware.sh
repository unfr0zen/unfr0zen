#!/bin/sh
# Build what the app installs on the thermostat, into build/: the diamond3's
# ROM bootstrap, boot wrapper and recovery U-Boot, the OMAP boards' x-loaders, NAND
# flasher, dropbear and kernel with a USB serial port, the radio's firmware and zbwake.ko for each
# gen3 release, nestlocal for ARMv7 carrying them, Doom, and the UNFR0ZEN
# screen's object for the app to link into the unit's nlclient. The build logs
# go to .work/.
#
#     scripts/build-firmware.sh
#
# On macOS this builds inside Docker, from Dockerfile; on Linux with the native
# toolchains (see README.md). JOBS=N limits the parallel make jobs (default 8).
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
mkdir -p .work build
nestlocal_target=armv7-unknown-linux-musleabihf
export NESTLOCAL_TARGET_DIR=${NESTLOCAL_TARGET_DIR:-.work/nestlocal-target}

if [ "$(uname -s)" = Darwin ]; then
    command -v docker > /dev/null || { echo "Missing build tool: docker. See README.md." >&2; exit 1; }
    image=unfr0zen-build
    echo "Building Docker image $image"
    docker build -q -t "$image" - < Dockerfile > /dev/null
    # The repository at the same path inside, so the submodule's and .work's
    # paths stay valid on both sides; cargo keeps its downloads in .work.
    run() {
        docker run --rm -v "$root:$root" -w "$root" --user "$(id -u):$(id -g)" \
            -e HOME=/tmp -e CARGO_HOME="$root/.work/cargo" -e JOBS -e NESTLOCAL_RADIO -e NESTLOCAL_RADIO_EM357 -e NESTLOCAL_ZBWAKE -e NESTLOCAL_TARGET_DIR "$image" "$@"
    }
else
    missing=
    for tool in git make gcc bc bison flex python3 clang cmake ninja arm-none-eabi-gcc arm-none-eabi-as arm-none-eabi-ld \
                arm-none-eabi-objcopy arm-none-eabi-nm arm-none-eabi-objdump cargo cargo-zigbuild; do
        command -v "$tool" > /dev/null || missing="$missing $tool"
    done
    [ -z "$missing" ] || { echo "Missing build tools:$missing. See README.md." >&2; exit 1; }
    python3 -c 'import PIL' 2> /dev/null || { echo "The boot logo needs Pillow (python3-pil). See README.md." >&2; exit 1; }
    if command -v rustup > /dev/null; then
        rustup target add "$nestlocal_target" > /dev/null 2>&1 || true
    fi
    run() { "$@"; }
fi

step() {  # step NAME COMMAND...: a build step, its output in .work/build-NAME.log
    name=$1
    shift
    log=.work/build-$name.log
    echo "Building $name; log: $log"
    run "$@" > "$log" 2>&1 || { tail -n 40 "$log" >&2; exit 1; }
}

run python3 platform/diamond3/bootstrap/build_bootstrap.py
step recovery sh platform/diamond3/uboot/build_uboot.sh
step j49 sh platform/j49/build.sh build/j49
step j49-dropbear sh platform/j49/dropbear.sh build/j49
# The j49's x-loader and kernel with a USB serial port build in Docker
# containers of their own, from the host. Without Docker (and no build of
# them already) the app is built without them: it reaches no j49 without the
# x-loader, and --usb-shell is unavailable on a j49 without the kernel.
if command -v docker > /dev/null || [ -f build/j49/x-load.bin ]; then
    echo "Building j49-xloader; log: .work/build-j49-xloader.log"
    sh platform/j49/xloader.sh build/j49 > .work/build-j49-xloader.log 2>&1 || { tail -n 40 .work/build-j49-xloader.log >&2; exit 1; }
else
    echo "Skipping the j49 x-loader: it needs Docker; the app will reach no j49" >&2
fi
if command -v docker > /dev/null || [ -f build/diamond/x-load.bin ]; then
    echo "Building diamond-xloader; log: .work/build-diamond-xloader.log"
    sh platform/j49/xloader.sh build/diamond diamond > .work/build-diamond-xloader.log 2>&1 || { tail -n 40 .work/build-diamond-xloader.log >&2; exit 1; }
else
    echo "Skipping the diamond x-loader: it needs Docker; the app will reach no diamond" >&2
fi
if command -v docker > /dev/null || [ -f build/j49/uImage ]; then
    echo "Building j49-kernel; log: .work/build-j49-kernel.log"
    sh platform/j49/kernel.sh build/j49 > .work/build-j49-kernel.log 2>&1 || { tail -n 40 .work/build-j49-kernel.log >&2; exit 1; }
else
    echo "Skipping the j49 kernel: it needs Docker; --usb-shell will be unavailable on a j49" >&2
fi
# The radio's firmware, from Silicon Labs' SDK and OpenThread at pinned
# commits, is built by the Thread step below; nestlocal carries it.
echo "Fetching the radio's SDK into .work/zigbee-sdk"
run sh platform/common/radio/fetch-sdk.sh .work/zigbee-sdk > /dev/null
export NESTLOCAL_RADIO="$root/build/radio/radio.bin"
export NESTLOCAL_RADIO_EM357="$root/build/radio-em357-zigbee/radio.bin"
# zbwake.ko for each gen3 release: those missing or older than their sources.
step zbwake sh platform/diamond3/zbwake/build.sh build/zbwake
# The j49's, built with its kernel, beside them for nestlocal to carry.
[ ! -f build/j49/zbwake.ko ] || cp build/j49/zbwake.ko build/zbwake/j49.ko
export NESTLOCAL_ZBWAKE="$root/build/zbwake"
step thread-source sh platform/common/radio/thread/fetch-openthread.sh
step thread sh platform/common/radio/thread/build.sh
cp build/thread/nestlocal build/nestlocal-armv7
step doom sh platform/common/doom/build.sh build/doom-armv7
# One relocatable object for every firmware build: the app links it at the
# addresses and layouts discovered in the unit's nlclient. No compiler or
# linker is required when the finished app patches a unit.
step custom-screen clang --target=arm-linux-gnueabihf -march=armv7-a -marm -mfpu=vfpv3-d16 -mfloat-abi=hard \
    -std=c11 -O2 -fPIE -ffreestanding -fno-builtin -fno-stack-protector -fno-unwind-tables \
    -fno-asynchronous-unwind-tables -Wall -Wextra -Werror -c platform/common/nlclient/src/custom_screen.c -o build/custom-screen.o
echo "Built the ROM bootstrap, boot wrapper, recovery U-Boot, the OMAP x-loaders and flasher, the radio's firmware, zbwake.ko, nestlocal, Doom and the screen object under build/."
