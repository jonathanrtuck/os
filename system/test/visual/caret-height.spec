# Text caret height — verify cursor is positioned correctly with proper height.
# At 1600x1200, clicked at (600,120) on subtitle line.
# Caret should be a thin vertical line near x=600, height 12-65px
# (ascender + descender of the font at cursor position, NOT full line height).
# Subtitle is ~14pt text, so caret can be as short as ~12px.
# Uses diff against caret-baseline.png to isolate the caret.
frame_not_blank
caret_between ref=/tmp/visual-tests/caret-baseline.png x=580 y=110 tol=60 min_h=12 max_h=65
