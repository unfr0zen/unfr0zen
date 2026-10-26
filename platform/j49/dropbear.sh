#!/bin/sh
# Builds OUT/dropbearmulti, the SSH server --ssh installs on a j49, which has
# none of its own: dropbear and dropbearkey in one static ARMv7 binary, from
# the pinned release (cached in .work/j49). Built for plain ARMv7-A, which the
# j49's Cortex-A8 runs; zig as nestlocal's builds use it.
#
#     platform/j49/dropbear.sh OUT
set -eu

[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(dirname "$(dirname "$dir")")
mkdir -p "$1"
out=$(CDPATH= cd -- "$1" && pwd)

version=2025.88
sha256=783f50ea27b17c16da89578fafdb6decfa44bb8f6590e5698a4e4d3672dc53d4
tarball=$root/.work/j49/dropbear-$version.tar.bz2
mkdir -p "$root/.work/j49"
if [ ! -f "$tarball" ]; then
    curl -fsSL -o "$tarball.part" "https://matt.ucc.asn.au/dropbear/releases/dropbear-$version.tar.bz2"
    mv "$tarball.part" "$tarball"
fi
echo "$sha256  $tarball" | sha256sum -c - > /dev/null || { echo "$tarball is not the pinned dropbear $version" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xjf "$tarball" -C "$work"
cd "$work/dropbear-$version"
cat > "$work/cc" <<'EOF'
#!/bin/sh
if command -v zig > /dev/null 2>&1; then
    exec zig cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@"
fi
exec python3 -m ziglang cc -target arm-linux-musleabihf -mcpu=generic+v7a "$@"
EOF
chmod +x "$work/cc"
# Root logs in with its password; no other users, no login records (the
# j49 has no utmp), no X11 or agent forwarding.
cat > localoptions.h <<'EOF'
#define DROPBEAR_X11FWD 0
#define DROPBEAR_SVR_AGENTFWD 0
#define DROPBEAR_SVR_PASSWORD_AUTH 1
#define DROPBEAR_SVR_PUBKEY_AUTH 1
#define DROPBEAR_DSS 0
EOF
CC="$work/cc" LDFLAGS=-s ./configure --host=arm-linux-musleabihf --enable-static --disable-zlib --disable-lastlog \
    --disable-utmp --disable-utmpx --disable-wtmp --disable-wtmpx --disable-pututline --disable-pututxline > configure.log 2>&1
make -j"${JOBS:-8}" PROGRAMS="dropbear dropbearkey" MULTI=1 STATIC=1 > make.log 2>&1 || { tail -n 40 make.log >&2; exit 1; }
cp dropbearmulti "$out/dropbearmulti"
echo "Built $out/dropbearmulti ($(wc -c < "$out/dropbearmulti") bytes)"
