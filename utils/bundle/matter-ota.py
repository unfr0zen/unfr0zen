#!/usr/bin/env python3
"""Wraps signed setup bundles in unfr0zen's update packages, the Matter OTA
images that units install over MQTT and Matter alike (nestlocal's
package.rs):

    python3 utils/bundle/matter-ota.py BUNDLE...

Each BUNDLE, unfr0zen-ota-<device>-<tag>.tgz, gives
unfr0zen-ota-<device>-<tag>.ota beside it: the bundle as the image's
payload, under Matter's test vendor and the device's own product (PRODUCTS,
package.rs's table), its SoftwareVersion the tag numbered as nestlocal's
update::matter_version numbers it. Standard library only, as it runs where
the release is signed.
"""

import hashlib
import re
import struct
import sys
from pathlib import Path

VENDOR = 0xFFF1
PRODUCTS = {"diamond3": 0x8001, "j49": 0x8002, "diamond": 0x8003}
FILE_ID = 0x1BEEF11E
SHA256 = 1  # IANA Named Information Hash Algorithm


def matter_version(tag):
    """vMAJOR.MINOR.PATCH a byte each, then -rcN as N, the release as 128."""
    m = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?", tag)
    if not m:
        raise SystemExit(f"matter-ota: {tag} is not vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-rcN")
    major, minor, patch = (int(n) for n in m.groups()[:3])
    if max(major, minor, patch) > 255:
        raise SystemExit(f"matter-ota: {tag} does not fit a Matter version")
    last = min(int(m[4]), 127) if m[4] else 128
    return major << 24 | minor << 16 | patch << 8 | last


def tlv_header(product, version, tag, payload):
    """The image header: an anonymous TLV structure, context tags 0 to 9."""

    def ctx(number, kind, data):
        return bytes([0x20 | kind, number]) + data

    def text(number, kind, data):
        if len(data) > 255:
            raise SystemExit("matter-ota: a header string past 255 bytes")
        return ctx(number, kind, bytes([len(data)]) + data)

    return (
        b"\x15"
        + ctx(0, 0x05, struct.pack("<H", VENDOR))
        + ctx(1, 0x05, struct.pack("<H", product))
        + ctx(2, 0x06, struct.pack("<I", version))
        + text(3, 0x0C, tag.encode())
        + ctx(4, 0x07, struct.pack("<Q", len(payload)))
        + ctx(8, 0x04, bytes([SHA256]))
        + text(9, 0x10, hashlib.sha256(payload).digest())
        + b"\x18"
    )


def wrap(bundle):
    m = re.fullmatch(r"unfr0zen-ota-([a-z0-9]+)-(.+)\.tgz", bundle.name)
    if not m or m[1] not in PRODUCTS:
        raise SystemExit(f"matter-ota: {bundle} is not unfr0zen-ota-<device>-<tag>.tgz for {', '.join(PRODUCTS)}")
    device, tag = m[1], m[2]
    payload = bundle.read_bytes()
    header = tlv_header(PRODUCTS[device], matter_version(tag), tag, payload)
    image = struct.pack("<IQI", FILE_ID, 16 + len(header) + len(payload), len(header)) + header + payload
    out = bundle.with_suffix(".ota")
    out.write_bytes(image)
    print(
        f"{out}: {device} {VENDOR:04X}:{PRODUCTS[device]:04X} {tag} ({matter_version(tag):#010x}), {len(image)} bytes"
    )


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: matter-ota.py BUNDLE...")
    for path in sys.argv[1:]:
        wrap(Path(path))
