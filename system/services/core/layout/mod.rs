//! Layout algorithm and scene building.
//!
//! Provides monospace line-breaking, cursor/selection positioning, glyph
//! shaping, scroll filtering, and the scene graph building functions that
//! assemble the text editor UI. All scene mutation flows through free
//! functions that accept a `SceneWriter` — `SceneState` delegates here.

mod full;
mod incremental;
mod loading;

use alloc::vec::Vec;

use icon_lib as icons;

pub(crate) use full::rich_xy_to_byte;
// Re-export all public items from submodules.
pub use full::{
    build_clock_update, build_cursor_update, build_document_content, build_full_scene,
    build_rich_document_content, build_selection_update, RichFonts, CURSOR_HOTSPOT_OFFSET,
};
pub use incremental::{delete_line, insert_line, update_single_line};
pub use loading::{build_loading_scene, update_spinner_angle};
// Style table is used by scene_state (via main.rs re-export path).
// Font identity constants removed. Style IDs are assigned at runtime
// by core's StyleTable — sequential, collision-free by construction.
use scene::{Color, Content, DataRef, NodeFlags, ShapedGlyph, NULL};

// ── Float math helpers (no_std) ─────────────────────────────────────

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Delegates to the canonical implementation in `drawing`.
#[inline]
pub(crate) fn round_f32(x: f32) -> i32 {
    drawing::round_f32(x)
}

// ── Well-known node indices ─────────────────────────────────────────

/// Well-known node indices for direct mutation.
pub const N_ROOT: u16 = 0;
pub const N_TITLE_BAR: u16 = 1;
pub const N_TITLE_TEXT: u16 = 2;
pub const N_CLOCK_TEXT: u16 = 3;
pub const N_SHADOW: u16 = 4;
pub const N_CONTENT: u16 = 5;
pub const N_DOC_TEXT: u16 = 6;
pub const N_CURSOR: u16 = 7;

// ── Pointer cursor node (8) ──────────────────────────────────────────
//
// Top-level node rendered above all content. Position updated each
// frame from MSG_POINTER_ABS. Auto-hides after 3 s of inactivity with
// a 300 ms EaseOut fade.
pub const N_POINTER: u16 = 8;

// ── Title bar icon (9) ──────────────────────────────────────────────
//
// Document type icon in the title bar, baseline-aligned with the title
// text. Content::Path with stroke_width > 0 for outline Tabler icons.
pub const N_TITLE_ICON: u16 = 9;

// ── Document strip (10..12) ─────────────────────────────────────────
//
// Horizontal strip of document spaces. N_STRIP is a child of N_CONTENT
// with width = N × viewport. content_transform.tx slides the viewport.
// Each document occupies one viewport-width "space" in the strip.
pub const N_STRIP: u16 = 10;

// White page surface for the text document (space 0). A4 proportions,
// centered horizontally. N_DOC_TEXT is a child of this node.
pub const N_PAGE: u16 = 11;

// Image content in space 1. Centered in the second viewport-width
// region of the strip. The image IS its own surface (no page bg).
pub const N_DOC_IMAGE: u16 = 12;

/// Number of well-known nodes (indices 0..12). Dynamic nodes start at 13.
pub const WELL_KNOWN_COUNT: u16 = 13;

// ── Configuration ───────────────────────────────────────────────────

/// Shared configuration for scene building functions. Avoids passing
/// 25+ parameters individually to build_editor_scene, update_document_content,
/// update_selection, and update_clock.
pub struct SceneConfig<'a> {
    pub fb_width: u32,
    pub fb_height: u32,
    pub title_bar_h: u32,
    pub shadow_depth: u32,
    /// Text inset within the page surface (padding from page edge).
    pub text_inset_x: u32,
    pub chrome_bg: drawing::Color,
    pub chrome_border: drawing::Color,
    pub chrome_title_color: drawing::Color,
    pub chrome_clock_color: drawing::Color,
    pub bg_color: drawing::Color,
    pub text_color: drawing::Color,
    pub cursor_color: drawing::Color,
    pub sel_color: drawing::Color,
    /// Page surface background color (white paper).
    pub page_bg: drawing::Color,
    /// Page width in points (A4 proportions, derived from viewport).
    pub page_width: u32,
    /// Page height in points.
    pub page_height: u32,
    pub font_size: u16,
    /// Character advance in 16.16 fixed-point points.
    /// Single source of truth for character width — same precision as
    /// scene graph ShapedGlyph advances.
    pub char_width_fx: i32,
    pub line_height: u32,
    pub font_data: &'a [u8],
    pub upem: u16,
    pub axes: &'a [fonts::rasterize::AxisValue],
    /// Content Region content_id for the mono font.
    pub mono_content_id: u32,
    /// Mono font typographic ascent (font units, positive).
    pub mono_ascender: i16,
    /// Mono font typographic descent (font units, negative).
    pub mono_descender: i16,
    /// Mono font line gap (font units).
    pub mono_line_gap: i16,
    /// Sans font data (Inter) for chrome text (title, clock).
    /// Falls back to font_data (mono) when empty.
    pub sans_font_data: &'a [u8],
    pub sans_upem: u16,
    /// Content Region content_id for the sans font.
    pub sans_content_id: u32,
    /// Sans font typographic ascent (font units, positive).
    pub sans_ascender: i16,
    /// Sans font typographic descent (font units, negative).
    pub sans_descender: i16,
    /// Sans font line gap (font units).
    pub sans_line_gap: i16,
}

// ── Icon rendering ─────────────────────────────────────────────────

/// Scale pre-compiled icon path data from viewbox space to point space
/// and push to the scene writer's data buffer. Returns the DataRef for
/// use in `Content::Path`.
///
/// Concatenates all sub-paths from the icon into a single contour blob.
/// Color is applied uniformly (monochrome). For layered rendering with
/// per-layer opacity, use separate nodes per layer group.
fn scale_icon_paths(
    w: &mut scene::SceneWriter<'_>,
    icon: &icons::Icon,
    size_pt: u32,
) -> (DataRef, u32) {
    let scale = size_pt as f32 / icon.viewbox;
    let mut buf = Vec::new();

    for icon_path in icon.paths {
        let cmds = icon_path.commands;
        let mut pos = 0;
        while pos + 4 <= cmds.len() {
            let tag = u32::from_le_bytes([cmds[pos], cmds[pos + 1], cmds[pos + 2], cmds[pos + 3]]);
            match tag {
                0 => {
                    // MoveTo: tag(4) + x(4) + y(4) = 12
                    if pos + 12 > cmds.len() { break; }
                    let x = f32::from_le_bytes([cmds[pos+4], cmds[pos+5], cmds[pos+6], cmds[pos+7]]) * scale;
                    let y = f32::from_le_bytes([cmds[pos+8], cmds[pos+9], cmds[pos+10], cmds[pos+11]]) * scale;
                    buf.extend_from_slice(&0u32.to_le_bytes());
                    buf.extend_from_slice(&x.to_le_bytes());
                    buf.extend_from_slice(&y.to_le_bytes());
                    pos += 12;
                }
                1 => {
                    // LineTo: tag(4) + x(4) + y(4) = 12
                    if pos + 12 > cmds.len() { break; }
                    let x = f32::from_le_bytes([cmds[pos+4], cmds[pos+5], cmds[pos+6], cmds[pos+7]]) * scale;
                    let y = f32::from_le_bytes([cmds[pos+8], cmds[pos+9], cmds[pos+10], cmds[pos+11]]) * scale;
                    buf.extend_from_slice(&1u32.to_le_bytes());
                    buf.extend_from_slice(&x.to_le_bytes());
                    buf.extend_from_slice(&y.to_le_bytes());
                    pos += 12;
                }
                2 => {
                    // CubicTo: tag(4) + c1x(4) + c1y(4) + c2x(4) + c2y(4) + x(4) + y(4) = 28
                    if pos + 28 > cmds.len() { break; }
                    let mut coords = [0f32; 6];
                    for ci in 0..6 {
                        let off = pos + 4 + ci * 4;
                        coords[ci] = f32::from_le_bytes([cmds[off], cmds[off+1], cmds[off+2], cmds[off+3]]) * scale;
                    }
                    buf.extend_from_slice(&2u32.to_le_bytes());
                    for c in &coords {
                        buf.extend_from_slice(&c.to_le_bytes());
                    }
                    pos += 28;
                }
                3 => {
                    // Close: tag(4) = 4
                    buf.extend_from_slice(&3u32.to_le_bytes());
                    pos += 4;
                }
                _ => break,
            }
        }
    }

    let hash = scene::fnv1a(&buf);
    let data_ref = w.push_data(&buf);
    (data_ref, hash)
}

/// Set up a scene node as an icon using Content::Path.
///
/// This is the single rendering helper that maps icon layers to theme
/// colors. Currently monochrome (all layers get the same color).
/// Layered rendering (Primary/Secondary opacity) is a future enhancement.
pub(crate) fn emit_icon(
    w: &mut scene::SceneWriter<'_>,
    node_id: u16,
    icon: &icons::Icon,
    x: i32,
    y: i32,
    size_pt: u32,
    color: Color,
) {
    let (data_ref, hash) = scale_icon_paths(w, icon, size_pt);

    // Stroke width: scale from viewbox units to points, encode as 8.8 fixed-point.
    let sw_pt = icon.stroke_width * (size_pt as f32 / icon.viewbox);
    let sw_fixed = (sw_pt * 256.0) as u16;

    let n = w.node_mut(node_id);
    n.x = scene::pt(x);
    n.y = scene::pt(y as i32);
    n.width = scene::upt(size_pt);
    n.height = scene::upt(size_pt);
    n.content = Content::Path {
        color,
        fill_rule: scene::FillRule::Winding,
        stroke_width: sw_fixed,
        contours: data_ref,
    };
    n.content_hash = hash;
    n.flags = NodeFlags::VISIBLE;
}

// ── Layout types ────────────────────────────────────────────────────

/// Local layout run type — used for line-breaking before writing to
/// the scene graph. Each LayoutRun describes one visual text line.
pub struct LayoutRun {
    /// Placeholder DataRef: offset = byte position in source text,
    /// length = byte count. Replaced with actual data buffer ref
    /// before writing to the scene graph.
    pub glyphs: DataRef,
    /// Number of glyphs (= bytes for monospace ASCII).
    pub glyph_count: u16,
    /// Starting point position relative to the parent node.
    pub y: i32,
    /// Text color.
    pub color: Color,
    /// Font size in points.
    pub font_size: u16,
}

// ── Text layout (delegates to layout library) ─────────────────────

/// Monospace font metrics for the layout library.
///
/// Every character has the same advance width. The `char_width` is set
/// to 1.0 so that `max_width = chars_per_line` — the library wraps at
/// the same character boundaries as the old hand-written code.
struct UnitMetrics {
    line_height: f32,
}

impl layout_lib::FontMetrics for UnitMetrics {
    fn char_width(&self, _ch: char) -> f32 {
        1.0
    }
    fn line_height(&self) -> f32 {
        self.line_height
    }
}

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
///
/// Delegates to the layout library. The single source of truth for
/// line-breaking logic — used by both scene building (cursor/selection
/// positioning) and scroll calculation.
pub fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
    let metrics = UnitMetrics { line_height: 1.0 };
    let max_width = chars_per_line as f32;
    layout_lib::byte_to_line_col(
        text,
        byte_offset,
        &metrics,
        max_width,
        &layout_lib::CharBreaker,
    )
}

/// Break text into visual lines using the unified layout library.
///
/// Delegates to `layout_lib::layout_paragraph` with `CharBreaker` (character-
/// level wrapping) and unit-width metrics, then wraps each `LayoutLine`
/// into a `LayoutRun` with color and font_size for scene graph construction.
pub fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i32,
    color: Color,
    font_size: u16,
) -> Vec<LayoutRun> {
    let metrics = UnitMetrics {
        line_height: line_height as f32,
    };
    let max_width = chars_per_line as f32;
    let para = layout_lib::layout_paragraph(
        text,
        &metrics,
        max_width,
        layout_lib::Alignment::Left,
        &layout_lib::CharBreaker,
    );

    para.lines
        .iter()
        .map(|line| LayoutRun {
            glyphs: DataRef {
                offset: line.byte_offset,
                length: line.byte_length,
            },
            glyph_count: line.byte_length as u16,
            y: line.y,
            color,
            font_size,
        })
        .collect()
}

/// Extract source text bytes for a run using its placeholder DataRef.
pub fn line_bytes_for_run<'a>(text: &'a [u8], run: &LayoutRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter runs to those visible in a scrolled viewport.
///
/// `scroll_y` is the scroll offset in pixels (f32). Runs keep their
/// document-relative y positions. The caller sets `content_transform`
/// on the container node so the renderer handles the viewport offset.
pub fn scroll_runs(
    runs: Vec<LayoutRun>,
    scroll_y: scene::Mpt,
    line_height: u32,
    viewport_height_pt: i32,
) -> Vec<LayoutRun> {
    let scroll_pt = scroll_y >> 10;

    runs.into_iter()
        .filter(|run| {
            let doc_y = run.y;

            // Above the scroll window?
            if doc_y + line_height as i32 <= scroll_pt {
                return false;
            }
            // Below the scroll window?
            if doc_y >= scroll_pt + viewport_height_pt {
                return false;
            }

            true
        })
        .collect()
}

/// Shape text through HarfBuzz and convert from font units to points.
///
/// Calls `fonts::shape_with_variations()` to produce real glyph IDs and
/// metrics, then converts font-unit values to scene-graph points using
/// `value_pt = value_fu * point_size / upem`.
pub fn shape_text(
    font_data: &[u8],
    text: &[u8],
    point_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<ShapedGlyph> {
    // Use lossy conversion so invalid UTF-8 bytes render as replacement
    // characters instead of causing the entire line to disappear (review 6.7).
    let s = alloc::string::String::from_utf8_lossy(text);
    if s.is_empty() || font_data.is_empty() || upem == 0 {
        return Vec::new();
    }
    let shaped = fonts::shape_with_variations(font_data, &s, &[], axes);
    let ps = point_size as i64;
    let u = upem as i64;
    // Convert font units to 16.16 fixed-point points:
    //   value_16_16 = (value_fu * point_size * 65536) / upem
    // Using i64 to avoid overflow for large font-unit values.
    shaped
        .iter()
        .map(|g| ShapedGlyph {
            glyph_id: g.glyph_id,
            _pad: 0,
            x_advance: ((g.x_advance as i64 * ps * 65536) / u) as i32,
            x_offset: ((g.x_offset as i64 * ps * 65536) / u) as i32,
            y_offset: ((g.y_offset as i64 * ps * 65536) / u) as i32,
        })
        .collect()
}

/// Shape text using the chrome font (Inter/sans). Returns shaped glyphs
/// ready for `push_shaped_glyphs()`. All chrome text (title bar, clock)
/// uses this — single source of truth for font selection.
pub(crate) fn shape_chrome_text(cfg: &SceneConfig, text: &[u8]) -> Vec<ShapedGlyph> {
    shape_text(
        cfg.sans_font_data,
        text,
        cfg.font_size,
        cfg.sans_upem,
        cfg.axes,
    )
}

// ── Rich text (multi-style) layout ──────────────────────────────────

/// A styled segment within a visual line. Produced by splitting
/// `LineBreak` ranges back into per-run segments.
pub struct RichSegment {
    /// Style from the piece table palette.
    pub style_id: u8,
    /// Byte range in scratch text buffer.
    pub text_start: usize,
    pub text_len: usize,
    /// Y position in document coordinates (points).
    pub y: i32,
}

/// A visual line of rich text, containing one or more styled segments.
pub struct RichLine {
    pub segments: Vec<RichSegment>,
    pub y: i32,
    /// Computed line height in points (max of all segment heights).
    pub line_height: i32,
}

/// Font data pointer + metrics for a given style, resolved from the
/// Content Region. Core resolves these once per font family.
pub struct FontInfo<'a> {
    pub data: &'a [u8],
    pub upem: u16,
    /// Content Region content_id for the font data (TTF bytes).
    pub content_id: u32,
    /// Typographic ascent in font units (positive, above baseline).
    pub ascender: i16,
    /// Typographic descent in font units (negative, below baseline).
    pub descender: i16,
    /// Typographic line gap in font units.
    pub line_gap: i16,
    /// OS/2 sCapHeight (height of capital H above baseline, font units). 0 if unavailable.
    pub cap_height: i16,
}

// ── Style table (sequential ID assignment) ──────────────────────────

/// A registered (content_id, axes) combination with font metrics.
struct StyleEntry {
    content_id: u32,
    axes: Vec<fonts::rasterize::AxisValue>,
    ascent_fu: u16,
    descent_fu: u16,
    upem: u16,
}

/// Sequential style ID assignment. Collision-free by construction.
///
/// Maps unique (content_id, axes) combinations to sequential u32 IDs.
/// Created fresh each scene build, then serialized into the scene data
/// buffer as a style registry for the renderer.
pub struct StyleTable {
    entries: Vec<StyleEntry>,
}

impl StyleTable {
    /// Create an empty style table.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Get or assign a style_id for the given font + axes combination.
    ///
    /// Linear scan for dedup (typically < 10 entries), assigns the next
    /// sequential ID on miss.
    pub fn style_id_for(
        &mut self,
        content_id: u32,
        axes: &[fonts::rasterize::AxisValue],
        ascent_fu: u16,
        descent_fu: u16,
        upem: u16,
    ) -> u32 {
        for (i, e) in self.entries.iter().enumerate() {
            if e.content_id == content_id && axes_eq(&e.axes, axes) {
                return i as u32;
            }
        }
        let id = self.entries.len() as u32;
        self.entries.push(StyleEntry {
            content_id,
            axes: axes.to_vec(),
            ascent_fu,
            descent_fu,
            upem,
        });
        id
    }

    /// Number of registered styles.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Convert to registry entries for scene data buffer serialization.
    pub fn to_registry_entries(&self) -> Vec<protocol::content::StyleRegistryEntry> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let mut axes = [protocol::content::StyleAxisValue {
                    tag: [0; 4],
                    value: 0.0,
                }; protocol::content::MAX_STYLE_AXES];
                let axis_count = e.axes.len().min(protocol::content::MAX_STYLE_AXES);
                for (j, av) in e.axes.iter().take(axis_count).enumerate() {
                    axes[j] = protocol::content::StyleAxisValue {
                        tag: av.tag,
                        value: av.value,
                    };
                }
                protocol::content::StyleRegistryEntry {
                    style_id: i as u32,
                    content_id: e.content_id,
                    ascent_fu: e.ascent_fu,
                    descent_fu: e.descent_fu,
                    upem: e.upem,
                    axis_count: axis_count as u8,
                    _pad: 0,
                    axes,
                }
            })
            .collect()
    }
}

/// Compare two axis slices for equality (same tags and values).
fn axes_eq(a: &[fonts::rasterize::AxisValue], b: &[fonts::rasterize::AxisValue]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for (av, bv) in a.iter().zip(b.iter()) {
        if av.tag != bv.tag || av.value != bv.value {
            return false;
        }
    }
    true
}

/// Write the style registry into the scene data buffer as the first item.
///
/// Returns the number of bytes written. The registry lives at offset 0
/// of the data buffer; the renderer reads from this fixed offset.
pub(crate) fn write_style_registry(
    w: &mut scene::SceneWriter<'_>,
    style_table: &StyleTable,
) -> usize {
    let registry_entries = style_table.to_registry_entries();
    // 8192 bytes is enough for ~100 entries (80 bytes each + 8 byte header).
    let mut registry_buf = [0u8; 8192];
    let registry_size =
        protocol::content::write_style_registry(&mut registry_buf, &registry_entries);
    if registry_size > 0 {
        let _registry_ref = w.push_data(&registry_buf[..registry_size]);
    }
    registry_size
}

/// Create a StyleTable with the two base styles (mono=0, sans=1)
/// registered from the SceneConfig. Returns `(style_table, mono_style_id, sans_style_id)`.
///
/// The order is deterministic: mono is always 0, sans is always 1.
/// This matches what the incremental path assumes.
pub(crate) fn base_style_table(cfg: &SceneConfig) -> (StyleTable, u32, u32) {
    let mut st = StyleTable::new();
    let mono_id = st.style_id_for(
        cfg.mono_content_id,
        cfg.axes,
        cfg.mono_ascender as u16,
        (-cfg.mono_descender) as u16,
        cfg.upem,
    );
    let sans_id = st.style_id_for(
        cfg.sans_content_id,
        &[],
        cfg.sans_ascender as u16,
        (-cfg.sans_descender) as u16,
        cfg.sans_upem,
    );
    (st, mono_id, sans_id)
}

/// Measure character advance width in points using font metrics.
///
/// When `axes` is non-empty, uses HVAR-adjusted advances for correct
/// variable-font widths (e.g. bold Inter is wider than regular).
fn char_advance_pt(
    font_data: &[u8],
    ch: char,
    font_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> f32 {
    if upem == 0 || font_data.is_empty() {
        return 8.0; // fallback
    }
    let gid = fonts::rasterize::glyph_id_for_char(font_data, ch).unwrap_or(0);
    let advance_fu = if !axes.is_empty() {
        fonts::rasterize::glyph_h_advance_with_axes(font_data, gid, axes).unwrap_or(0) as f32
    } else {
        let (adv, _) = fonts::rasterize::glyph_h_metrics(font_data, gid).unwrap_or((0, 0));
        adv as f32
    };
    (advance_fu * font_size as f32) / upem as f32
}

/// Build MeasuredChar stream from piece table styled runs, then break
/// into lines using the layout library's `break_measured_lines`.
///
/// Returns a list of `RichLine`, each containing styled segments ready
/// for shaping and scene graph construction.
///
/// `scratch` is a caller-provided buffer for extracting logical text.
/// `resolve_font` maps a piecetable::Style to font data + metrics.
pub fn layout_rich_lines(
    pt_buf: &[u8],
    scratch: &mut [u8],
    line_width_pt: f32,
    line_height: i32,
    mono_font: &FontInfo<'_>,
    sans_font: &FontInfo<'_>,
    serif_font: &FontInfo<'_>,
) -> Vec<RichLine> {
    let run_count = piecetable::styled_run_count(pt_buf);
    if run_count == 0 {
        return Vec::new();
    }

    // Copy all logical text into scratch buffer.
    let text_len = piecetable::text_len(pt_buf);
    let copied = piecetable::text_slice(pt_buf, 0, text_len, scratch);
    let text = &scratch[..copied];

    // Build MeasuredChar stream.
    let mut measured: Vec<layout_lib::MeasuredChar> = Vec::new();

    for run_idx in 0..run_count {
        let Some(run) = piecetable::styled_run(pt_buf, run_idx) else {
            continue;
        };
        let Some(style) = piecetable::style(pt_buf, run.style_id) else {
            continue;
        };
        let fi = match style.font_family {
            piecetable::FONT_MONO => mono_font,
            piecetable::FONT_SERIF => serif_font,
            _ => sans_font,
        };

        // Build variation axes for this run's style.
        let mut axes_buf = [fonts::rasterize::AxisValue {
            tag: [0; 4],
            value: 0.0,
        }; 3];
        let mut axis_count = 0;
        if style.weight != 400 {
            axes_buf[axis_count] = fonts::rasterize::AxisValue {
                tag: *b"wght",
                value: style.weight as f32,
            };
            axis_count += 1;
        }
        // Italic uses a separate font file — no ital axis needed.
        // Optical size for fonts that support it (Inter, Source Serif 4).
        axes_buf[axis_count] = fonts::rasterize::AxisValue {
            tag: *b"opsz",
            value: style.font_size_pt as f32,
        };
        axis_count += 1;
        let axes = &axes_buf[..axis_count];

        // Walk the bytes of this run's text, decoding UTF-8.
        let run_start = run.byte_offset as usize;
        let run_end = run_start + run.byte_len as usize;
        let run_text = if run_end <= text.len() {
            &text[run_start..run_end]
        } else {
            continue;
        };

        let mut offset = run_start;
        for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
            let byte_len = ch.len_utf8() as u8;
            let width = if ch == '\n' {
                0.0
            } else {
                char_advance_pt(fi.data, ch, style.font_size_pt as u16, fi.upem, axes)
            };
            measured.push(layout_lib::MeasuredChar {
                byte_offset: offset as u32,
                byte_len,
                width,
                run_index: run_idx as u16,
                is_whitespace: ch == ' ' || ch == '\t',
                is_newline: ch == '\n',
            });
            offset += byte_len as usize;
        }
    }

    // Break into lines.
    let line_breaks =
        layout_lib::break_measured_lines(&measured, line_width_pt, layout_lib::BreakMode::Word);

    // Split each line back into per-style segments and compute per-line height.
    let mut result: Vec<RichLine> = Vec::with_capacity(line_breaks.len());
    let mut y = 0i32;

    // Helper: compute line height (points) from font metrics and font size.
    let style_line_height = |style: &piecetable::Style, fi: &FontInfo<'_>| -> i32 {
        if fi.upem == 0 {
            return line_height;
        }
        let asc = (fi.ascender as i32).abs();
        let desc = (fi.descender as i32).abs();
        let gap = (fi.line_gap as i32).max(0);
        let h = ((asc + desc + gap) as f32 * style.font_size_pt as f32) / fi.upem as f32;
        // Round up to avoid clipping.
        (h + 0.5) as i32
    };

    // Helper: compute ascent in points for a style.
    let style_ascent_pt = |style: &piecetable::Style, fi: &FontInfo<'_>| -> f32 {
        if fi.upem == 0 {
            return 0.0;
        }
        let asc = (fi.ascender as i32).abs();
        (asc as f32 * style.font_size_pt as f32) / fi.upem as f32
    };

    for lb in &line_breaks {
        let mut segments: Vec<RichSegment> = Vec::new();
        let mut max_line_h = line_height; // fallback to global line_height
        let mut max_ascent_pt: f32 = 0.0;

        // Find MeasuredChars in this line's byte range.
        for mc in &measured {
            if mc.byte_offset < lb.byte_start {
                continue;
            }
            if mc.byte_offset >= lb.byte_end {
                break;
            }
            if mc.is_newline {
                continue;
            }

            let run_idx = mc.run_index as usize;
            let Some(run) = piecetable::styled_run(pt_buf, run_idx) else {
                continue;
            };

            // Coalesce into current segment if same style.
            if let Some(last) = segments.last_mut() {
                if last.style_id == run.style_id
                    && last.text_start + last.text_len == mc.byte_offset as usize
                {
                    last.text_len += mc.byte_len as usize;
                    continue;
                }
            }

            // Compute this segment's line height and ascent contribution.
            if let Some(style) = piecetable::style(pt_buf, run.style_id) {
                let fi = match style.font_family {
                    piecetable::FONT_MONO => mono_font,
                    piecetable::FONT_SERIF => serif_font,
                    _ => sans_font,
                };
                let h = style_line_height(style, fi);
                if h > max_line_h {
                    max_line_h = h;
                }
                let asc = style_ascent_pt(style, fi);
                if asc > max_ascent_pt {
                    max_ascent_pt = asc;
                }
            }

            // y will be corrected in the baseline alignment pass below.
            segments.push(RichSegment {
                style_id: run.style_id,
                text_start: mc.byte_offset as usize,
                text_len: mc.byte_len as usize,
                y,
            });
        }

        // Baseline alignment: offset each segment so all baselines match.
        // Each segment's baseline = seg.y + seg_ascent. We want all baselines
        // at line_y + max_ascent, so seg.y = line_y + (max_ascent - seg_ascent).
        for seg in &mut segments {
            if let Some(style) = piecetable::style(pt_buf, seg.style_id) {
                let fi = match style.font_family {
                    piecetable::FONT_MONO => mono_font,
                    piecetable::FONT_SERIF => serif_font,
                    _ => sans_font,
                };
                let seg_asc = style_ascent_pt(style, fi);
                seg.y = y + (max_ascent_pt - seg_asc) as i32;
            }
        }

        result.push(RichLine {
            segments,
            y,
            line_height: max_line_h,
        });
        y += max_line_h;
    }

    result
}

/// Shape a rich text segment and return scene-graph glyphs.
/// Uses the style to determine font, size, and axes.
pub fn shape_rich_segment(
    font_data: &[u8],
    text: &[u8],
    font_size: u16,
    upem: u16,
    weight: u16,
    italic: bool,
) -> Vec<scene::ShapedGlyph> {
    // Build axis values for variable font.
    let mut axes_buf = [fonts::rasterize::AxisValue {
        tag: *b"wght",
        value: 0.0,
    }; 3];
    let mut axis_count = 0;
    if weight != 400 {
        axes_buf[axis_count] = fonts::rasterize::AxisValue {
            tag: *b"wght",
            value: weight as f32,
        };
        axis_count += 1;
    }
    // Italic uses a separate font file — no ital axis needed.
    // Optical size for fonts that support it.
    axes_buf[axis_count] = fonts::rasterize::AxisValue {
        tag: *b"opsz",
        value: font_size as f32,
    };
    axis_count += 1;
    let axes = &axes_buf[..axis_count];

    shape_text(font_data, text, font_size, upem, axes)
}

/// Count lines in a text buffer (newlines + 1).
pub fn count_lines(text: &[u8]) -> usize {
    let mut count: usize = 1;
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'\n' {
            count += 1;
        }
        i += 1;
    }
    count
}

// ── Rich text navigation ───────────────────────────────────────────
//
// These functions use the cached `RichLine` layout to provide cursor
// navigation that matches the proportional rendering exactly. Each
// function mirrors a monospace counterpart (byte_to_line_col, etc.)
// but operates on variable-width styled lines instead of fixed columns.

/// Find which `RichLine` contains `cursor_pos`. Returns the line index.
/// Returns `rich_lines.len()` (one past last valid index) if cursor is
/// past all segments — callers must handle this as "trailing empty line."
/// Returns 0 if `rich_lines` is empty.
pub(crate) fn rich_byte_to_line(rich_lines: &[RichLine], cursor_pos: usize) -> usize {
    if rich_lines.is_empty() {
        return 0;
    }
    for (i, line) in rich_lines.iter().enumerate() {
        if line.segments.is_empty() {
            continue;
        }
        let line_start = line.segments[0].text_start;
        let last_seg = line.segments.last().unwrap();
        let line_end = last_seg.text_start + last_seg.text_len;
        // Cursor on this line: start <= cursor_pos <= end.
        if cursor_pos >= line_start && cursor_pos <= line_end {
            return i;
        }
    }
    // Past all segments — trailing empty line (conceptual).
    rich_lines.len()
}

/// Compute cursor x position (points) on its line via shaped glyph advances.
/// Uses the same shaping as the renderer (`shape_rich_segment`) for exact alignment.
pub(crate) fn rich_cursor_x(
    rich_lines: &[RichLine],
    pt_buf: &[u8],
    text: &[u8],
    cursor_pos: usize,
    fonts: &full::RichFonts<'_>,
) -> f32 {
    if rich_lines.is_empty() {
        return 0.0;
    }
    let line_idx = rich_byte_to_line(rich_lines, cursor_pos);
    // Past-end sentinel: cursor is on the trailing empty line at x=0.
    if line_idx >= rich_lines.len() {
        return 0.0;
    }
    let line = &rich_lines[line_idx];
    let mut x: f32 = 0.0;
    for seg in &line.segments {
        let seg_end = seg.text_start + seg.text_len;
        if cursor_pos < seg.text_start {
            break;
        }
        let Some(style) = piecetable::style(pt_buf, seg.style_id) else {
            continue;
        };
        let fi = fonts.resolve(style);
        let font_size = style.font_size_pt as u16;
        let italic = style.flags & piecetable::FLAG_ITALIC != 0;
        let seg_text = &text[seg.text_start..seg_end.min(text.len())];
        let shaped =
            shape_rich_segment(fi.data, seg_text, font_size, fi.upem, style.weight, italic);

        // Count characters to measure up to cursor_pos within this segment.
        let measure_end = cursor_pos.min(seg_end) - seg.text_start;
        let measure_text = &seg_text[..measure_end.min(seg_text.len())];
        let char_count = core::str::from_utf8(measure_text)
            .unwrap_or("")
            .chars()
            .count();

        for g in shaped.iter().take(char_count) {
            x += g.x_advance as f32 / 65536.0;
        }
        if cursor_pos <= seg_end {
            break;
        }
        // Past this segment — add remaining glyph advances.
        for g in shaped.iter().skip(char_count) {
            x += g.x_advance as f32 / 65536.0;
        }
    }
    x
}

/// Given a line index and target x (points), find the closest byte offset.
/// Uses the same shaping as the renderer for exact alignment.
pub(crate) fn rich_x_to_byte(
    rich_lines: &[RichLine],
    pt_buf: &[u8],
    text: &[u8],
    line_idx: usize,
    target_x: f32,
    fonts: &full::RichFonts<'_>,
) -> usize {
    if rich_lines.is_empty() {
        return 0;
    }
    let line_idx = line_idx.min(rich_lines.len() - 1);
    let line = &rich_lines[line_idx];
    if line.segments.is_empty() {
        // Empty line — return line start from surrounding context.
        return if line_idx > 0 {
            rich_line_end(rich_lines, line_idx - 1)
        } else {
            0
        };
    }

    let mut x: f32 = 0.0;
    let mut best_pos = line.segments[0].text_start;

    for seg in &line.segments {
        let seg_end = seg.text_start + seg.text_len;
        let Some(style) = piecetable::style(pt_buf, seg.style_id) else {
            continue;
        };
        let fi = fonts.resolve(style);
        let font_size = style.font_size_pt as u16;
        let italic = style.flags & piecetable::FLAG_ITALIC != 0;
        let seg_text = &text[seg.text_start..seg_end.min(text.len())];
        let shaped =
            shape_rich_segment(fi.data, seg_text, font_size, fi.upem, style.weight, italic);

        // Walk characters and glyphs together to find the closest position.
        let mut char_byte_offset = seg.text_start;
        for (gi, ch) in core::str::from_utf8(seg_text)
            .unwrap_or("")
            .chars()
            .enumerate()
        {
            let advance = if gi < shaped.len() {
                shaped[gi].x_advance as f32 / 65536.0
            } else {
                0.0
            };
            // If midpoint of this glyph is past target, we found the position.
            if x + advance * 0.5 >= target_x {
                return char_byte_offset;
            }
            x += advance;
            char_byte_offset += ch.len_utf8();
        }
        best_pos = seg_end;
    }
    best_pos
}

/// Byte offset of the start of a rich line.
pub(crate) fn rich_line_start(rich_lines: &[RichLine], line_idx: usize) -> usize {
    if rich_lines.is_empty() {
        return 0;
    }
    let idx = line_idx.min(rich_lines.len() - 1);
    let line = &rich_lines[idx];
    if let Some(first) = line.segments.first() {
        first.text_start
    } else if idx > 0 {
        // Empty line — position is just past the end of the previous line.
        rich_line_end(rich_lines, idx - 1)
    } else {
        0
    }
}

/// Byte offset of the end of a rich line.
pub(crate) fn rich_line_end(rich_lines: &[RichLine], line_idx: usize) -> usize {
    if rich_lines.is_empty() {
        return 0;
    }
    let idx = line_idx.min(rich_lines.len() - 1);
    let line = &rich_lines[idx];
    if let Some(last) = line.segments.last() {
        last.text_start + last.text_len
    } else if idx > 0 {
        rich_line_end(rich_lines, idx - 1)
    } else {
        0
    }
}

/// Count how many rich lines fit in a viewport of `viewport_height_pt` points.
/// Rich lines have variable height, so this scans from a starting line
/// and sums `line_height` until the viewport is exceeded.
pub(crate) fn rich_viewport_lines(rich_lines: &[RichLine], viewport_height_pt: i32) -> usize {
    if rich_lines.is_empty() {
        return 0;
    }
    // Use the average line height across all lines for a consistent page count.
    let total_h: i32 = rich_lines.iter().map(|l| l.line_height).sum();
    let avg_h = total_h / rich_lines.len() as i32;
    if avg_h <= 0 {
        return 1;
    }
    (viewport_height_pt / avg_h).max(1) as usize
}

/// Convert a `drawing::Color` to the scene graph `Color` type.
pub(crate) fn dc(c: drawing::Color) -> Color {
    Color::rgba(c.r, c.g, c.b, c.a)
}

/// Compute `chars_per_line` from config using page width and text inset.
pub(crate) fn chars_per_line(cfg: &SceneConfig) -> u32 {
    let dw = doc_width(cfg);
    if cfg.char_width_fx > 0 {
        ((dw as i64 * 65536) / cfg.char_width_fx as i64).max(1) as u32
    } else {
        80
    }
}

/// Compute the text-area width within the page surface.
pub(crate) fn doc_width(cfg: &SceneConfig) -> u32 {
    cfg.page_width.saturating_sub(2 * cfg.text_inset_x)
}

/// Allocate per-line Glyphs child nodes under N_DOC_TEXT, linking them
/// as a sibling chain. Returns the last allocated line node ID (or NULL
/// if none were allocated).
///
/// This is the shared line-node construction code used by both
/// `build_full_scene` and `build_document_content`.
pub(crate) fn allocate_line_nodes(
    w: &mut scene::SceneWriter<'_>,
    line_glyph_refs: &[(DataRef, u16, i32)],
    doc_width: u32,
    line_height: u32,
    scene_text_color: Color,
    font_size: u16,
    style_id: u32,
) -> u16 {
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_line_node: u16 = NULL;

    for &(glyph_ref, glyph_count, y) in line_glyph_refs {
        if let Some(line_id) = w.alloc_node() {
            let n = w.node_mut(line_id);
            n.y = scene::pt(y);
            n.width = scene::upt(doc_width);
            n.height = scene::upt(line_height);
            n.content = Content::Glyphs {
                color: scene_text_color,
                glyphs: glyph_ref,
                glyph_count,
                font_size,
                style_id,
            };
            n.content_hash = scene::fnv1a(&glyph_ref.offset.to_le_bytes());
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;

            if prev_line_node == NULL {
                w.node_mut(N_DOC_TEXT).first_child = line_id;
            } else {
                w.node_mut(prev_line_node).next_sibling = line_id;
            }
            prev_line_node = line_id;
        }
    }

    prev_line_node
}

/// Shape visible runs and collect glyph data refs for line-node
/// construction. Used by both full and incremental scene builds.
pub(crate) fn shape_visible_runs(
    w: &mut scene::SceneWriter<'_>,
    visible_runs: &[LayoutRun],
    doc_text: &[u8],
    font_data: &[u8],
    font_size: u16,
    upem: u16,
    axes: &[fonts::rasterize::AxisValue],
) -> Vec<(DataRef, u16, i32)> {
    let mut line_glyph_refs: Vec<(DataRef, u16, i32)> = Vec::with_capacity(visible_runs.len());

    for run in visible_runs {
        let line_text = line_bytes_for_run(doc_text, run);
        let shaped = shape_text(font_data, line_text, font_size, upem, axes);
        let glyph_ref = w.push_shaped_glyphs(&shaped);

        line_glyph_refs.push((glyph_ref, shaped.len() as u16, run.y));
    }

    line_glyph_refs
}

/// Update the clock text via re-push within an already-open back buffer.
/// Real shaping may produce different glyph counts, so we re-push data
/// and update the Content::Glyphs reference rather than overwriting in place.
pub(crate) fn update_clock_inline(
    w: &mut scene::SceneWriter<'_>,
    clock_text: &[u8],
    cfg: &SceneConfig,
) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs {
        color, style_id, ..
    } = clock_node.content
    {
        let new_glyphs = shape_chrome_text(cfg, clock_text);
        let new_ref = w.push_shaped_glyphs(&new_glyphs);
        let new_count = new_glyphs.len() as u16;

        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            style_id,
        };
        n.content_hash = scene::fnv1a(clock_text);
        w.mark_dirty(N_CLOCK_TEXT);
    }
}

/// Allocate selection rectangle nodes as children of N_DOC_TEXT (after
/// the cursor node). Each line of the selection gets one rect node.
///
/// Selection rects use document-relative y positions. The renderer
/// applies `content_transform` from the parent container to offset them
/// visually. `scroll_pt` and `content_h` are used only for visibility
/// culling.
#[allow(clippy::too_many_arguments)]
pub(crate) fn allocate_selection_rects(
    w: &mut scene::SceneWriter<'_>,
    doc_text: &[u8],
    sel_lo: usize,
    sel_hi: usize,
    chars_per_line: usize,
    char_width_fx: i32,
    line_height: u32,
    sel_color: Color,
    content_h: u32,
    scroll_pt: i32,
) {
    let (sel_start_line, sel_start_col) = byte_to_line_col(doc_text, sel_lo, chars_per_line);
    let (sel_end_line, sel_end_col) = byte_to_line_col(doc_text, sel_hi, chars_per_line);
    let mut prev_sel_node: u16 = NULL;

    for line in sel_start_line..=sel_end_line {
        let col_start = if line == sel_start_line {
            sel_start_col
        } else {
            0
        };
        let col_end = if line == sel_end_line {
            sel_end_col
        } else {
            chars_per_line
        };

        if col_start >= col_end {
            continue;
        }

        // Document-relative y for this selection line.
        let sel_y = line as i32 * line_height as i32;

        // Visibility culling: skip lines outside the scroll window.
        if sel_y + line_height as i32 <= scroll_pt || sel_y >= scroll_pt + content_h as i32 {
            continue;
        }

        if let Some(sel_id) = w.alloc_node() {
            let n = w.node_mut(sel_id);
            n.x = ((col_start as i64 * char_width_fx as i64) >> 6) as scene::Mpt;
            n.y = scene::pt(sel_y);
            n.width = (((col_end - col_start) as u64 * char_width_fx as u64) >> 6) as scene::Umpt;
            n.height = scene::upt(line_height);
            n.background = sel_color;
            n.content = Content::None;
            n.flags = NodeFlags::VISIBLE;
            n.next_sibling = NULL;

            if prev_sel_node == NULL {
                w.node_mut(N_CURSOR).next_sibling = sel_id;
            } else {
                w.node_mut(prev_sel_node).next_sibling = sel_id;
            }

            w.mark_dirty(sel_id);
            prev_sel_node = sel_id;
        }
    }
}
