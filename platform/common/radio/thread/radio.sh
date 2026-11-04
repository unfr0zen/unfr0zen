#!/bin/sh
# The radio's firmware, from Silicon Labs' SDK (fetch-sdk.sh) and OpenThread
# (fetch-openthread.sh) at their pinned commits: the gen3's EM3581 image into
# build/radio, and the gen2's EM357 Zigbee and Thread personalities into
# build/radio-em357-zigbee and build/radio-em357-thread. Run inside the build
# image, with the repository mounted at its host path.
set -eu
thread_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$thread_root"
SDK=${SDK:-$thread_root/.work/zigbee-sdk/gecko_sdk_suite/v2.6}
GSDK=${GSDK:-$thread_root/.work/zigbee-sdk/gecko_sdk}
OT=${OT:-$thread_root/.work/openthread}
OT_BUILD=${OT_BUILD:-$thread_root/.work/openthread-radio}
OT_BUILD_EM357=${OT_BUILD_EM357:-$thread_root/.work/openthread-radio-em357}

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
