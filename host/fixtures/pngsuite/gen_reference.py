#!/usr/bin/env python3
"""Generate BGRA reference data for PngSuite test images.

For each valid PNG (not x*.png corrupt files), decodes with Pillow and writes
raw BGRA8888 bytes to a .bgra file. These serve as ground truth for our decoder.

Also writes a manifest (manifest.txt) with: filename, width, height, color_type,
bit_depth — one line per image, tab-separated.
"""

import os
import struct
from pathlib import Path

from PIL import Image

# PNG color type constants (from IHDR)
COLOR_TYPES = {
    0: "grayscale",
    2: "rgb",
    3: "indexed",
    4: "grayscale_alpha",
    6: "rgba",
}

def png_color_type(path: Path) -> tuple[int, int]:
    """Read color_type and bit_depth directly from IHDR chunk."""
    with open(path, "rb") as f:
        sig = f.read(8)
        assert sig == b"\x89PNG\r\n\x1a\n", f"Bad PNG signature: {path}"
        chunk_len = struct.unpack(">I", f.read(4))[0]
        chunk_type = f.read(4)
        assert chunk_type == b"IHDR" and chunk_len == 13
        ihdr = f.read(13)
        width = struct.unpack(">I", ihdr[0:4])[0]
        height = struct.unpack(">I", ihdr[4:8])[0]
        bit_depth = ihdr[8]
        color_type = ihdr[9]
        return color_type, bit_depth

def to_bgra(img: Image.Image, color_type: int, bit_depth: int) -> bytes:
    """Convert a Pillow image to BGRA8888 bytes.

    Handles two Pillow limitations:
    1. 16-bit images: convert("RGBA") clips to 0-255 instead of scaling >> 8.
    2. Sub-byte grayscale tRNS: Pillow compares tRNS key against scaled value,
       not the original bit-depth value. We compare manually.
    """
    if bit_depth == 16:
        return _to_bgra_16bit(img, color_type)

    # Check for grayscale tRNS with sub-byte depths (Pillow bug workaround).
    trns = img.info.get("transparency")
    if color_type == 0 and isinstance(trns, int) and bit_depth in (1, 2, 4):
        return _to_bgra_gray_trns(img, trns, bit_depth)

    rgba = img.convert("RGBA")
    pixels = rgba.tobytes()  # RGBA order
    # Swap R and B channels
    out = bytearray(len(pixels))
    for i in range(0, len(pixels), 4):
        out[i] = pixels[i + 2]      # B
        out[i + 1] = pixels[i + 1]  # G
        out[i + 2] = pixels[i]      # R
        out[i + 3] = pixels[i + 3]  # A
    return bytes(out)


def _to_bgra_gray_trns(img: Image.Image, trns_key: int, bit_depth: int) -> bytes:
    """Handle grayscale tRNS for sub-byte bit depths.

    Pillow scales pixel values to 0-255 but keeps tRNS key at original bit depth,
    so comparison fails. We scale both and compare correctly.
    """
    scale = {1: 255, 2: 85, 4: 17}[bit_depth]
    trns_scaled = trns_key * scale
    w, h = img.size
    L = img.convert("L")
    out = bytearray(w * h * 4)
    for y in range(h):
        for x in range(w):
            g = L.getpixel((x, y))
            a = 0 if g == trns_scaled else 255
            idx = (y * w + x) * 4
            out[idx] = g
            out[idx + 1] = g
            out[idx + 2] = g
            out[idx + 3] = a
    return bytes(out)


def _to_bgra_16bit(img: Image.Image, color_type: int) -> bytes:
    """Handle 16-bit PNG images correctly.

    Pillow behavior varies by color type:
    - Grayscale 16-bit (type 0): mode "I;16", values are 16-bit → shift >> 8
    - Grayscale+Alpha 16-bit (type 4): mode "LA", already 8-bit per channel
    - RGB 16-bit (type 2): mode "RGB", already 8-bit per channel
    - RGBA 16-bit (type 6): mode "RGBA", already 8-bit per channel

    We detect via mode rather than color_type to handle Pillow's conversions.
    """
    w, h = img.size
    out = bytearray(w * h * 4)
    mode = img.mode

    if mode in ("I;16", "I"):
        # Genuinely 16-bit — shift down to 8-bit
        for y in range(h):
            for x in range(w):
                val = img.getpixel((x, y))
                g = (val >> 8) & 0xFF
                idx = (y * w + x) * 4
                out[idx] = g
                out[idx + 1] = g
                out[idx + 2] = g
                out[idx + 3] = 255
    elif mode == "LA":
        # Grayscale+Alpha, already 8-bit
        for y in range(h):
            for x in range(w):
                g, a = img.getpixel((x, y))
                idx = (y * w + x) * 4
                out[idx] = g
                out[idx + 1] = g
                out[idx + 2] = g
                out[idx + 3] = a
    elif mode == "RGB":
        # RGB, already 8-bit. For 16-bit source, tRNS key is in 16-bit space.
        # Convert key to 8-bit (>> 8) for comparison since Pillow already downsampled.
        trns = img.info.get("transparency")
        trns_8bit = None
        if isinstance(trns, tuple) and len(trns) == 3:
            trns_8bit = ((trns[0] >> 8) & 0xFF, (trns[1] >> 8) & 0xFF, (trns[2] >> 8) & 0xFF)
        for y in range(h):
            for x in range(w):
                r, g, b = img.getpixel((x, y))
                a = 0 if (trns_8bit is not None and (r, g, b) == trns_8bit) else 255
                idx = (y * w + x) * 4
                out[idx] = b
                out[idx + 1] = g
                out[idx + 2] = r
                out[idx + 3] = a
    elif mode == "RGBA":
        # RGBA, already 8-bit
        for y in range(h):
            for x in range(w):
                r, g, b, a = img.getpixel((x, y))
                idx = (y * w + x) * 4
                out[idx] = b
                out[idx + 1] = g
                out[idx + 2] = r
                out[idx + 3] = a
    else:
        raise ValueError(f"Unexpected mode {mode} for 16-bit color type {color_type}")

    return bytes(out)

def main():
    suite_dir = Path(__file__).parent
    ref_dir = suite_dir / "reference"
    ref_dir.mkdir(exist_ok=True)

    manifest_lines = []
    skipped = []
    errors = []

    for png_path in sorted(suite_dir.glob("*.png")):
        name = png_path.name
        # Skip corrupt test files (x*.png)
        if name.startswith("x"):
            skipped.append(name)
            continue

        try:
            color_type, bit_depth = png_color_type(png_path)
            img = Image.open(png_path)
            img.load()
            bgra = to_bgra(img, color_type, bit_depth)
            ref_path = ref_dir / (png_path.stem + ".bgra")
            ref_path.write_bytes(bgra)
            manifest_lines.append(
                f"{name}\t{img.width}\t{img.height}\t{color_type}\t{bit_depth}"
            )
        except Exception as e:
            errors.append(f"{name}: {e}")

    # Write manifest
    manifest_path = ref_dir / "manifest.txt"
    manifest_path.write_text("\n".join(manifest_lines) + "\n")

    print(f"Generated {len(manifest_lines)} reference files in {ref_dir}")
    if skipped:
        print(f"Skipped {len(skipped)} corrupt test files (x*.png)")
    if errors:
        print(f"Errors ({len(errors)}):")
        for e in errors:
            print(f"  {e}")

if __name__ == "__main__":
    main()
