#!/bin/sh
# Run inside the thread build image, with the repository mounted at its host path.
set -eu
thread_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$thread_root"
SDK=${SDK:-$thread_root/.work/zigbee-sdk/gecko_sdk_suite/v2.6}
GSDK=${GSDK:-$thread_root/.work/zigbee-sdk/gecko_sdk}
OT=${OT:-$thread_root/.work/openthread}
OT_BUILD=${OT_BUILD:-$thread_root/.work/openthread-radio}
OT_BUILD_EM357=${OT_BUILD_EM357:-$thread_root/.work/openthread-radio-em357}
OT_HOST_BUILD=${OT_HOST_BUILD:-$thread_root/.work/openthread-host}
export NESTLOCAL_ZBWAKE=${NESTLOCAL_ZBWAKE:-$thread_root/build/zbwake}
sh "$thread_root/platform/diamond3/zbwake/build.sh" "$NESTLOCAL_ZBWAKE"
# nestlocal carries the radio images: it installs them for Zigbee.
export NESTLOCAL_RADIO=${NESTLOCAL_RADIO:-$thread_root/build/radio/radio.bin}
export NESTLOCAL_RADIO_EM357=${NESTLOCAL_RADIO_EM357:-$thread_root/build/radio-em357-zigbee/radio.bin}

cmake -S "$OT" -B "$OT_BUILD" -GNinja \
  -DCMAKE_TOOLCHAIN_FILE="$thread_root/platform/common/radio/thread/arm-toolchain.cmake" \
  -DOT_PLATFORM=external -DOT_FTD=OFF -DOT_MTD=OFF -DOT_RCP=ON \
  -DOT_APP_CLI=OFF -DOT_APP_NCP=OFF -DOT_APP_RCP=OFF \
  -DOT_LOG_LEVEL=NONE -DOT_THREAD_VERSION=1.1 \
  -DOT_PROJECT_CONFIG="$thread_root/platform/common/radio/em3581/openthread-rcp-config.h"
cmake --build "$OT_BUILD" --target openthread-rcp -j8
make -C platform/common/radio -j8 SDK="$SDK" GSDK="$GSDK" OT="$OT" OT_BUILD="$OT_BUILD" OUT="$thread_root/build/radio"
# The gen2's EM357 holds one personality at a time: a Zigbee image and a
# Thread one, the latter with OpenThread's buffers sized for its RAM.
cmake -S "$OT" -B "$OT_BUILD_EM357" -GNinja \
  -DCMAKE_TOOLCHAIN_FILE="$thread_root/platform/common/radio/thread/arm-toolchain.cmake" \
  -DOT_PLATFORM=external -DOT_FTD=OFF -DOT_MTD=OFF -DOT_RCP=ON \
  -DOT_APP_CLI=OFF -DOT_APP_NCP=OFF -DOT_APP_RCP=OFF \
  -DOT_LOG_LEVEL=NONE -DOT_THREAD_VERSION=1.1 \
  -DOT_PROJECT_CONFIG="$thread_root/platform/common/radio/em357/openthread-rcp-config.h"
cmake --build "$OT_BUILD_EM357" --target openthread-rcp -j8
make -C platform/common/radio -j8 PART=em357 PERSONALITY=zigbee SDK="$SDK" GSDK="$GSDK" OUT="$thread_root/build/radio-em357-zigbee"
make -C platform/common/radio -j8 PART=em357 PERSONALITY=thread SDK="$SDK" GSDK="$GSDK" OT="$OT" OT_BUILD="$OT_BUILD_EM357" \
  OUT="$thread_root/build/radio-em357-thread"
mkdir -p build/thread

cmake -S platform/common/radio/thread/host -B "$OT_HOST_BUILD/sleep" -GNinja \
  -DOT_SOURCE="$OT" \
  -DCMAKE_TOOLCHAIN_FILE="$thread_root/platform/common/radio/thread/linux-toolchain.cmake" \
  -DCMAKE_BUILD_TYPE=Release -DOT_LINKER_MAP=OFF \
  -DOT_PLATFORM=posix -DOT_FTD=ON -DOT_MTD=OFF -DOT_RCP=OFF \
  -DOT_APP_NCP=OFF -DOT_APP_RCP=OFF -DOT_DAEMON=ON -DOT_READLINE=OFF \
  -DOT_POSIX_RCP_HDLC_BUS=ON -DOT_PLATFORM_NETIF=ON \
  -DOT_SRP_CLIENT=ON -DOT_ECDSA=ON -DOT_THREAD_VERSION=1.3 \
  -DOT_LOG_LEVEL=INFO -DOT_LOG_LEVEL_DYNAMIC=ON
cmake --build "$OT_HOST_BUILD/sleep" --target ot-daemon ot-ctl -j8
for name in ot-daemon ot-ctl; do
  arm-none-eabi-strip -o "build/thread/$name" "$OT_HOST_BUILD/sleep/openthread/src/posix/$name"
done
nestlocal_target_dir=${NESTLOCAL_TARGET_DIR:-.work/nestlocal-target}
cargo zigbuild --locked -p nestlocal --bin nestlocal --bin thread-rcp --profile thermostat \
  --target armv7-unknown-linux-musleabihf --target-dir "$nestlocal_target_dir"
cp "$nestlocal_target_dir"/armv7-unknown-linux-musleabihf/thermostat/thread-rcp build/thread/
cp "$nestlocal_target_dir"/armv7-unknown-linux-musleabihf/thermostat/nestlocal build/thread/
