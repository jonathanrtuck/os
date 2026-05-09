#!/usr/bin/env python3
"""Visual verification tool for hypervisor screenshots.

Usage:
    verify.py IMAGE --assert ASSERTION [--tolerance N]

Assertions:
    solid_color R,G,B       Every pixel matches RGB (0-255), within tolerance
    uniform                 All pixels are the same color (reports the color)
    not_black               Image is not all black (0,0,0)
    dimensions WxH          Image dimensions match exactly
    pixel_at X,Y,R,G,B     Pixel at (X,Y) matches RGB within tolerance

Exit code 0 = PASS, 1 = FAIL, 2 = error.
"""

import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    print("FAIL: Pillow not installed", file=sys.stderr)
    sys.exit(2)


def load_image(path: str) -> Image.Image:
    img = Image.open(path).convert("RGBA")
    return img


def assert_solid_color(img: Image.Image, args: str, tolerance: int) -> bool:
    parts = args.split(",")
    if len(parts) != 3:
        print(f"FAIL: solid_color expects R,G,B, got '{args}'")
        return False

    target = tuple(int(x.strip()) for x in parts)
    pixels = img.getdata()
    mismatches = 0
    first_bad = None

    for i, px in enumerate(pixels):
        r, g, b = px[0], px[1], px[2]
        if (abs(r - target[0]) > tolerance or
                abs(g - target[1]) > tolerance or
                abs(b - target[2]) > tolerance):
            mismatches += 1
            if first_bad is None:
                x = i % img.width
                y = i // img.width
                first_bad = (x, y, r, g, b)

    total = img.width * img.height
    if mismatches == 0:
        print(f"PASS: all {total} pixels match ({target[0]},{target[1]},{target[2]}) "
              f"+/-{tolerance}")
        return True
    else:
        pct = mismatches / total * 100
        print(f"FAIL: {mismatches}/{total} pixels ({pct:.1f}%) don't match "
              f"({target[0]},{target[1]},{target[2]}) +/-{tolerance}")
        if first_bad:
            print(f"  first mismatch at ({first_bad[0]},{first_bad[1]}): "
                  f"got ({first_bad[2]},{first_bad[3]},{first_bad[4]})")
        return False


def assert_uniform(img: Image.Image, args: str, tolerance: int) -> bool:
    pixels = img.getdata()
    first = pixels[0]
    r0, g0, b0 = first[0], first[1], first[2]
    mismatches = 0

    for i, px in enumerate(pixels):
        if (abs(px[0] - r0) > tolerance or
                abs(px[1] - g0) > tolerance or
                abs(px[2] - b0) > tolerance):
            mismatches += 1

    if mismatches == 0:
        print(f"PASS: uniform color ({r0},{g0},{b0}) across {img.width}x{img.height}")
        return True
    else:
        total = img.width * img.height
        pct = mismatches / total * 100
        print(f"FAIL: {mismatches}/{total} pixels ({pct:.1f}%) differ from "
              f"({r0},{g0},{b0})")
        return False


def assert_not_black(img: Image.Image, args: str, tolerance: int) -> bool:
    pixels = img.getdata()
    for px in pixels:
        if px[0] > tolerance or px[1] > tolerance or px[2] > tolerance:
            print(f"PASS: image is not all black "
                  f"(found non-black pixel, e.g. ({px[0]},{px[1]},{px[2]}))")
            return True

    print("FAIL: image is all black")
    return False


def assert_dimensions(img: Image.Image, args: str, tolerance: int) -> bool:
    parts = args.lower().split("x")
    if len(parts) != 2:
        print(f"FAIL: dimensions expects WxH, got '{args}'")
        return False

    expected_w, expected_h = int(parts[0]), int(parts[1])
    if img.width == expected_w and img.height == expected_h:
        print(f"PASS: dimensions {img.width}x{img.height}")
        return True
    else:
        print(f"FAIL: expected {expected_w}x{expected_h}, "
              f"got {img.width}x{img.height}")
        return False


def assert_pixel_at(img: Image.Image, args: str, tolerance: int) -> bool:
    parts = args.split(",")
    if len(parts) != 5:
        print(f"FAIL: pixel_at expects X,Y,R,G,B, got '{args}'")
        return False

    x, y = int(parts[0]), int(parts[1])
    er, eg, eb = int(parts[2]), int(parts[3]), int(parts[4])

    if x >= img.width or y >= img.height:
        print(f"FAIL: ({x},{y}) out of bounds ({img.width}x{img.height})")
        return False

    px = img.getpixel((x, y))
    r, g, b = px[0], px[1], px[2]

    if (abs(r - er) <= tolerance and
            abs(g - eg) <= tolerance and
            abs(b - eb) <= tolerance):
        print(f"PASS: pixel ({x},{y}) = ({r},{g},{b}) matches "
              f"({er},{eg},{eb}) +/-{tolerance}")
        return True
    else:
        print(f"FAIL: pixel ({x},{y}) = ({r},{g},{b}), "
              f"expected ({er},{eg},{eb}) +/-{tolerance}")
        return False


def assert_region_variance(img: Image.Image, args: str, tolerance: int) -> bool:
    """Check that a rectangular region has many distinct pixel values.

    Proves anti-aliased glyph rendering (many shades) vs solid-color
    rectangles (1-2 colors). Args: X,Y,W,H,MIN_COLORS
    """
    parts = args.split(",")
    if len(parts) != 5:
        print(f"FAIL: region_variance expects X,Y,W,H,MIN_COLORS, got '{args}'")
        return False

    x, y, w, h, min_colors = (int(p) for p in parts)
    colors = set()

    for py in range(y, min(y + h, img.height)):
        for px in range(x, min(x + w, img.width)):
            r, g, b = img.getpixel((px, py))[:3]
            colors.add((r, g, b))

    if len(colors) >= min_colors:
        print(f"PASS: region ({x},{y},{w},{h}) has {len(colors)} distinct colors "
              f"(>= {min_colors})")
        return True
    else:
        print(f"FAIL: region ({x},{y},{w},{h}) has only {len(colors)} distinct colors "
              f"(expected >= {min_colors})")
        for c in sorted(colors)[:10]:
            print(f"  {c}")
        return False


def assert_color_in_region(img: Image.Image, args: str, tolerance: int) -> bool:
    """Check that a specific color appears at least once in a region.

    Args: X,Y,W,H,R,G,B — region bounds and target color.
    """
    parts = args.split(",")
    if len(parts) != 7:
        print(f"FAIL: color_in_region expects X,Y,W,H,R,G,B, got '{args}'")
        return False

    x, y, w, h, er, eg, eb = (int(p) for p in parts)
    count = 0

    for py in range(y, min(y + h, img.height)):
        for px in range(x, min(x + w, img.width)):
            r, g, b = img.getpixel((px, py))[:3]
            if (abs(r - er) <= tolerance and
                    abs(g - eg) <= tolerance and
                    abs(b - eb) <= tolerance):
                count += 1

    if count > 0:
        print(f"PASS: found {count} pixels matching ({er},{eg},{eb}) "
              f"+/-{tolerance} in region ({x},{y},{w},{h})")
        return True
    else:
        print(f"FAIL: no pixels matching ({er},{eg},{eb}) +/-{tolerance} "
              f"in region ({x},{y},{w},{h})")
        return False


ASSERTIONS = {
    "solid_color": assert_solid_color,
    "uniform": assert_uniform,
    "not_black": assert_not_black,
    "dimensions": assert_dimensions,
    "pixel_at": assert_pixel_at,
    "region_variance": assert_region_variance,
    "color_in_region": assert_color_in_region,
}


def main():
    if len(sys.argv) < 4 or sys.argv[2] != "--assert":
        print(__doc__)
        sys.exit(2)

    image_path = sys.argv[1]
    assertion_parts = sys.argv[3:]

    tolerance = 2
    assertion_name = assertion_parts[0]
    assertion_args = ""

    i = 1
    while i < len(assertion_parts):
        if assertion_parts[i] == "--tolerance" and i + 1 < len(assertion_parts):
            tolerance = int(assertion_parts[i + 1])
            i += 2
        else:
            assertion_args = assertion_parts[i]
            i += 1

    if not Path(image_path).exists():
        print(f"FAIL: image not found: {image_path}")
        sys.exit(2)

    if assertion_name not in ASSERTIONS:
        print(f"FAIL: unknown assertion '{assertion_name}'. "
              f"Available: {', '.join(ASSERTIONS.keys())}")
        sys.exit(2)

    img = load_image(image_path)
    result = ASSERTIONS[assertion_name](img, assertion_args, tolerance)
    sys.exit(0 if result else 1)


if __name__ == "__main__":
    main()
