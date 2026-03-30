# Cursor italic skew — caret tilts when placed next to italic text.
# At 1600x1200, clicked on "Sans 40pt Italic Magenta" line (y~325).
# Diffs against baseline (no cursor) to isolate caret.
# The skew factor from Inter Italic hhea is ~0.166 (run=339, rise=2048).
# For a ~40px caret: lean ≈ 0.166 * 40 ≈ 6.6px. Use min_lean=0.5 for robustness.
frame_not_blank
caret_skewed ref=/tmp/visual-tests/italic-baseline.png x=500 y=325 tol=40 min_lean=0.5
