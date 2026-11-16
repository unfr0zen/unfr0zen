#!/bin/sh
# Builds OUT/busybox, the static ARMv7 busybox of the gen 1's and gen 2's
# initramfs, from the pinned release (cached in .work/j49) against musl with
# zig, as dropbear.sh builds dropbear. Their Linux 2.6.37 predates what
# Debian's glibc build needs: its directory reads fail there.
#
#     platform/j49/busybox.sh OUT
set -eu

[ $# -eq 1 ] || {
    echo "usage: $0 OUT" >&2
    exit 2
}
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname "$(dirname "$dir")")
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)

version=1.37.0
sha256=3311dff32e746499f4df0d5df04d7eb396382d7e108bb9250e7b519b837043a4
tarball=$root/.work/j49/busybox-$version.tar.bz2
mkdir -p "$root/.work/j49"
if [ ! -f "$tarball" ]; then
    curl -fsSL -o "$tarball.part" "https://busybox.net/downloads/busybox-$version.tar.bz2"
    mv "$tarball.part" "$tarball"
fi
echo "$sha256  $tarball" | sha256sum -c - >/dev/null || {
    echo "$tarball is not the pinned busybox $version" >&2
    exit 1
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xjf "$tarball" -C "$work"
cd "$work/busybox-$version"
cat >"$work/cc" <<'EOF'
#!/bin/sh
# Without the GNU ld options busybox's trylink adds, which zig's lld refuses.
for arg; do
    shift
    case "$arg" in
        -Wl,--warn-common|-Wl,-Map,*|-Wl,--verbose|-Wl,--sort-common|-Wl,--sort-section,*) ;;
        *) set -- "$@" "$arg" ;;
    esac
done
if command -v zig > /dev/null 2>&1; then
    exec zig cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@"
fi
exec python3 -m ziglang cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@"
EOF
chmod +x "$work/cc"
make defconfig >config.log 2>&1
# Static; without tc, whose source needs kernel headers musl's lack, and
# without the x86 SHA instructions. Its globals' pointers not const: clang
# reads them before they are set (busybox's BB_GLOBAL_CONST), and awk crashes.
sed -i -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    -e "s/^CONFIG_EXTRA_CFLAGS=.*/CONFIG_EXTRA_CFLAGS=\"-DBB_GLOBAL_CONST=''\"/" \
    -e 's/^CONFIG_SHA1_HWACCEL=y$/# CONFIG_SHA1_HWACCEL is not set/' \
    -e 's/^CONFIG_SHA256_HWACCEL=y$/# CONFIG_SHA256_HWACCEL is not set/' .config
make oldconfig </dev/null >>config.log 2>&1
# Stripped as it links: the host's strip does not take ARM objects.
make -j"${JOBS:-8}" CC="$work/cc" HOSTCC=cc LDFLAGS=-s STRIP=true busybox_unstripped >make.log 2>&1 || {
    grep -E "error|undefined" make.log | head -n 20 >&2
    tail -n 10 make.log >&2
    exit 1
}
cp busybox_unstripped "$out/busybox"
echo "Built $out/busybox ($(wc -c <"$out/busybox") bytes)"
