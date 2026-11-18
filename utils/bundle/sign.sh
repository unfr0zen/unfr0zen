#!/bin/sh
# Signs setup bundles packed unsigned, as `unfr0zen-bundle pack` signs them:
# manifest.sig, the base64 Ed25519 signature of manifest.json's bytes, added
# to the bundle. The release signs in a job of its own with this script, so
# no code compiled from the workspace's dependencies runs where the key is.
#
#   UNFR0ZEN_BUNDLE_KEY=SEED sh utils/bundle/sign.sh PUBLIC-KEY OUT-DIR BUNDLE...
#
# SEED is the base64 Ed25519 seed; PUBLIC-KEY the file holding the base64
# public key it must match (utils/bundle/release.pub). Each BUNDLE is written
# signed to OUT-DIR under its own name. Needs openssl 3, tar and gzip.
set -eu

[ $# -ge 3 ] || {
    echo "usage: UNFR0ZEN_BUNDLE_KEY=SEED $0 PUBLIC-KEY OUT-DIR BUNDLE..." >&2
    exit 2
}
[ -n "${UNFR0ZEN_BUNDLE_KEY:-}" ] || {
    echo "sign.sh: UNFR0ZEN_BUNDLE_KEY is not set" >&2
    exit 1
}
public=$1
out=$2
shift 2

umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# The seed as PKCS#8 DER: the fixed Ed25519 prefix, then its 32 bytes.
printf '\060\056\002\001\000\060\005\006\003\053\145\160\004\042\004\040' >"$work/key.der"
printf '%s' "$UNFR0ZEN_BUNDLE_KEY" | base64 -d >>"$work/key.der"
[ "$(wc -c <"$work/key.der")" -eq 48 ] || {
    echo "sign.sh: UNFR0ZEN_BUNDLE_KEY is not a base64 32-byte seed" >&2
    exit 1
}
# Its public key, the last 32 bytes of the DER, against the one units trust.
derived=$(openssl pkey -inform DER -in "$work/key.der" -pubout -outform DER | tail -c 32 | base64)
[ "$derived" = "$(tr -d ' \n' <"$public")" ] || {
    echo "sign.sh: the key's public key $derived is not $public's" >&2
    exit 1
}

mkdir -p "$out"
for bundle in "$@"; do
    name=$(basename "$bundle")
    rm -rf "$work/tar"
    mkdir "$work/tar"
    gzip -dc "$bundle" >"$work/bundle.tar"
    if tar -tf "$work/bundle.tar" | grep -qx manifest.sig; then
        echo "sign.sh: $bundle is already signed" >&2
        exit 1
    fi
    tar -xOf "$work/bundle.tar" manifest.json >"$work/manifest.json"
    openssl pkeyutl -sign -rawin -keyform DER -inkey "$work/key.der" -in "$work/manifest.json" \
        -out "$work/manifest.bin"
    # One line, as pack writes it.
    printf '%s\n' "$(base64 <"$work/manifest.bin" | tr -d '\n')" >"$work/tar/manifest.sig"
    chmod 644 "$work/tar/manifest.sig"
    (cd "$work/tar" && COPYFILE_DISABLE=1 tar -rf "$work/bundle.tar" manifest.sig)
    gzip -9n <"$work/bundle.tar" >"$out/$name"
    echo "$out/$name: signed by $derived"
done
