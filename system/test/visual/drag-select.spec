# Drag selection — verify selection highlight appears after click-drag.
# At 800x600, drag from (250,60) to (400,60) across "Stress Test" in the title.
# Selection highlight = blue-tinted rect (rgba 59,130,246,60) behind selected text.
# The drag region spans roughly x=250..400, y=55..100 at 800x600.
frame_not_blank
content_in_region x0=250 y0=50 x1=400 y1=105
