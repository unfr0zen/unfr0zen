#!/usr/bin/env python3
"""Build the exact tested ROM bootstrap from ARM and public HAB metadata sources.

PEM inputs preserve the signed metadata of the original board-specific image.
No private keys or precompiled code are required. HAB records use big-endian
lengths/offsets; IVT/boot-data words use little-endian ARM encoding.
"""
import base64
import hashlib
from pathlib import Path
import struct
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
EXPECTED_SHA256 = 'd60cc77f12d3bf217e800b9b3f576fa78cc2082836e48f8f6fc744a086e87146'


def header(tag, size, parameter=0x40):
    return struct.pack('>BHB', tag, size, parameter)


def record(name, tag):
    lines = (HERE / 'hab' / f'{name}.pem').read_text().splitlines()
    der = base64.b64decode(''.join(lines[1:-1]), validate=True)
    if der[:2] != b'\x30\x82' or int.from_bytes(der[2:4], 'big') + 4 != len(der):
        raise ValueError(f'Invalid DER source: {name}')
    size = (4 + len(der) + 3) & ~3
    return header(tag, size) + der + bytes(size - 4 - len(der))


def assemble(name, address):
    stage = ROOT / '.work' / name
    subprocess.run(['arm-none-eabi-as', '-mcpu=cortex-a9', '-o', str(stage.with_suffix('.o')),
                    str(HERE / f'{name}.S')], check=True)
    subprocess.run(['arm-none-eabi-ld', f'-Ttext={address:#010x}', '-e', '_start',
                    '-o', str(stage.with_suffix('.elf')), str(stage.with_suffix('.o'))], check=True)
    subprocess.run(['arm-none-eabi-objcopy', '-O', 'binary', str(stage.with_suffix('.elf')),
                    str(stage.with_suffix('.bin'))], check=True)
    return stage.with_suffix('.bin').read_bytes()


def main():
    work, build = ROOT / '.work', ROOT / 'build'
    work.mkdir(exist_ok=True)
    build.mkdir(exist_ok=True)
    # The boot wrapper is uploaded by the loader; build it here so USB hosts need no ARM tools.
    wrapper = assemble('boot_entry', 0x0090b000)
    (build / 'boot-wrapper.bin').write_bytes(wrapper)
    payload = assemble('rom_entry', 0x00907400)
    if len(payload) != 64:
        raise ValueError('Unexpected first-stage size')

    records = [('csf-cert', 0xd7), ('srk-cert', 0xd7), ('csf-signature', 0xd8),
               ('image-cert', 0xd7), ('image-signature', 0xd8)]
    offsets, bodies = {}, bytearray()
    for name, tag in records:
        offsets[name] = 0x48 + len(bodies)
        bodies.extend(record(name, tag))

    def install(flags, certificate_format, hash_algorithm, source, target, name):
        return header(0xbe, 12, flags) + bytes([certificate_format, hash_algorithm, source, target]) + struct.pack('>I', offsets[name])

    def authenticate(key, name, blocks=b''):
        return header(0xca, 12 + len(blocks), 0) + bytes([key, 0xc5, 0xff, 0]) + struct.pack('>I', offsets[name]) + blocks

    commands = install(2, 9, 0, 0, 1, 'srk-cert')
    commands += install(0, 3, 0x17, 0, 0, 'csf-cert')
    commands += authenticate(1, 'csf-signature')
    commands += install(0, 9, 0, 0, 2, 'image-cert')
    commands += authenticate(2, 'image-signature', struct.pack('>II', 0x00907000, 0x2000))
    csf = header(0xd4, 4 + len(commands)) + commands + bodies
    image = bytearray(0x3c00)
    struct.pack_into('<8I', image, 0, 0x412000d1, 0x00907400, 0, 0,
                     0x00907020, 0x00907000, 0x00909000, 0)
    struct.pack_into('<3I', image, 0x20, 0x00907000, 0x4000, 0)
    image[0x400:0x400 + len(payload)] = payload
    image[0x2000:0x2000 + len(csf)] = csf
    digest = hashlib.sha256(image).hexdigest()
    if digest != EXPECTED_SHA256:
        raise ValueError(f'Bootstrap differs from the tested image: {digest}')
    target = build / 'rom-stage.imx'
    target.write_bytes(image)
    print(f'Built {target} ({len(image)} bytes); known-good SHA-256 matched')


if __name__ == '__main__':
    main()
