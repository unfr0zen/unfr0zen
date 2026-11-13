#!/bin/sh
# unfr0zen-apply for the thermostat, a static ARMv7 binary, into
# build/unfr0zen-apply-armv7 for the app's patch bundle. Run inside the build
# image, with the repository mounted at its host path.
set -eu
cd "$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
target=armv7-unknown-linux-musleabihf
target_dir=${NESTLOCAL_TARGET_DIR:-.work/nestlocal-target}
mkdir -p build
cargo zigbuild --locked -p unfr0zen-apply --profile thermostat --target "$target" --target-dir "$target_dir"
cp "$target_dir/$target/thermostat/unfr0zen-apply" build/unfr0zen-apply-armv7
