#!/usr/bin/env python3
"""Rebuild the licensed Browser Sans bitmap fallback (requires Pillow).

Pass LiberationSans-Regular.ttf from a Liberation Fonts installation.
The shipped bitmap is sufficient for ordinary builds; Pillow is optional.
"""
import argparse
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser()
parser.add_argument("font", type=Path)
args = parser.parse_args()
font = ImageFont.truetype(str(args.font), 12)
ascent, descent = font.getmetrics()
advances, pixels = [], bytearray()
for value in range(256):
    try:
        character = bytes([value]).decode("cp1252")
    except UnicodeDecodeError:
        character = chr(value)
    control = value < 32 or value == 127
    advances.append(0 if control else round(font.getlength(character)))
    glyph = Image.new("L", (16, 16))
    if not control:
        ImageDraw.Draw(glyph).text((0, ascent), character, font=font, fill=255, anchor="ls")
    pixels.extend(glyph.tobytes())
if ascent + descent > 16 or max(advances) > 16:
    raise ValueError("Font exceeds the 16 by 16 glyph cell")
output = Path(__file__).resolve().parent.parent / "crates/wlam-host/assets/default-font.bin"
output.write_bytes(bytes([ascent, descent, ascent + descent, max(advances)]) + bytes(advances) + pixels)
