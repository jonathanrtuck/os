# Baseline alignment — mixed-size text on same line shares baseline.
# At 1600x1200, text shifts ~8px between boots. Padded ranges:
#   Line with 36pt + smaller text: y=130-180
#   Line with 28pt + smaller text: y=335-380
# Mode-based baseline (excludes descenders) must match within tolerance.
frame_not_blank
baseline_aligned x0=425 y0=130 x1=1061 y1=180 tol=2
baseline_aligned x0=425 y0=335 x1=979 y1=380 tol=2
