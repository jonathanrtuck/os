// --------------------------------------------------------------------------
// Color palette — centralized UI color definitions for Document OS.
//
// Every color used by the compositor and chrome is defined here as a named
// constant. This makes the palette reviewable as a cohesive set and makes
// it trivial to tune the overall theme.
//
// Design: blank slate — pure black background, pure white text. No grey,
// no gradients, no chrome styling. Starting point for visual polish.
// --------------------------------------------------------------------------

/// Pure black background.
pub const BG_BASE: Color = Color::rgb(0, 0, 0);
/// Background gradient center (unused — kept for API compatibility).
pub const BG_CENTER: Color = Color::rgb(0, 0, 0);
/// Content area background (same as BG_BASE in blank slate).
pub const BG_CONTENT: Color = Color::rgb(0, 0, 0);
/// Title bar background — fully transparent (blank slate).
pub const CHROME_BG: Color = Color::TRANSPARENT;
/// Chrome separator line — fully transparent (blank slate).
pub const CHROME_BORDER: Color = Color::TRANSPARENT;
/// Primary text in the editor — pure white.
pub const TEXT_PRIMARY: Color = Color::rgb(255, 255, 255);
/// Cursor color — pure white.
pub const TEXT_CURSOR: Color = Color::rgb(255, 255, 255);
/// Selection highlight — semi-transparent white overlay.
pub const TEXT_SELECTION: Color = Color::rgba(255, 255, 255, 60);
/// Chrome title text — pure white.
pub const CHROME_TITLE: Color = Color::rgb(255, 255, 255);
/// Chrome subtitle text — pure white.
pub const CHROME_SUBTITLE: Color = Color::rgb(255, 255, 255);
/// Chrome status text — pure white.
pub const CHROME_STATUS: Color = Color::rgb(255, 255, 255);
/// Chrome clock text — pure white.
pub const CHROME_CLOCK: Color = Color::rgb(255, 255, 255);
/// Drop shadow peak opacity — disabled (blank slate).
pub const SHADOW_PEAK: Color = Color::TRANSPARENT;
/// Drop shadow transparent end — fully transparent.
pub const SHADOW_ZERO: Color = Color::TRANSPARENT;
/// Mouse cursor fill — pure white.
pub const CURSOR_FILL: Color = Color::rgb(255, 255, 255);
/// Mouse cursor outline — pure black.
pub const CURSOR_OUTLINE: Color = Color::rgb(0, 0, 0);
