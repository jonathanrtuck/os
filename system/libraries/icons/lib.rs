//! Icon library — named vector icons with mimetype refinement.
//!
//! Provides a single lookup function: `get(name, mimetype) -> &Icon`.
//! Icons are pre-compiled from Tabler Icons (MIT) SVGs at build time
//! into native path commands. The library is pure data — no rendering,
//! no color, no theme knowledge.
//!
//! # Naming
//!
//! Icon names are semantic OS concepts (`"document"`, `"search"`,
//! `"alert"`), not source icon set names. The mapping from OS names
//! to source SVGs is in the build-time icon manifest.
//!
//! # Mimetype refinement
//!
//! Any icon can have mimetype-specific variants. `document` uses this
//! heavily (text vs image vs audio variants). System UI icons like
//! `search` or `plus` can gain variants later without API changes.

#![no_std]

mod data;

// ── Types ──────────────────────────────────────────────────────────

/// Semantic rendering layer for icon sub-paths.
///
/// Assigned by the designer during curation. The caller maps layers
/// to theme colors via a rendering helper — never per-path logic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Layer {
    /// Main shape: outline, primary geometry. Full visual weight.
    Primary = 0,
    /// Supporting detail: interior marks, decorations. Reduced opacity.
    Secondary = 1,
}

/// One contour within an icon, tagged with its rendering layer.
#[derive(Clone, Copy)]
pub struct IconPath {
    /// Pre-compiled native path commands (MoveTo/LineTo/CubicTo/Close).
    /// Binary format: tag(u32 LE) + coords(f32 LE each).
    /// MoveTo = 12 bytes, LineTo = 12, CubicTo = 28, Close = 4.
    pub commands: &'static [u8],
    /// Which semantic layer this contour belongs to.
    pub layer: Layer,
}

/// Path command tag for Close (must match `scene::primitives::PATH_CLOSE`).
const PATH_CLOSE_TAG: u32 = 3;
/// Size of a Close command in bytes.
const PATH_CLOSE_SIZE: usize = 4;

impl IconPath {
    /// Whether this contour ends with a Close command.
    ///
    /// Open contours (no Close) should be stroked only — filling them
    /// implicitly closes with a straight line from end to start, which
    /// creates visual artifacts (wedge shapes from arcs, etc.).
    pub const fn is_closed(&self) -> bool {
        let len = self.commands.len();
        if len < PATH_CLOSE_SIZE {
            return false;
        }
        // Last 4 bytes should be the Close tag (u32 LE).
        let b = self.commands;
        let i = len - PATH_CLOSE_SIZE;
        let tag = b[i] as u32
            | (b[i + 1] as u32) << 8
            | (b[i + 2] as u32) << 16
            | (b[i + 3] as u32) << 24;
        tag == PATH_CLOSE_TAG
    }
}

/// A named icon: vector path data with rendering hints and a11y label.
///
/// All fields are `'static` — icons live in `.rodata`, no heap allocation.
#[derive(Clone, Copy)]
pub struct Icon {
    /// Lookup identifier (e.g., `"document"`, `"play"`, `"alert"`).
    pub name: &'static str,
    /// Accessibility label (e.g., `"Text document"`, `"Play media"`).
    pub label: &'static str,
    /// Sub-paths with layer annotations.
    pub paths: &'static [IconPath],
    /// Viewbox size (width = height). 24.0 for Tabler-sourced icons.
    pub viewbox: f32,
    /// Default stroke width in viewbox units.
    /// 0.0 = filled geometry. 2.0 = Tabler outline default.
    pub stroke_width: f32,
}

impl Icon {
    /// Whether all contours in this icon end with a Close command.
    ///
    /// Icons with all-closed paths can be rendered with fill+stroke
    /// (solid body + outline). Icons with any open path should be
    /// rendered stroke-only — filling open paths creates visual
    /// artifacts from implicit closure.
    pub fn all_paths_closed(&self) -> bool {
        let mut i = 0;
        while i < self.paths.len() {
            if !self.paths[i].is_closed() {
                return false;
            }
            i += 1;
        }
        !self.paths.is_empty()
    }
}

// ── Lookup ─────────────────────────────────────────────────────────

/// Look up an icon by name with optional mimetype refinement.
///
/// Always returns an icon — never fails. Fallback chain:
///   1. Exact name + exact mimetype (if provided)
///   2. Exact name + mimetype category, e.g. `image/*` (if provided)
///   3. Base icon for name (no mimetype)
///   4. Universal fallback (base `document` icon)
///
/// The mimetype parameter applies to all icons, not just `document`.
/// Icons without registered variants simply ignore it (steps 1-2 skip).
pub fn get(name: &str, mimetype: Option<&str>) -> &'static Icon {
    // Try exact name + exact mimetype.
    if let Some(mt) = mimetype {
        if let Some(icon) = data::lookup(name, Some(mt)) {
            return icon;
        }
        // Try category fallback: "text/plain" → "text/".
        if let Some(slash) = mt.find('/') {
            let category = &mt[..=slash]; // e.g., "text/"
            if let Some(icon) = data::lookup_category(name, category) {
                return icon;
            }
        }
    }
    // Try base icon (no mimetype).
    if let Some(icon) = data::lookup(name, None) {
        return icon;
    }
    // Universal fallback.
    data::fallback()
}

/// Extract the mimetype category prefix (e.g., `"text/"` from `"text/plain"`).
/// Returns `None` if the mimetype has no slash.
pub fn mimetype_category(mimetype: &str) -> Option<&str> {
    mimetype.find('/').map(|i| &mimetype[..=i])
}
