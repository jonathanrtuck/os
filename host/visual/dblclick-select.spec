# Double-click word selection — verify selection highlight appears.
# At 800x600, double-clicked at (280,60) on "Style" in the title line.
# Selection highlight is a blue-tinted rect (rgba 59,130,246,60) behind "Style".
# The title region spans roughly x=230..370, y=55..100 at 800x600.
frame_not_blank
content_in_region x0=230 y0=50 x1=370 y1=105
