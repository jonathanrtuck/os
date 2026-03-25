// --------------------------------------------------------------------------
// Color palette — centralized UI color definitions for Document OS.
//
// Every color used by the compositor and chrome is defined here as a named
// constant. This makes the palette reviewable as a cohesive set and makes
// it trivial to tune the overall theme.
//
// Design: document surfaces on a dark desk. White page for text, image
// floats directly. Chrome is minimal — white text on transparent black.
// --------------------------------------------------------------------------

/// Dark desk (root background).
pub const BG_BASE: Color = Color::rgb(0x20, 0x20, 0x20);
/// Background gradient center (unused — kept for API compatibility).
pub const BG_CENTER: Color = Color::rgb(0x20, 0x20, 0x20);
/// Content area background (same as BG_BASE — the "desk" behind documents).
pub const BG_CONTENT: Color = Color::rgb(0x20, 0x20, 0x20);
/// Title bar background — fully transparent (blank slate).
pub const CHROME_BG: Color = Color::TRANSPARENT;
/// Chrome separator line — fully transparent (blank slate).
pub const CHROME_BORDER: Color = Color::TRANSPARENT;

// ── Page surface colors ─────────────────────────────────────────────
/// Document page background — white paper.
pub const PAGE_BG: Color = Color::rgb(255, 255, 255);
/// Primary text on the page — near-black.
pub const TEXT_PRIMARY: Color = Color::rgb(32, 32, 32);
/// Cursor on the page — near-black.
pub const TEXT_CURSOR: Color = Color::rgb(32, 32, 32);
/// Selection highlight on the page — macOS-style blue.
pub const TEXT_SELECTION: Color = Color::rgba(59, 130, 246, 60);

// ── Chrome colors (title bar, clock) ────────────────────────────────
/// Chrome title text — pure white on dark background.
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
