#!/bin/sh
# The whole Thread stack in one go: zbwake.ko for each gen3 release, the radio
# images (radio.sh), OpenThread's host side (host.sh) and nestlocal carrying
# them (nestlocal.sh). Run inside the build image, with the repository mounted
# at its host path.
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
thread_root=$(CDPATH= cd -- "$dir/../../../.." && pwd)
cd "$thread_root"
export NESTLOCAL_ZBWAKE=${NESTLOCAL_ZBWAKE:-$thread_root/build/zbwake}
sh "$thread_root/platform/diamond3/zbwake/build.sh" "$NESTLOCAL_ZBWAKE"
sh "$dir/radio.sh"
sh "$dir/host.sh"
sh "$dir/nestlocal.sh"
