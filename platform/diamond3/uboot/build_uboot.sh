#!/bin/sh
# Recreate and build the pinned, patched recovery U-Boot inside this repository.
set -eu
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(CDPATH= cd -- "$dir/../../.." && pwd)
tree="$repo_dir/.work/u-boot-recovery"
revision=f8a2983ec83afd43731d905b4ff0ffd57b57f2f0
mkdir -p "$repo_dir/.work" "$repo_dir/build"
upstream="$dir/upstream"
git -C "$repo_dir" submodule update --init -- platform/diamond3/uboot/upstream
test "$(git -C "$upstream" rev-parse HEAD)" = "$revision"
if [ ! -d "$tree/.git" ]; then
    git clone --no-hardlinks "$upstream" "$tree"
    git -C "$tree" checkout --detach "$revision"
fi
test "$(git -C "$tree" rev-parse HEAD)" = "$revision"
# Patches live either as a single recovery.patch or, when split per
# file, as patches/recovery/*.patch. Each split patch touches a distinct
# file, so applying them together is order-independent and atomic.
if [ -d "$dir/patches/recovery" ]; then
    patches=$(ls "$dir/patches/recovery"/*.patch)
else
    patches="$dir/recovery.patch"
fi
if git -C "$tree" apply --reverse --check $patches 2>/dev/null; then
    : # The current patches are already applied.
elif git -C "$tree" apply --check $patches 2>/dev/null; then
    git -C "$tree" apply $patches
else
    # The patches changed since this checkout was created. Build a replacement
    # before moving the old tree, which may contain local edits or build output.
    replacement=$(mktemp -d "$tree.new.XXXXXX")
    git clone --no-hardlinks "$upstream" "$replacement"
    git -C "$replacement" checkout --detach "$revision"
    git -C "$replacement" apply --check $patches
    git -C "$replacement" apply $patches
    backup="$tree.stale-$(date +%Y%m%d-%H%M%S)-$$"
    mv "$tree" "$backup"
    mv "$replacement" "$tree"
    echo "Recreated stale recovery checkout; previous tree: $backup"
fi
cp "$dir/recovery.config" "$tree/.config"
make -C "$tree" CROSS_COMPILE=arm-none-eabi- olddefconfig
# The boot logo is generated outside the tree, which stays clean.
python3 "$dir/uboot_logo.py" "$dir/boot-logo.png" \
    "$repo_dir/.work/boot-logo.bmp.gz"
make -C "$tree" CROSS_COMPILE=arm-none-eabi- -j"${JOBS:-8}" \
    NEST_BOOT_LOGO="$repo_dir/.work/boot-logo.bmp.gz" u-boot-dtb.bin
cp "$tree/u-boot-dtb.bin" "$repo_dir/build/u-boot-recovery.bin"
python3 "$dir/record_uboot_build.py" "$tree" "$repo_dir/build/u-boot-recovery.bin"
