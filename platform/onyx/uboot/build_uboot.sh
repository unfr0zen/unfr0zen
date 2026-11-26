#!/bin/sh
# Build the Thermostat E's recovery U-Boot: the upstream tree with this
# directory's complete patch series (the diagnostics land in the 6UL's
# smaller OCRAM, so the diamond3 patch set is not shared), into
# build/u-boot-recovery-onyx.bin.
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$dir/../../.." && pwd)
tree="$repo_dir/.work/u-boot-recovery-onyx"
revision=f8a2983ec83afd43731d905b4ff0ffd57b57f2f0
upstream="$repo_dir/platform/diamond3/uboot/upstream"
mkdir -p "$repo_dir/.work" "$repo_dir/build/onyx"
git -C "$repo_dir" submodule update --init -- platform/diamond3/uboot/upstream
test "$(git -C "$upstream" rev-parse HEAD)" = "$revision"

if [ ! -d "$tree/.git" ]; then
    git clone --no-hardlinks "$upstream" "$tree"
    git -C "$tree" checkout --detach "$revision"
fi
test "$(git -C "$tree" rev-parse HEAD)" = "$revision"

set --
for patch in "$dir"/patches/onyx/*.patch; do
    set -- "$@" "$patch"
done
if git -C "$tree" apply --reverse --check "$@" 2>/dev/null; then
    : # The current patches are already applied.
elif git -C "$tree" apply --check "$@" 2>/dev/null; then
    git -C "$tree" apply "$@"
else
    replacement=$(mktemp -d "$tree.new.XXXXXX")
    trap 'rm -rf "$replacement"' EXIT
    git clone --no-hardlinks "$upstream" "$replacement"
    git -C "$replacement" checkout --detach "$revision"
    git -C "$replacement" apply --check "$@"
    git -C "$replacement" apply "$@"
    backup="$tree.stale-$(date +%Y%m%d-%H%M%S)-$$"
    mv "$tree" "$backup"
    mv "$replacement" "$tree"
    trap - EXIT
    echo "Recreated stale recovery checkout; previous tree: $backup"
fi

cp "$dir/onyx.config" "$tree/.config"
make -C "$tree" CROSS_COMPILE=arm-none-eabi- olddefconfig
python3 "$repo_dir/platform/common/uboot/uboot_logo.py" --size 320 --upright \
    --framebuffer "$repo_dir/build/onyx/recovery-logo.bgrx.gz" \
    "$repo_dir/platform/common/uboot/boot-logo.png" "$repo_dir/.work/onyx-boot-logo.bmp.gz"
make -C "$tree" CROSS_COMPILE=arm-none-eabi- -j"${JOBS:-8}" \
    NEST_BOOT_LOGO="$repo_dir/.work/onyx-boot-logo.bmp.gz" u-boot-dtb.bin
cp "$tree/u-boot-dtb.bin" "$repo_dir/build/u-boot-recovery-onyx.bin"
python3 "$repo_dir/platform/common/uboot/record_uboot_build.py" "$tree" "$repo_dir/build/u-boot-recovery-onyx.bin"
echo "Built build/u-boot-recovery-onyx.bin"

# The ROM accepts Google's signed SDP loader for this board.
official="$repo_dir/build/u-boot-sdp-onyx.hab"
official_sha=1c204314ed1e536979e5512217e5baa386796500d6c0aca9460862aabb13b033
if [ ! -f "$official" ] || [ "$(sha256sum "$official" | cut -d' ' -f1)" != "$official_sha" ]; then
    curl -fsSL 'https://nest-open-source.googlesource.com/nest-learning-thermostat/5.7/u-boot/+/3e6d8487fcc10d400584dce41900e66733cb41a3/bin/onyx/release/u-boot-sdp-5.8d44-onyx-0NUR.hab?format=TEXT' |
        base64 -d >"$official.part"
    test "$(sha256sum "$official.part" | cut -d' ' -f1)" = "$official_sha"
    mv "$official.part" "$official"
fi
