# Click placement — verify cursor was placed mid-word and typing inserted there.
# At 800x600, clicked at (330,68) on "Style Stress Test" title line.
# Typed 'Z' which should appear within the title text region.
# The title region spans roughly x=230..400, y=55..100 at 800x600.
frame_not_blank
content_in_region x0=230 y0=50 x1=400 y1=105
