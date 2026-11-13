"""Draws the Prometheus logo: the Greek letter Pi (Π), the first letter of
Προμηθεύς, in the colours of the fire he stole.

Writes next to this file:
    logo.svg        the logo as a vector (also copied to ../ui for the window)
    logo-1024.png   a large picture of it
    icon.ico        the Windows icon (16 to 256 pixels)

    pip install pillow
    python make_logo.py

The shapes are in a 100 x 100 square. The same numbers go into the SVG and
into the Pillow drawing, so the two always agree.
"""

import io
import struct
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter

HERE = Path(__file__).parent

# Rounded rectangles: (left, top, right, bottom, corner radius).
CROSSBAR = (15, 21, 85, 39, 5)      # overhangs the legs, as a capital Pi does
LEFT_LEG = (26, 21, 42, 81, 5)
RIGHT_LEG = (58, 21, 74, 81, 5)
LETTER = (CROSSBAR, LEFT_LEG, RIGHT_LEG)
TILE_RADIUS = 22

NAVY_TOP = (24, 18, 58)
NAVY_BOTTOM = (6, 8, 22)
FIRE = [(0.0, (255, 224, 90)), (0.5, (255, 140, 30)), (1.0, (230, 58, 40))]
GLOW = (255, 120, 30)


def svg():
    stops = "".join(f'<stop offset="{o}" stop-color="rgb{c}"/>' for o, c in FIRE)
    rects = "".join(
        f'<rect x="{l}" y="{t}" width="{r - l}" height="{b - t}" rx="{rad}"/>'
        for l, t, r, b, rad in LETTER
    )
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="512" height="512" role="img" aria-label="Prometheus">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="rgb{NAVY_TOP}"/><stop offset="1" stop-color="rgb{NAVY_BOTTOM}"/></linearGradient>
    <linearGradient id="fire" gradientUnits="userSpaceOnUse" x1="0" y1="21" x2="0" y2="81">{stops}</linearGradient>
    <filter id="glow" x="-50%" y="-50%" width="200%" height="200%"><feGaussianBlur stdDeviation="5"/></filter>
  </defs>
  <rect width="100" height="100" rx="{TILE_RADIUS}" fill="url(#bg)"/>
  <g fill="rgb{GLOW}" opacity="0.65" filter="url(#glow)">{rects}</g>
  <g fill="url(#fire)">{rects}</g>
</svg>
"""


def gradient(size, y0, y1, stops):
    """A vertical gradient picture. `stops` are (position 0..1, colour);
    the gradient runs from `y0` to `y1` (in pixels), clamped outside."""
    column = Image.new("RGB", (1, size))
    for y in range(size):
        t = min(max((y - y0) / (y1 - y0), 0.0), 1.0)
        for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
            if t0 <= t <= t1:
                f = (t - t0) / (t1 - t0)
                column.putpixel((0, y), tuple(round(a + (b - a) * f) for a, b in zip(c0, c1)))
                break
    return column.resize((size, size))


def draw(size):
    scale = 4                       # drawn large, then shrunk, for smooth edges
    big = size * scale
    k = big / 100

    letter = Image.new("L", (big, big), 0)
    d = ImageDraw.Draw(letter)
    for l, t, r, b, rad in LETTER:
        d.rounded_rectangle((l * k, t * k, r * k, b * k), radius=rad * k, fill=255)

    img = gradient(big, 0, big, [(0, NAVY_TOP), (1, NAVY_BOTTOM)]).convert("RGBA")

    glow = Image.new("RGBA", (big, big), GLOW + (0,))
    glow.putalpha(letter.filter(ImageFilter.GaussianBlur(5 * k)).point(lambda v: int(v * 0.65)))
    img = Image.alpha_composite(img, glow)
    img.paste(gradient(big, 21 * k, 81 * k, FIRE), (0, 0), letter)

    tile = Image.new("L", (big, big), 0)
    ImageDraw.Draw(tile).rounded_rectangle(
        (0, 0, big - 1, big - 1), radius=TILE_RADIUS * k, fill=255
    )
    img.putalpha(ImageChops.multiply(img.getchannel("A"), tile))
    return img.resize((size, size), Image.LANCZOS)


def ico(sizes):
    frames = []
    for s in sizes:
        buffer = io.BytesIO()
        draw(s).save(buffer, format="PNG")
        frames.append((s, buffer.getvalue()))
    out = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    offset = 6 + 16 * len(frames)
    for s, data in frames:
        out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    for _, data in frames:
        out += data
    return bytes(out)


if __name__ == "__main__":
    for where in (HERE, HERE.parent / "ui"):      # the window shows it too
        (where / "logo.svg").write_text(svg(), encoding="utf-8", newline="\n")
    draw(1024).save(HERE / "logo-1024.png")
    (HERE / "icon.ico").write_bytes(ico([16, 24, 32, 48, 64, 128, 256]))
    print("wrote logo.svg, logo-1024.png, icon.ico")
