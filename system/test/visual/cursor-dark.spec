# Cursor visible on dark background (outside page).
# Mouse moved to (400, 400) which is left of the page.
# Expect pointer (arrow) shape since we're outside the document.
frame_not_blank
page_centered tol=5
cursor_visible_at x=400 y=400 size=32 tol=15
cursor_shape_is x=400 y=400 shape=pointer
