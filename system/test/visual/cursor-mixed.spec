# Click on mixed-size line — cursor placement works on lines with different font sizes.
# At 1600x1200, clicked on small text in right side of the 36pt Green line.
# Text shifts ~8px between boots; padded range covers both positions.
# Typed 'Z' should appear in the small text region (right side of line).
frame_not_blank
content_in_region x0=700 y0=125 x1=1100 y1=190
