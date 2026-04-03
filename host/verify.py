#!/usr/bin/env python3
"""Visual assertion tool for the document-centric OS.

Returns PASS/FAIL verdicts with numerical evidence.  Never returns raw
measurements for subjective interpretation — every assertion has an
expected value, a tolerance, and a binary outcome.

Usage:
    verify.py <screenshot> --spec <specfile>
    verify.py <screenshot> --assert 'page_centered tol=3' --assert 'frame_not_blank'
    verify.py <screenshot> --assert 'ssim_above ref=/tmp/baseline.png threshold=0.99'

Exit code 0 = all assertions PASS.  Non-zero = at least one FAIL.

Spec file format (one assertion per line, # comments):
    frame_not_blank
    page_centered tol=5
    content_in_region x0=100 y0=50 x1=700 y1=550
    cursor_visible_at x=400 y=300 size=32 tol=10
    pixel_is x=100 y=100 r=255 g=255 b=255 tol=15
    ssim_above ref=/tmp/baseline.png threshold=0.99
    region_not_blank x0=100 y0=100 x1=200 y1=200
    no_content_outside_page margin=5
    page_width expected=600 tol=10
"""

import argparse
import sys
from pathlib import Path

import numpy as np
from PIL import Image

# ── Colour thresholds ─────────────────────────────────────────────
# Background is dark gray (32,32,32) = lum 96.
# Page is white (255,255,255) = lum 765.

BG_LUM     = 120    # sum(r,g,b) ≤ this → background (dark gray or black)
WHITE_LUM  = 680    # sum(r,g,b) ≥ this → page (white)
BRIGHT_MIN = 200    # per-channel minimum for "bright" pixel (page edge detection)


# ── Image helpers ─────────────────────────────────────────────────

def load(path: str) -> np.ndarray:
    """Load image as (H, W, 3) uint8 RGB numpy array."""
    img = Image.open(path).convert("RGB")
    return np.array(img)


def luminance_sum(arr: np.ndarray) -> np.ndarray:
    """Per-pixel sum of RGB channels — shape (H, W)."""
    return arr.astype(np.uint16).sum(axis=2)


def find_page_edges(arr: np.ndarray, row: int | None = None):
    """Find left/right edges of the white page in a given row.

    Scans for contiguous bright region (all channels > BRIGHT_MIN).
    Returns (left_x, right_x) or (None, None).
    """
    h, w, _ = arr.shape
    if row is None:
        row = h // 2
    scanline = arr[row]  # shape (W, 3)
    bright = np.all(scanline > BRIGHT_MIN, axis=1)  # shape (W,)
    indices = np.where(bright)[0]
    if len(indices) == 0:
        return None, None
    return int(indices[0]), int(indices[-1])


def find_page_bounds(arr: np.ndarray):
    """Find the bounding box of the white page area.

    Scans multiple rows to find the full vertical extent.
    Returns (left, top, right, bottom) or None.
    """
    h, w, _ = arr.shape
    # Check all channels > BRIGHT_MIN at every pixel
    bright = np.all(arr > BRIGHT_MIN, axis=2)  # (H, W)

    # Find rows with substantial bright content (at least 10% of width)
    row_counts = bright.sum(axis=1)
    bright_rows = np.where(row_counts > w * 0.1)[0]
    if len(bright_rows) == 0:
        return None

    top = int(bright_rows[0])
    bottom = int(bright_rows[-1])

    # Find columns with bright content in the middle band
    mid_band = bright[top:bottom + 1]
    col_counts = mid_band.sum(axis=0)
    bright_cols = np.where(col_counts > 0)[0]
    if len(bright_cols) == 0:
        return None

    left = int(bright_cols[0])
    right = int(bright_cols[-1])
    return left, top, right, bottom


def find_non_background_region(arr: np.ndarray, x0: int, y0: int,
                                x1: int, y1: int) -> int:
    """Count pixels in region that are not background (black) or page (white).

    These are "content" pixels — text, icons, cursor, UI elements.
    """
    region = arr[y0:y1, x0:x1]
    lum = luminance_sum(region)
    # Not black and not white
    content = (lum > BG_LUM) & (lum < WHITE_LUM)
    return int(content.sum())


# ── Assertions ────────────────────────────────────────────────────
#
# Each returns (passed: bool, evidence: str).
# Evidence is a human-readable explanation of what was measured.

def assert_frame_not_blank(arr: np.ndarray, **_kw) -> tuple[bool, str]:
    """At least some non-black pixels exist."""
    lum = luminance_sum(arr)
    non_black = int((lum > BG_LUM).sum())
    total = arr.shape[0] * arr.shape[1]
    pct = non_black / total * 100
    passed = non_black > 0
    return passed, f"non-black pixels: {non_black}/{total} ({pct:.1f}%)"


def assert_page_centered(arr: np.ndarray, tol: int = 3, **_kw) -> tuple[bool, str]:
    """White page center_x equals frame center_x within tolerance."""
    h, w, _ = arr.shape
    frame_cx = w // 2
    left, right = find_page_edges(arr)
    if left is None:
        return False, "no page detected"
    page_cx = (left + right) // 2
    delta = abs(page_cx - frame_cx)
    passed = delta <= tol
    return passed, (f"page_cx={page_cx} frame_cx={frame_cx} "
                    f"delta={delta} tol={tol}")


def assert_page_width(arr: np.ndarray, expected: int = 0,
                      tol: int = 10, **_kw) -> tuple[bool, str]:
    """Page width matches expected value within tolerance."""
    left, right = find_page_edges(arr)
    if left is None:
        return False, "no page detected"
    width = right - left
    delta = abs(width - expected)
    passed = delta <= tol
    return passed, f"page_width={width} expected={expected} delta={delta} tol={tol}"


def assert_content_in_region(arr: np.ndarray, x0: int = 0, y0: int = 0,
                              x1: int = 0, y1: int = 0,
                              **_kw) -> tuple[bool, str]:
    """Non-background, non-page pixels exist in the specified region."""
    count = find_non_background_region(arr, x0, y0, x1, y1)
    passed = count > 0
    return passed, f"content pixels in ({x0},{y0})-({x1},{y1}): {count}"


def assert_region_not_blank(arr: np.ndarray, x0: int = 0, y0: int = 0,
                             x1: int = 0, y1: int = 0,
                             **_kw) -> tuple[bool, str]:
    """Any non-black pixels exist in the specified region."""
    region = arr[y0:y1, x0:x1]
    lum = luminance_sum(region)
    non_black = int((lum > BG_LUM).sum())
    passed = non_black > 0
    return passed, f"non-black pixels in ({x0},{y0})-({x1},{y1}): {non_black}"


def assert_cursor_visible_at(arr: np.ndarray, x: int = 0, y: int = 0,
                              size: int = 32, tol: int = 10,
                              **_kw) -> tuple[bool, str]:
    """Cursor-like content exists near the expected position.

    Detects the cursor by finding pixels that differ significantly
    from the dominant (most common) color in a local patch.  This
    works on both dark backgrounds and the white page — the cursor
    is always a foreign element at its position.
    """
    h, w, _ = arr.shape
    half = size // 2 + tol
    rx0 = max(0, x - half)
    ry0 = max(0, y - half)
    rx1 = min(w, x + half)
    ry1 = min(h, y + half)

    patch = arr[ry0:ry1, rx0:rx1]
    if patch.size == 0:
        return False, f"patch ({rx0},{ry0})-({rx1},{ry1}) is empty"

    # Dominant color = median (robust to the cursor covering < 50% of patch).
    median_r = int(np.median(patch[:, :, 0]))
    median_g = int(np.median(patch[:, :, 1]))
    median_b = int(np.median(patch[:, :, 2]))

    # Count pixels that differ from the dominant color by > 20 per channel.
    diff = np.abs(patch.astype(np.int16) - np.array([median_r, median_g, median_b],
                                                     dtype=np.int16))
    max_diff = diff.max(axis=2)
    foreign = int((max_diff > 20).sum())

    # Cursor should have at least a few dozen foreign pixels.
    min_pixels = max(4, (size * size) // 20)
    passed = foreign >= min_pixels
    return passed, (f"foreign pixels near ({x},{y}) in "
                    f"({rx0},{ry0})-({rx1},{ry1}): {foreign} "
                    f"(need ≥{min_pixels}, bg=({median_r},{median_g},{median_b}))")


def _extract_cursor_mask(arr: np.ndarray, cx: int, cy: int,
                         half: int = 40, threshold: int = 20):
    """Extract a 32x32 normalized binary mask of the cursor at (cx, cy).

    Returns the mask as a (32, 32) bool array, or None if no cursor found.
    """
    h, w, _ = arr.shape
    x0, y0 = max(0, cx - half), max(0, cy - half)
    x1, y1 = min(w, cx + half), min(h, cy + half)
    patch = arr[y0:y1, x0:x1]
    if patch.size == 0:
        return None

    med = np.array([np.median(patch[:, :, c]) for c in range(3)],
                   dtype=np.int16)
    diff = np.abs(patch.astype(np.int16) - med).max(axis=2)
    mask = diff > threshold

    ys, xs = np.where(mask)
    if len(xs) == 0:
        return None

    # Crop to tight bounding box, pad to square, then resize to 32x32.
    # Padding preserves aspect ratio — a tall narrow I-beam stays narrow
    # instead of stretching to fill the 32x32 canvas.
    tight = mask[ys.min():ys.max() + 1, xs.min():xs.max() + 1]
    mh, mw = tight.shape
    side = max(mw, mh)
    padded = np.zeros((side, side), dtype=bool)
    py = (side - mh) // 2
    px = (side - mw) // 2
    padded[py:py + mh, px:px + mw] = tight

    from PIL import Image as PILImage
    mask_img = PILImage.fromarray(padded.astype(np.uint8) * 255, "L")
    normalized = np.array(
        mask_img.resize((32, 32), PILImage.Resampling.NEAREST)) > 128
    return normalized


def assert_cursor_shape_is(arr: np.ndarray, x: int = 0, y: int = 0,
                           shape: str = "", refs: str = "",
                           **_kw) -> tuple[bool, str]:
    """Verify the cursor at (x, y) matches a named shape.

    Extracts a binary mask of the cursor, then compares against all
    reference masks in the refs directory using IoU (intersection over
    union).  The best-matching reference must be the expected shape,
    and the IoU must exceed 0.35 to count as a match.

    Uses half=24 (tighter than cursor_visible_at) to avoid picking
    up nearby text or UI elements — we only want the cursor itself.

    Parameters:
        x, y:   cursor position in framebuffer pixels
        shape:  expected shape name (e.g., "pointer", "cursor-text")
        refs:   directory containing <name>.png reference masks (32x32 L)
    """
    if not refs:
        refs = str(Path(__file__).parent / "visual" / "refs")

    # Use tight radius to isolate cursor from nearby text.
    mask = _extract_cursor_mask(arr, x, y, half=24)
    if mask is None:
        return False, f"no cursor found at ({x},{y})"

    # Load all reference masks from the directory.
    refs_dir = Path(refs)
    if not refs_dir.is_dir():
        return False, f"refs directory not found: {refs}"

    scores: dict[str, float] = {}
    for ref_path in sorted(refs_dir.glob("*.png")):
        ref_name = ref_path.stem
        ref_img = Image.open(ref_path).convert("L")
        ref_mask = np.array(ref_img.resize((32, 32),
                            Image.Resampling.NEAREST)) > 128
        intersection = np.logical_and(mask, ref_mask).sum()
        union = np.logical_or(mask, ref_mask).sum()
        iou = float(intersection) / float(union) if union > 0 else 0.0
        scores[ref_name] = iou

    if not scores:
        return False, f"no reference masks found in {refs}"

    best_name = max(scores, key=scores.get)
    best_iou = scores[best_name]
    expected_iou = scores.get(shape, 0.0)

    scores_str = ", ".join(f"{k}={v:.3f}" for k, v in sorted(scores.items()))
    passed = best_name == shape and best_iou >= 0.35
    return passed, (f"best match: {best_name} (IoU={best_iou:.3f}), "
                    f"expected: {shape} (IoU={expected_iou:.3f}). "
                    f"All: [{scores_str}]")


def assert_cursor_not_visible(arr: np.ndarray, **_kw) -> tuple[bool, str]:
    """No cursor-like content outside the page region.

    Checks that there are no significant non-background pixel clusters
    outside the detected page area.
    """
    h, w, _ = arr.shape
    bounds = find_page_bounds(arr)
    if bounds is None:
        # No page — check entire frame for unexpected content
        lum = luminance_sum(arr)
        content = int(((lum > BG_LUM) & (lum < WHITE_LUM)).sum())
        passed = content < 100
        return passed, f"no page detected; content pixels in frame: {content}"

    left, top, right, bottom = bounds
    # Create mask for outside-page area
    mask = np.ones((h, w), dtype=bool)
    mask[top:bottom + 1, left:right + 1] = False

    lum = luminance_sum(arr)
    outside_content = int(((lum[mask] > BG_LUM) & (lum[mask] < WHITE_LUM)).sum())
    passed = outside_content < 100
    return passed, (f"content pixels outside page "
                    f"({left},{top})-({right},{bottom}): {outside_content}")


def assert_no_content_outside_page(arr: np.ndarray, margin: int = 5,
                                    **_kw) -> tuple[bool, str]:
    """No non-background pixels exist outside the page + margin.

    Useful for verifying nothing leaked outside the document area.
    Allows a small margin for anti-aliasing at page edges.
    """
    h, w, _ = arr.shape
    bounds = find_page_bounds(arr)
    if bounds is None:
        return False, "no page detected"
    left, top, right, bottom = bounds

    # Expand bounds by margin
    pl = max(0, left - margin)
    pt = max(0, top - margin)
    pr = min(w, right + margin + 1)
    pb = min(h, bottom + margin + 1)

    lum = luminance_sum(arr)
    non_black = lum > BG_LUM

    # Zero out the page region (allowed area)
    allowed = np.zeros((h, w), dtype=bool)
    allowed[pt:pb, pl:pr] = True
    outside = non_black & ~allowed
    count = int(outside.sum())

    passed = count == 0
    return passed, (f"non-background pixels outside page+{margin}px: {count} "
                    f"page=({left},{top})-({right},{bottom})")


def assert_pixel_is(arr: np.ndarray, x: int = 0, y: int = 0,
                    r: int = 0, g: int = 0, b: int = 0,
                    tol: int = 15, **_kw) -> tuple[bool, str]:
    """Pixel at (x, y) has expected RGB value within tolerance."""
    actual = arr[y, x]
    dr = abs(int(actual[0]) - r)
    dg = abs(int(actual[1]) - g)
    db = abs(int(actual[2]) - b)
    max_d = max(dr, dg, db)
    passed = max_d <= tol
    return passed, (f"pixel ({x},{y}): actual=({actual[0]},{actual[1]},{actual[2]}) "
                    f"expected=({r},{g},{b}) max_delta={max_d} tol={tol}")


def assert_ssim_above(arr: np.ndarray, ref: str = "",
                      threshold: float = 0.99,
                      **_kw) -> tuple[bool, str]:
    """Structural similarity against a reference image exceeds threshold."""
    from skimage.metrics import structural_similarity as ssim

    ref_arr = load(ref)
    if arr.shape != ref_arr.shape:
        return False, (f"shape mismatch: {arr.shape} vs {ref_arr.shape}")

    score = ssim(arr, ref_arr, channel_axis=2)
    passed = score >= threshold
    return passed, f"SSIM={score:.6f} threshold={threshold}"


def assert_pixel_diff(arr: np.ndarray, ref: str = "",
                      max_pixels: int = 0, tol: int = 10,
                      **_kw) -> tuple[bool, str]:
    """Number of differing pixels against reference is within limit."""
    ref_arr = load(ref)
    if arr.shape != ref_arr.shape:
        return False, f"shape mismatch: {arr.shape} vs {ref_arr.shape}"

    diff = np.abs(arr.astype(np.int16) - ref_arr.astype(np.int16))
    max_channel_diff = diff.max(axis=2)  # per-pixel max channel diff
    differing = int((max_channel_diff > tol).sum())
    total = arr.shape[0] * arr.shape[1]
    passed = differing <= max_pixels
    return passed, (f"differing pixels: {differing}/{total} "
                    f"(max_allowed={max_pixels}, channel_tol={tol})")


def assert_caret_between(arr: np.ndarray, ref: str = "",
                         x: int = 0, y: int = 0, tol: int = 20,
                         min_h: int = 10, max_h: int = 80,
                         **_kw) -> tuple[bool, str]:
    """Text caret (thin vertical line) exists near (x, y) with height in range.

    Diffs against a reference (no-cursor) frame to isolate the caret.
    Finds the tallest thin vertical feature in the diff and checks:
    - Position within `tol` of expected (x, y)
    - Height between min_h and max_h pixels
    """
    ref_arr = load(ref)
    if arr.shape != ref_arr.shape:
        return False, f"shape mismatch: {arr.shape} vs {ref_arr.shape}"

    diff = np.abs(arr.astype(np.int16) - ref_arr.astype(np.int16)).max(axis=2)
    mask = diff > 20

    # Search near expected position
    half = tol + 30
    h, w = mask.shape
    rx0 = max(0, x - half)
    ry0 = max(0, y - half * 2)
    rx1 = min(w, x + half)
    ry1 = min(h, y + half * 2)
    patch = mask[ry0:ry1, rx0:rx1]

    if patch.sum() < 5:
        return False, f"no caret-like diff near ({x},{y})"

    # Find columns with vertical runs
    best_col = -1
    best_height = 0
    best_top = 0
    for cx in range(patch.shape[1]):
        col = patch[:, cx]
        # Find longest contiguous run of True
        run_start = None
        for cy in range(len(col)):
            if col[cy]:
                if run_start is None:
                    run_start = cy
            else:
                if run_start is not None:
                    run_h = cy - run_start
                    if run_h > best_height:
                        best_height = run_h
                        best_col = cx
                        best_top = run_start
                    run_start = None
        if run_start is not None:
            run_h = len(col) - run_start
            if run_h > best_height:
                best_height = run_h
                best_col = cx
                best_top = run_start

    if best_height < 5:
        return False, f"no vertical feature found near ({x},{y})"

    actual_x = rx0 + best_col
    actual_y = ry0 + best_top + best_height // 2
    dx = abs(actual_x - x)
    dy = abs(actual_y - y)

    pos_ok = dx <= tol and dy <= tol
    h_ok = min_h <= best_height <= max_h

    msgs = []
    msgs.append(f"caret at ({actual_x},{ry0+best_top})-({actual_x},{ry0+best_top+best_height})")
    msgs.append(f"height={best_height} (range {min_h}..{max_h})")
    msgs.append(f"offset=({dx},{dy}) tol={tol}")
    if not pos_ok:
        msgs.append("POSITION OUT OF RANGE")
    if not h_ok:
        msgs.append("HEIGHT OUT OF RANGE")

    return pos_ok and h_ok, " | ".join(msgs)


def assert_weight_varies(arr: np.ndarray, x0: int = 0, y0: int = 0,
                         x1: int = 0, y1: int = 0,
                         segments: int = 3,
                         **_kw) -> tuple[bool, str]:
    """Font weight variation — dark pixel density increases left-to-right.

    Divides the region into `segments` vertical bands and checks that
    they have different dark-pixel densities (not all the same weight).
    """
    h, w, _ = arr.shape
    x0, y0 = max(0, x0), max(0, y0)
    x1, y1 = min(w, x1), min(h, y1)
    region = arr[y0:y1, x0:x1]
    if region.size == 0:
        return False, "empty region"

    # Dark pixel = any channel < 160 (text on white page)
    dark = (region.min(axis=2) < 160)
    rw = x1 - x0
    seg_w = rw // segments
    densities = []
    for i in range(segments):
        sx0 = i * seg_w
        sx1 = sx0 + seg_w
        band = dark[:, sx0:sx1]
        density = band.sum() / max(1, band.size)
        densities.append(density)

    # Check that densities are not all the same (vary by > 20%)
    if max(densities) < 0.001:
        return False, f"no dark content in region"
    ratio = min(densities) / max(densities) if max(densities) > 0 else 1.0
    varies = ratio < 0.80  # at least 20% variation
    msg = f"densities={[f'{d:.4f}' for d in densities]} ratio={ratio:.3f}"
    return varies, msg


def assert_italic_slant(arr: np.ndarray, ref: str = "",
                        x0: int = 0, y0: int = 0,
                        x1: int = 0, y1: int = 0,
                        threshold: float = 0.80,
                        **_kw) -> tuple[bool, str]:
    """Italic text region matches a known-good reference via SSIM.

    Extracts the text band (rows with dark content) from the region,
    resizes to match the reference dimensions, and computes structural
    similarity.  This catches italic-font failures (wrong glyphs,
    roman fallback, missing rendering) without relying on fragile
    geometric lean measurement.
    """
    from skimage.metrics import structural_similarity as ssim

    ref_arr = load(ref)

    # Extract text band from test region
    region = arr[y0:y1, x0:x1]
    dark = region.min(axis=2) < 120
    row_sums = dark.sum(axis=1)
    text_rows = np.where(row_sums > 3)[0]
    if len(text_rows) < 6:
        return False, "no text band in region"
    band = region[int(text_rows[0]):int(text_rows[-1]) + 1]

    # Center-crop taller to match shorter (avoids resize distortion).
    if band.shape[0] > ref_arr.shape[0]:
        excess = band.shape[0] - ref_arr.shape[0]
        band = band[excess // 2: excess // 2 + ref_arr.shape[0]]
    elif ref_arr.shape[0] > band.shape[0]:
        excess = ref_arr.shape[0] - band.shape[0]
        ref_arr = ref_arr[excess // 2: excess // 2 + band.shape[0]]

    # Ensure matching width (should always match, but be safe).
    min_w = min(band.shape[1], ref_arr.shape[1])
    band = band[:, :min_w]
    ref_arr = ref_arr[:, :min_w]

    score = ssim(band, ref_arr, channel_axis=2)
    passed = score >= threshold
    return passed, (f"SSIM={score:.4f} threshold={threshold} "
                    f"band={band.shape[0]}x{band.shape[1]} "
                    f"ref={ref_arr.shape[0]}x{ref_arr.shape[1]}")


def _find_text_bands(dark, min_gap: int = 3):
    """Find separate horizontal text bands in a dark mask.

    Groups consecutive dark rows, merging bands separated by < min_gap
    rows.  Returns list of (y_start, y_end) tuples.
    """
    row_sums = dark.sum(axis=1)
    bands = []
    in_band = False
    start = 0
    for ry in range(len(row_sums)):
        if row_sums[ry] > 3:
            if not in_band:
                start = ry
                in_band = True
        else:
            if in_band:
                bands.append((start, ry))
                in_band = False
    if in_band:
        bands.append((start, len(row_sums)))
    # Merge bands separated by < min_gap
    merged = []
    for b in bands:
        if merged and b[0] - merged[-1][1] < min_gap:
            merged[-1] = (merged[-1][0], b[1])
        else:
            merged.append(b)
    return merged


def assert_baseline_aligned(arr: np.ndarray, x0: int = 0, y0: int = 0,
                             x1: int = 0, y1: int = 0,
                             tol: int = 2,
                             **_kw) -> tuple[bool, str]:
    """Mixed-size text in region has aligned baselines (left vs right halves).

    Finds the tallest text band in the region (ignoring thin subtitle
    tails or decoration lines from adjacent content), then uses mode-based
    baseline comparison between left and right halves.
    """
    from collections import Counter
    region = arr[y0:y1, x0:x1]
    dark = region.min(axis=2) < 120

    # Find the tallest text band (the target mixed-size line).
    bands = _find_text_bands(dark, min_gap=3)
    if not bands:
        return False, "no text content in region"
    tallest = max(bands, key=lambda b: b[1] - b[0])
    crop_y0, crop_y1 = tallest
    dark = dark[crop_y0:crop_y1, :]
    if dark.shape[0] < 3:
        return False, "text band too short"

    rw = dark.shape[1]
    mid = rw // 2

    def mode_baseline(half):
        bottoms = []
        for cx in range(half.shape[1]):
            col = half[:, cx]
            dark_rows = np.where(col)[0]
            if len(dark_rows) > 0:
                bottoms.append(int(dark_rows[-1]))
        if not bottoms:
            return -1
        return Counter(bottoms).most_common(1)[0][0]

    left_base = mode_baseline(dark[:, :mid])
    right_base = mode_baseline(dark[:, mid:])
    if left_base < 0 or right_base < 0:
        return False, "insufficient content in one or both halves"
    delta = abs(left_base - right_base)
    passed = delta <= tol
    return passed, (f"left_baseline=y{left_base + y0 + crop_y0} "
                    f"right_baseline=y{right_base + y0 + crop_y0} "
                    f"delta={delta}px tol={tol}")


def assert_underline_gap(arr: np.ndarray, x0: int = 0, y0: int = 0,
                          x1: int = 0, y1: int = 0,
                          min_gap: int = 0, max_gap: int = 8,
                          **_kw) -> tuple[bool, str]:
    """Underline decoration is below text body with correct gap.

    Detects the underline by its distinctive row-density signature:
    1-4 consecutive rows where dark pixel count exceeds 40% of text
    width (a solid horizontal line, much denser than character rows).
    Then finds the text body above it by searching for the last row
    with moderate density.  The gap must be within range.
    """
    region = arr[y0:y1, x0:x1]
    dark = region.min(axis=2) < 120
    row_sums = dark.sum(axis=1)

    # Text width
    text_cols = np.where(dark.sum(axis=0) > 0)[0]
    if len(text_cols) < 10:
        return False, "no text content in region"
    text_w = int(text_cols[-1] - text_cols[0])

    # Underline rows: high density (> 40% of text width) in thin runs.
    density_threshold = text_w * 0.4
    in_run = False
    underlines = []
    start = 0
    for ry in range(len(row_sums)):
        if row_sums[ry] > density_threshold:
            if not in_run:
                start = ry
                in_run = True
        else:
            if in_run:
                run_h = ry - start
                if run_h <= 4:  # underline = thin dense stripe
                    underlines.append((start, ry))
                in_run = False

    if not underlines:
        return False, "no underline (thin dense row run) found"

    # For each underline candidate, find the text body above it.
    for ul_start, ul_end in underlines:
        text_bottom = -1
        for ty in range(ul_start - 1, -1, -1):
            if row_sums[ty] > text_w * 0.05 and row_sums[ty] <= density_threshold:
                text_bottom = ty
                break
        if text_bottom < 0:
            continue

        gap = ul_start - text_bottom
        passed = min_gap <= gap <= max_gap
        return passed, (f"text_bottom=y{text_bottom + y0} "
                        f"ul_top=y{ul_start + y0} "
                        f"gap={gap}px range=[{min_gap},{max_gap}]")

    return False, "underline found but no text body above it"


def assert_caret_skewed(arr: np.ndarray, ref: str = "",
                         x: int = 0, y: int = 0, tol: int = 30,
                         min_lean: int = 2,
                         **_kw) -> tuple[bool, str]:
    """Text caret near (x, y) is skewed (non-vertical, italic lean).

    Diffs against a reference (no-cursor) frame to isolate the caret,
    then measures horizontal displacement from top to bottom of the
    caret.  A skewed caret has at least min_lean pixels of shift.
    """
    ref_arr = load(ref)
    if arr.shape != ref_arr.shape:
        return False, f"shape mismatch: {arr.shape} vs {ref_arr.shape}"

    diff = np.abs(arr.astype(np.int16) - ref_arr.astype(np.int16)).max(axis=2)
    mask = diff > 20

    half = tol + 30
    h, w = mask.shape
    rx0, ry0 = max(0, x - half), max(0, y - half * 2)
    rx1, ry1 = min(w, x + half), min(h, y + half * 2)
    patch = mask[ry0:ry1, rx0:rx1]

    if patch.sum() < 5:
        return False, f"no caret-like diff near ({x},{y})"

    ys, xs = np.where(patch)
    if len(xs) == 0:
        return False, "no diff pixels found"

    y_min, y_max = int(ys.min()), int(ys.max())
    y_mid = (y_min + y_max) // 2

    top_xs = xs[ys <= y_mid]
    bot_xs = xs[ys > y_mid]

    if len(top_xs) == 0 or len(bot_xs) == 0:
        return False, "caret too small to measure lean"

    top_com = float(top_xs.mean())
    bot_com = float(bot_xs.mean())
    lean = top_com - bot_com

    passed = abs(lean) >= min_lean
    return passed, (f"caret lean={lean:+.1f}px (top_com={top_com:.1f} "
                    f"bot_com={bot_com:.1f}) min_lean={min_lean}")


# ── Assertion registry ────────────────────────────────────────────

ASSERTIONS = {
    "frame_not_blank":           assert_frame_not_blank,
    "page_centered":             assert_page_centered,
    "page_width":                assert_page_width,
    "content_in_region":         assert_content_in_region,
    "region_not_blank":          assert_region_not_blank,
    "cursor_visible_at":         assert_cursor_visible_at,
    "cursor_shape_is":           assert_cursor_shape_is,
    "cursor_not_visible":        assert_cursor_not_visible,
    "no_content_outside_page":   assert_no_content_outside_page,
    "pixel_is":                  assert_pixel_is,
    "ssim_above":                assert_ssim_above,
    "pixel_diff":                assert_pixel_diff,
    "caret_between":             assert_caret_between,
    "weight_varies":             assert_weight_varies,
    "italic_slant":              assert_italic_slant,
    "baseline_aligned":          assert_baseline_aligned,
    "underline_gap":             assert_underline_gap,
    "caret_skewed":              assert_caret_skewed,
}

# Parameters that should be parsed as specific types
PARAM_TYPES = {
    "x": int, "y": int, "x0": int, "y0": int, "x1": int, "y1": int,
    "r": int, "g": int, "b": int,
    "tol": int, "size": int, "margin": int, "expected": int,
    "max_pixels": int,
    "min_h": int, "max_h": int,
    "min_gap": int, "max_gap": int,
    "min_lean": float,
    "segments": int,
    "threshold": float, "min_diff": float,
    "ref": str,
    "shape": str,
    "refs": str,
}


# ── Spec parsing ──────────────────────────────────────────────────

def parse_assertion(line: str) -> tuple[str, dict]:
    """Parse 'assertion_name key=value key=value ...' into (name, kwargs)."""
    parts = line.strip().split()
    name = parts[0]
    kwargs = {}
    for part in parts[1:]:
        if "=" not in part:
            raise ValueError(f"bad parameter (expected key=value): {part!r}")
        k, v = part.split("=", 1)
        if k in PARAM_TYPES:
            kwargs[k] = PARAM_TYPES[k](v)
        else:
            kwargs[k] = v
    return name, kwargs


def load_spec(path: str) -> list[tuple[str, dict]]:
    """Load assertion spec from file."""
    assertions = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            assertions.append(parse_assertion(line))
    return assertions


# ── Main ──────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="Visual assertion tool — PASS/FAIL verdicts")
    parser.add_argument("screenshot", help="Path to screenshot PNG")
    parser.add_argument("--spec", action="append", default=[],
                        help="Path to assertion spec file (repeatable)")
    parser.add_argument("--assert", dest="inline_asserts", action="append",
                        default=[], help="Inline assertion (repeatable)")
    parser.add_argument("--json", action="store_true",
                        help="Output results as JSON")
    args = parser.parse_args()

    # Collect all assertions
    all_assertions: list[tuple[str, dict]] = []
    for spec_path in args.spec:
        all_assertions.extend(load_spec(spec_path))
    for inline in args.inline_asserts:
        all_assertions.append(parse_assertion(inline))

    if not all_assertions:
        print("ERROR: no assertions specified (use --spec or --assert)")
        sys.exit(2)

    # Load image
    arr = load(args.screenshot)

    # Run assertions
    results = []
    all_passed = True
    for name, kwargs in all_assertions:
        if name not in ASSERTIONS:
            print(f"ERROR: unknown assertion: {name!r}")
            print(f"  available: {', '.join(sorted(ASSERTIONS))}")
            sys.exit(2)

        fn = ASSERTIONS[name]
        passed, evidence = fn(arr, **kwargs)
        results.append({"assertion": name, "params": kwargs,
                        "passed": passed, "evidence": evidence})
        if not passed:
            all_passed = False

    # Output
    if args.json:
        import json
        print(json.dumps({"passed": all_passed, "results": results}, indent=2))
    else:
        for r in results:
            verdict = "PASS" if r["passed"] else "FAIL"
            params_str = " ".join(f"{k}={v}" for k, v in r["params"].items())
            label = r["assertion"]
            if params_str:
                label += f" ({params_str})"
            print(f"  [{verdict}] {label}")
            print(f"         {r['evidence']}")
        print()
        if all_passed:
            print(f"PASSED — {len(results)} assertion(s)")
        else:
            failed = sum(1 for r in results if not r["passed"])
            print(f"FAILED — {failed}/{len(results)} assertion(s) failed")

    sys.exit(0 if all_passed else 1)


if __name__ == "__main__":
    main()
