//! Full scene graph builds.
//!
//! Contains `build_full_scene` (initial scene from scratch) and
//! `build_document_content` (compaction rebuild of document content
//! within an existing scene). Layout computation is handled by the
//! layout engine (B) — this module reads pre-computed results.

use alloc::vec::Vec;

use scene::{fnv1a, Border, Color, Content, FillRule, NodeFlags, NULL};

use super::{
    allocate_line_nodes, allocate_selection_rects, byte_to_line_col, chars_per_line, dc, doc_width,
    emit_icon, shape_chrome_text, update_clock_inline, SceneConfig, N_CLOCK_TEXT, N_CONTENT,
    N_CURSOR, N_DOC_IMAGE, N_DOC_TEXT, N_PAGE, N_ROOT, N_SHADOW, N_STRIP, N_TITLE_BAR,
    N_TITLE_ICON, N_TITLE_TEXT, WELL_KNOWN_COUNT,
};

const DOCUMENT_SHADOW_BLUR_RADIUS: u8 = 64;
const DOCUMENT_SHADOW_COLOR: Color = Color::rgba(0, 0, 0, 255);
const DOCUMENT_SHADOW_OFFSET_X: i16 = 0;
const DOCUMENT_SHADOW_OFFSET_Y: i16 = 0;
const DOCUMENT_SHADOW_SPREAD: i8 = 36;

// ── Full scene builds (called by SceneState methods) ────────────────

/// Build the full scene graph into a fresh (cleared) SceneWriter.
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
    slide_offset: scene::Mpt,
    active_space: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h_u32 = cfg.fb_height.saturating_sub(content_y);
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

    // ── Style registry ────────────────────────────────────────────────
    let (mono_style_id, sans_style_id) = if let Some(header) = crate::read_layout_header() {
        let registry_bytes = crate::read_layout_style_registry(&header);
        if !registry_bytes.is_empty() {
            let _registry_ref = w.push_data(registry_bytes);
        }
        (0u32, 1u32)
    } else {
        let (style_table, mono_id, sans_id) = super::base_style_table(cfg);
        super::write_style_registry(w, &style_table);
        (mono_id, sans_id)
    };

    // ── Push data ────────────────────────────────────────────────────

    // Chrome glyphs (always shaped locally — not part of document layout).
    let title_glyphs = shape_chrome_text(cfg, title_label);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = shape_chrome_text(cfg, clock_text);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Document text glyphs — read pre-shaped from layout engine (B).
    let line_glyph_refs = if let Some(header) = crate::read_layout_header() {
        super::push_layout_results_to_scene(w, &header)
    } else {
        alloc::vec::Vec::new()
    };

    // ── Allocate well-known nodes (sequential IDs 0..12) ─────────────

    for _ in 0..WELL_KNOWN_COUNT {
        w.alloc_node().unwrap();
    }

    // ── Chrome (title bar, shadow) ───────────────────────────────────

    {
        let n = w.node_mut(N_ROOT);
        n.first_child = N_CONTENT;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.fb_height);
        n.background = dc(cfg.bg_color);
        n.flags = NodeFlags::VISIBLE;
    }
    {
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

    let icon_size_pt = cfg.line_height + 2;
    let mimetype = if active_space != 0 {
        Some("image/png")
    } else {
        Some("text/plain")
    };
    let icon = icon_lib::get("document", mimetype);

    let icon_x: i32 = 6;
    let icon_y = ((cfg.title_bar_h.saturating_sub(icon_size_pt)) / 2).saturating_sub(1);
    let title_text_x = icon_x + icon_size_pt as i32 + 8;

    emit_icon(
        w,
        N_TITLE_ICON,
        icon,
        icon_x,
        icon_y as i32,
        icon_size_pt,
        Color::TRANSPARENT,
        dc(cfg.chrome_title_color),
    );
    w.node_mut(N_TITLE_ICON).next_sibling = N_TITLE_TEXT;
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

    // ── Content viewport + document strip ────────────────────────────

    {
        let n = w.node_mut(N_CONTENT);
        n.first_child = N_STRIP;
        n.next_sibling = N_TITLE_BAR;
        n.width = scene::upt(cfg.fb_width);
        n.height = scene::upt(cfg.fb_height);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    {
        let n = w.node_mut(N_STRIP);
        n.first_child = N_PAGE;
        n.y = scene::pt(content_y as i32);
        n.width = scene::upt(cfg.fb_width * 2);
        n.height = scene::upt(content_h_u32);
        n.child_offset_x = -scene::mpt_to_f32(slide_offset);
        n.child_offset_y = 0.0;
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
        n.cursor_shape = scene::CURSOR_TEXT;
        n.flags = NodeFlags::VISIBLE;
    }
    {
        let n = w.node_mut(N_DOC_TEXT);
        n.x = scene::pt(page_padding as i32);
        n.y = scene::pt(page_padding as i32);
        n.width = scene::upt(doc_width);
        n.height = scene::upt(text_area_h);
        n.child_offset_x = 0.0;
        n.child_offset_y = -scene::mpt_to_f32(scroll_y);
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

    let (img_display_w, img_display_h) = {
        let s = crate::state();
        if s.image_width > 0 && s.image_height > 0 {
            let max_w = cfg.fb_width.saturating_sub(48);
            let max_h = content_h_u32.saturating_sub(48);
            let src_w = s.image_width as u32;
            let src_h = s.image_height as u32;
            let scale_w_num = max_w;
            let scale_w_den = src_w;
            let scale_h_num = max_h;
            let scale_h_den = src_h;
            let (s_num, s_den) = if (scale_w_num as u64) * (scale_h_den as u64)
                < (scale_h_num as u64) * (scale_w_den as u64)
            {
                (scale_w_num, scale_w_den)
            } else {
                (scale_h_num, scale_h_den)
            };
            if s_num >= s_den {
                (src_w, src_h)
            } else {
                (
                    (src_w * s_num / s_den).max(1),
                    (src_h * s_num / s_den).max(1),
                )
            }
        } else {
            (128, 128)
        }
    };
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

    w.node_mut(N_TITLE_BAR).next_sibling = NULL;

    w.set_root(N_ROOT);
}

/// Update only the clock text.
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

/// Update cursor position.
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

/// Update cursor position and selection rects.
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

    let first = w.node(N_DOC_TEXT).first_child;
    let line_count = w.children_until(first, N_CURSOR).count() as u16;

    w.set_node_count(WELL_KNOWN_COUNT + line_count);
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

/// Update document content (line nodes + cursor + selection).
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
    active_space: u8,
) {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    let page_padding = cfg.text_inset_x;
    let text_area_h = cfg.page_height.saturating_sub(2 * page_padding);
    let scroll_pt = scroll_y >> 10;

    w.set_node_count(WELL_KNOWN_COUNT);
    w.reset_data();

    // ── Style registry ──────────────────────────────────────────────
    let (mono_style_id, sans_style_id) = if let Some(header) = crate::read_layout_header() {
        let registry_bytes = crate::read_layout_style_registry(&header);
        if !registry_bytes.is_empty() {
            let _registry_ref = w.push_data(registry_bytes);
        }
        (0u32, 1u32)
    } else {
        let (style_table, mono_id, sans_id) = super::base_style_table(cfg);
        super::write_style_registry(w, &style_table);
        (mono_id, sans_id)
    };

    // Re-push title icon path data.
    {
        let icon_size_pt = cfg.line_height + 2;
        let mimetype = if active_space != 0 {
            Some("image/png")
        } else {
            Some("text/plain")
        };
        let icon = icon_lib::get("document", mimetype);
        let icon_x: i32 = 6;
        let icon_y = ((cfg.title_bar_h.saturating_sub(icon_size_pt)) / 2).saturating_sub(1);
        emit_icon(
            w,
            N_TITLE_ICON,
            icon,
            icon_x,
            icon_y as i32,
            icon_size_pt,
            Color::TRANSPARENT,
            dc(cfg.chrome_title_color),
        );
        w.node_mut(N_TITLE_ICON).next_sibling = N_TITLE_TEXT;
    }

    let title_glyphs = shape_chrome_text(cfg, title_label);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
    let clock_glyphs = shape_chrome_text(cfg, clock_text);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Document text glyphs — read pre-shaped from layout engine (B).
    let line_glyph_refs = if let Some(header) = crate::read_layout_header() {
        super::push_layout_results_to_scene(w, &header)
    } else {
        alloc::vec::Vec::new()
    };

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

    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).child_offset_x = 0.0;
    w.node_mut(N_DOC_TEXT).child_offset_y = -scene::mpt_to_f32(scroll_y);
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

    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_dirty(N_DOC_TEXT);

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
            text_area_h,
            scroll_pt,
        );
    }
}

// ── Rich text scene building (reads from layout engine B) ───────────

/// Allocate per-segment Glyphs nodes for rich text using B's pre-shaped
/// VisibleRun data. Each run becomes one Glyphs node, with optional
/// underline/strikethrough decoration nodes.
fn allocate_rich_line_nodes_from_b(
    w: &mut scene::SceneWriter<'_>,
    header: &protocol::layout::LayoutResultsHeader,
    doc_width: u32,
) -> u16 {
    w.node_mut(N_DOC_TEXT).first_child = NULL;
    let mut prev_node: u16 = NULL;

    for i in 0..header.visible_run_count as usize {
        let run = crate::read_visible_run(header, i);
        let glyphs = crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);
        let glyph_ref = w.push_shaped_glyphs(glyphs);
        let glyph_count = run.glyph_count;

        let color = Color::rgba(
            ((run.color_rgba >> 24) & 0xFF) as u8,
            ((run.color_rgba >> 16) & 0xFF) as u8,
            ((run.color_rgba >> 8) & 0xFF) as u8,
            (run.color_rgba & 0xFF) as u8,
        );

        // Use B's font_size; fall back to style registry lookup.
        let font_size = if run.font_size > 0 {
            run.font_size
        } else {
            12 // fallback
        };

        if let Some(node_id) = w.alloc_node() {
            let n = w.node_mut(node_id);
            n.x = run.x_mpt;
            n.y = scene::pt(run.y_pt);
            n.width = scene::upt(doc_width);
            n.height = scene::upt(header.line_height_pt);
            n.content = Content::Glyphs {
                color,
                glyphs: glyph_ref,
                glyph_count,
                font_size,
                style_id: run.style_id,
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

        // Compute pen advance for decorations.
        let seg_width_mpt: i32 = glyphs.iter().map(|g| g.x_advance >> 6).sum();
        let seg_width_pt = (seg_width_mpt >> 10).max(0) as u32;

        // Underline decoration.
        if run.flags & piecetable::FLAG_UNDERLINE != 0 && seg_width_pt > 0 {
            let underline_y = run.y_pt + (font_size as i32 * 9 / 10);
            let thickness = (font_size as u32 / 14).max(1);
            if let Some(ul_id) = w.alloc_node() {
                let n = w.node_mut(ul_id);
                n.x = run.x_mpt;
                n.y = scene::pt(underline_y);
                n.width = scene::upt(seg_width_pt + 1);
                n.height = scene::upt(thickness);
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

        // Strikethrough decoration.
        if run.flags & piecetable::FLAG_STRIKETHROUGH != 0 && seg_width_pt > 0 {
            let strike_y = run.y_pt + (font_size as i32 * 5 / 10);
            let thickness = (font_size as u32 / 14).max(1);
            if let Some(st_id) = w.alloc_node() {
                let n = w.node_mut(st_id);
                n.x = run.x_mpt;
                n.y = scene::pt(strike_y);
                n.width = scene::upt(seg_width_pt + 1);
                n.height = scene::upt(thickness);
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
    }

    prev_node
}

/// Compute cursor position for rich text using B's layout results.
fn rich_cursor_position_from_b(
    header: &protocol::layout::LayoutResultsHeader,
    cursor_pos: u32,
    cached_lines: &[protocol::layout::LineInfo],
) -> RichCursorInfo {
    let default_height = 18u32;

    // Find the cursor's line.
    let line_idx = super::line_info_byte_to_line(cached_lines, cursor_pos as usize);

    if line_idx < cached_lines.len() {
        let li = &cached_lines[line_idx];

        // Find cursor x by walking VisibleRuns on this line.
        let mut cursor_x_mpt: i32 = 0;
        let mut cursor_color: u32 = 0x202020FF;
        let mut cursor_font_size: u16 = 14;

        for ri in 0..header.visible_run_count as usize {
            let run = crate::read_visible_run(header, ri);
            if run.y_pt != li.y_pt {
                continue;
            }
            let run_start = run.byte_offset as usize;
            let run_end = run_start + run.byte_length as usize;

            if (cursor_pos as usize) < run_start {
                cursor_x_mpt = run.x_mpt;
                cursor_color = run.color_rgba;
                cursor_font_size = if run.font_size > 0 { run.font_size } else { 14 };
                break;
            }

            if (cursor_pos as usize) >= run_end {
                // Past this run — compute end x.
                let glyphs =
                    crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);
                cursor_x_mpt = run.x_mpt;
                for g in glyphs {
                    cursor_x_mpt += g.x_advance >> 6;
                }
                cursor_color = run.color_rgba;
                cursor_font_size = if run.font_size > 0 { run.font_size } else { 14 };
                continue;
            }

            // Cursor is within this run.
            let doc = crate::doc_text_for_range(run_start, run_end);
            let glyphs =
                crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);
            cursor_x_mpt = run.x_mpt;
            let mut byte_pos = run_start;
            let mut gi = 0usize;
            let s = core::str::from_utf8(doc).unwrap_or("");
            for ch in s.chars() {
                if byte_pos >= cursor_pos as usize {
                    break;
                }
                if gi < glyphs.len() {
                    cursor_x_mpt += glyphs[gi].x_advance >> 6;
                    gi += 1;
                }
                byte_pos += ch.len_utf8();
            }
            cursor_color = run.color_rgba;
            cursor_font_size = if run.font_size > 0 { run.font_size } else { 14 };
            break;
        }

        // Cursor height = cap height approximation from font size.
        let cap_h_pt = cursor_font_size as f32 * 0.7;
        let mpt = scene::MPT_PER_PT as f32;
        let cursor_y_f = li.y_pt as f32 + (li.line_height_pt as f32 * 0.15);
        let cursor_y = (cursor_y_f * mpt) as scene::Mpt;
        let cursor_h_mpt = (cap_h_pt * mpt) as scene::Umpt;

        let c = cursor_color;
        return RichCursorInfo {
            x: cursor_x_mpt,
            y: cursor_y,
            height: cursor_h_mpt.max(scene::upt(2)),
            style_weight: 400,
            color: [
                ((c >> 24) & 0xFF) as u8,
                ((c >> 16) & 0xFF) as u8,
                ((c >> 8) & 0xFF) as u8,
                (c & 0xFF) as u8,
            ],
            caret_skew: 0.0,
        };
    }

    // Past end — position on trailing line.
    let last_y = cached_lines
        .last()
        .map_or(0, |l| l.y_pt + l.line_height_pt as i32);
    let mpt = scene::MPT_PER_PT as f32;
    RichCursorInfo {
        x: 0,
        y: (last_y as f32 * mpt) as scene::Mpt,
        height: scene::upt(default_height),
        style_weight: 400,
        color: [32, 32, 32, 255],
        caret_skew: 0.0,
    }
}

/// Cursor metrics for rich text.
struct RichCursorInfo {
    x: scene::Mpt,
    y: scene::Mpt,
    height: scene::Umpt,
    style_weight: u16,
    color: [u8; 4],
    caret_skew: f32,
}

/// Build or rebuild document content for a text/rich document.
/// Reads all layout data from B's shared memory — no local computation.
#[allow(clippy::too_many_arguments)]
pub fn build_rich_document_content(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    _pt_buf: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    title_label: &[u8],
    clock_text: &[u8],
    scroll_y: scene::Mpt,
    mark_clock_changed: bool,
    cursor_opacity: u8,
    active_space: u8,
) {
    let doc_width = doc_width(cfg);
    let page_padding = cfg.text_inset_x;
    let text_area_h = cfg.page_height.saturating_sub(2 * page_padding);
    let scroll_pt = scroll_y >> 10;

    // Remove old dynamic nodes.
    w.set_node_count(WELL_KNOWN_COUNT);
    w.reset_data();

    // ── Style registry ───────────────────────────────────────────────
    let (_mono_style_id, sans_style_id) = if let Some(header) = crate::read_layout_header() {
        let registry_bytes = crate::read_layout_style_registry(&header);
        if !registry_bytes.is_empty() {
            let _registry_ref = w.push_data(registry_bytes);
        }
        (0u32, 1u32)
    } else {
        let (style_table, mono_id, sans_id) = super::base_style_table(cfg);
        super::write_style_registry(w, &style_table);
        (mono_id, sans_id)
    };

    // Re-push title icon path data.
    {
        let icon_size_pt = cfg.line_height + 2;
        let mimetype = if active_space != 0 {
            Some("image/png")
        } else {
            Some("text/rich")
        };
        let icon = icon_lib::get("document", mimetype);
        let icon_x: i32 = 6;
        let icon_y = ((cfg.title_bar_h.saturating_sub(icon_size_pt)) / 2).saturating_sub(1);
        emit_icon(
            w,
            N_TITLE_ICON,
            icon,
            icon_x,
            icon_y as i32,
            icon_size_pt,
            Color::TRANSPARENT,
            dc(cfg.chrome_title_color),
        );
        w.node_mut(N_TITLE_ICON).next_sibling = N_TITLE_TEXT;
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

    // Set up N_DOC_TEXT.
    w.node_mut(N_DOC_TEXT).next_sibling = NULL;
    w.node_mut(N_DOC_TEXT).child_offset_x = 0.0;
    w.node_mut(N_DOC_TEXT).child_offset_y = -scene::mpt_to_f32(scroll_y);
    w.node_mut(N_DOC_TEXT).content = Content::None;
    w.node_mut(N_DOC_TEXT).content_hash = 0;

    // Read layout results from B and allocate glyph nodes.
    let header_opt = crate::read_layout_header();
    let prev_line_node = if let Some(ref header) = header_opt {
        allocate_rich_line_nodes_from_b(w, header, doc_width)
    } else {
        NULL
    };

    // Link cursor after line nodes.
    if prev_line_node == NULL {
        w.node_mut(N_DOC_TEXT).first_child = N_CURSOR;
    } else {
        w.node_mut(prev_line_node).next_sibling = N_CURSOR;
    }

    w.mark_dirty(N_DOC_TEXT);

    // Cursor positioning using B's layout results.
    let cached_lines = crate::cached_line_info();
    let cursor_info = if let Some(ref header) = header_opt {
        rich_cursor_position_from_b(header, cursor_pos, cached_lines)
    } else {
        RichCursorInfo {
            x: 0,
            y: 0,
            height: scene::upt(18),
            style_weight: 400,
            color: [32, 32, 32, 255],
            caret_skew: 0.0,
        }
    };

    {
        let cursor_w = if cursor_info.style_weight >= 600 {
            3u32
        } else {
            2u32
        };
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_info.x;
        n.y = cursor_info.y;
        n.width = scene::upt(cursor_w);
        n.height = cursor_info.height;
        n.background = Color::rgba(
            cursor_info.color[0],
            cursor_info.color[1],
            cursor_info.color[2],
            cursor_info.color[3],
        );
        n.opacity = cursor_opacity;
        n.content = Content::None;
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
        n.transform = if cursor_info.caret_skew != 0.0 {
            scene::AffineTransform::skew_x(cursor_info.caret_skew)
        } else {
            scene::AffineTransform::identity()
        };
    }
    w.mark_dirty(N_CURSOR);

    // Selection rendering using B's layout results.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };

    if sel_lo < sel_hi {
        if let Some(ref header) = header_opt {
            allocate_rich_selection_rects(
                w,
                header,
                cached_lines,
                sel_lo,
                sel_hi,
                dc(cfg.sel_color),
                scroll_pt,
                text_area_h as i32,
            );
        }
    }
}

/// Allocate selection rects for rich text using B's layout results.
#[allow(clippy::too_many_arguments)]
fn allocate_rich_selection_rects(
    w: &mut scene::SceneWriter<'_>,
    header: &protocol::layout::LayoutResultsHeader,
    lines: &[protocol::layout::LineInfo],
    sel_lo: usize,
    sel_hi: usize,
    sel_color: Color,
    scroll_pt: i32,
    viewport_h: i32,
) {
    let mut prev_sel_node: u16 = NULL;

    for li in lines {
        let line_start = li.byte_offset as usize;
        let line_end = line_start + li.byte_length as usize;

        // Skip lines outside the selection.
        if line_end <= sel_lo || line_start >= sel_hi {
            continue;
        }

        // Visibility culling.
        let line_bottom = li.y_pt + li.line_height_pt as i32;
        if line_bottom <= scroll_pt || li.y_pt >= scroll_pt + viewport_h {
            continue;
        }

        let clamp_lo = sel_lo.max(line_start);
        let clamp_hi = sel_hi.min(line_end);

        // Walk runs on this line to find x_start and x_end.
        let mut x_start_mpt: i32 = 0;
        let mut x_end_mpt: i32 = 0;
        let mut found_start = false;

        for ri in 0..header.visible_run_count as usize {
            let run = crate::read_visible_run(header, ri);
            if run.y_pt != li.y_pt {
                continue;
            }
            let run_start = run.byte_offset as usize;
            let run_end = run_start + run.byte_length as usize;

            if run_end <= clamp_lo || run_start >= clamp_hi {
                // Run is outside the selection range on this line.
                let glyphs =
                    crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);
                let _ = glyphs; // just need to advance past
                continue;
            }

            let doc = crate::doc_text_for_range(run_start, run_end);
            let glyphs =
                crate::read_glyph_data(header, run.glyph_data_offset, run.glyph_count);

            let s = core::str::from_utf8(doc).unwrap_or("");
            let mut byte_pos = run_start;
            let mut pen_x = run.x_mpt;
            let mut gi = 0usize;

            for ch in s.chars() {
                if byte_pos == clamp_lo {
                    x_start_mpt = pen_x;
                    found_start = true;
                }
                let adv = if gi < glyphs.len() {
                    let a = glyphs[gi].x_advance >> 6;
                    gi += 1;
                    a
                } else {
                    0
                };
                byte_pos += ch.len_utf8();
                pen_x += adv;
                if byte_pos >= clamp_hi {
                    x_end_mpt = pen_x;
                    break;
                }
            }
            if byte_pos >= clamp_hi {
                break;
            }
        }

        if found_start && x_end_mpt <= x_start_mpt {
            // Extend to end of line content.
            continue;
        }
        if !found_start {
            continue;
        }

        let rect_w_mpt = x_end_mpt - x_start_mpt;
        let rect_w_pt = ((rect_w_mpt >> 10) + 1).max(1) as u32;

        if let Some(sel_id) = w.alloc_node() {
            let n = w.node_mut(sel_id);
            n.x = x_start_mpt;
            n.y = scene::pt(li.y_pt);
            n.width = scene::upt(rect_w_pt);
            n.height = scene::upt(li.line_height_pt);
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

/// Hit-test: map (x_pt, y_pt) in document coordinates to a byte offset.
/// Uses B's pre-computed LineInfo and VisibleRun glyph data.
pub(crate) fn rich_xy_to_byte(x_pt: f32, y_pt: f32) -> usize {
    let cached_lines = crate::cached_line_info();
    if cached_lines.is_empty() {
        return 0;
    }

    // Find which line the y coordinate falls on.
    let mut target_line = None;
    for (i, li) in cached_lines.iter().enumerate() {
        let line_bottom = li.y_pt + li.line_height_pt as i32;
        if (y_pt as i32) < line_bottom {
            target_line = Some(i);
            break;
        }
    }

    let Some(line_idx) = target_line else {
        // Past all lines — return end of document.
        let last = cached_lines.last().unwrap();
        return (last.byte_offset + last.byte_length as u32) as usize;
    };

    let li = &cached_lines[line_idx];
    let target_x_mpt = (x_pt * scene::MPT_PER_PT as f32) as i32;

    super::line_info_x_to_byte(cached_lines, line_idx, target_x_mpt)
}
