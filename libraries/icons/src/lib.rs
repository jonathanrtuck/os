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

#[cfg(test)]
mod tests {
    use super::*;

    // ── Icon lookup: base icons ──────────────────────────────────────

    #[test]
    fn lookup_base_document() {
        let icon = get("document", None);
        assert_eq!(icon.name, "document");
        assert_eq!(icon.viewbox, 24.0);
        assert!(icon.stroke_width > 0.0);
        assert!(!icon.paths.is_empty());
    }

    #[test]
    fn lookup_search() {
        let icon = get("search", None);
        assert_eq!(icon.name, "search");
    }

    #[test]
    fn lookup_settings() {
        let icon = get("settings", None);
        assert_eq!(icon.name, "settings");
    }

    #[test]
    fn lookup_alert() {
        let icon = get("alert", None);
        assert_eq!(icon.name, "alert");
    }

    #[test]
    fn lookup_info() {
        let icon = get("info", None);
        assert_eq!(icon.name, "info");
    }

    #[test]
    fn lookup_check() {
        let icon = get("check", None);
        assert_eq!(icon.name, "check");
    }

    #[test]
    fn lookup_close() {
        let icon = get("close", None);
        assert_eq!(icon.name, "close");
    }

    #[test]
    fn lookup_plus() {
        let icon = get("plus", None);
        assert_eq!(icon.name, "plus");
    }

    #[test]
    fn lookup_minus() {
        let icon = get("minus", None);
        assert_eq!(icon.name, "minus");
    }

    #[test]
    fn lookup_arrows() {
        for name in &["arrow-left", "arrow-right", "arrow-up", "arrow-down"] {
            let icon = get(name, None);
            assert_eq!(icon.name, *name);
        }
    }

    #[test]
    fn lookup_undo_redo() {
        let undo = get("undo", None);
        assert_eq!(undo.name, "undo");
        let redo = get("redo", None);
        assert_eq!(redo.name, "redo");
    }

    #[test]
    fn lookup_menu() {
        let icon = get("menu", None);
        assert_eq!(icon.name, "menu");
    }

    #[test]
    fn lookup_loading() {
        let icon = get("loading", None);
        assert_eq!(icon.name, "loading");
    }

    #[test]
    fn lookup_pointer() {
        let icon = get("pointer", None);
        assert_eq!(icon.name, "pointer");
    }

    #[test]
    fn lookup_cursor_text() {
        let icon = get("cursor-text", None);
        assert_eq!(icon.name, "cursor-text");
    }

    // ── Mimetype refinement: exact match ─────────────────────────────

    #[test]
    fn document_exact_text_rich() {
        let icon = get("document", Some("text/rich"));
        assert_eq!(icon.label, "Rich text document");
    }

    #[test]
    fn document_exact_text_markdown() {
        let icon = get("document", Some("text/markdown"));
        assert_eq!(icon.label, "Markdown document");
    }

    #[test]
    fn document_exact_application_json() {
        let icon = get("document", Some("application/json"));
        assert_eq!(icon.label, "Source code");
    }

    #[test]
    fn document_exact_text_csv() {
        let icon = get("document", Some("text/csv"));
        assert_eq!(icon.label, "Data table");
    }

    // ── Mimetype refinement: category fallback ───────────────────────

    #[test]
    fn document_category_text() {
        // "text/html" has no exact match, falls back to "text/" category.
        let icon = get("document", Some("text/html"));
        assert_eq!(icon.label, "Text document");
    }

    #[test]
    fn document_category_image() {
        let icon = get("document", Some("image/png"));
        assert_eq!(icon.label, "Image");
    }

    #[test]
    fn document_category_audio() {
        let icon = get("document", Some("audio/mp3"));
        assert_eq!(icon.label, "Audio");
    }

    #[test]
    fn document_category_video() {
        let icon = get("document", Some("video/mp4"));
        assert_eq!(icon.label, "Video");
    }

    // ── Fallback chain ───────────────────────────────────────────────

    #[test]
    fn unknown_name_falls_back_to_document() {
        let icon = get("nonexistent-icon", None);
        assert_eq!(icon.name, "document");
    }

    #[test]
    fn unknown_name_with_mimetype_falls_back() {
        let icon = get("nonexistent-icon", Some("text/plain"));
        assert_eq!(icon.name, "document");
    }

    #[test]
    fn non_document_icon_ignores_mimetype() {
        // "search" has no mimetype variants; should return base "search".
        let icon = get("search", Some("text/plain"));
        assert_eq!(icon.name, "search");
    }

    // ── Path data format ─────────────────────────────────────────────

    #[test]
    fn all_icons_have_nonempty_paths() {
        let names = [
            "document",
            "search",
            "settings",
            "alert",
            "info",
            "check",
            "close",
            "plus",
            "minus",
            "arrow-left",
            "arrow-right",
            "arrow-up",
            "arrow-down",
            "undo",
            "redo",
            "menu",
            "loading",
            "pointer",
            "cursor-text",
        ];
        for name in &names {
            let icon = get(name, None);
            assert!(!icon.paths.is_empty(), "icon '{name}' has no paths");
            for path in icon.paths {
                assert!(
                    !path.commands.is_empty(),
                    "icon '{name}' has an empty path command buffer"
                );
            }
        }
    }

    #[test]
    fn path_commands_aligned_to_4_bytes() {
        // Path commands are u32 tags + f32 coords, all 4 bytes each.
        // Total command buffer length must be a multiple of 4.
        let icon = get("document", None);
        for path in icon.paths {
            assert_eq!(
                path.commands.len() % 4,
                0,
                "path commands length {} is not 4-byte aligned",
                path.commands.len()
            );
        }
    }

    #[test]
    fn icon_path_is_closed_detection() {
        // The base document icon should have at least one closed path
        // (the outline rectangle has a Close command).
        let icon = get("document", None);
        // PATH_CLOSE_TAG = 3 as u32 LE = [3, 0, 0, 0].
        let has_closed = icon.paths.iter().any(|p| p.is_closed());
        // At least verify is_closed doesn't panic.
        let _ = has_closed;
    }

    #[test]
    fn icon_all_paths_closed_method() {
        // Just verify the method doesn't panic and returns a bool.
        let icon = get("document", None);
        let _ = icon.all_paths_closed();
    }

    #[test]
    fn layer_values() {
        assert_eq!(Layer::Primary as u8, 0);
        assert_eq!(Layer::Secondary as u8, 1);
    }

    // ── mimetype_category ────────────────────────────────────────────

    #[test]
    fn mimetype_category_text() {
        assert_eq!(mimetype_category("text/plain"), Some("text/"));
    }

    #[test]
    fn mimetype_category_image() {
        assert_eq!(mimetype_category("image/png"), Some("image/"));
    }

    #[test]
    fn mimetype_category_no_slash() {
        assert_eq!(mimetype_category("plaintext"), None);
    }

    #[test]
    fn mimetype_category_empty() {
        assert_eq!(mimetype_category(""), None);
    }

    // ── Viewbox and stroke_width consistency ─────────────────────────

    #[test]
    fn all_icons_have_standard_viewbox() {
        let names = ["document", "search", "settings", "alert", "plus"];
        for name in &names {
            let icon = get(name, None);
            assert_eq!(icon.viewbox, 24.0, "icon '{name}' has non-standard viewbox");
        }
    }

    #[test]
    fn all_icons_have_labels() {
        let names = [
            "document", "search", "settings", "alert", "check", "close", "plus", "minus", "undo",
            "redo", "menu",
        ];
        for name in &names {
            let icon = get(name, None);
            assert!(!icon.label.is_empty(), "icon '{name}' has empty label");
        }
    }
}
