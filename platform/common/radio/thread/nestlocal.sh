#!/bin/sh
# nestlocal and thread-rcp for the thermostat, static ARMv7 binaries into
# build/thread, with nestlocal copied to build/nestlocal-armv7 for the app.
# nestlocal carries the radio images (radio.sh) and zbwake.ko for each release
# (platform/diamond3/zbwake/build.sh, the j49's beside them as j49.ko): their
# paths are NESTLOCAL_RADIO, NESTLOCAL_RADIO_EM357 and NESTLOCAL_ZBWAKE.
# libwakefilter.so (platform/common/wakefilter), which the app installs beside
# nestlocal for Matter, builds here too, into build/wakefilter. Run inside the
# build image, with the repository mounted at its host path.
set -eu
thread_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$thread_root"
export NESTLOCAL_ZBWAKE=${NESTLOCAL_ZBWAKE:-$thread_root/build/zbwake}
export NESTLOCAL_RADIO=${NESTLOCAL_RADIO:-$thread_root/build/radio/radio.bin}
export NESTLOCAL_RADIO_EM357=${NESTLOCAL_RADIO_EM357:-$thread_root/build/radio-em357-zigbee/radio.bin}
nestlocal_target_dir=${NESTLOCAL_TARGET_DIR:-.work/nestlocal-target}
mkdir -p build/thread
sh platform/common/wakefilter/build.sh build/wakefilter
cargo zigbuild --locked -p nestlocal --bin nestlocal --bin thread-rcp --profile thermostat \
  --target armv7-unknown-linux-musleabihf --target-dir "$nestlocal_target_dir"
cp "$nestlocal_target_dir"/armv7-unknown-linux-musleabihf/thermostat/thread-rcp build/thread/
cp "$nestlocal_target_dir"/armv7-unknown-linux-musleabihf/thermostat/nestlocal build/thread/
cp build/thread/nestlocal build/nestlocal-armv7
