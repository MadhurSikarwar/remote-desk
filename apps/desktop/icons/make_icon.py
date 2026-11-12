"""Regenerates icon.ico (a monitor on a blue tile). Standard library only.

    python make_icon.py
"""

import struct
import zlib
from pathlib import Path

BLUE = (31, 111, 235, 255)
WHITE = (255, 255, 255, 255)
CLEAR = (0, 0, 0, 0)


def pixel(x, y):
    """Colour at a point of the unit square."""
    radius = 0.2
    cx = min(max(x, radius), 1 - radius)
    cy = min(max(y, radius), 1 - radius)
    if (x - cx) ** 2 + (y - cy) ** 2 > radius**2:
        return CLEAR
    if 0.2 <= x <= 0.8 and 0.26 <= y <= 0.62:
        inside = 0.25 <= x <= 0.75 and 0.31 <= y <= 0.57
        return BLUE if inside else WHITE
    if 0.46 <= x <= 0.54 and 0.62 < y <= 0.72:
        return WHITE
    if 0.34 <= x <= 0.66 and 0.72 < y <= 0.77:
        return WHITE
    return BLUE


def render(size, samples=4):
    rows = bytearray()
    step = 1 / (size * samples)
    for py in range(size):
        rows.append(0)
        for px in range(size):
            total = [0, 0, 0, 0]
            for sy in range(samples):
                for sx in range(samples):
                    colour = pixel((px * samples + sx + 0.5) * step, (py * samples + sy + 0.5) * step)
                    alpha = colour[3]
                    total[0] += colour[0] * alpha
                    total[1] += colour[1] * alpha
                    total[2] += colour[2] * alpha
                    total[3] += alpha
            if total[3]:
                rows += bytes(round(total[i] / total[3]) for i in range(3))
            else:
                rows += b"\0\0\0"
            rows.append(round(total[3] / samples**2))
    return bytes(rows)


def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))


def png(size):
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(render(size), 9))
        + chunk(b"IEND", b"")
    )


def main():
    sizes = [16, 32, 48, 256]
    images = [png(size) for size in sizes]
    out = struct.pack("<HHH", 0, 1, len(sizes))
    offset = 6 + 16 * len(sizes)
    for size, image in zip(sizes, images):
        out += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(image), offset)
        offset += len(image)
    out += b"".join(images)
    Path(__file__).with_name("icon.ico").write_bytes(out)


if __name__ == "__main__":
    main()
