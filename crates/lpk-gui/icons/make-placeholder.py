"""Writes the placeholder source icon (PLAN E5-2, decision (e): the final set is the owner's).

    python crates/lpk-gui/icons/make-placeholder.py crates/lpk-gui/icons/source.png
    cd crates/lpk-gui && cargo tauri icon icons/source.png

`cargo tauri icon` then writes the sizes tauri.conf.json lists (and others: only the desktop
ones are kept). Standard library only: a 1024x1024 RGBA PNG of a rounded blue square with a
white box-and-lid mark.
"""
import struct
import sys
import zlib

N = 1024


def inside_rounded(x, y, x0, y0, x1, y1, r):
    if x < x0 or x >= x1 or y < y0 or y >= y1:
        return False
    cx = min(max(x, x0 + r), x1 - r - 1)
    cy = min(max(y, y0 + r), y1 - r - 1)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def pixel(x, y):
    if not inside_rounded(x, y, 64, 64, N - 64, N - 64, 180):
        return (0, 0, 0, 0)
    # The lid and the box: two white bars, the box hollow.
    if inside_rounded(x, y, 232, 280, 792, 400, 40):
        return (255, 255, 255, 255)
    if inside_rounded(x, y, 272, 430, 752, 780, 48) and not inside_rounded(
        x, y, 332, 490, 692, 720, 24
    ):
        return (255, 255, 255, 255)
    if 452 <= x < 572 and 520 <= y < 580:
        return (255, 255, 255, 255)
    return (0, 95, 184, 255)


def chunk(kind, data):
    c = struct.pack(">I", len(data)) + kind + data
    return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)


def main(out):
    raw = bytearray()
    for y in range(N):
        raw.append(0)
        for x in range(N):
            raw.extend(pixel(x, y))
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(out, "wb") as f:
        f.write(png)


if __name__ == "__main__":
    main(sys.argv[1])
