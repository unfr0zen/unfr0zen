#!/usr/bin/env python3
"""Frames the app's screenshots as macOS windows, for the README and the website.

    UNFR0ZEN_SCREENSHOTS=RAW cargo test -p unfr0zen-gui screenshots -- --ignored
    python3 utils/app-screenshots.py RAW SITE

RAW holds the bare windows at 2x (1-setup.png, 2-backups.png, 3-logs.png). Each
gets rounded corners, a light edge, the traffic lights and a shadow. SITE gets
app1.png to app3.png at 2x for the website; assets/app.png becomes the README's
animated PNG of the three at 1x. Needs Pillow.
"""

import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

NAMES = ["1-setup.png", "2-backups.png", "3-logs.png"]
# At 1x: the room around the window, its corner radius, and the traffic
# lights' centre, spacing and radius, as macOS draws them.
LEFT, TOP, BOTTOM = 56, 38, 74
RADIUS = 10
LIGHT_AT, LIGHT_STEP, LIGHT_RADIUS = 15.5, 23, 6
LIGHTS = [(255, 95, 87), (254, 188, 46), (67, 69, 75)]
EDGE = (255, 255, 255, 34)


def framed(window: Image.Image, scale: int) -> Image.Image:
    """The window on a transparent canvas: rounded, edged, lit and shadowed."""
    w, h = window.size
    left, top, bottom, radius = LEFT * scale, TOP * scale, BOTTOM * scale, RADIUS * scale
    canvas = Image.new("RGBA", (w + 2 * left, h + top + bottom), (0, 0, 0, 0))
    mask = Image.new("L", (w, h), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, w - 1, h - 1), radius, fill=255)
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((left, top + 14 * scale, left + w, top + h + 14 * scale), radius,
                                             fill=(0, 0, 0, 150))
    canvas = Image.alpha_composite(canvas, shadow.filter(ImageFilter.GaussianBlur(18 * scale)))
    body = window.convert("RGBA")
    draw = ImageDraw.Draw(body)
    for i, color in enumerate(LIGHTS):
        cx, cy = (LIGHT_AT + i * LIGHT_STEP) * scale, LIGHT_AT * scale
        r = LIGHT_RADIUS * scale
        draw.ellipse((cx - r, cy - r, cx + r, cy + r), fill=color)
    draw.rounded_rectangle((0, 0, w - 1, h - 1), radius, outline=EDGE, width=scale)
    canvas.paste(body, (left, top), mask)
    return canvas


def main() -> None:
    raw, site = Path(sys.argv[1]), Path(sys.argv[2])
    windows = [Image.open(raw / name) for name in NAMES]
    for i, window in enumerate(windows, 1):
        framed(window, 2).save(site / f"app{i}.png", optimize=True)
    half = [framed(window.resize((window.width // 2, window.height // 2), Image.LANCZOS), 1) for window in windows]
    readme = Path(__file__).resolve().parent.parent / "assets" / "app.png"
    half[0].save(readme, save_all=True, append_images=half[1:], duration=5000, loop=0, optimize=True)


if __name__ == "__main__":
    main()
