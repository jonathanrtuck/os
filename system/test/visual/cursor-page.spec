# Cursor visible on white page (blank area below text).
# Mouse moved to (2000, 2000) — inside page but below all text content.
# Verifies cursor plane compositing works on white background.
# Shape assertion deferred — cursor-text shape rendering is unverified.
frame_not_blank
page_centered tol=5
cursor_visible_at x=2000 y=2000 size=32 tol=15
