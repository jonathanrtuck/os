# Italic rendering — verify italic text matches known-good reference.
# At 1600x1200, "Sans 40pt Italic Magenta" is in region y≈305-390.
# SSIM against a reference crop catches major rendering failures.
# Threshold 0.35 tolerates subpixel rendering differences from Y-position
# shifts (~8px between boots). A roman fallback or missing glyphs would
# produce SSIM < 0.2 (completely different glyph shapes).
frame_not_blank
italic_slant ref=visual/baselines/italic-line.png x0=425 y0=305 x1=900 y1=390 threshold=0.35
