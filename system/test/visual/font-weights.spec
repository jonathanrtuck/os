# Font weight variation — verify Thin/Light/Regular/Bold/Black have different densities.
# At 1600x1200, weight labels line spans y=320-336, x=470-1100.
# If weights aren't applied, all text renders at the same weight.
frame_not_blank
weight_varies x0=470 y0=320 x1=1100 y1=336 segments=5
