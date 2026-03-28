//! Full scene graph builds.
//!
//! Contains `build_full_scene` (initial scene from scratch) and
//! `build_document_content` (compaction rebuild of document content
//! within an existing scene).

use alloc::vec::Vec;

use scene::{fnv1a, Border, Color, Content, FillRule, NodeFlags, NULL};

use super::{
    allocate_line_nodes, allocate_selection_rects, byte_to_line_col, chars_per_line, dc, doc_width,
    layout_mono_lines, layout_rich_lines, scroll_runs, shape_chrome_text, shape_rich_segment,
    shape_visible_runs, update_clock_inline, FontInfo, RichLine, SceneConfig, N_CLOCK_TEXT,
    N_CONTENT, N_CURSOR, N_DOC_IMAGE, N_DOC_TEXT, N_PAGE, N_POINTER, N_ROOT, N_SHADOW, N_STRIP,
    N_TITLE_BAR, N_TITLE_ICON, N_TITLE_TEXT, WELL_KNOWN_COUNT,
};
use crate::icons;

const DOCUMENT_SHADOW_BLUR_RADIUS: u8 = 64;
const DOCUMENT_SHADOW_COLOR: Color = Color::rgba(0, 0, 0, 255);
const DOCUMENT_SHADOW_OFFSET_X: i16 = 0;
const DOCUMENT_SHADOW_OFFSET_Y: i16 = 0;
const DOCUMENT_SHADOW_SPREAD: i8 = 36;

// ── Pointer cursor constants ─────────────────────────────────────────

/// Pointer cursor display size in points.
const CURSOR_SIZE_PT: u32 = 18;
/// Hotspot offset in points (arrow tip is inset by this amount from node origin).
pub const CURSOR_HOTSPOT_OFFSET: i32 = {
    // offset_viewbox * display_pt / viewbox_size, rounded
    // = 1.0 * 18 / 14 ≈ 1.3 → 1
    (CURSOR_SIZE_PT as f32 * icons::CURSOR_VIEWBOX.recip()) as i32
};

/// Push cursor image data and set up N_POINTER as Content::InlineImage.
fn setup_cursor(w: &mut scene::SceneWriter<'_>, mouse_x: u32, mouse_y: u32, pointer_opacity: u8) {
    let cursor_px = CURSOR_SIZE_PT * 2; // 2× for Retina
    let cursor_pixels = icons::rasterize_cursor(cursor_px);
    let cursor_ref = w.push_data(&cursor_pixels);
    let cursor_hash = fnv1a(&cursor_pixels);
    let n = w.node_mut(N_POINTER);
    n.x = scene::pt(mouse_x as i32 - CURSOR_HOTSPOT_OFFSET);
    n.y = scene::pt(mouse_y as i32 - CURSOR_HOTSPOT_OFFSET);
    n.width = scene::upt(CURSOR_SIZE_PT);
    n.height = scene::upt(CURSOR_SIZE_PT);
    n.content = Content::InlineImage {
        data: cursor_ref,
        src_width: cursor_px as u16,
        src_height: cursor_px as u16,
    };
    n.content_hash = cursor_hash;
    n.opacity = pointer_opacity;
    n.flags = NodeFlags::VISIBLE;
    n.next_sibling = NULL;
}

// ── Full scene builds (called by SceneState methods) ────────────────

/// Build the full scene graph into a fresh (cleared) SceneWriter.
///
/// Both document spaces are always present in the strip. The `slide_offset`
/// determines which space is visible (0.0 = text, fb_width = image).
/// The title bar always reflects `active_space`.
#[allow(clippy::too_many_arguments)]
pub fn build_full_scene(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: scene::Mpt,
    cursor_opacity: u8,
    mouse_x: u32,
    mouse_y: u32,
    pointer_opacity: u8,
    slide_offset: scene::Mpt,
    active_space: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Text layout (always computed — text document is always in the scene).
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h_u32 = cfg.fb_height.saturating_sub(content_y);
    let visible_runs = scroll_runs(all_runs, scroll_y, cfg.line_height, content_h_u32 as i32);
    let scroll_pt = scroll_y >> 10;
    let cursor_byte = cursor_pos as usize;
    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_byte, cpl as usize);
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };
    let has_selection = sel_lo < sel_hi;

    w.clear();

    // ── Style registry (first item in data buffer) ───────────────────

    let (style_table, mono_style_id, sans_style_id) = super::base_style_table(cfg);
    super::write_style_registry(w, &style_table);

    // ── Push data ────────────────────────────────────────────────────

    // Chrome glyphs.
    let title_glyphs = shape_chrome_text(cfg, title_label);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = shape_chrome_text(cfg, clock_text);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Document text glyphs.
    let line_glyph_refs = shape_visible_runs(
        w,
        &visible_runs,
        doc_text,
        cfg.font_data,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );

    // ── Allocate well-known nodes (sequential IDs 0..12) ─────────────

    for _ in 0..WELL_KNOWN_COUNT {
        w.alloc_node().unwrap();
    }

    // ── Chrome (title bar, shadow) ───────────────────────────────────

    {
        // Root: background desk color, content first, title bar overlaid on top.
        let n = w.node_mut(N_ROOT);
        n.first_child = N_CONTENT;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.fb_height);
        n.background = dc(cfg.bg_color);
        n.flags = NodeFlags::VISIBLE;
    }
    {
        // Title bar: paints AFTER content (higher z-order) so it overlays
        // document shadows that extend into the title bar region.
        let n = w.node_mut(N_TITLE_BAR);
        n.first_child = N_TITLE_ICON;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.title_bar_h);
        n.background = dc(cfg.chrome_bg);
        n.border = Border {
            color: dc(cfg.chrome_border),
            width: 1,
            _pad: [0; 3],
        };
        n.flags = NodeFlags::VISIBLE;
    }

    let text_y_offset = (cfg.title_bar_h.saturating_sub(cfg.line_height)) / 2;

    // Title bar icon.
    let icon_size_pt = cfg.line_height * 3 / 4;
    let icon_size_px = icon_size_pt * 2;
    let icon_paths = if active_space != 0 {
        icons::PHOTO
    } else {
        icons::FILE_TEXT
    };
    let icon_pixels =
        icons::rasterize_icon(icon_paths, icon_size_px, dc(cfg.chrome_title_color), 1.5);
    let icon_data_ref = w.push_data(&icon_pixels);
    let icon_hash = fnv1a(&icon_pixels);

    let icon_x: i32 = 10;
    let icon_y = (cfg.title_bar_h.saturating_sub(icon_size_pt)) / 2;
    let title_text_x = icon_x + icon_size_pt as i32 + 6;

    {
        let n = w.node_mut(N_TITLE_ICON);
        n.next_sibling = N_TITLE_TEXT;
        n.x = scene::pt(icon_x);
        n.y = scene::pt(icon_y as i32);
        n.width = scene::upt(icon_size_pt);
        n.height = scene::upt(icon_size_pt);
        n.content = Content::InlineImage {
            data: icon_data_ref,
            src_width: icon_size_px as u16,
            src_height: icon_size_px as u16,
        };
        n.content_hash = icon_hash;
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_TITLE_TEXT);
        n.next_sibling = N_CLOCK_TEXT;
        n.x = scene::pt(title_text_x);
        n.y = scene::pt(text_y_offset as i32);
        n.width = scene::upt(cfg.fb_width / 2);
        n.height = scene::upt(cfg.line_height);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_title_color),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(title_label);
        n.flags = NodeFlags::VISIBLE;
    }

    let clock_x = (cfg.fb_width - 12 - 80) as i32;
    {
        let n = w.node_mut(N_CLOCK_TEXT);
        n.x = scene::pt(clock_x);
        n.y = scene::pt(text_y_offset as i32);
        n.width = scene::upt(80);
        n.height = scene::upt(cfg.line_height);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_clock_color),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(clock_text);
        n.flags = NodeFlags::VISIBLE;
    }
    // N_SHADOW: unused (document shadows are on N_PAGE / N_DOC_IMAGE directly).

    // ── Content viewport + document strip ────────────────────────────

    {
        // Full-height content area — allows document shadows to extend
        // into the title bar region (title bar overlays on top).
        let n = w.node_mut(N_CONTENT);
        n.first_child = N_STRIP;
        n.next_sibling = N_TITLE_BAR;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.fb_height);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    {
        // N_STRIP: horizontal strip of document spaces, offset below title bar.
        // content_transform.tx slides between spaces.
        let n = w.node_mut(N_STRIP);
        n.first_child = N_PAGE;
        n.y = scene::pt(content_y as i32);
        n.width = scene::upt(cfg.fb_width * 2); // 2 spaces
        n.height = scene::upt(content_h_u32);
        n.content_transform =
            scene::AffineTransform::translate(-scene::mpt_to_f32(slide_offset), 0.0);
        n.flags = NodeFlags::VISIBLE;
    }

    // ── Space 0: text document page ──────────────────────────────────

    let page_x = ((cfg.fb_width - cfg.page_width) / 2) as i32;
    let page_y = ((content_h_u32 - cfg.page_height) / 2) as i32;
    let page_padding = cfg.text_inset_x;
    let text_area_h = cfg.page_height.saturating_sub(2 * page_padding);

    {
        let n = w.node_mut(N_PAGE);
        n.first_child = N_DOC_TEXT;
        n.next_sibling = N_DOC_IMAGE;
        n.x = scene::pt(page_x);
        n.y = scene::pt(page_y);
        n.width = scene::upt(cfg.page_width);
        n.height = scene::upt(cfg.page_height);
        n.background = dc(cfg.page_bg);
        n.shadow_color = DOCUMENT_SHADOW_COLOR;
        n.shadow_offset_x = DOCUMENT_SHADOW_OFFSET_X;
        n.shadow_offset_y = DOCUMENT_SHADOW_OFFSET_Y;
        n.shadow_blur_radius = DOCUMENT_SHADOW_BLUR_RADIUS;
        n.shadow_spread = DOCUMENT_SHADOW_SPREAD;
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_DOC_TEXT);
        n.x = scene::pt(page_padding as i32);
        n.y = scene::pt(page_padding as i32);
        n.width = scene::upt(doc_width);
        n.height = scene::upt(text_area_h);
        n.content_transform = scene::AffineTransform::translate(0.0, -scene::mpt_to_f32(scroll_y));
        n.content = Content::None;
        n.content_hash = fnv1a(doc_text);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }

    // Per-line Glyphs child nodes.
    let prev_line_node = allocate_line_nodes(
        w,
        &line_glyph_refs,
        doc_width,
        cfg.line_height,
        scene_text_color,
        cfg.font_size,
        mono_style_id,
    );

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    // Cursor.
    let cursor_x = ((cursor_col as i64 * cfg.char_width_fx as i64) >> 6) as scene::Mpt;
    let cursor_y = scene::pt(cursor_line as i32 * cfg.line_height as i32);
    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.width = scene::upt(2);
        n.height = scene::upt(cfg.line_height);
        n.background = dc(cfg.cursor_color);
        n.opacity = cursor_opacity;
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    // Selection.
    if has_selection {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            cpl as usize,
            cfg.char_width_fx,
            cfg.line_height,
            dc(cfg.sel_color),
            text_area_h,
            scroll_pt,
        );
    }

    // ── Space 1: image document (Content Region) ──────────────────────

    // Image display size: actual decoded dimensions, scaled to fit the content area
    // with padding while preserving aspect ratio.
    let (img_display_w, img_display_h) = {
        let s = crate::state();
        if s.image_width > 0 && s.image_height > 0 {
            let max_w = cfg.fb_width.saturating_sub(48); // padding
            let max_h = content_h_u32.saturating_sub(48);
            let src_w = s.image_width as u32;
            let src_h = s.image_height as u32;
            // Scale to fit: min(max_w/src_w, max_h/src_h), capped at 1.0 (don't upscale).
            // Use integer arithmetic: scale_num/scale_den.
            let scale_w_num = max_w;
            let scale_w_den = src_w;
            let scale_h_num = max_h;
            let scale_h_den = src_h;
            // Compare scale_w and scale_h: scale_w < scale_h iff w_num*h_den < h_num*w_den.
            let (s_num, s_den) = if (scale_w_num as u64) * (scale_h_den as u64)
                < (scale_h_num as u64) * (scale_w_den as u64)
            {
                (scale_w_num, scale_w_den)
            } else {
                (scale_h_num, scale_h_den)
            };
            // Don't upscale beyond native size.
            if s_num >= s_den {
                (src_w, src_h)
            } else {
                (
                    (src_w * s_num / s_den).max(1),
                    (src_h * s_num / s_den).max(1),
                )
            }
        } else {
            (128, 128) // fallback if no image decoded
        }
    };
    // Position in strip: viewport_width + centered within second space.
    let img_x = cfg.fb_width as i32 + ((cfg.fb_width as i32 - img_display_w as i32) / 2).max(0);
    let img_y = ((content_h_u32 as i32 - img_display_h as i32) / 2).max(0);
    {
        let s = crate::state();
        let n = w.node_mut(N_DOC_IMAGE);
        n.x = scene::pt(img_x);
        n.y = scene::pt(img_y);
        n.width = scene::upt(img_display_w);
        n.height = scene::upt(img_display_h);
        if s.image_content_id != 0 {
            n.content = Content::Image {
                content_id: s.image_content_id,
                src_width: s.image_width,
                src_height: s.image_height,
            };
            n.content_hash = s.image_content_id;
        } else {
            n.content = Content::None;
            n.content_hash = 0;
        }
        n.shadow_color = DOCUMENT_SHADOW_COLOR;
        n.shadow_offset_x = DOCUMENT_SHADOW_OFFSET_X;
        n.shadow_offset_y = DOCUMENT_SHADOW_OFFSET_Y;
        n.shadow_blur_radius = DOCUMENT_SHADOW_BLUR_RADIUS;
        n.shadow_spread = DOCUMENT_SHADOW_SPREAD;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    // ── Pointer cursor (top-level, highest z-order) ──────────────────
    // Sibling chain: N_CONTENT → N_TITLE_BAR → N_POINTER
    // (content set above; title bar links to pointer here)

    w.node_mut(N_TITLE_BAR).next_sibling = N_POINTER;
    setup_cursor(w, mouse_x, mouse_y, pointer_opacity);

    w.set_root(N_ROOT);
}

/// Update only the clock text in an already-open back buffer, then
/// mark N_CLOCK_TEXT changed.
pub fn build_clock_update(w: &mut scene::SceneWriter<'_>, cfg: &SceneConfig, clock_text: &[u8]) {
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
        n.content_hash = fnv1a(clock_text);
        w.mark_dirty(N_CLOCK_TEXT);
    }
}

/// Update cursor position in an already-open back buffer. Optionally
/// updates the clock if `clock_text` is provided.
pub fn build_cursor_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    cursor_pos: u32,
    doc_text: &[u8],
    chars_per_line: u32,
    clock_text: Option<&[u8]>,
    cursor_opacity: u8,
) {
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, chars_per_line as usize);
    let cursor_x = ((cursor_col as i64 * cfg.char_width_fx as i64) >> 6) as scene::Mpt;
    let cursor_y = scene::pt(cursor_line as i32 * cfg.line_height as i32);

    let n = w.node_mut(N_CURSOR);
    n.x = cursor_x;
    n.y = cursor_y;
    n.opacity = cursor_opacity;

    w.mark_dirty(N_CURSOR);

    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg);
    }
}

/// Update cursor position and selection rects in an already-open back
/// buffer.
#[allow(clippy::too_many_arguments)]
pub fn build_selection_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    doc_text: &[u8],
    content_h: u32,
    scroll_pt: i32,
    cursor_opacity: u8,
) {
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Clock text is updated only by update_document_content (timer-driven).
    // Skipping here prevents data buffer leak (~64 bytes per selection update).

    // Count per-line Glyphs children under N_DOC_TEXT (stop at
    // N_CURSOR). These must be preserved — only selection rects
    // (allocated after cursor) are truncated.
    let first = w.node(N_DOC_TEXT).first_child;
    let line_count = w.children_until(first, N_CURSOR).count() as u16;

    // Truncate selection rects only, keeping well-known + line nodes.
    w.set_node_count(WELL_KNOWN_COUNT + line_count);

    // N_DOC_TEXT is the sole child of N_CONTENT — no siblings.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;

    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = ((cursor_col as i64 * cfg.char_width_fx as i64) >> 6) as scene::Mpt;
    let cursor_y = scene::pt(cursor_line as i32 * cfg.line_height as i32);

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.opacity = cursor_opacity;
        n.next_sibling = NULL;
    }
    w.mark_dirty(N_CURSOR);

    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            cpl as usize,
            cfg.char_width_fx,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_pt,
        );
    }
}

/// Update document content (line nodes + cursor + selection) in an
/// already-open back buffer. Compacts the data buffer.
#[allow(clippy::too_many_arguments)]
pub fn build_document_content(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: scene::Mpt,
    mark_clock_changed: bool,
    cursor_opacity: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    let page_padding = cfg.text_inset_x;
    let text_area_h = cfg.page_height.saturating_sub(2 * page_padding);
    let scroll_pt = scroll_y >> 10;

    // Remove old dynamic nodes (line nodes + selection rects).
    // set_node_count automatically clears dangling first_child pointers
    // on surviving nodes that referenced the now-dead dynamic nodes.
    w.set_node_count(WELL_KNOWN_COUNT);

    // ── Data buffer compaction ──────────────────────────────
    // Note: reset_data invalidates all DataRef values (clip_path, content).
    // Surviving nodes with stale DataRefs will produce empty data lookups
    // (the reader returns &[] for out-of-bounds refs). This is safe but
    // means clip paths on demo nodes won't render after compaction — they
    // are re-pushed in build_full_scene on the next full rebuild.
    w.reset_data();

    // ── Style registry (first item in data buffer) ───────────────────
    let (style_table, mono_style_id, sans_style_id) = super::base_style_table(cfg);
    super::write_style_registry(w, &style_table);

    // Re-push pointer cursor image data (invalidated by reset_data).
    {
        let cursor_px = CURSOR_SIZE_PT * 2;
        let cursor_pixels = icons::rasterize_cursor(cursor_px);
        let cursor_ref = w.push_data(&cursor_pixels);
        let cursor_hash = fnv1a(&cursor_pixels);
        let n = w.node_mut(N_POINTER);
        n.content = Content::InlineImage {
            data: cursor_ref,
            src_width: cursor_px as u16,
            src_height: cursor_px as u16,
        };
        n.content_hash = cursor_hash;
    }

    // Re-push title icon pixel data (invalidated by reset_data).
    {
        let icon_size_pt = cfg.line_height * 3 / 4;
        let icon_size_px = icon_size_pt * 2;
        let icon_pixels = icons::rasterize_icon(
            icons::FILE_TEXT,
            icon_size_px,
            dc(cfg.chrome_title_color),
            1.5,
        );
        let icon_data_ref = w.push_data(&icon_pixels);
        let icon_hash = fnv1a(&icon_pixels);
        let n = w.node_mut(N_TITLE_ICON);
        n.content = Content::InlineImage {
            data: icon_data_ref,
            src_width: icon_size_px as u16,
            src_height: icon_size_px as u16,
        };
        n.content_hash = icon_hash;
    }

    // Content::Image nodes reference the Content Region (not the scene data buffer),
    // so no re-push is needed for space 1 after reset_data. The content_id reference
    // is stable across data buffer compaction.

    // Re-push title glyph data (shaped with chrome font).
    let title_glyphs = shape_chrome_text(cfg, title_label);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

    // Re-push clock glyph data (shaped with chrome font).
    let clock_glyphs = shape_chrome_text(cfg, clock_text);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Re-layout visible document text lines.
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );
    let visible_runs = scroll_runs(all_runs, scroll_y, cfg.line_height, text_area_h as i32);

    // Push visible line glyph data.
    let line_glyph_refs = shape_visible_runs(
        w,
        &visible_runs,
        doc_text,
        cfg.font_data,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );

    // Update N_TITLE_TEXT content references (data was reset).
    {
        let n = w.node_mut(N_TITLE_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_title_color),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(title_label);
    }

    // Update N_CLOCK_TEXT content references (data was reset).
    {
        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_clock_color),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(clock_text);
    }
    if mark_clock_changed {
        w.mark_dirty(N_CLOCK_TEXT);
    }

    // Re-create per-line Glyphs children under N_DOC_TEXT.
    // N_DOC_TEXT is the sole child of N_PAGE — no siblings.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).content_transform =
        scene::AffineTransform::translate(0.0, -scene::mpt_to_f32(scroll_y));
    w.node_mut(N_DOC_TEXT).content = Content::None;
    w.node_mut(N_DOC_TEXT).content_hash = fnv1a(doc_text);

    let prev_line_node = allocate_line_nodes(
        w,
        &line_glyph_refs,
        doc_width,
        cfg.line_height,
        scene_text_color,
        cfg.font_size,
        mono_style_id,
    );

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_dirty(N_DOC_TEXT);

    // Update cursor position (document-relative).
    let (cursor_line, cursor_col) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = ((cursor_col as i64 * cfg.char_width_fx as i64) >> 6) as scene::Mpt;
    let cursor_y = scene::pt(cursor_line as i32 * cfg.line_height as i32);

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
        n.opacity = cursor_opacity;
        n.next_sibling = NULL;
    }
    w.mark_dirty(N_CURSOR);

    // Build selection rects.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            cpl as usize,
            cfg.char_width_fx,
            cfg.line_height,
            dc(cfg.sel_color),
            text_area_h,
            scroll_pt,
        );
    }
}

// ── Rich text scene building ────────────────────────────────────────

/// Configuration for rich text rendering. Extends SceneConfig with
/// font data pointers for all three font families (mono, sans, serif).
pub struct RichFonts<'a> {
    pub mono_data: &'a [u8],
    pub mono_upem: u16,
    pub mono_content_id: u32,
    pub mono_ascender: i16,
    pub mono_descender: i16,
    pub mono_line_gap: i16,
    pub sans_data: &'a [u8],
    pub sans_upem: u16,
    pub sans_content_id: u32,
    pub sans_ascender: i16,
    pub sans_descender: i16,
    pub sans_line_gap: i16,
    pub serif_data: &'a [u8],
    pub serif_upem: u16,
    pub serif_content_id: u32,
    pub serif_ascender: i16,
    pub serif_descender: i16,
    pub serif_line_gap: i16,
    pub mono_italic_data: &'a [u8],
    pub mono_italic_upem: u16,
    pub mono_italic_content_id: u32,
    pub mono_italic_ascender: i16,
    pub mono_italic_descender: i16,
    pub mono_italic_line_gap: i16,
    pub sans_italic_data: &'a [u8],
    pub sans_italic_upem: u16,
    pub sans_italic_content_id: u32,
    pub sans_italic_ascender: i16,
    pub sans_italic_descender: i16,
    pub sans_italic_line_gap: i16,
    pub serif_italic_data: &'a [u8],
    pub serif_italic_upem: u16,
    pub serif_italic_content_id: u32,
    pub serif_italic_ascender: i16,
    pub serif_italic_descender: i16,
    pub serif_italic_line_gap: i16,
}

impl<'a> RichFonts<'a> {
    /// Resolve a piecetable Style to font data + metrics.
    /// When FLAG_ITALIC is set, returns the italic font variant.
    pub fn resolve(&self, style: &piecetable::Style) -> FontInfo<'_> {
        let italic = style.flags & piecetable::FLAG_ITALIC != 0;
        match (style.font_family, italic) {
            (piecetable::FONT_MONO, false) => FontInfo {
                data: self.mono_data,
                upem: self.mono_upem,
                content_id: self.mono_content_id,
                ascender: self.mono_ascender,
                descender: self.mono_descender,
                line_gap: self.mono_line_gap,
            },
            (piecetable::FONT_MONO, true) => FontInfo {
                data: if self.mono_italic_data.is_empty() {
                    self.mono_data
                } else {
                    self.mono_italic_data
                },
                upem: if self.mono_italic_upem > 0 {
                    self.mono_italic_upem
                } else {
                    self.mono_upem
                },
                content_id: if self.mono_italic_data.is_empty() {
                    self.mono_content_id
                } else {
                    self.mono_italic_content_id
                },
                ascender: if self.mono_italic_upem > 0 {
                    self.mono_italic_ascender
                } else {
                    self.mono_ascender
                },
                descender: if self.mono_italic_upem > 0 {
                    self.mono_italic_descender
                } else {
                    self.mono_descender
                },
                line_gap: if self.mono_italic_upem > 0 {
                    self.mono_italic_line_gap
                } else {
                    self.mono_line_gap
                },
            },
            (piecetable::FONT_SERIF, false) => FontInfo {
                data: self.serif_data,
                upem: self.serif_upem,
                content_id: self.serif_content_id,
                ascender: self.serif_ascender,
                descender: self.serif_descender,
                line_gap: self.serif_line_gap,
            },
            (piecetable::FONT_SERIF, true) => FontInfo {
                data: if self.serif_italic_data.is_empty() {
                    self.serif_data
                } else {
                    self.serif_italic_data
                },
                upem: if self.serif_italic_upem > 0 {
                    self.serif_italic_upem
                } else {
                    self.serif_upem
                },
                content_id: if self.serif_italic_data.is_empty() {
                    self.serif_content_id
                } else {
                    self.serif_italic_content_id
                },
                ascender: if self.serif_italic_upem > 0 {
                    self.serif_italic_ascender
                } else {
                    self.serif_ascender
                },
                descender: if self.serif_italic_upem > 0 {
                    self.serif_italic_descender
                } else {
                    self.serif_descender
                },
                line_gap: if self.serif_italic_upem > 0 {
                    self.serif_italic_line_gap
                } else {
                    self.serif_line_gap
                },
            },
            (_, false) => FontInfo {
                data: self.sans_data,
                upem: self.sans_upem,
                content_id: self.sans_content_id,
                ascender: self.sans_ascender,
                descender: self.sans_descender,
                line_gap: self.sans_line_gap,
            },
            (_, true) => FontInfo {
                data: if self.sans_italic_data.is_empty() {
                    self.sans_data
                } else {
                    self.sans_italic_data
                },
                upem: if self.sans_italic_upem > 0 {
                    self.sans_italic_upem
                } else {
                    self.sans_upem
                },
                content_id: if self.sans_italic_data.is_empty() {
                    self.sans_content_id
                } else {
                    self.sans_italic_content_id
                },
                ascender: if self.sans_italic_upem > 0 {
                    self.sans_italic_ascender
                } else {
                    self.sans_ascender
                },
                descender: if self.sans_italic_upem > 0 {
                    self.sans_italic_descender
                } else {
                    self.sans_descender
                },
                line_gap: if self.sans_italic_upem > 0 {
                    self.sans_italic_line_gap
                } else {
                    self.sans_line_gap
                },
            },
        }
    }
}

/// Allocate per-line, per-segment Glyphs nodes for rich text under
/// N_DOC_TEXT. Each styled segment in a line becomes its own Glyphs
/// node, linked as siblings. Returns the last allocated node ID.
fn allocate_rich_line_nodes(
    w: &mut scene::SceneWriter<'_>,
    rich_lines: &[RichLine],
    scratch: &[u8],
    pt_buf: &[u8],
    fonts: &RichFonts<'_>,
    style_table: &mut super::StyleTable,
    doc_width: u32,
    line_height: u32,
    scroll_y: scene::Mpt,
    viewport_height: i32,
) -> u16 {
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_node: u16 = NULL;
    let scroll_pt = scroll_y >> 10;

    for line in rich_lines {
        // Visibility culling using per-line height.
        let line_bottom = line.y + line.line_height;
        if line_bottom <= scroll_pt {
            continue;
        }
        if line.y >= scroll_pt + viewport_height {
            continue;
        }

        // Track running x position within the line (points, fractional).
        let mut pen_x: f32 = 0.0;

        for seg in &line.segments {
            let seg_text = if seg.text_start + seg.text_len <= scratch.len() {
                &scratch[seg.text_start..seg.text_start + seg.text_len]
            } else {
                continue;
            };

            let Some(style) = piecetable::style(pt_buf, seg.style_id) else {
                continue;
            };
            let fi = fonts.resolve(style);
            let font_size = style.font_size_pt as u16;
            let italic = style.flags & piecetable::FLAG_ITALIC != 0;

            let shaped =
                shape_rich_segment(fi.data, seg_text, font_size, fi.upem, style.weight, italic);
            if shaped.is_empty() {
                continue;
            }

            let glyph_ref = w.push_shaped_glyphs(&shaped);
            let glyph_count = shaped.len() as u16;

            // Resolve style_id from the StyleTable using the font's
            // content_id and the style's variation axes.
            let mut axes_buf = [fonts::rasterize::AxisValue {
                tag: *b"wght",
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
            let style_id = style_table.style_id_for(
                fi.content_id,
                &axes_buf[..axis_count],
                fi.ascender as u16,
                (-fi.descender) as u16,
                fi.upem,
            );

            let color = Color::rgba(
                style.color[0],
                style.color[1],
                style.color[2],
                style.color[3],
            );

            if let Some(node_id) = w.alloc_node() {
                let n = w.node_mut(node_id);
                n.x = scene::pt(pen_x as i32);
                n.y = scene::pt(seg.y);
                n.width = scene::upt(doc_width);
                n.height = scene::upt(line.line_height as u32);
                n.content = Content::Glyphs {
                    color,
                    glyphs: glyph_ref,
                    glyph_count,
                    font_size,
                    style_id,
                };
                n.content_hash = scene::fnv1a(&glyph_ref.offset.to_le_bytes());
                n.flags = NodeFlags::VISIBLE;
                n.next_sibling = NULL;

                if prev_node == NULL {
                    w.node_mut(N_DOC_TEXT).first_child = node_id;
                } else {
                    w.node_mut(prev_node).next_sibling = node_id;
                }
                prev_node = node_id;
            }

            // Advance pen by segment width (x_advance is 16.16 fixed-point).
            let seg_width: f32 = shaped.iter().map(|g| g.x_advance as f32 / 65536.0).sum();

            // Underline: thin line at baseline + descent/4.
            if style.flags & piecetable::FLAG_UNDERLINE != 0 && seg_width > 0.0 {
                let asc_pt = if fi.upem > 0 {
                    (fi.ascender as i32).abs() as f32 * font_size as f32 / fi.upem as f32
                } else {
                    font_size as f32 * 0.8
                };
                let desc_pt = if fi.upem > 0 {
                    (fi.descender as i32).abs() as f32 * font_size as f32 / fi.upem as f32
                } else {
                    font_size as f32 * 0.2
                };
                let underline_y = seg.y as f32 + asc_pt + desc_pt * 0.3;
                let thickness = (font_size as f32 / 14.0).max(1.0);
                if let Some(ul_id) = w.alloc_node() {
                    let n = w.node_mut(ul_id);
                    n.x = scene::pt(pen_x as i32);
                    n.y = scene::pt(underline_y as i32);
                    n.width = scene::upt(seg_width as u32 + 1);
                    n.height = scene::upt(thickness as u32);
                    n.background = color;
                    n.content = Content::None;
                    n.flags = NodeFlags::VISIBLE;
                    n.next_sibling = NULL;
                    if prev_node != NULL {
                        w.node_mut(prev_node).next_sibling = ul_id;
                    }
                    w.mark_dirty(ul_id);
                    prev_node = ul_id;
                }
            }

            // Strikethrough: thin line at mid-height of text.
            if style.flags & piecetable::FLAG_STRIKETHROUGH != 0 && seg_width > 0.0 {
                let asc_pt = if fi.upem > 0 {
                    (fi.ascender as i32).abs() as f32 * font_size as f32 / fi.upem as f32
                } else {
                    font_size as f32 * 0.8
                };
                let strike_y = seg.y as f32 + asc_pt * 0.6;
                let thickness = (font_size as f32 / 14.0).max(1.0);
                if let Some(st_id) = w.alloc_node() {
                    let n = w.node_mut(st_id);
                    n.x = scene::pt(pen_x as i32);
                    n.y = scene::pt(strike_y as i32);
                    n.width = scene::upt(seg_width as u32 + 1);
                    n.height = scene::upt(thickness as u32);
                    n.background = color;
                    n.content = Content::None;
                    n.flags = NodeFlags::VISIBLE;
                    n.next_sibling = NULL;
                    if prev_node != NULL {
                        w.node_mut(prev_node).next_sibling = st_id;
                    }
                    w.mark_dirty(st_id);
                    prev_node = st_id;
                }
            }

            pen_x += seg_width;
        }
    }

    prev_node
}

/// Build or rebuild document content for a text/rich document.
///
/// Replaces `build_document_content` for rich text. Layout uses the
/// piece table's styled runs, shaping each segment with its own font.
#[allow(clippy::too_many_arguments)]
pub fn build_rich_document_content(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    pt_buf: &[u8],
    fonts: &RichFonts<'_>,
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: scene::Mpt,
    mark_clock_changed: bool,
    cursor_opacity: u8,
) {
    let doc_width = doc_width(cfg);
    let page_padding = cfg.text_inset_x;
    let text_area_h = cfg.page_height.saturating_sub(2 * page_padding);
    let scroll_pt = scroll_y >> 10;

    // Remove old dynamic nodes.
    w.set_node_count(WELL_KNOWN_COUNT);
    w.reset_data();

    // ── Style registry (first item in data buffer) ───────────────────
    // Start with base styles (mono=0, sans=1), then register styles
    // from the piece table's style palette for rich text segments.
    let (mut style_table, _mono_style_id, sans_style_id) = super::base_style_table(cfg);

    // Register each unique style from the piece table palette.
    let palette_count = piecetable::style_count(pt_buf);
    for si in 0..palette_count {
        if let Some(style) = piecetable::style(pt_buf, si as u8) {
            let fi = fonts.resolve(style);
            // Build axis values for variable font variations.
            let mut axes_buf = [fonts::rasterize::AxisValue {
                tag: *b"wght",
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
            axes_buf[axis_count] = fonts::rasterize::AxisValue {
                tag: *b"opsz",
                value: style.font_size_pt as f32,
            };
            axis_count += 1;
            let axes = &axes_buf[..axis_count];
            let _ = style_table.style_id_for(
                fi.content_id,
                axes,
                fi.ascender as u16,
                (-fi.descender) as u16,
                fi.upem,
            );
        }
    }
    super::write_style_registry(w, &style_table);

    // Re-push pointer cursor image data.
    {
        let cursor_px = CURSOR_SIZE_PT * 2;
        let cursor_pixels = icons::rasterize_cursor(cursor_px);
        let cursor_ref = w.push_data(&cursor_pixels);
        let cursor_hash = fnv1a(&cursor_pixels);
        let n = w.node_mut(N_POINTER);
        n.content = Content::InlineImage {
            data: cursor_ref,
            src_width: cursor_px as u16,
            src_height: cursor_px as u16,
        };
        n.content_hash = cursor_hash;
    }

    // Re-push title icon.
    {
        let icon_size_pt = cfg.line_height * 3 / 4;
        let icon_size_px = icon_size_pt * 2;
        let icon_pixels = icons::rasterize_icon(
            icons::FILE_TEXT,
            icon_size_px,
            dc(cfg.chrome_title_color),
            1.5,
        );
        let icon_data_ref = w.push_data(&icon_pixels);
        let icon_hash = fnv1a(&icon_pixels);
        let n = w.node_mut(N_TITLE_ICON);
        n.content = Content::InlineImage {
            data: icon_data_ref,
            src_width: icon_size_px as u16,
            src_height: icon_size_px as u16,
        };
        n.content_hash = icon_hash;
    }

    // Re-push chrome text.
    let title_glyphs = shape_chrome_text(cfg, title_label);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = shape_chrome_text(cfg, clock_text);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Update chrome node references.
    {
        let n = w.node_mut(N_TITLE_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_title_color),
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(title_label);
    }
    {
        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color: dc(cfg.chrome_clock_color),
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            font_size: cfg.font_size,
            style_id: sans_style_id,
        };
        n.content_hash = fnv1a(clock_text);
    }
    if mark_clock_changed {
        w.mark_dirty(N_CLOCK_TEXT);
    }

    // Layout rich text.
    let line_width_pt = doc_width as f32;
    let mut scratch = [0u8; 32768];
    let text_len = piecetable::text_slice(pt_buf, 0, piecetable::text_len(pt_buf), &mut scratch);

    let mono_fi = FontInfo {
        data: fonts.mono_data,
        upem: fonts.mono_upem,
        content_id: fonts.mono_content_id,
        ascender: fonts.mono_ascender,
        descender: fonts.mono_descender,
        line_gap: fonts.mono_line_gap,
    };
    let sans_fi = FontInfo {
        data: fonts.sans_data,
        upem: fonts.sans_upem,
        content_id: fonts.sans_content_id,
        ascender: fonts.sans_ascender,
        descender: fonts.sans_descender,
        line_gap: fonts.sans_line_gap,
    };
    let serif_fi = FontInfo {
        data: fonts.serif_data,
        upem: fonts.serif_upem,
        content_id: fonts.serif_content_id,
        ascender: fonts.serif_ascender,
        descender: fonts.serif_descender,
        line_gap: fonts.serif_line_gap,
    };
    let rich_lines = layout_rich_lines(
        pt_buf,
        &mut scratch,
        line_width_pt,
        cfg.line_height as i32,
        &mono_fi,
        &sans_fi,
        &serif_fi,
    );

    // Set up N_DOC_TEXT.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).content_transform =
        scene::AffineTransform::translate(0.0, -scene::mpt_to_f32(scroll_y));
    w.node_mut(N_DOC_TEXT).content = Content::None;
    w.node_mut(N_DOC_TEXT).content_hash = text_len as u32;

    // Allocate per-segment glyph nodes.
    let prev_line_node = allocate_rich_line_nodes(
        w,
        &rich_lines,
        &scratch[..text_len],
        pt_buf,
        fonts,
        &mut style_table,
        doc_width,
        cfg.line_height,
        scroll_y,
        text_area_h as i32,
    );

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_dirty(N_DOC_TEXT);

    // Cursor positioning for rich text.
    // Use byte position in logical text. For now, use a simple approach:
    // walk the rich_lines to find which line/column the cursor is on.
    let cursor_info =
        rich_cursor_position(pt_buf, &scratch[..text_len], cursor_pos, &rich_lines, fonts);

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_info.x;
        n.y = cursor_info.y;
        n.width = scene::upt(2);
        n.height = scene::upt(cursor_info.height);
        n.background = dc(cfg.cursor_color);
        n.opacity = cursor_opacity;
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }
    w.mark_dirty(N_CURSOR);

    // Selection rendering for rich text with proportional x-positioning.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        let sel_color = dc(cfg.sel_color);
        let scroll_pt = scroll_y >> 10;
        let mut prev_sel_node: u16 = NULL;

        for line in &rich_lines {
            // Compute line's byte range from its segments.
            let line_byte_start = line.segments.first().map_or(0, |s| s.text_start);
            let line_byte_end = line
                .segments
                .last()
                .map_or(0, |s| s.text_start + s.text_len);

            // Skip lines outside the selection.
            if line_byte_end <= sel_lo || line_byte_start >= sel_hi {
                continue;
            }

            // Visibility culling.
            let line_bottom = line.y + line.line_height;
            if line_bottom <= scroll_pt || line.y >= scroll_pt + text_area_h as i32 {
                continue;
            }

            // Walk segments to compute x_start and x_end of selection on this line.
            let mut pen_x: f32 = 0.0;
            let mut x_start: f32 = 0.0;
            let mut x_end: f32 = 0.0;
            let mut found_start = false;
            let clamp_lo = sel_lo.max(line_byte_start);
            let clamp_hi = sel_hi.min(line_byte_end);

            for seg in &line.segments {
                let seg_start = seg.text_start;
                let seg_end = seg.text_start + seg.text_len;
                let Some(style) = piecetable::style(pt_buf, seg.style_id) else {
                    continue;
                };
                let fi = fonts.resolve(style);

                // Build axes for char_advance_pt.
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
                axes_buf[axis_count] = fonts::rasterize::AxisValue {
                    tag: *b"opsz",
                    value: style.font_size_pt as f32,
                };
                axis_count += 1;
                let axes = &axes_buf[..axis_count];

                let seg_text_slice = &scratch[seg_start..seg_end.min(text_len)];
                let mut byte_pos = seg_start;
                for ch in core::str::from_utf8(seg_text_slice).unwrap_or("").chars() {
                    if byte_pos == clamp_lo {
                        x_start = pen_x;
                        found_start = true;
                    }
                    let adv = super::char_advance_pt(
                        fi.data,
                        ch,
                        style.font_size_pt as u16,
                        fi.upem,
                        axes,
                    );
                    byte_pos += ch.len_utf8();
                    pen_x += adv;
                    if byte_pos >= clamp_hi {
                        x_end = pen_x;
                        break;
                    }
                }

                if byte_pos >= clamp_hi {
                    break;
                }
            }

            // If selection extends to end of line, use full pen_x.
            if found_start && x_end <= x_start {
                x_end = pen_x;
            }
            if !found_start {
                continue;
            }

            let rect_w = x_end - x_start;
            if rect_w <= 0.0 {
                continue;
            }

            if let Some(sel_id) = w.alloc_node() {
                let n = w.node_mut(sel_id);
                n.x = scene::pt(x_start as i32);
                n.y = scene::pt(line.y);
                n.width = scene::upt(rect_w as u32 + 1);
                n.height = scene::upt(line.line_height as u32);
                n.background = sel_color;
                n.content = Content::None;
                n.flags = NodeFlags::VISIBLE;
                n.next_sibling = NULL;

                if prev_sel_node == NULL {
                    // Link selection after cursor.
                    w.node_mut(N_CURSOR).next_sibling = sel_id;
                } else {
                    w.node_mut(prev_sel_node).next_sibling = sel_id;
                }
                w.mark_dirty(sel_id);
                prev_sel_node = sel_id;
            }
        }
    }
}

/// Cursor metrics for rich text: position, height, and baseline offset.
struct RichCursorInfo {
    x: scene::Mpt,
    y: scene::Mpt,
    height: u32,
}

/// Compute cursor position and height for a rich text document.
///
/// Returns baseline-aligned y and style-matched height. The cursor spans
/// from (baseline - ascent) to (baseline + |descent|), aligned with the
/// text at the cursor position.
fn rich_cursor_position(
    pt_buf: &[u8],
    text: &[u8],
    cursor_pos: u32,
    rich_lines: &[RichLine],
    fonts: &RichFonts<'_>,
) -> RichCursorInfo {
    let default_height = 18u32; // fallback

    // Find which line the cursor is on.
    for line in rich_lines {
        if line.segments.is_empty() {
            continue;
        }
        let line_start = line.segments[0].text_start;
        let last_seg = line.segments.last().unwrap();
        let line_end = last_seg.text_start + last_seg.text_len;

        if (cursor_pos as usize) < line_start || (cursor_pos as usize) > line_end {
            continue;
        }

        // Compute max ascent for this line (for baseline alignment).
        let mut max_ascent_pt: f32 = 0.0;
        for seg in &line.segments {
            if let Some(style) = piecetable::style(pt_buf, seg.style_id) {
                let fi = fonts.resolve(style);
                if fi.upem > 0 {
                    let asc = (fi.ascender as i32).abs() as f32 * style.font_size_pt as f32
                        / fi.upem as f32;
                    if asc > max_ascent_pt {
                        max_ascent_pt = asc;
                    }
                }
            }
        }

        // Cursor is on this line. Measure x by walking chars up to cursor_pos.
        // Also track the style at the cursor for height computation.
        let mut x_pt: f32 = 0.0;
        let mut cursor_style_id: u8 = 0;
        for seg in &line.segments {
            let seg_end = seg.text_start + seg.text_len;
            if cursor_pos as usize <= seg.text_start {
                break;
            }
            cursor_style_id = seg.style_id;
            let Some(style) = piecetable::style(pt_buf, seg.style_id) else {
                continue;
            };
            let fi = fonts.resolve(style);

            // Build variation axes for this segment's style.
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
            axes_buf[axis_count] = fonts::rasterize::AxisValue {
                tag: *b"opsz",
                value: style.font_size_pt as f32,
            };
            axis_count += 1;
            let axes = &axes_buf[..axis_count];

            let seg_text = &text[seg.text_start..seg_end.min(text.len())];
            let measure_end = (cursor_pos as usize).min(seg_end) - seg.text_start;
            let measure_text = &seg_text[..measure_end.min(seg_text.len())];

            for ch in core::str::from_utf8(measure_text).unwrap_or("").chars() {
                x_pt +=
                    super::char_advance_pt(fi.data, ch, style.font_size_pt as u16, fi.upem, axes);
            }

            if cursor_pos as usize <= seg_end {
                break;
            }
        }

        // Compute cursor height and y from the style at cursor position.
        let (cursor_h, cursor_ascent_pt) =
            if let Some(style) = piecetable::style(pt_buf, cursor_style_id) {
                let fi = fonts.resolve(style);
                if fi.upem > 0 {
                    let asc = (fi.ascender as i32).abs() as f32 * style.font_size_pt as f32
                        / fi.upem as f32;
                    let desc = (fi.descender as i32).abs() as f32 * style.font_size_pt as f32
                        / fi.upem as f32;
                    ((asc + desc) as u32, asc)
                } else {
                    (style.font_size_pt as u32, style.font_size_pt as f32 * 0.8)
                }
            } else {
                (default_height, default_height as f32 * 0.8)
            };

        // Baseline-aligned y: line.y + (max_ascent - cursor_ascent).
        let baseline_offset = (max_ascent_pt - cursor_ascent_pt) as i32;
        let cursor_y = scene::pt(line.y + baseline_offset);

        let x_fx = (x_pt * 65536.0) as i64;
        let cursor_x = (x_fx >> 6) as scene::Mpt;
        return RichCursorInfo {
            x: cursor_x,
            y: cursor_y,
            height: cursor_h.max(2),
        };
    }

    // Cursor past end — put it on the last line.
    if let Some(last_line) = rich_lines.last() {
        RichCursorInfo {
            x: 0,
            y: scene::pt(last_line.y),
            height: default_height,
        }
    } else {
        RichCursorInfo {
            x: 0,
            y: 0,
            height: default_height,
        }
    }
}
