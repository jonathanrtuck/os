// --------------------------------------------------------------------------
// Color palette — centralized UI color definitions for Document OS.
//
// Every color used by the compositor and chrome is defined here as a named
// constant. This makes the palette reviewable as a cohesive set and makes
// it trivial to tune the overall theme.
//
// Design: pure monochrome dark theme — neutral greys only (R=G=B for every
// color). No blue, warm, or cool tint. Background is near-black, chrome
// overlays are translucent grey panels, text is light grey/white.
// --------------------------------------------------------------------------

/// Deep near-black background (R=G=B=16).
/// Used as the radial gradient edge color for the full-screen background (z=0).
pub const BG_BASE: Color = Color::rgb(16, 16, 16);
/// Background gradient center — brighter than BG_BASE by ~12 RGB units.
/// The background surface uses a radial gradient from BG_CENTER (center) to
/// BG_BASE (edges) with per-pixel noise to break up banding.
pub const BG_CENTER: Color = Color::rgb(28, 28, 28);
/// Content area background — slightly lighter than BG_BASE (R=G=B=20)
/// to give the editor region a subtle distinction under the chrome.
pub const BG_CONTENT: Color = Color::rgb(20, 20, 20);
/// Translucent chrome panel background (title bar).
/// Alpha 170/255 ≈ 67% opaque — content visibly peeks through.
pub const CHROME_BG: Color = Color::rgba(48, 48, 48, 170);
/// Chrome separator line (bottom of title bar).
/// Subtle divider that reinforces the boundary without being harsh.
pub const CHROME_BORDER: Color = Color::rgba(80, 80, 80, 220);
/// Primary text in the editor — high contrast on dark background (R=G=B=220).
pub const TEXT_PRIMARY: Color = Color::rgb(220, 220, 220);
/// Cursor color — pure bright white, immediately locatable.
pub const TEXT_CURSOR: Color = Color::rgb(255, 255, 255);
/// Selection highlight — semi-transparent grey overlay behind
/// selected text. Alpha 140 keeps the text readable.
pub const TEXT_SELECTION: Color = Color::rgba(100, 100, 100, 140);
/// Chrome title text — bright grey for readability (R=G=B=210).
pub const CHROME_TITLE: Color = Color::rgb(210, 210, 210);
/// Chrome subtitle text — dim secondary info (R=G=B=100).
pub const CHROME_SUBTITLE: Color = Color::rgb(100, 100, 100);
/// Chrome status text — medium contrast (R=G=B=140).
pub const CHROME_STATUS: Color = Color::rgb(140, 140, 140);
/// Chrome clock text — slightly brighter than status text (R=G=B=200)
/// to draw the eye toward the time display.
pub const CHROME_CLOCK: Color = Color::rgb(200, 200, 200);
/// SVG icon tint in the title bar — neutral light grey (R=G=B=190).
pub const CHROME_ICON: Color = Color::rgb(190, 190, 190);
/// Drop shadow peak opacity — used for the gradient shadows between
/// chrome and content. Pure black with alpha 160 for visible darkening.
pub const SHADOW_PEAK: Color = Color::rgba(0, 0, 0, 160);
/// Drop shadow transparent end — fully transparent black.
pub const SHADOW_ZERO: Color = Color::rgba(0, 0, 0, 0);
/// Mouse cursor fill — bright white, opaque.
pub const CURSOR_FILL: Color = Color::rgb(255, 255, 255);
/// Mouse cursor outline — dark grey, opaque.
pub const CURSOR_OUTLINE: Color = Color::rgb(40, 40, 40);
