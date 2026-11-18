#!/bin/sh
# nestlocal and thread-rcp for the thermostat, ARMv7 binaries against the
# gen 1's and gen 2's glibc 2.19 (which the gen 3's 2.31 also runs; the gen 3's GPU
# libraries, which nestlocal loads at run time for the theme, are glibc
# shared objects) into build/thread, with nestlocal copied to
# build/nestlocal-armv7 for the app.
# nestlocal carries the radio images (radio.sh) and zbwake.ko for each release
# (platform/diamond3/zbwake/build.sh, the j49's beside them as j49.ko): their
# paths are NESTLOCAL_RADIO, NESTLOCAL_RADIO_EM357 and NESTLOCAL_ZBWAKE.
# libwakefilter.so (platform/common/wakefilter), which the app installs beside
# nestlocal for Matter, builds here too, into build/wakefilter, and so does
# liblocalcloud.so (platform/common/localcloud), preloaded into nlclient
# beside nestlocal, into build/localcloud, and what draws customizable themes
# (platform/common/theme) into build/theme. Run inside the build
# image, with the repository mounted at its host path.
set -eu
thread_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$thread_root"
export NESTLOCAL_ZBWAKE="${NESTLOCAL_ZBWAKE:-$thread_root/build/zbwake}"
export NESTLOCAL_RADIO="${NESTLOCAL_RADIO:-$thread_root/build/radio/radio.bin}"
export NESTLOCAL_RADIO_EM357="${NESTLOCAL_RADIO_EM357:-$thread_root/build/radio-em357-zigbee/radio.bin}"
nestlocal_target_dir=${NESTLOCAL_TARGET_DIR:-.work/nestlocal-target}
mkdir -p build/thread
sh platform/common/wakefilter/build.sh build/wakefilter
sh platform/common/localcloud/build.sh build/localcloud
sh platform/common/theme/build.sh build/theme
# For the Cortex-A8 (the gen 1's and gen 2's; the gen 3's A9 runs it too).
# A fixed-address executable, bound lazily: their glibc 2.19 loader
# crashes binding a Rust binary now, and its 2.6.37 kernel maps a PIE among
# the libraries, where the loader crashes now and then (client-ng's
# findings, whose build this follows).
target=armv7-unknown-linux-gnueabihf
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C link-arg=-Wl,-z,lazy -C relocation-model=static -C link-arg=-no-pie -C target-cpu=cortex-a8"
cargo zigbuild --locked -p nestlocal --bin nestlocal --bin thread-rcp --profile thermostat \
    --target $target.2.19 --target-dir "$nestlocal_target_dir"
cp "$nestlocal_target_dir"/$target/thermostat/thread-rcp build/thread/
cp "$nestlocal_target_dir"/$target/thermostat/nestlocal build/thread/
cp build/thread/nestlocal build/nestlocal-armv7
