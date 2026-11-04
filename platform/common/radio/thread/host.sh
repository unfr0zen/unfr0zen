#!/bin/sh
# OpenThread's host side for the thermostat, from the pinned source
# (fetch-openthread.sh): ot-daemon and ot-ctl as static ARMv7 binaries into
# build/thread. Run inside the build image, with the repository mounted at its
# host path.
set -eu
thread_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../../.." && pwd)
cd "$thread_root"
OT=${OT:-$thread_root/.work/openthread}
OT_HOST_BUILD=${OT_HOST_BUILD:-$thread_root/.work/openthread-host}
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
