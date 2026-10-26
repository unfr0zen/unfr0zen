#!/usr/bin/env python3
"""Save image provenance and function addresses for interpreting boot markers."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def record(tree, image):
    elf = tree / 'u-boot'
    names = subprocess.check_output(['arm-none-eabi-nm', '-n', str(elf)], text=True)
    functions = []
    for line in names.splitlines():
        fields = line.split()
        if len(fields) == 3 and fields[1] in ('T', 't', 'W', 'w'):
            functions.append({'address': fields[0], 'name': fields[2]})
    metadata = {
        'image_sha256': hashlib.sha256(image.read_bytes()).hexdigest(),
        'image_bytes': image.stat().st_size,
        'config_sha256': hashlib.sha256((tree / '.config').read_bytes()).hexdigest(),
        'upstream_revision': subprocess.check_output(
            ['git', '-C', str(tree), 'rev-parse', 'HEAD'], text=True).strip(),
        'functions': functions,
    }
    image.with_suffix('.json').write_text(json.dumps(metadata, indent=2) + '\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('tree', type=Path)
    parser.add_argument('image', type=Path)
    args = parser.parse_args()
    record(args.tree, args.image)
