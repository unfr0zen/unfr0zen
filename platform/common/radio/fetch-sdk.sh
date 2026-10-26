#!/bin/sh
# Fetch what the radio firmware builds from, into DIR (default .work/zigbee-sdk): the parts
# of two public repositories it reads, at pinned commits.
#
#   gecko_sdk_suite  Gecko SDK 2.6 (EmberZNet 6.6), a mirror on GitHub: the
#                    EM3581 (gen3) and EM357 (gen2) stack libraries and the
#                    HAL sources
#   gecko_sdk        Silicon Labs' gecko_sdk 4.5: the EM358x and EM35x CMSIS
#                    startup
#
#     platform/common/radio/fetch-sdk.sh [DIR]
#
# Prints the make variables to build with: SDK=... GSDK=...
set -eu

dir=${1:-.work/zigbee-sdk}

# fetch NAME URL COMMIT PATTERN...: a sparse checkout of COMMIT, blobs fetched
# only for the paths kept.
fetch() {
    name=$1 url=$2 commit=$3
    shift 3
    repo=$dir/$name
    if [ "$(git -C "$repo" rev-parse HEAD 2> /dev/null)" = "$commit" ]; then
        # The paths kept may have grown since.
        git -C "$repo" sparse-checkout set --no-cone "$@"
        return
    fi
    rm -rf "$repo"
    git init -q "$repo"
    git -C "$repo" remote add origin "$url"
    git -C "$repo" config remote.origin.promisor true
    git -C "$repo" config remote.origin.partialclonefilter blob:none
    git -C "$repo" sparse-checkout set --no-cone "$@"
    git -C "$repo" fetch -q --depth 1 --filter=blob:none origin "$commit"
    git -C "$repo" checkout -q FETCH_HEAD
}

mkdir -p "$dir"
fetch gecko_sdk_suite https://github.com/tomyqg/gecko_sdk_suite.git 095c36313756a5954964785851d576bd81f35c02 \
    /v2.6/platform/base/hal/ \
    /v2.6/protocol/zigbee/stack/ \
    /v2.6/protocol/zigbee/app/util/ezsp/ \
    /v2.6/protocol/zigbee/app/em260/ \
    /v2.6/protocol/zigbee/app/ncp/plugin/ \
    /v2.6/protocol/zigbee/app/framework/plugin/zigbee-event-logger-stub/ \
    '/v2.6/protocol/zigbee/build/*-cortexm3-iar-em3581-em3xx/' \
    '/v2.6/protocol/zigbee/build/*-cortexm3-iar-em357-em3xx/'
fetch gecko_sdk https://github.com/SiliconLabs/gecko_sdk.git 8a2efd2cb191cb250fd4c1e655b16b8a80be1997 \
    /platform/Device/SiliconLabs/em358x/ \
    /platform/Device/SiliconLabs/em35x/ \
    /platform/CMSIS/Core/Include/

root=$(cd "$dir" && pwd)
echo "SDK=$root/gecko_sdk_suite/v2.6 GSDK=$root/gecko_sdk"
