#!/usr/bin/env python3
"""Packs PNG files into a Windows .ico: make-ico.py OUT.ico IN.png...

Sizes below 256 are stored as bitmaps, which every Windows tool reads;
256 is stored as the PNG itself. Inputs must be 8-bit RGBA, not interlaced.
"""
import struct
import sys
import zlib


def read_png(data):
    """Returns (width, height, rows of RGBA bytes, top row first)."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise SystemExit("make-ico: not a PNG file")
    at, packed, header = 8, b"", None
    while at < len(data):
        length, kind = struct.unpack(">I4s", data[at : at + 8])
        body = data[at + 8 : at + 8 + length]
        if kind == b"IHDR":
            header = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            packed += body
        at += 12 + length
    width, height, depth, color, _, _, interlace = header
    if (depth, color, interlace) != (8, 6, 0):
        raise SystemExit("make-ico: PNG must be 8-bit RGBA, not interlaced")
    raw, stride, rows, previous = zlib.decompress(packed), width * 4, [], None
    for y in range(height):
        start = y * (stride + 1)
        kind, row = raw[start], bytearray(raw[start + 1 : start + 1 + stride])
        for x in range(stride):
            left = row[x - 4] if x >= 4 else 0
            up = previous[x] if previous else 0
            corner = previous[x - 4] if previous and x >= 4 else 0
            if kind == 1:
                row[x] = (row[x] + left) & 255
            elif kind == 2:
                row[x] = (row[x] + up) & 255
            elif kind == 3:
                row[x] = (row[x] + (left + up) // 2) & 255
            elif kind == 4:
                guess = left + up - corner
                nearest = min((abs(guess - left), 0), (abs(guess - up), 1), (abs(guess - corner), 2))
                row[x] = (row[x] + (left, up, corner)[nearest[1]]) & 255
        rows.append(row)
        previous = row
    return width, height, rows


def bitmap(width, height, rows):
    """A 32-bit icon bitmap: header, BGRA rows bottom first, empty mask."""
    header = struct.pack("<IiiHHIIiiII", 40, width, height * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    pixels = b""
    for row in reversed(rows):
        swapped = bytearray(row)
        swapped[0::4], swapped[2::4] = row[2::4], row[0::4]
        pixels += bytes(swapped)
    mask_row = b"\0" * (((width + 31) // 32) * 4)
    return header + pixels + mask_row * height


def main():
    out, sources = sys.argv[1], sys.argv[2:]
    images = []
    for source in sources:
        data = open(source, "rb").read()
        width, height, rows = read_png(data)
        images.append((width, height, data if width >= 256 else bitmap(width, height, rows)))
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b""
    for width, height, data in images:
        # A size of 256 is written as 0.
        entries += struct.pack(
            "<BBBBHHII", width % 256, height % 256, 0, 0, 1, 32, len(data), offset
        )
        offset += len(data)
    with open(out, "wb") as ico:
        ico.write(header + entries + b"".join(data for _, _, data in images))


if __name__ == "__main__":
    main()
