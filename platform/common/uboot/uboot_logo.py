#!/usr/bin/env python3
"""Convert the upright recovery logo to the board's gzipped BMP and optional framebuffer image."""

import argparse
import gzip
import io
from pathlib import Path

try:
    from PIL import Image
except ImportError as exc:
    raise SystemExit("Pillow is required: python3 -m pip install Pillow") from exc


WIDTH = HEIGHT = 480


def convert(source, output, size=WIDTH, rotate=True, framebuffer=None):
    with Image.open(source) as image:
        if image.size != (WIDTH, HEIGHT):
            raise ValueError(f"logo must be {WIDTH}x{HEIGHT}; got {image.width}x{image.height}")
        rgba = image.convert("RGBA")
    upright = Image.new("RGB", rgba.size, (0, 0, 0))
    upright.paste(rgba, mask=rgba.getchannel("A"))
    bmp = io.BytesIO()
    if size != WIDTH:
        upright = upright.resize((size, size), Image.Resampling.LANCZOS)
    if rotate:
        upright = upright.transpose(Image.Transpose.ROTATE_270)
    upright.save(bmp, format="BMP")
    # mtime=0 keeps the output, and so the U-Boot image, reproducible.
    output.write_bytes(gzip.compress(bmp.getvalue(), compresslevel=9, mtime=0))
    if framebuffer is not None:
        framebuffer.write_bytes(gzip.compress(upright.tobytes("raw", "BGRX"), compresslevel=9, mtime=0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="upright 480x480 image")
    parser.add_argument("output", type=Path, help="gzipped 24-bit BMP to write")
    parser.add_argument("--size", type=int, default=WIDTH, help="square panel region in pixels")
    parser.add_argument("--upright", action="store_true", help="panel is mounted upright")
    parser.add_argument("--framebuffer", type=Path, help="also write compressed XRGB8888 pixels")
    args = parser.parse_args()
    try:
        convert(
            args.source,
            args.output,
            size=args.size,
            rotate=not args.upright,
            framebuffer=args.framebuffer,
        )
    except (OSError, ValueError) as exc:
        parser.error(str(exc))


if __name__ == "__main__":
    main()
