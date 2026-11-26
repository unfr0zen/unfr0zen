#!/usr/bin/env python3
"""Writes unfr0zen's update packages, one per device, which units install
over MQTT, Matter and Zigbee alike (nestlocal's package.rs), and
Zigbee2MQTT's OTA index of them:

    python3 utils/bundle/package.py [--base URL] BUNDLE...

Each BUNDLE, a signed unfr0zen-ota-<device>-<tag>.tgz, gives
unfr0zen-<device>-<tag>.ota beside it: a Matter OTA image under Matter's
test vendor and the device's own product (PRODUCTS, package.rs's table),
whose payload is a Zigbee OTA image under Nest Labs' manufacturer code with
the same number as its image type, holding the package's own URL (the
release's asset, or under --base) and the bundle. Both carry the tag as
nestlocal's update::matter_version numbers it. unfr0zen-zigbee-ota.json
beside them lists the packages for Zigbee2MQTT's
zigbee_ota_override_index_location. Standard library only, as it runs where
the release is signed.
"""

import argparse
import hashlib
import json
import re
import struct
from pathlib import Path

REPOSITORY = "unfr0zen/unfr0zen"
VENDOR = 0xFFF1
PRODUCTS = {"diamond3": 0x8001, "j49": 0x8002, "diamond": 0x8003, "onyx": 0x8004}
FILE_ID = 0x1BEEF11E
SHA256 = 1  # IANA Named Information Hash Algorithm
MANUFACTURER = 0x10D6  # Nest Labs, nestlocal's zigbee::zcl::MANUFACTURER_CODE
ZIGBEE_FILE_ID = 0x0BEEF11E
ZIGBEE_HEADER = 56
ZIGBEE_PRO = 2
UPGRADE_IMAGE = 0x0000
PACKAGE_URL = 0xF001  # manufacturer-specific; Zigbee2MQTT reads 0xF000 as Telink's
INDEX = "unfr0zen-zigbee-ota.json"


def matter_version(tag):
    """vMAJOR.MINOR.PATCH a byte each, then -rcN as N, the release as 128."""
    m = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)(?:-rc(\d+))?", tag)
    if not m:
        raise SystemExit(f"package: {tag} is not vMAJOR.MINOR.PATCH or vMAJOR.MINOR.PATCH-rcN")
    major, minor, patch = (int(n) for n in m.groups()[:3])
    if max(major, minor, patch) > 255:
        raise SystemExit(f"package: {tag} does not fit a Matter version")
    last = min(int(m[4]), 127) if m[4] else 128
    return major << 24 | minor << 16 | patch << 8 | last


def matter_header(product, version, tag, payload):
    """The Matter image header: an anonymous TLV structure, context tags 0 to 9."""

    def ctx(number, kind, data):
        return bytes([0x20 | kind, number]) + data

    def text(number, kind, data):
        if len(data) > 255:
            raise SystemExit("package: a header string past 255 bytes")
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


def zigbee_image(product, version, tag, url, bundle):
    """The Zigbee OTA image: the header, then the URL and the bundle."""
    name = f"unfr0zen {tag}".encode()
    if len(name) > 32:
        raise SystemExit(f"package: {tag} does not fit the Zigbee header string")

    def element(kind, data):
        return struct.pack("<HI", kind, len(data)) + data

    body = element(PACKAGE_URL, url.encode()) + element(UPGRADE_IMAGE, bundle)
    header = struct.pack(
        "<IHHHHHIH32sI",
        ZIGBEE_FILE_ID,
        0x0100,  # header version
        ZIGBEE_HEADER,
        0,  # field control: no optional fields
        MANUFACTURER,
        product,
        version,
        ZIGBEE_PRO,
        name,
        ZIGBEE_HEADER + len(body),
    )
    return header + body


def write(bundle, base):
    m = re.fullmatch(r"unfr0zen-ota-([a-z0-9]+)-(.+)\.tgz", bundle.name)
    if not m or m[1] not in PRODUCTS:
        raise SystemExit(f"package: {bundle} is not unfr0zen-ota-<device>-<tag>.tgz for {', '.join(PRODUCTS)}")
    device, tag = m[1], m[2]
    product, version = PRODUCTS[device], matter_version(tag)
    out = bundle.with_name(f"unfr0zen-{device}-{tag}.ota")
    url = f"{base or f'https://github.com/{REPOSITORY}/releases/download/{tag}'}/{out.name}"
    payload = zigbee_image(product, version, tag, url, bundle.read_bytes())
    header = matter_header(product, version, tag, payload)
    package = struct.pack("<IQI", FILE_ID, 16 + len(header) + len(payload), len(header)) + header + payload
    # A Zigbee OTA server sends from the first Zigbee file identifier on.
    if package.find(struct.pack("<I", ZIGBEE_FILE_ID)) != 16 + len(header):
        raise SystemExit(f"package: {out.name}'s Matter header holds the Zigbee file identifier")
    out.write_bytes(package)
    print(f"{out}: {device} {product:04X} {tag} ({version:#010x}), {len(package)} bytes")
    return {
        "fileName": out.name,
        "fileVersion": version,
        "fileSize": len(package),
        "url": url,
        "imageType": product,
        "manufacturerCode": MANUFACTURER,
        "sha512": hashlib.sha512(package).hexdigest(),
        "otaHeaderString": f"unfr0zen {tag}",
        "releaseNotes": f"https://github.com/{REPOSITORY}/releases/tag/{tag}",
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Write unfr0zen's update packages from signed setup bundles.")
    parser.add_argument("--base", help="where the packages are served, instead of the GitHub release")
    parser.add_argument("bundles", nargs="+", type=Path)
    args = parser.parse_args()
    entries = [write(path, args.base.rstrip("/") if args.base else None) for path in args.bundles]
    index = args.bundles[0].with_name(INDEX)
    index.write_text(json.dumps(entries, indent=2) + "\n")
    print(f"{index}: {len(entries)} packages")
