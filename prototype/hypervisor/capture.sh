#!/bin/bash
# Capture the Hypervisor window content to a PNG.
# Usage: ./capture.sh [output.png]
#
# Window is always centered at 1024x800 (including title bar).
# At 2x Retina: pixel coords (1032, 326) size (2048, 1600).

OUT="${1:-/tmp/hypervisor-capture.png}"
TMP=$(mktemp /tmp/hyp-XXXXXX.png)

screencapture -x "$TMP" 2>/dev/null

python3 -c "
from PIL import Image
img = Image.open('$TMP')
# Window at (516,163) in points = (1032,326) in pixels, size 2048x1600
cropped = img.crop((1032, 326, 1032+2048, 326+1600))
cropped.save('$OUT')
print(f'Captured {cropped.size[0]}x{cropped.size[1]}')
"

rm -f "$TMP"
