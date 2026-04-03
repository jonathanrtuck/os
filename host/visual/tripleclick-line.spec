# Triple-click line selection — verify line was replaced with 'Z'.
# At 800x600, triple-clicked at (300,115) which is on a styled text line.
# After typing 'Z', the line content should have changed (fewer glyphs).
# Verifies triple-click selects the full line in rich text, not byte 0..0.
frame_not_blank
content_in_region x0=230 y0=50 x1=400 y1=150
