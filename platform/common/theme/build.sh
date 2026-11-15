#!/bin/sh
# Builds what draws customizable home screen themes on the gen 3 into OUT:
# libunfr0zen-theme.so, the library preloaded into nlclient (theme.c), and
# nestlocal's theme library libnestlocal_theme.so (crates/nestlocal-theme, with
# LuaJIT), which it loads. Both for the gen 3's Cortex-A9 against glibc 2.19,
# as nlclient. Run inside the build image, from the repository root.
#
#     platform/common/theme/build.sh OUT
set -eu
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
if command -v zig > /dev/null 2>&1; then zig=zig; else zig="python3 -m ziglang"; fi
# Its GL calls resolve against nlclient's own libGLESv2, bound lazily: a
# process without GL that inherits the preload loads it unharmed.
$zig cc -target arm-linux-gnueabihf.2.19 -mcpu=cortex_a9 -O2 -Wall -Wextra -Werror -shared -fPIC -s \
    -Wl,-z,lazy -Wl,-soname,libunfr0zen-theme.so -o "$out/libunfr0zen-theme.so" "$dir/theme.c" -ldl -lpthread

target=armv7-unknown-linux-gnueabihf
target_dir=${THEME_TARGET_DIR:-.work/theme-target}
# LuaJIT's build: target C through zig, linked and archived by the ARM GNU
# tools; its 32-bit host tools natively, or under qemu-i386 on arm64.
export STATIC_CC="$zig cc -target arm-linux-gnueabihf.2.19 -mcpu=cortex_a9 -fPIC -O2"
export TARGET_LD=arm-linux-gnueabihf-gcc
export TARGET_AR="arm-linux-gnueabihf-ar rcus"
export TARGET_STRIP=arm-linux-gnueabihf-strip
if [ "$(uname -m)" = aarch64 ]; then export HOST_CC="$dir/host-cc.py"; fi
# At opt-level 3 rustc passes the linker -O1, which zig's linker ignores with
# a warning that rustc repeats: its linker messages are not shown.
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C target-cpu=cortex-a9 -C link-arg=$(arm-linux-gnueabihf-gcc -print-libgcc-file-name) -A linker_messages"
cargo zigbuild --locked -p nestlocal-theme --no-default-features --features luajit --lib --release \
    --target "$target.2.19" --target-dir "$target_dir"
cp "$target_dir/$target/release/libnestlocal_theme.so" "$out/libnestlocal_theme.so"
