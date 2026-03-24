//! Full scene graph builds.
//!
//! Contains `build_full_scene` (initial scene from scratch) and
//! `build_document_content` (compaction rebuild of document content
//! within an existing scene).

use alloc::vec::Vec;

use scene::{fnv1a, Border, Color, Content, FillRule, NodeFlags, NULL};

use super::{
    allocate_line_nodes, allocate_selection_rects, byte_to_line_col, chars_per_line, dc, doc_width,
    layout_mono_lines, scroll_runs, shape_chrome_text, shape_visible_runs, update_clock_inline,
    SceneConfig, FONT_SANS, N_CLOCK_TEXT, N_CONTENT, N_CURSOR, N_DOC_IMAGE, N_DOC_TEXT, N_PAGE,
    N_POINTER, N_ROOT, N_SHADOW, N_STRIP, N_TITLE_BAR, N_TITLE_ICON, N_TITLE_TEXT,
    WELL_KNOWN_COUNT,
};
use crate::icons;

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
        let n = w.node_mut(N_ROOT);
        n.first_child = N_TITLE_BAR;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.fb_height);
        n.background = dc(cfg.bg_color);
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_TITLE_BAR);
        n.first_child = N_TITLE_ICON;
        n.next_sibling = N_SHADOW;
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
            axis_hash: FONT_SANS,
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
            axis_hash: FONT_SANS,
        };
        n.content_hash = fnv1a(clock_text);
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_SHADOW);
        n.next_sibling = N_CONTENT;
        n.y = scene::pt(cfg.title_bar_h as i32);
        n.width = scene::upt(cfg.fb_width);
        n.height = 0;
        n.background = Color::TRANSPARENT;
        n.flags = NodeFlags::VISIBLE;
    }

    // ── Content viewport + document strip ────────────────────────────

    {
        let n = w.node_mut(N_CONTENT);
        n.first_child = N_STRIP;
        n.next_sibling = NULL;
        n.y = scene::pt(content_y as i32);
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(content_h_u32);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    {
        // N_STRIP: horizontal strip of document spaces.
        // content_transform.tx slides between spaces.
        let n = w.node_mut(N_STRIP);
        n.first_child = N_PAGE;
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
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    // ── Pointer cursor (top-level, highest z-order) ──────────────────

    w.node_mut(N_CONTENT).next_sibling = N_POINTER;
    setup_cursor(w, mouse_x, mouse_y, pointer_opacity);

    w.set_root(N_ROOT);
}

/// Update only the clock text in an already-open back buffer, then
/// mark N_CLOCK_TEXT changed.
pub fn build_clock_update(w: &mut scene::SceneWriter<'_>, cfg: &SceneConfig, clock_text: &[u8]) {
    let clock_node = w.node(N_CLOCK_TEXT);
    if let Content::Glyphs { color, .. } = clock_node.content {
        let new_glyphs = shape_chrome_text(cfg, clock_text);
        let new_ref = w.push_shaped_glyphs(&new_glyphs);
        let new_count = new_glyphs.len() as u16;

        let n = w.node_mut(N_CLOCK_TEXT);
        n.content = Content::Glyphs {
            color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            axis_hash: FONT_SANS,
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
            axis_hash: FONT_SANS,
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
            axis_hash: FONT_SANS,
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
