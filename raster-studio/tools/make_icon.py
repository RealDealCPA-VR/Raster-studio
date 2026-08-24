#!/usr/bin/env python3
"""Generate a simple Raster Studio application icon.

Writes a 32x32 32-bit ICO containing one BGRA frame plus an empty AND mask,
with a simple "RS" tile motif (a light rounded square with a darker corner
accent) so the binary and the installer can carry a real icon rather than the
OS default. Pure standard library; run `python tools/make_icon.py` from
`raster-studio/` and it emits `assets/raster-studio.ico`.
"""
import struct, zlib, os

S = 32

def px(x, y):
    # A rounded-corner tile with a corner fold.
    # Background: transparent outside the rounded tile.
    def inside_rounded(x, y, r=6):
        cx = min(max(x, r), S - 1 - r)
        cy = min(max(y, r), S - 1 - r)
        return (x - cx) ** 2 + (y - cy) ** 2 <= r * r

    outer = inside_rounded(x, y)
    # Inner filled square (the "R" field) and a corner accent triangle.
    if not outer:
        return (0, 0, 0, 0)
    # Tile fill: light slate.
    if x + y < S // 2:
        return (60, 90, 140, 255)      # corner accent (deep blue)
    if 8 <= x <= 23 and 8 <= y <= 23 and not (x + y > 40):
        return (235, 240, 248, 255)    # glyph block (near white)
    return (120, 150, 190, 255)        # main tile (blue-grey)

pixels = b"".join(
    bytes(px(x, y))  # BGRA
    for y in range(S)
    for x in range(S)
)
and_mask = b"\x00" * ((S * S) // 8)

bih = struct.pack("<IiiHHIIiiII", 40, S, S * 2, 1, 32, 0, 0, 0, 0, 0, 0)
frame = bih + pixels + and_mask

header = struct.pack("<HHH", 0, 1, 1)
entry = struct.pack(
    "<BBBBHHII", S, S, 0, 0, 1, 32, len(frame), 6 + 16
)

data = header + entry + frame
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "raster-studio.ico")
os.makedirs(os.path.dirname(out), exist_ok=True)
with open(out, "wb") as f:
    f.write(data)
print("wrote", os.path.normpath(out), len(data), "bytes")
