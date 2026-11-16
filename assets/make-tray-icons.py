#!/usr/bin/env python3
"""Draws the tray icons as raw RGBA (no image decoder needed in the app):

    python3 make-tray-icons.py      (needs Pillow)

tray-template-{on,off}.rgba  44x44, black + alpha: macOS draws these in the
                             menu bar's own colour (a "template" image).
tray-{on,off}.rgba           32x32, colour, for Windows and Linux.
"On" is the full mark, the route and the signal dot; "off" is the route alone.
"""
import pathlib
from PIL import Image, ImageDraw

here = pathlib.Path(__file__).parent
S = 8  # draw large, shrink: smooth edges


def mark(size, ink, signal, dot, tile=None, inset=0.0):
    big = size * S
    img = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    if tile:
        d.rounded_rectangle([0, 0, big - 1, big - 1], radius=big * 0.22, fill=tile)
    # The mark's 64x64 grid, fitted inside the inset.
    scale = big * (1 - 2 * inset) / 64
    off = big * inset
    p = lambda x, y: (off + x * scale, off + y * scale + 6 * scale)  # nudge down: the mark sits high in its grid
    d.line([p(4, 20), p(24, 20), p(40, 44), p(45, 44)], fill=ink, width=round(9 * scale), joint="curve")
    if dot:
        r = 6.5 * scale
        cx, cy = p(55, 44)
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=signal)
    return img.resize((size, size), Image.LANCZOS)


def save(img, name):
    (here / name).write_bytes(img.tobytes())


black = (0, 0, 0, 255)
save(mark(44, black, black, True, inset=0.04), "tray-template-on.rgba")
save(mark(44, black, black, False, inset=0.04), "tray-template-off.rgba")
save(mark(32, (241, 241, 238, 255), (255, 106, 43, 255), True, tile=(30, 30, 29, 255), inset=0.16), "tray-on.rgba")
save(mark(32, (162, 162, 157, 255), None, False, tile=(30, 30, 29, 255), inset=0.16), "tray-off.rgba")
