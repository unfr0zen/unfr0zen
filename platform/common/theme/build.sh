#!/bin/sh
# Builds what draws customizable home screen themes into OUT:
# libunfr0zen-theme.so, the library preloaded into nlclient (theme.c), and
# nestlocal's theme library libnestlocal_theme.so (crates/nestlocal-theme, with
# LuaJIT), which it loads. Both for the Cortex-A8, which an A9 runs too,
# against glibc 2.19, as nlclient. Run inside the build image,
# from the repository root.
#
#     platform/common/theme/build.sh OUT
set -eu
[ $# -eq 1 ] || {
    echo "usage: $0 OUT" >&2
    exit 2
}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)
if command -v zig >/dev/null 2>&1; then zig=zig; else zig="python3 -m ziglang"; fi
# Its GL calls resolve against nlclient's own libGLESv2, bound lazily: a
# process without GL that inherits the preload loads it unharmed.
$zig cc -target arm-linux-gnueabihf.2.19 -mcpu=cortex_a8 -O2 -Wall -Wextra -Werror -shared -fPIC -s \
    -Wl,-z,lazy -Wl,-soname,libunfr0zen-theme.so -o "$out/libunfr0zen-theme.so" "$dir/theme.c" -ldl -lpthread

# With NEON, which Rust's armv7 target leaves off and the cores it runs on
# have: the CPU painter's pixel work runs on it rather than on the A8's slow
# VFP.
target=thumbv7neon-unknown-linux-gnueabihf
target_dir=${THEME_TARGET_DIR:-.work/theme-target}
# LuaJIT's build: target C through zig, linked and archived by the ARM GNU
# tools; its 32-bit host tools natively, or under qemu-i386 on arm64.
export STATIC_CC="$zig cc -target arm-linux-gnueabihf.2.19 -mcpu=cortex_a8 -fPIC -O2"
export TARGET_LD=arm-linux-gnueabihf-gcc
export TARGET_AR="arm-linux-gnueabihf-ar rcus"
export TARGET_STRIP=arm-linux-gnueabihf-strip
if [ "$(uname -m)" = aarch64 ]; then export HOST_CC="$dir/host-cc.py"; fi
# At opt-level 3 rustc passes the linker -O1, which zig's linker ignores with
# a warning that rustc repeats: its linker messages are not shown. Linked to
# be bound lazily (rustc asks for at once): glibc 2.19's loader binds a
# library at once only when its .rel.plt follows its .rel.dyn, which zig's
# linker does not lay out.
libgcc=$(arm-linux-gnueabihf-gcc -print-libgcc-file-name)
export CARGO_TARGET_THUMBV7NEON_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C target-cpu=cortex-a8 -C linker-flavor=gcc -C link-arg=-Wl,-z,lazy -C link-arg=$libgcc -A linker_messages"
# Linked by zig against glibc 2.19 (cargo-zigbuild does not know this
# target); rustc's own flavor for it passes ld's arguments, hence gcc's.
mkdir -p "$target_dir"
linker=$(CDPATH= cd -- "$target_dir" && pwd)/zig-ld
printf '#!/bin/sh\nexec %s cc -target arm-linux-gnueabihf.2.19 -mcpu=cortex_a8 "$@"\n' "$zig" >"$linker"
chmod +x "$linker"
export CARGO_TARGET_THUMBV7NEON_UNKNOWN_LINUX_GNUEABIHF_LINKER="$linker"
cargo build --locked -p nestlocal-theme --no-default-features --features luajit --lib --release \
    --target "$target" --target-dir "$target_dir"
cp "$target_dir/$target/release/libnestlocal_theme.so" "$out/libnestlocal_theme.so"
