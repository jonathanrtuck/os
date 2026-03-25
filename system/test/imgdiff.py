#!/usr/bin/env python3
"""Compare two hypervisor screenshots for layout differences.

Usage:
    python3 imgdiff.py <image_a> <image_b>
    python3 imgdiff.py <image>              # measure single image

Reports:
    - Page left edge x (first white pixel in middle row)
    - Page right edge x (last white pixel in middle row)
    - Page center x
    - Image center (test image bounding box, if present)
    - Pixel-level diff count between two images
"""

import sys
from PIL import Image

def find_page_edges(img, row=None):
    """Find the left and right edges of the white page in a row.
    Returns (left_x, right_x) or (None, None) if no page found."""
    w, h = img.size
    if row is None:
        row = h // 2
    left = None
    right = None
    for x in range(w):
        r, g, b = img.getpixel((x, row))[:3]
        bright = r > 200 and g > 200 and b > 200
        if bright and left is None:
            left = x
        if bright:
            right = x
    return left, right

def find_colored_region(img):
    """Find bounding box of the test image (colorful non-black, non-white pixels)."""
    w, h = img.size
    min_x, min_y, max_x, max_y = w, h, 0, 0
    # Sample every 4th pixel for speed
    for y in range(0, h, 4):
        for x in range(0, w, 4):
            r, g, b = img.getpixel((x, y))[:3]
            # Not black and not white and has some color variation
            lum = r + g + b
            if 30 < lum < 700 and max(r, g, b) - min(r, g, b) > 20:
                min_x = min(min_x, x)
                min_y = min(min_y, y)
                max_x = max(max_x, x)
                max_y = max(max_y, y)
    if max_x > min_x and max_y > min_y:
        cx = (min_x + max_x) // 2
        cy = (min_y + max_y) // 2
        return min_x, min_y, max_x, max_y, cx, cy
    return None

def measure(path):
    img = Image.open(path)
    w, h = img.size
    print(f"  {path}")
    print(f"    size: {w}x{h}")

    mid = h // 2
    left, right = find_page_edges(img, mid)
    if left is not None:
        center = (left + right) // 2
        page_w = right - left
        print(f"    page edges (row {mid}): left={left} right={right} center={center} width={page_w}")
    else:
        print(f"    page edges (row {mid}): not found")

    colored = find_colored_region(img)
    if colored:
        x0, y0, x1, y1, cx, cy = colored
        print(f"    colored region: ({x0},{y0})-({x1},{y1}) center=({cx},{cy})")
    else:
        print(f"    colored region: not found")

    return img

def diff_images(img_a, img_b):
    """Count pixels that differ between two images."""
    if img_a.size != img_b.size:
        print(f"  sizes differ: {img_a.size} vs {img_b.size}")
        return
    w, h = img_a.size
    diff_count = 0
    max_diff = 0
    # Sample every pixel
    for y in range(0, h, 2):
        for x in range(0, w, 2):
            pa = img_a.getpixel((x, y))[:3]
            pb = img_b.getpixel((x, y))[:3]
            d = abs(pa[0]-pb[0]) + abs(pa[1]-pb[1]) + abs(pa[2]-pb[2])
            if d > 10:  # threshold for noise
                diff_count += 1
                max_diff = max(max_diff, d)
    total = (w // 2) * (h // 2)
    print(f"  pixel diff: {diff_count}/{total} sampled pixels differ (max channel diff={max_diff})")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: imgdiff.py <image_a> [image_b]")
        sys.exit(1)

    img_a = measure(sys.argv[1])
    if len(sys.argv) >= 3:
        print()
        img_b = measure(sys.argv[2])
        print()
        diff_images(img_a, img_b)
