# Underline position — decoration below text baseline, not through it.
# At 1600x1200 (positions shift ~8px between boots):
#   "Serif 22pt Bold Italic Underline Blue" text+underline: y≈230-270
#   "Serif 32pt Bold Underline Orange" text+underline: y≈290-340
# Gap between text body bottom and underline top must be 0-6px.
frame_not_blank
underline_gap x0=425 y0=230 x1=975 y1=270 min_gap=0 max_gap=6
underline_gap x0=425 y0=290 x1=900 y1=340 min_gap=0 max_gap=6
