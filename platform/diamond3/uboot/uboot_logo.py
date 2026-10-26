#!/usr/bin/env python3
"""Convert the recovery U-Boot boot logo to the gzipped BMP it embeds.

The diamond3 panel is 480x640 and mounted a quarter turn from upright: the
round window shows the top 480x480 of the framebuffer turned back a quarter
turn counter-clockwise. The logo is given upright and written turned a
quarter turn clockwise, so U-Boot draws it at (0, 0) unrotated.
"""

import argparse
import gzip
import io
from pathlib import Path

try:
    from PIL import Image
except ImportError as exc:
    raise SystemExit('Pillow is required: python3 -m pip install Pillow') from exc


WIDTH = HEIGHT = 480


def convert(source, output):
    with Image.open(source) as image:
        if image.size != (WIDTH, HEIGHT):
            raise ValueError(f'logo must be {WIDTH}x{HEIGHT}; got {image.width}x{image.height}')
        rgba = image.convert('RGBA')
    upright = Image.new('RGB', rgba.size, (0, 0, 0))
    upright.paste(rgba, mask=rgba.getchannel('A'))
    bmp = io.BytesIO()
    upright.transpose(Image.Transpose.ROTATE_270).save(bmp, format='BMP')
    # mtime=0 keeps the output, and so the U-Boot image, reproducible.
    output.write_bytes(gzip.compress(bmp.getvalue(), compresslevel=9, mtime=0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path, help='upright 480x480 image')
    parser.add_argument('output', type=Path, help='gzipped 24-bit BMP to write')
    args = parser.parse_args()
    try:
        convert(args.source, args.output)
    except (OSError, ValueError) as exc:
        parser.error(str(exc))


if __name__ == '__main__':
    main()
