#!/usr/bin/env python3
"""Regenerates icon.png, icon.icns and icon.ico from icon.svg (macOS, needs Pillow).

    python3 make-icons.py

Quick Look rasterizes the SVG but paints the page white, so the area outside
the rounded tile is cut back to transparent with a mask of the same shape.
"""
import pathlib, subprocess, tempfile
from PIL import Image, ImageDraw

here = pathlib.Path(__file__).parent
with tempfile.TemporaryDirectory() as tmp:
    subprocess.run(["qlmanage", "-t", "-s", "1024", "-o", tmp, here / "icon.svg"], check=True, capture_output=True)
    icon = Image.open(pathlib.Path(tmp) / "icon.svg.png").convert("RGB")

scale = 4  # draw the mask large and shrink it: smooth edges
mask = Image.new("L", (1024 * scale, 1024 * scale), 0)
ImageDraw.Draw(mask).rounded_rectangle([100 * scale, 100 * scale, 924 * scale, 924 * scale], radius=185 * scale, fill=255)
icon.putalpha(mask.resize((1024, 1024), Image.LANCZOS))

icon.save(here / "icon.png")
icon.save(here / "icon.icns")
icon.save(here / "icon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
