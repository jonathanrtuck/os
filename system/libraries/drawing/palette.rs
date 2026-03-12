// --------------------------------------------------------------------------
// Color palette — centralized UI color definitions for Document OS.
//
// Every color used by the compositor and chrome is defined here as a named
// constant. This makes the palette reviewable as a cohesive set and makes
// it trivial to tune the overall theme.
//
// Design: dark theme with cool blue-grey tones. Background is near-black
// with a slight blue cast. Chrome overlays are translucent dark panels.
// Text uses light grey/blue for readability. Accent colors (cursor,
// selection) use saturated blue to stand out without clashing.
// --------------------------------------------------------------------------

/// Deep near-black background with a subtle blue cast.
/// Used as the full-screen background fill (z=0).
pub const BG_BASE: Color = Color::rgb(18, 18, 26);

/// Content area background — slightly lighter than BG_BASE to
/// give the editor region a subtle distinction under the chrome.
pub const BG_CONTENT: Color = Color::rgb(24, 24, 36);

/// Translucent chrome panel background (title bar, status bar).
/// Alpha 220/255 ≈ 86% opaque — lets content peek through.
pub const CHROME_BG: Color = Color::rgba(30, 30, 48, 220);

/// Chrome separator line (bottom of title bar, top of status bar).
/// Subtle divider that reinforces the boundary without being harsh.
pub const CHROME_BORDER: Color = Color::rgba(60, 60, 80, 200);

/// Primary text in the editor — high contrast on dark background.
pub const TEXT_PRIMARY: Color = Color::rgb(200, 210, 230);

/// Cursor color — bright accent blue, same as the cursor bar.
pub const TEXT_CURSOR: Color = Color::rgb(100, 180, 255);

/// Selection highlight — semi-transparent blue overlay behind
/// selected text. Alpha 180 keeps the text readable.
pub const TEXT_SELECTION: Color = Color::rgba(50, 80, 160, 180);

/// Chrome title text ("Document OS") — slightly muted white.
pub const CHROME_TITLE: Color = Color::rgb(200, 200, 220);

/// Chrome subtitle text (right-aligned descriptive text) — dim,
/// secondary information that doesn't compete with the title.
pub const CHROME_SUBTITLE: Color = Color::rgb(90, 90, 110);

/// Chrome status text (left side of status bar) — medium contrast.
pub const CHROME_STATUS: Color = Color::rgb(130, 130, 150);

/// Chrome clock text (right side of status bar) — slightly brighter
/// than status text to draw the eye toward the time display.
pub const CHROME_CLOCK: Color = Color::rgb(160, 170, 190);

/// SVG icon tint in the title bar — soft blue-grey that harmonizes
/// with the title text.
pub const CHROME_ICON: Color = Color::rgb(180, 190, 220);

/// Drop shadow peak opacity — used for the gradient shadows between
/// chrome and content. Pure black with controlled alpha.
pub const SHADOW_PEAK: Color = Color::rgba(0, 0, 0, 80);

/// Drop shadow transparent end — fully transparent black.
pub const SHADOW_ZERO: Color = Color::rgba(0, 0, 0, 0);
