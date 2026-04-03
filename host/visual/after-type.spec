# After typing text — verify document content changed.
# Typed 'x' at the default cursor position.
# The text content area should have content (stress test text + new char).
frame_not_blank
page_centered tol=5
content_in_region x0=1200 y0=100 x1=2900 y1=1100
