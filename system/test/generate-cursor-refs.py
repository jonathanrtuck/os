#!/usr/bin/env python3
"""Generate cursor shape reference masks from SVG source files.

Rasterizes each cursor SVG at the same parameters the OS uses
(64×64 texture, 24pt at 2× scale, stroke-width 2, margin 2 viewbox
units), then extracts a 32×32 binary mask of non-transparent pixels.

These reference masks are the ground truth for cursor_shape_is
assertions — derived from the specification (SVG), not from the
rendered output.

Usage:
    python3 generate-cursor-refs.py
"""

import io
import sys
from pathlib import Path

import cairosvg
import numpy as np
from PIL import Image

# ── OS rendering parameters (must match metal-render/main.rs) ─────

CURSOR_TEX_SIZE = 64          # pixels
CURSOR_SIZE_PT = 24.0         # points
SCALE_FACTOR = 2.0            # Retina
VIEWBOX = 24.0                # SVG viewbox (both icons are 24×24)
STROKE_WIDTH = 2.0            # SVG stroke-width in viewbox units

# Margin in viewbox units for stroke overflow.
MARGIN_VB = STROKE_WIDTH / 2.0 + 1.0  # = 2.0

# Pixel scale: maps viewbox units to pixels.
PX_SCALE = (CURSOR_SIZE_PT * SCALE_FACTOR) / VIEWBOX  # = 2.0

# The SVG content occupies [MARGIN_VB, MARGIN_VB+VIEWBOX] in viewbox
# units, which maps to pixels [MARGIN_VB*PX_SCALE, (MARGIN_VB+VIEWBOX)*PX_SCALE]
# = [4, 52] in the 64×64 texture.

# ── Paths ─────────────────────────────────────────────────────────

SCRIPT_DIR = Path(__file__).parent
SVG_DIR = SCRIPT_DIR.parent / "resources" / "icons"
REFS_DIR = SCRIPT_DIR / "visual" / "refs"

CURSORS = {
    "pointer": SVG_DIR / "pointer.svg",
    "cursor-text": SVG_DIR / "cursor-text.svg",
}


def svg_to_mask(svg_path: Path) -> np.ndarray:
    """Rasterize an SVG and extract a 32×32 binary mask.

    Renders the SVG into a CURSOR_TEX_SIZE×CURSOR_TEX_SIZE image with
    black fill + white stroke on a transparent background.  Any pixel
    with alpha > 0 becomes True in the mask.
    """
    # Read the SVG source and modify it for our rendering:
    # - Set explicit fill="black" and stroke="white" (replacing currentColor)
    # - Keep the original stroke-width
    svg_text = svg_path.read_text()
    svg_text = svg_text.replace('fill="none"', 'fill="black"')
    svg_text = svg_text.replace('stroke="currentColor"', 'stroke="white"')

    # Rasterize at the cursor texture size.
    # cairosvg maps the SVG viewbox to the output size.  We want the
    # 24×24 viewbox to land at the correct offset within a 64×64 image.
    #
    # Approach: rasterize the SVG at the content size (VIEWBOX * PX_SCALE),
    # then paste into a 64×64 canvas at the margin offset.
    content_px = int(VIEWBOX * PX_SCALE)  # 48
    margin_px = int(MARGIN_VB * PX_SCALE)  # 4

    png_data = cairosvg.svg2png(
        bytestring=svg_text.encode("utf-8"),
        output_width=content_px,
        output_height=content_px,
    )

    content_img = Image.open(io.BytesIO(png_data)).convert("RGBA")

    # Create the full cursor texture canvas.
    canvas = Image.new("RGBA", (CURSOR_TEX_SIZE, CURSOR_TEX_SIZE), (0, 0, 0, 0))
    canvas.paste(content_img, (margin_px, margin_px))

    # Extract binary mask: any pixel with alpha > 0.
    arr = np.array(canvas)
    alpha = arr[:, :, 3]
    mask = alpha > 0

    # Crop to tight bounding box, pad to square (preserving aspect ratio),
    # then resize to 32×32.  Padding ensures a tall narrow I-beam stays
    # narrow in the 32×32 canvas, making it easily distinguishable from
    # the roughly-square pointer arrow.
    ys, xs = np.where(mask)
    if len(xs) == 0:
        print(f"  WARNING: no non-transparent pixels in {svg_path.name}")
        return np.zeros((32, 32), dtype=bool)

    tight = mask[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
    mw, mh = tight.shape[1], tight.shape[0]

    side = max(mw, mh)
    padded = np.zeros((side, side), dtype=bool)
    py = (side - mh) // 2
    px = (side - mw) // 2
    padded[py:py + mh, px:px + mw] = tight

    mask_img = Image.fromarray(padded.astype(np.uint8) * 255, "L")
    normalized = np.array(
        mask_img.resize((32, 32), Image.Resampling.NEAREST)
    ) > 128

    print(f"  {svg_path.name}: {mw}×{mh} (pad→{side}×{side}) → 32×32 "
          f"({int(normalized.sum())} mask pixels)")

    return normalized


def main():
    REFS_DIR.mkdir(parents=True, exist_ok=True)

    print("Generating cursor reference masks from SVG sources:")
    for name, svg_path in CURSORS.items():
        if not svg_path.exists():
            print(f"  ERROR: {svg_path} not found")
            sys.exit(1)

        mask = svg_to_mask(svg_path)
        out_path = REFS_DIR / f"{name}.png"
        Image.fromarray(mask.astype(np.uint8) * 255, "L").save(out_path)
        print(f"  → {out_path}")

    print("Done.")


if __name__ == "__main__":
    main()
