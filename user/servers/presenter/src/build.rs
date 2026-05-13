//! Scene building — compiles document state + layout into a scene graph tree.

use scene::{Color, Content, FillRule, NodeFlags, SCENE_SIZE, SceneWriter, ShapedGlyph, pt, upt};

use super::{
    MAX_GLYPHS_PER_LINE, STYLE_MONO, STYLE_SANS, font, font_data_for_style, pack_run_style_id,
    parse_layout_header, parse_line_at, parse_visible_run_at, read_rtc_seconds, scale_icon_paths,
    shape_text,
};

fn build_font_axes(weight: u16, font_size: u16) -> ([fonts::metrics::AxisValue; 2], usize) {
    let mut axes = [fonts::metrics::AxisValue {
        tag: [0; 4],
        value: 0.0,
    }; 2];
    let mut count = 0;

    if weight != 400 {
        axes[count] = fonts::metrics::AxisValue {
            tag: *b"wght",
            value: weight as f32,
        };
        count += 1;
    }

    axes[count] = fonts::metrics::AxisValue {
        tag: *b"opsz",
        value: font_size as f32,
    };
    count += 1;

    (axes, count)
}

fn aspect_fit(src_w: u32, src_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (0, 0);
    }

    let w_scaled_h = max_w as u64 * src_h as u64;
    let h_scaled_w = max_h as u64 * src_w as u64;

    if w_scaled_h <= h_scaled_w {
        let h = (max_w as u64 * src_h as u64 / src_w as u64) as u32;

        (max_w.min(src_w), h.min(src_h))
    } else {
        let w = (max_h as u64 * src_w as u64 / src_h as u64) as u32;

        (w.min(src_w), max_h.min(src_h))
    }
}

// ── Selection geometry ────────────────────────────────────────────

pub(crate) struct SelectionSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) color: Color,
    pub(crate) char_width_mpt: scene::Mpt,
}

pub(crate) fn build_selection_nodes(
    scene: &mut SceneWriter,
    results_buf: &[u8],
    viewport: scene::NodeId,
    line_count: usize,
    sel: &SelectionSpan,
) {
    let line_height = presenter_service::LINE_HEIGHT;

    for i in 0..line_count {
        let line_info = parse_line_at(results_buf, i);
        let line_byte_start = line_info.byte_offset as usize;
        let line_byte_end = line_byte_start + line_info.byte_length as usize;

        if sel.end <= line_byte_start || sel.start >= line_byte_end {
            continue;
        }

        let col_start = sel.start.saturating_sub(line_byte_start);
        let col_end = if sel.end < line_byte_end {
            sel.end - line_byte_start
        } else {
            line_info.byte_length as usize
        };

        if col_start >= col_end {
            continue;
        }

        let x_mpt = col_start as i32 * sel.char_width_mpt;
        let w_mpt = (col_end - col_start) as i32 * sel.char_width_mpt;
        let sel_node = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(sel_node);

            n.x = x_mpt;
            n.y = pt(line_info.y);
            n.width = w_mpt as u32;
            n.height = upt(line_height);
            n.background = sel.color;
            n.role = scene::ROLE_SELECTION;
        }

        scene.add_child(viewport, sel_node);
    }
}

// ── Rich text selection ──────────────────────────────────────────

fn byte_to_x_rich(
    doc_buf: &[u8],
    results_buf: &[u8],
    run_count: usize,
    byte_pos: usize,
    target_line: usize,
) -> f32 {
    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);

        if vr.line_index as usize != target_line {
            continue;
        }

        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        if byte_pos < run_start {
            return vr.x;
        }
        if byte_pos > run_end {
            continue;
        }

        let font_data = font_data_for_style(vr.font_family, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, ac) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..ac];
        let text_len = piecetable::text_len(doc_buf) as usize;
        let extract_end = byte_pos.min(text_len);
        let extract_start = run_start.min(extract_end);
        let mut buf = alloc::vec![0u8; extract_end - extract_start + 1];
        let copied =
            piecetable::text_slice(doc_buf, extract_start as u32, extract_end as u32, &mut buf);
        let mut x = vr.x;

        for ch in core::str::from_utf8(&buf[..copied]).unwrap_or("").chars() {
            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let adv = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);

            x += (adv as f32 * vr.font_size as f32) / upem as f32;
        }

        return x;
    }

    0.0
}

#[allow(clippy::too_many_arguments)]
fn build_rich_selection_nodes(
    scene: &mut SceneWriter,
    results_buf: &[u8],
    viewport: scene::NodeId,
    header: &layout_service::LayoutHeader,
    sel_start: usize,
    sel_end: usize,
    color: Color,
    doc_va: usize,
) {
    let line_count = header.line_count as usize;
    let run_count = header.visible_run_count as usize;
    // SAFETY: doc_va is a valid RO mapping.
    let (content_len, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            content_len,
        )
    };

    if !piecetable::validate(doc_buf) {
        return;
    }

    for i in 0..line_count {
        let line = parse_line_at(results_buf, i);
        let line_start = line.byte_offset as usize;
        let line_end = line_start + line.byte_length as usize;

        if sel_end <= line_start || sel_start >= line_end {
            continue;
        }

        let x_start = if sel_start <= line_start {
            0.0
        } else {
            byte_to_x_rich(doc_buf, results_buf, run_count, sel_start, i)
        };
        let x_end = if sel_end >= line_end {
            byte_to_x_rich(doc_buf, results_buf, run_count, line_end, i)
        } else {
            byte_to_x_rich(doc_buf, results_buf, run_count, sel_end, i)
        };
        let w = x_end - x_start;

        if w <= 0.0 {
            continue;
        }

        // Use the line's actual height from its font sizes.
        let mut max_font = 14u16;

        for run_i in 0..run_count {
            let vr = parse_visible_run_at(results_buf, run_i);

            if vr.line_index as usize == i && vr.font_size > max_font {
                max_font = vr.font_size;
            }
        }

        let line_h = (max_font as u32 * 14) / 10;

        if let Some(sel_node) = scene.alloc_node() {
            let n = scene.node_mut(sel_node);

            n.x = scene::f32_to_mpt(x_start);
            n.y = pt(line.y);
            n.width = scene::f32_to_mpt(w) as u32;
            n.height = upt(line_h);
            n.background = color;
            n.role = scene::ROLE_SELECTION;

            scene.add_child(viewport, sel_node);
        }
    }
}

// ── Rich text hit testing ────────────────────────────────────────

pub(crate) fn xy_to_byte_rich(
    results_buf: &[u8],
    header: &layout_service::LayoutHeader,
    rel_x: f32,
    rel_y: i32,
    doc_va: usize,
    content_len: usize,
) -> usize {
    let line_count = header.line_count as usize;
    let run_count = header.visible_run_count as usize;

    if line_count == 0 || run_count == 0 {
        return 0;
    }

    // Find the target line by y position using actual line positions.
    let mut target_line = line_count - 1;

    for i in 0..line_count {
        let next_y = if i + 1 < line_count {
            parse_line_at(results_buf, i + 1).y
        } else {
            i32::MAX
        };

        if rel_y < next_y {
            target_line = i;

            break;
        }
    }

    // SAFETY: doc_va is a valid RO mapping.
    let (cl, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            cl,
        )
    };

    if !piecetable::validate(doc_buf) {
        return 0;
    }

    // Find the run on target_line closest to rel_x, then measure glyphs.
    let mut best_pos = parse_line_at(results_buf, target_line).byte_offset as usize;

    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);

        if vr.line_index as usize != target_line {
            continue;
        }

        let font_data = font_data_for_style(vr.font_family, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, axis_count) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..axis_count];
        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        // If click is before this run's start, snap to run start.
        if rel_x < vr.x {
            best_pos = run_start;

            break;
        }

        // Walk glyphs in this run to find the character boundary.
        let text_len = piecetable::text_len(doc_buf) as usize;
        let extract_end = run_end.min(text_len);
        let extract_len = extract_end.saturating_sub(run_start);
        let mut text_buf = alloc::vec![0u8; extract_len + 1];
        let copied =
            piecetable::text_slice(doc_buf, run_start as u32, extract_end as u32, &mut text_buf);
        let mut x = vr.x;
        let mut byte_offset = run_start;

        for ch in core::str::from_utf8(&text_buf[..copied])
            .unwrap_or("")
            .chars()
        {
            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);
            let advance_pt = (advance_fu as f32 * vr.font_size as f32) / upem as f32;

            if rel_x < x + advance_pt * 0.5 {
                return byte_offset.min(content_len);
            }

            x += advance_pt;
            byte_offset += ch.len_utf8();
        }

        best_pos = byte_offset;
    }

    best_pos.min(content_len)
}

// ── Rich text cursor ─────────────────────────────────────────────

pub(crate) struct RichCursorInfo {
    pub(crate) x: f32,
    pub(crate) y: i32,
    pub(crate) height: u32,
    pub(crate) color_rgba: u32,
    pub(crate) weight: u16,
    pub(crate) caret_skew: f32,
}

pub(crate) fn compute_rich_cursor(
    results_buf: &[u8],
    layout_header: &layout_service::LayoutHeader,
    cursor_pos: usize,
    doc_va: usize,
) -> RichCursorInfo {
    const FALLBACK_COLOR: u32 = 0x20_20_20_FF;

    let run_count = layout_header.visible_run_count as usize;

    if run_count == 0 {
        return RichCursorInfo {
            x: 0.0,
            y: 0,
            height: 20,
            color_rgba: FALLBACK_COLOR,
            weight: 400,
            caret_skew: 0.0,
        };
    }

    // SAFETY: doc_va is a valid RO mapping.
    let (content_len, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            content_len,
        )
    };

    if !piecetable::validate(doc_buf) {
        return RichCursorInfo {
            x: 0.0,
            y: 0,
            height: 20,
            color_rgba: FALLBACK_COLOR,
            weight: 400,
            caret_skew: 0.0,
        };
    }

    // Find the run containing cursor_pos and compute x offset within it.
    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);
        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        if cursor_pos < run_start || cursor_pos > run_end {
            continue;
        }

        let font_data = font_data_for_style(vr.font_family, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, axis_count) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..axis_count];
        // Extract run text from piecetable.
        let text_len = piecetable::text_len(doc_buf) as usize;
        let chars_before = cursor_pos - run_start;
        let extract_len = chars_before.min(text_len.saturating_sub(run_start));
        let mut text_buf = alloc::vec![0u8; extract_len + 1];
        let copied =
            piecetable::text_slice(doc_buf, run_start as u32, cursor_pos as u32, &mut text_buf);
        let mut x = vr.x;

        for ch in core::str::from_utf8(&text_buf[..copied])
            .unwrap_or("")
            .chars()
        {
            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);

            x += (advance_fu as f32 * vr.font_size as f32) / upem as f32;
        }

        let line_height = (vr.font_size as u32 * 14) / 10;

        return RichCursorInfo {
            x,
            y: vr.y,
            height: line_height,
            color_rgba: vr.color_rgba,
            weight: vr.weight,
            caret_skew: fonts::metrics::caret_skew(font_data),
        };
    }

    // No run contains the cursor — find the line from LineInfo.
    let line_count = layout_header.line_count as usize;

    for i in 0..line_count {
        let line = parse_line_at(results_buf, i);
        let line_start = line.byte_offset as usize;
        let line_end = line_start + line.byte_length as usize;

        if cursor_pos >= line_start && cursor_pos <= line_end {
            let default_h = 20u32;
            let mut line_h = default_h;

            for run_i in 0..run_count {
                let vr = parse_visible_run_at(results_buf, run_i);

                if vr.line_index as usize == i {
                    line_h = (vr.font_size as u32 * 14) / 10;

                    break;
                }
            }

            // For empty lines, use the previous line's font size.
            if line_h == default_h && i > 0 {
                for run_i in (0..run_count).rev() {
                    let vr = parse_visible_run_at(results_buf, run_i);

                    if vr.line_index as usize == i - 1 {
                        line_h = (vr.font_size as u32 * 14) / 10;

                        break;
                    }
                }
            }

            return RichCursorInfo {
                x: 0.0,
                y: line.y,
                height: line_h,
                color_rgba: FALLBACK_COLOR,
                weight: 400,
                caret_skew: 0.0,
            };
        }
    }

    // Past all lines — position after last line.
    if line_count > 0 {
        let last = parse_line_at(results_buf, line_count - 1);
        let last_h = if run_count > 0 {
            let vr = parse_visible_run_at(results_buf, run_count - 1);

            (vr.font_size as u32 * 14) / 10
        } else {
            20
        };

        return RichCursorInfo {
            x: 0.0,
            y: last.y + last_h as i32,
            height: last_h,
            color_rgba: FALLBACK_COLOR,
            weight: 400,
            caret_skew: 0.0,
        };
    }

    RichCursorInfo {
        x: 0.0,
        y: 0,
        height: 20,
        color_rgba: FALLBACK_COLOR,
        weight: 400,
        caret_skew: 0.0,
    }
}

// ── Rich text rendering ──────────────────────────────────────────

fn build_rich_text_nodes(
    scene: &mut SceneWriter,
    viewport: scene::NodeId,
    layout_header: &layout_service::LayoutHeader,
    results_buf: &[u8],
    doc_va: usize,
    display_width: u32,
    glyphs: &mut [ShapedGlyph; MAX_GLYPHS_PER_LINE],
) {
    let run_count = layout_header.visible_run_count as usize;

    if run_count == 0 {
        return;
    }

    // SAFETY: doc_va is a valid RO mapping of the document buffer.
    let (content_len, _, _, _) = unsafe { document_service::read_doc_header(doc_va) };
    let doc_buf = unsafe {
        core::slice::from_raw_parts(
            (doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
            content_len,
        )
    };

    if !piecetable::validate(doc_buf) {
        return;
    }

    let text_len = piecetable::text_len(doc_buf) as usize;
    let mut text_scratch = alloc::vec![0u8; text_len + 1];
    let copied = piecetable::text_slice(doc_buf, 0, text_len as u32, &mut text_scratch);

    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);
        let byte_start = vr.byte_offset as usize;
        let byte_len = vr.byte_length as usize;

        if byte_start + byte_len > copied || byte_len == 0 {
            continue;
        }

        let run_text = &text_scratch[byte_start..byte_start + byte_len];
        let font_data = font_data_for_style(vr.font_family, vr.flags);
        let sid = pack_run_style_id(vr.font_family, vr.weight, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, axis_count) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..axis_count];
        let mut glyph_count = 0usize;

        for ch in core::str::from_utf8(run_text).unwrap_or("").chars() {
            if glyph_count >= MAX_GLYPHS_PER_LINE {
                break;
            }

            let gid = fonts::metrics::glyph_id_for_char(font_data, ch).unwrap_or(0);
            let advance_fu = fonts::metrics::glyph_h_advance_with_axes(font_data, gid, axes)
                .unwrap_or(upem as i32 / 2);
            let advance_fp = (advance_fu as i64 * vr.font_size as i64 * 65536 / upem as i64) as i32;

            glyphs[glyph_count] = ShapedGlyph {
                glyph_id: gid,
                _pad: 0,
                x_advance: advance_fp,
                x_offset: 0,
                y_offset: 0,
            };
            glyph_count += 1;
        }

        if glyph_count == 0 {
            continue;
        }

        let glyph_ref = scene.push_shaped_glyphs(&glyphs[..glyph_count]);
        let run_node = match scene.alloc_node() {
            Some(id) => id,
            None => break,
        };
        let color = Color::rgba(
            ((vr.color_rgba >> 24) & 0xFF) as u8,
            ((vr.color_rgba >> 16) & 0xFF) as u8,
            ((vr.color_rgba >> 8) & 0xFF) as u8,
            (vr.color_rgba & 0xFF) as u8,
        );
        let line_height = (vr.font_size as u32 * 14) / 10;

        {
            let n = scene.node_mut(run_node);

            n.x = scene::f32_to_mpt(vr.x);
            n.y = pt(vr.y);
            n.width = upt(display_width);
            n.height = upt(line_height);
            n.content = Content::Glyphs {
                color,
                glyphs: glyph_ref,
                glyph_count: glyph_count as u16,
                font_size: vr.font_size,
                style_id: sid,
            };
            n.role = scene::ROLE_PARAGRAPH;
        }

        scene.add_child(viewport, run_node);

        if vr.flags & piecetable::FLAG_UNDERLINE != 0 {
            let run_width: f32 = glyphs[..glyph_count]
                .iter()
                .map(|g| (g.x_advance as f32) / 65536.0)
                .sum();
            let baseline_y = vr.y + (vr.font_size as i32 * 11) / 10;
            let thickness = (vr.font_size as u32 / 14).max(1);

            if let Some(ul_id) = scene.alloc_node() {
                let n = scene.node_mut(ul_id);

                n.x = scene::f32_to_mpt(vr.x);
                n.y = pt(baseline_y);
                n.width = upt(run_width as u32 + 1);
                n.height = upt(thickness);
                n.background = color;

                scene.add_child(viewport, ul_id);
            }
        }
    }
}

// ── Showcase scene (last space) ─────────────────────────────────

fn build_showcase_nodes(
    scene: &mut SceneWriter,
    strip: scene::NodeId,
    display_width: u32,
    content_h: u32,
    now_ns: u64,
    has_audio: bool,
    base_x: i32,
) {
    let container = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };

    {
        let n = scene.node_mut(container);

        n.x = pt(base_x);
        n.width = upt(display_width);
        n.height = upt(content_h);
        n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
    }

    scene.add_child(strip, container);

    // Outer group — translated container with a subtle conical gradient bg.
    let group = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };

    {
        let g = scene.node_mut(group);

        g.x = pt(0);
        g.y = pt(0);
        g.width = upt(400);
        g.height = upt(400);
        g.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
        g.child_offset_x = scene::pt(48);
        g.child_offset_y = scene::pt(48);
        g.content = Content::Gradient {
            color_start: Color::rgba(255, 255, 255, 10),
            color_end: Color::rgba(255, 255, 255, 0),
            kind: scene::GradientKind::Radial,
            _pad: 48,
            angle_fp: scene::angle_to_fp(0.0),
        };
        g.transform = scene::AffineTransform::translate(20.0, 20.0);
    }

    scene.add_child(container, group);

    if let Some(id) = scene.alloc_node() {
        let a = scene.node_mut(id);

        a.x = pt(100);
        a.y = pt(60);
        a.width = upt(120);
        a.height = upt(120);
        a.background = Color::rgb(255, 0, 0);

        scene.add_child(group, id);
    }
    if let Some(id) = scene.alloc_node() {
        let b = scene.node_mut(id);

        b.x = pt(60);
        b.y = pt(100);
        b.width = upt(120);
        b.height = upt(120);
        b.background = Color::rgb(0, 255, 0);
        b.shadow_color = Color::rgba(0, 0, 0, 180);
        b.shadow_blur_radius = 16;
        b.shadow_spread = 4;
        b.corner_radius = 60;
        b.opacity = 128;
        b.transform = scene::AffineTransform::scale(0.8, 0.8);

        scene.add_child(group, id);
    }
    if let Some(id) = scene.alloc_node() {
        let c = scene.node_mut(id);

        c.x = pt(140);
        c.y = pt(140);
        c.width = upt(120);
        c.height = upt(120);
        c.background = Color::rgba(0, 0, 255, 128);
        c.backdrop_blur_radius = 64;
        c.shadow_color = Color::rgba(0, 0, 0, 180);
        c.shadow_blur_radius = 16;
        c.shadow_spread = 4;
        c.corner_radius = 16;
        c.opacity = 240;
        c.transform = scene::AffineTransform::rotate(0.3);

        scene.add_child(group, id);
    }

    // Star filled with conical gradient.
    {
        let mut path_buf = alloc::vec::Vec::new();
        let cx = 100.0f32;
        let cy = 100.0;
        let outer = 90.0;
        let inner = 38.0;

        const STAR_CS: [(f32, f32); 10] = [
            (0.0, -1.0),
            (0.5878, -0.8090),
            (0.9511, -0.3090),
            (0.9511, 0.3090),
            (0.5878, 0.8090),
            (0.0, 1.0),
            (-0.5878, 0.8090),
            (-0.9511, 0.3090),
            (-0.9511, -0.3090),
            (-0.5878, -0.8090),
        ];

        for (i, &(sin_a, cos_a)) in STAR_CS.iter().enumerate() {
            let r = if i % 2 == 0 { outer } else { inner };
            let px = cx + r * sin_a;
            let py = cy + r * cos_a;

            if i == 0 {
                scene::path_move_to(&mut path_buf, px, py);
            } else {
                scene::path_line_to(&mut path_buf, px, py);
            }
        }

        scene::path_close(&mut path_buf);

        let path_ref = scene.push_data(&path_buf);

        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(80);
            n.y = pt(300);
            n.width = upt(200);
            n.height = upt(200);
            n.content = Content::GradientPath {
                color_start: Color::rgb(241, 196, 15),
                color_end: Color::rgb(231, 76, 60),
                kind: scene::GradientKind::Conical,
                _pad: 0,
                angle_fp: scene::angle_to_fp(4.71239),
                contours: path_ref,
            };

            scene.add_child(group, id);
        }
    }

    // ── Section: Easing sampler ──
    //
    // 5 colored squares travel back and forth horizontally, each with a
    // different easing curve. 2-second ping-pong cycle.
    // Inspired by the Phase 1 demo (commit 9f1b04a5).
    const CYCLE_NS: u64 = 2_000_000_000;
    const FULL_NS: u64 = CYCLE_NS * 2;

    let phase_ns = now_ns % FULL_NS;
    let raw_t = if phase_ns < CYCLE_NS {
        phase_ns as f32 / CYCLE_NS as f32
    } else {
        1.0 - (phase_ns - CYCLE_NS) as f32 / CYCLE_NS as f32
    };
    let travel = 200i32;
    let sq = 16u32;
    let row_h = 22i32;
    let total_w = travel + sq as i32;
    let demo_x = 20 + (400 - total_w) / 2;
    let demo_y = 560i32;
    let easings: [(animation::Easing, Color); 5] = [
        (animation::Easing::Linear, Color::rgb(255, 100, 100)),
        (animation::Easing::EaseOut, Color::rgb(255, 180, 50)),
        (animation::Easing::EaseInOut, Color::rgb(100, 220, 100)),
        (animation::Easing::EaseInBack, Color::rgb(80, 160, 255)),
        (animation::Easing::EaseOutBounce, Color::rgb(220, 80, 220)),
    ];

    for (i, &(easing, color)) in easings.iter().enumerate() {
        let y = demo_y + i as i32 * row_h;

        // Track.
        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(demo_x);
            n.y = pt(y + sq as i32 / 2 - 1);
            n.width = upt(travel as u32 + sq);
            n.height = upt(2);
            n.background = Color::rgba(255, 255, 255, 20);

            scene.add_child(container, id);
        }

        // Square.
        let eased = animation::ease(easing, raw_t);
        let sq_x = demo_x + (eased * travel as f32) as i32;

        if let Some(id) = scene.alloc_node() {
            let n = scene.node_mut(id);

            n.x = pt(sq_x);
            n.y = pt(y);
            n.width = upt(sq);
            n.height = upt(sq);
            n.background = color;
            n.corner_radius = 3;

            scene.add_child(container, id);
        }
    }

    // ── Play button ──
    if has_audio {
        let btn_w = 48u32;
        let btn_h = 48u32;
        let btn_x = (400 - btn_w as i32) / 2 + 20;
        let btn_y = demo_y + easings.len() as i32 * row_h + 24;

        if let Some(btn_id) = scene.alloc_node() {
            let name_ref = scene.push_data(b"Play sound");
            let n = scene.node_mut(btn_id);

            n.x = pt(btn_x);
            n.y = pt(btn_y);
            n.width = upt(btn_w);
            n.height = upt(btn_h);
            n.background = Color::rgba(255, 255, 255, 20);
            n.corner_radius = 24;
            n.role = scene::ROLE_BUTTON;
            n.state = scene::STATE_FOCUSABLE;
            n.cursor_shape = scene::CURSOR_PRESSABLE;
            n.name = name_ref;

            scene.add_child(container, btn_id);

            let icon_data = icons::get("player-play", None);
            let icon_padding = 12u32;
            let icon_size_pt = btn_h.min(btn_w) - icon_padding * 2;
            let (icon_data_ref, icon_hash) = scale_icon_paths(scene, icon_data, icon_size_pt);
            let icon_sw_fixed = super::icon_stroke_width_fixed(
                icon_data.stroke_width.to_bits(),
                icon_size_pt,
                icon_data.viewbox.to_bits(),
            );

            if let Some(icon_id) = scene.alloc_node() {
                let icon = scene.node_mut(icon_id);

                icon.x = pt(icon_padding as i32);
                icon.y = pt(icon_padding as i32);
                icon.width = upt(icon_size_pt);
                icon.height = upt(icon_size_pt);
                icon.content = Content::Path {
                    color: Color::TRANSPARENT,
                    stroke_color: Color::rgba(255, 255, 255, 200),
                    fill_rule: FillRule::Winding,
                    stroke_width: icon_sw_fixed,
                    contours: icon_data_ref,
                };
                icon.content_hash = icon_hash;

                scene.add_child(btn_id, icon_id);
            }
        }
    }
}

// ── Presenter scene methods ──────────────────────────────────────

impl super::Presenter {
    pub(crate) fn visible_space_range(&self) -> (usize, usize) {
        let last = self.spaces.len().saturating_sub(1);

        if self.slide_animating {
            let current = self.slide_spring.value();
            let target = self.slide_spring.target();
            let dw_mpt = pt(self.display_width as i32);
            let lo = (current.min(target) / dw_mpt) as usize;
            let hi_val = current.max(target);
            let hi = if hi_val % dw_mpt != 0 {
                (hi_val / dw_mpt) as usize + 1
            } else {
                (hi_val / dw_mpt) as usize
            };

            (lo.min(last), hi.min(last))
        } else {
            (self.active_space, self.active_space)
        }
    }

    pub(crate) fn build_scene(&mut self) {
        if self.layout_dirty {
            let _ = ipc::client::call_simple(self.layout_ep, layout_service::RECOMPUTE, &[]);
            self.layout_dirty = false;
        }
        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };

        self.results_reader.read(&mut self.results_buf);

        let layout_header = parse_layout_header(&self.results_buf);
        let line_count = layout_header.line_count as usize;
        let is_rich_format = layout_header.format == 1;
        // Compute cursor position and auto-scroll to keep it visible.
        let (cursor_line_idx, cursor_col_in_line) = self.find_cursor_line(cursor_pos, line_count);
        let rich_cursor_info = if is_rich_format {
            let ci =
                compute_rich_cursor(&self.results_buf, &layout_header, cursor_pos, self.doc_va);

            self.ensure_cursor_visible_at(ci.y, ci.height as i32);

            Some(ci)
        } else {
            self.ensure_cursor_visible(cursor_line_idx as u32);

            None
        };

        self.clamp_scroll();
        self.write_viewport();

        // SAFETY: doc_va is valid and content_len comes from read_doc_header.
        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let bg = Color::rgb(
            presenter_service::BG_R,
            presenter_service::BG_G,
            presenter_service::BG_B,
        );
        let text_color = Color::rgb(
            presenter_service::TEXT_R,
            presenter_service::TEXT_G,
            presenter_service::TEXT_B,
        );
        let cursor_color = Color::rgb(
            presenter_service::CURSOR_R,
            presenter_service::CURSOR_G,
            presenter_service::CURSOR_B,
        );
        let sel_color = Color::rgba(
            presenter_service::SEL_R,
            presenter_service::SEL_G,
            presenter_service::SEL_B,
            presenter_service::SEL_A,
        );
        let page_bg = Color::rgb(
            presenter_service::PAGE_BG_R,
            presenter_service::PAGE_BG_G,
            presenter_service::PAGE_BG_B,
        );
        let title_color = Color::rgb(
            presenter_service::CHROME_TITLE_R,
            presenter_service::CHROME_TITLE_G,
            presenter_service::CHROME_TITLE_B,
        );
        let clock_color = Color::rgb(
            presenter_service::CHROME_CLOCK_R,
            presenter_service::CHROME_CLOCK_G,
            presenter_service::CHROME_CLOCK_B,
        );
        let has_selection = sel_anchor != cursor_pos;
        let sel_start = sel_anchor.min(cursor_pos);
        let sel_end = sel_anchor.max(cursor_pos);
        let (vis_lo, vis_hi) = self.visible_space_range();
        let back_idx = 1 - self.read_active_index();
        // SAFETY: scene_bufs[back_idx] is a valid &mut [u8] of SCENE_SIZE
        // bytes. We create a reborrow via raw pointer to split the borrow
        // from self, since SceneWriter only touches this buffer.
        let back_buf = unsafe {
            core::slice::from_raw_parts_mut(self.scene_bufs[back_idx].as_mut_ptr(), SCENE_SIZE)
        };
        let mut scene = SceneWriter::from_existing(back_buf);

        scene.reset();

        let title_bar_h = presenter_service::TITLE_BAR_H;
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let page_padding = presenter_service::PAGE_PADDING;
        let content_h = self.display_height.saturating_sub(title_bar_h);
        let page_h = content_h.saturating_sub(2 * page_margin);
        let page_w = (page_h as u64 * 210 / 297) as u32;
        let page_w = page_w.min(self.display_width.saturating_sub(2 * page_margin));
        let page_x = ((self.display_width - page_w) / 2) as i32;
        let page_y = page_margin as i32;
        let text_area_w = page_w.saturating_sub(2 * page_padding);
        let text_area_h = page_h.saturating_sub(2 * page_padding);
        // Root node — full screen background.
        let root = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(root);

            n.width = upt(self.display_width);
            n.height = upt(self.display_height);
            n.background = bg;
        }

        scene.set_root(root);

        // Document icon — mimetype-aware vector icon in the title bar.
        let icon_size_pt = presenter_service::LINE_HEIGHT + 2;
        let icon_mimetype = self
            .spaces
            .get(self.active_space)
            .and_then(|s| s.mimetype());
        let icon = icons::get("document", icon_mimetype);
        let (icon_data_ref, icon_hash) = scale_icon_paths(&mut scene, icon, icon_size_pt);
        let icon_sw_fixed = super::icon_stroke_width_fixed(
            icon.stroke_width.to_bits(),
            icon_size_pt,
            icon.viewbox.to_bits(),
        );
        let icon_x: i32 = 8;
        let icon_y = ((title_bar_h.saturating_sub(icon_size_pt)) / 2).saturating_sub(1) as i32;

        if let Some(icon_node) = scene.alloc_node() {
            let n = scene.node_mut(icon_node);

            n.x = pt(icon_x);
            n.y = pt(icon_y);
            n.width = upt(icon_size_pt);
            n.height = upt(icon_size_pt);
            n.content = Content::Path {
                color: Color::TRANSPARENT,
                stroke_color: title_color,
                fill_rule: FillRule::Winding,
                stroke_width: icon_sw_fixed,
                contours: icon_data_ref,
            };
            n.content_hash = icon_hash;

            scene.add_child(root, icon_node);
        }

        // Title bar text — "untitled" label, shaped with Inter.
        let title_text_y = (title_bar_h.saturating_sub(presenter_service::LINE_HEIGHT)) / 2;
        let (title_glyphs_count, title_width) = shape_text(
            font(init::FONT_IDX_SANS),
            "untitled",
            presenter_service::FONT_SIZE,
            &[],
            &mut self.glyphs,
        );
        let title_glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..title_glyphs_count]);

        if let Some(title_node) = scene.alloc_node() {
            let n = scene.node_mut(title_node);

            n.x = pt(36);
            n.y = pt(title_text_y as i32);
            n.width = (title_width as u32).saturating_add(upt(1));
            n.height = upt(presenter_service::LINE_HEIGHT);
            n.content = Content::Glyphs {
                color: title_color,
                glyphs: title_glyph_ref,
                glyph_count: title_glyphs_count as u16,
                font_size: presenter_service::FONT_SIZE,
                style_id: STYLE_SANS,
            };
            n.role = scene::ROLE_LABEL;

            scene.add_child(root, title_node);
        }

        // Clock text — right-aligned, HH:MM:SS format.
        // Use PL031 RTC for wall-clock time; fall back to monotonic uptime.
        let clock_secs = {
            let rtc = read_rtc_seconds(self.rtc_va);

            if rtc > 0 {
                rtc % 86400
            } else {
                let ns = abi::system::clock_read().unwrap_or(0);

                (ns / 1_000_000_000) % 86400
            }
        };
        let hours = (clock_secs / 3600) % 24;
        let minutes = (clock_secs / 60) % 60;
        let seconds = clock_secs % 60;
        let clock_chars: [u8; 8] = [
            b'0' + (hours / 10) as u8,
            b'0' + (hours % 10) as u8,
            b':',
            b'0' + (minutes / 10) as u8,
            b'0' + (minutes % 10) as u8,
            b':',
            b'0' + (seconds / 10) as u8,
            b'0' + (seconds % 10) as u8,
        ];

        let mut clock_str = [0u8; 8];

        clock_str.copy_from_slice(&clock_chars);

        let clock_text = core::str::from_utf8(&clock_str).unwrap_or("00:00:00");
        let tnum = fonts::Feature::new(fonts::Tag::new(b"tnum"), 1, ..);
        let (clock_count, clock_width) = shape_text(
            font(init::FONT_IDX_SANS),
            clock_text,
            presenter_service::FONT_SIZE,
            &[tnum],
            &mut self.glyphs,
        );
        let clock_glyph_ref = scene.push_shaped_glyphs(&self.glyphs[..clock_count]);
        let clock_text_w_mpt = (clock_width as u32).saturating_add(upt(1));
        let clock_x =
            self.display_width as i32 - 12 - (clock_text_w_mpt / scene::MPT_PER_PT as u32) as i32;

        if let Some(clock_node) = scene.alloc_node() {
            let n = scene.node_mut(clock_node);

            n.x = pt(clock_x);
            n.y = pt(title_text_y as i32);
            n.width = clock_text_w_mpt;
            n.height = upt(presenter_service::LINE_HEIGHT);
            n.content = Content::Glyphs {
                color: clock_color,
                glyphs: clock_glyph_ref,
                glyph_count: clock_count as u16,
                font_size: presenter_service::FONT_SIZE,
                style_id: STYLE_SANS,
            };
            n.role = scene::ROLE_LABEL;

            scene.add_child(root, clock_node);

            self.clock_node_id = clock_node;
            self.clock_glyph_ref = clock_glyph_ref;
        }

        // Content area — below title bar, clips children.
        let content_area = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(content_area);

            n.y = pt(title_bar_h as i32);
            n.width = upt(self.display_width);
            n.height = upt(content_h);
            n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
        }

        scene.add_child(root, content_area);

        // Strip node — holds all document spaces side by side.
        // child_offset_x slides to reveal the active space.
        let strip = match scene.alloc_node() {
            Some(id) => id,
            None => return,
        };

        {
            let n = scene.node_mut(strip);

            n.width = upt(self.display_width * self.spaces.len() as u32);
            n.height = upt(content_h);
            n.child_offset_x = -self.slide_spring.value();
        }

        scene.add_child(content_area, strip);

        // Render only visible spaces into the strip.
        let now_ns = abi::system::clock_read().unwrap_or(0);
        let has_audio = self.audio_ep.0 != 0;

        for space_idx in vis_lo..=vis_hi {
            let base_x = (self.display_width * space_idx as u32) as i32;

            match &self.spaces[space_idx] {
                super::Space::Text => {
                    let page = match scene.alloc_node() {
                        Some(id) => id,
                        None => break,
                    };

                    {
                        let n = scene.node_mut(page);

                        n.x = pt(base_x + page_x);
                        n.y = pt(page_y);
                        n.width = upt(page_w);
                        n.height = upt(page_h);
                        n.background = page_bg;
                        n.shadow_color = Color::rgba(0, 0, 0, 255);
                        n.shadow_blur_radius = presenter_service::SHADOW_BLUR_RADIUS;
                        n.shadow_spread = presenter_service::SHADOW_SPREAD;
                        n.cursor_shape = scene::CURSOR_TEXT;
                    }

                    scene.add_child(strip, page);

                    let viewport = match scene.alloc_node() {
                        Some(id) => id,
                        None => break,
                    };

                    {
                        let n = scene.node_mut(viewport);

                        n.x = pt(page_padding as i32);
                        n.y = pt(page_padding as i32);
                        n.width = upt(text_area_w);
                        n.height = upt(text_area_h);
                        n.flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
                        n.child_offset_y = -pt(self.scroll_y);
                        n.role = scene::ROLE_DOCUMENT;
                    }

                    scene.add_child(page, viewport);

                    if has_selection && line_count > 0 {
                        if layout_header.format == 1 {
                            build_rich_selection_nodes(
                                &mut scene,
                                &self.results_buf,
                                viewport,
                                &layout_header,
                                sel_start,
                                sel_end,
                                sel_color,
                                self.doc_va,
                            );
                        } else {
                            let span = SelectionSpan {
                                start: sel_start,
                                end: sel_end,
                                color: sel_color,
                                char_width_mpt: self.char_width_mpt,
                            };

                            build_selection_nodes(
                                &mut scene,
                                &self.results_buf,
                                viewport,
                                line_count,
                                &span,
                            );
                        }
                    }

                    let mut cursor_line = cursor_line_idx as u32;
                    let mut cursor_col = cursor_col_in_line as u32;
                    let char_advance = self.char_width_mpt * 64;
                    let is_rich = layout_header.format == 1;

                    if is_rich {
                        build_rich_text_nodes(
                            &mut scene,
                            viewport,
                            &layout_header,
                            &self.results_buf,
                            self.doc_va,
                            self.display_width,
                            &mut self.glyphs,
                        );
                    }

                    if !is_rich {
                        for i in 0..line_count.min(scene::MAX_NODES - 4) {
                            let line_info = parse_line_at(&self.results_buf, i);
                            let line_start = line_info.byte_offset as usize;
                            let line_len = line_info.byte_length as usize;

                            if line_len == 0 {
                                continue;
                            }

                            let line_bytes = if line_start + line_len <= content_len {
                                &content[line_start..line_start + line_len]
                            } else {
                                continue;
                            };
                            let glyph_count = line_len.min(MAX_GLYPHS_PER_LINE);
                            let mut needs_fallback = false;

                            for (j, &byte) in line_bytes.iter().enumerate().take(glyph_count) {
                                let mono_gid = if byte < 128 {
                                    self.cmap_mono[byte as usize]
                                } else {
                                    0
                                };

                                if mono_gid > 0 {
                                    self.glyphs[j] = ShapedGlyph {
                                        glyph_id: mono_gid,
                                        _pad: STYLE_MONO as u16,
                                        x_advance: char_advance,
                                        x_offset: 0,
                                        y_offset: 0,
                                    };
                                } else {
                                    let sans_gid = if byte < 128 {
                                        self.cmap_sans[byte as usize]
                                    } else {
                                        0
                                    };

                                    if sans_gid > 0 {
                                        self.glyphs[j] = ShapedGlyph {
                                            glyph_id: sans_gid,
                                            _pad: STYLE_SANS as u16,
                                            x_advance: char_advance,
                                            x_offset: 0,
                                            y_offset: 0,
                                        };

                                        needs_fallback = true;
                                    } else {
                                        self.glyphs[j] = ShapedGlyph {
                                            glyph_id: 0,
                                            _pad: STYLE_MONO as u16,
                                            x_advance: char_advance,
                                            x_offset: 0,
                                            y_offset: 0,
                                        };
                                    }
                                }
                            }

                            if !needs_fallback {
                                for j in 0..glyph_count {
                                    self.glyphs[j]._pad = 0;
                                }

                                let glyph_ref =
                                    scene.push_shaped_glyphs(&self.glyphs[..glyph_count]);
                                let line_node = match scene.alloc_node() {
                                    Some(id) => id,
                                    None => break,
                                };

                                {
                                    let n = scene.node_mut(line_node);

                                    n.x = line_info.x_mpt;
                                    n.y = pt(line_info.y);
                                    n.width = (line_info.width_mpt as u32).saturating_add(upt(1));
                                    n.height = upt(presenter_service::LINE_HEIGHT);
                                    n.content = Content::Glyphs {
                                        color: text_color,
                                        glyphs: glyph_ref,
                                        glyph_count: glyph_count as u16,
                                        font_size: presenter_service::FONT_SIZE,
                                        style_id: STYLE_MONO,
                                    };
                                    n.role = scene::ROLE_PARAGRAPH;
                                }

                                scene.add_child(viewport, line_node);
                            } else {
                                let mut run_start = 0;

                                while run_start < glyph_count {
                                    let run_style = self.glyphs[run_start]._pad as u32;
                                    let mut run_end = run_start + 1;

                                    while run_end < glyph_count
                                        && self.glyphs[run_end]._pad as u32 == run_style
                                    {
                                        run_end += 1;
                                    }

                                    let run_len = run_end - run_start;

                                    for j in run_start..run_end {
                                        self.glyphs[j]._pad = 0;
                                    }

                                    let glyph_ref =
                                        scene.push_shaped_glyphs(&self.glyphs[run_start..run_end]);
                                    let run_node = match scene.alloc_node() {
                                        Some(id) => id,
                                        None => break,
                                    };
                                    let run_x_mpt =
                                        line_info.x_mpt + run_start as i32 * self.char_width_mpt;

                                    {
                                        let n = scene.node_mut(run_node);

                                        n.x = run_x_mpt;
                                        n.y = pt(line_info.y);
                                        n.width = (run_len as u32 * self.char_width_mpt as u32)
                                            .saturating_add(upt(1));
                                        n.height = upt(presenter_service::LINE_HEIGHT);
                                        n.content = Content::Glyphs {
                                            color: text_color,
                                            glyphs: glyph_ref,
                                            glyph_count: run_len as u16,
                                            font_size: presenter_service::FONT_SIZE,
                                            style_id: run_style,
                                        };
                                        n.role = scene::ROLE_PARAGRAPH;
                                    }

                                    scene.add_child(viewport, run_node);

                                    run_start = run_end;
                                }
                            }
                        }
                    }

                    if line_count > 0 && cursor_pos >= content_len {
                        let last = parse_line_at(&self.results_buf, line_count - 1);
                        let last_end = last.byte_offset as usize + last.byte_length as usize;

                        if cursor_pos >= last_end && cursor_pos > last.byte_offset as usize {
                            cursor_line = (line_count - 1) as u32;
                            cursor_col = (cursor_pos - last.byte_offset as usize) as u32;
                        }
                    }

                    let (
                        cursor_x_mpt,
                        cursor_y,
                        cursor_h,
                        cursor_style_color,
                        cursor_weight,
                        cursor_skew_bits,
                    ) = if let Some(ci) = &rich_cursor_info {
                        (
                            scene::f32_to_mpt(ci.x),
                            ci.y,
                            ci.height,
                            Some(ci.color_rgba),
                            ci.weight,
                            ci.caret_skew.to_bits(),
                        )
                    } else {
                        (
                            cursor_col as i32 * self.char_width_mpt,
                            cursor_line as i32 * presenter_service::LINE_HEIGHT as i32,
                            presenter_service::LINE_HEIGHT,
                            None,
                            400u16,
                            0u32,
                        )
                    };
                    let effective_cursor_color = match cursor_style_color {
                        Some(rgba) => Color::rgba(
                            ((rgba >> 24) & 0xFF) as u8,
                            ((rgba >> 16) & 0xFF) as u8,
                            ((rgba >> 8) & 0xFF) as u8,
                            (rgba & 0xFF) as u8,
                        ),
                        None => cursor_color,
                    };
                    let cursor_w_mpt = scene::MPT_PER_PT as u32
                        + (cursor_weight.saturating_sub(100) as u32) * 3 * scene::MPT_PER_PT as u32
                            / 800;

                    if let Some(cursor_node) = scene.alloc_node() {
                        let n = scene.node_mut(cursor_node);

                        n.x = cursor_x_mpt;
                        n.y = pt(cursor_y);
                        n.width = cursor_w_mpt;
                        n.height = upt(cursor_h);
                        n.background = effective_cursor_color;

                        if cursor_skew_bits != 0 {
                            n.transform = scene::AffineTransform {
                                a: 1.0,
                                b: 0.0,
                                c: f32::from_bits(cursor_skew_bits),
                                d: 1.0,
                                tx: 0.0,
                                ty: 0.0,
                            };
                        }

                        if !has_selection {
                            n.animation = scene::Animation::cursor_blink(self.blink_start);
                        }

                        n.role = scene::ROLE_CARET;

                        scene.add_child(viewport, cursor_node);
                    }
                }

                super::Space::Image {
                    content_id,
                    width,
                    height,
                } => {
                    if *width > 0 && *height > 0 {
                        let max_w = self.display_width.saturating_sub(2 * page_margin);
                        let max_h = content_h.saturating_sub(2 * page_margin);
                        let src_w = *width as u32;
                        let src_h = *height as u32;
                        let (disp_w, disp_h) = aspect_fit(src_w, src_h, max_w, max_h);
                        let img_x = base_x + ((self.display_width as i32 - disp_w as i32) / 2);
                        let img_y = ((content_h as i32 - disp_h as i32) / 2).max(0);

                        if let Some(image_node) = scene.alloc_node() {
                            let n = scene.node_mut(image_node);

                            n.x = pt(img_x);
                            n.y = pt(img_y);
                            n.width = upt(disp_w);
                            n.height = upt(disp_h);
                            n.content = Content::Image {
                                content_id: *content_id,
                                src_width: *width,
                                src_height: *height,
                            };
                            n.shadow_color = Color::rgba(0, 0, 0, 255);
                            n.shadow_blur_radius = presenter_service::SHADOW_BLUR_RADIUS;
                            n.shadow_spread = presenter_service::SHADOW_SPREAD;

                            scene.add_child(strip, image_node);
                        }
                    }
                }

                super::Space::Video {
                    content_id,
                    width,
                    height,
                    playing,
                    ..
                } => {
                    if *width > 0 && *height > 0 {
                        let max_w = self.display_width.saturating_sub(2 * page_margin);
                        let max_h = content_h.saturating_sub(2 * page_margin);
                        let src_w = *width as u32;
                        let src_h = *height as u32;
                        let (vid_w, vid_h) = aspect_fit(src_w, src_h, max_w, max_h);
                        let vid_x = base_x + ((self.display_width as i32 - vid_w as i32) / 2);
                        let vid_y = ((content_h as i32 - vid_h as i32) / 2).max(0);

                        if let Some(video_node) = scene.alloc_node() {
                            let n = scene.node_mut(video_node);

                            n.x = pt(vid_x);
                            n.y = pt(vid_y);
                            n.width = upt(vid_w);
                            n.height = upt(vid_h);
                            n.content = Content::Image {
                                content_id: *content_id,
                                src_width: *width,
                                src_height: *height,
                            };
                            n.shadow_color = Color::rgba(0, 0, 0, 255);
                            n.shadow_blur_radius = presenter_service::SHADOW_BLUR_RADIUS;
                            n.shadow_spread = presenter_service::SHADOW_SPREAD;

                            scene.add_child(strip, video_node);
                        }

                        if let Some(btn_id) = scene.alloc_node() {
                            let btn_size = 80u32;
                            let btn_x =
                                base_x + ((self.display_width as i32 - btn_size as i32) / 2);
                            let btn_y = ((content_h as i32 - btn_size as i32) / 2).max(0);
                            let icon_name = if *playing {
                                "player-pause"
                            } else {
                                "player-play"
                            };
                            let name_ref = scene.push_data(b"Play video");
                            let n = scene.node_mut(btn_id);

                            n.x = pt(btn_x);
                            n.y = pt(btn_y);
                            n.width = upt(btn_size);
                            n.height = upt(btn_size);
                            n.background = Color::rgba(0, 0, 0, 120);
                            n.corner_radius = 40;
                            n.role = scene::ROLE_BUTTON;
                            n.state = scene::STATE_FOCUSABLE;
                            n.cursor_shape = scene::CURSOR_PRESSABLE;
                            n.name = name_ref;

                            scene.add_child(strip, btn_id);

                            let icon_data = icons::get(icon_name, None);
                            let icon_padding = 16u32;
                            let icon_size_pt = btn_size - icon_padding * 2;
                            let (icon_data_ref, icon_hash) =
                                scale_icon_paths(&mut scene, icon_data, icon_size_pt);
                            let icon_sw_fixed = super::icon_stroke_width_fixed(
                                icon_data.stroke_width.to_bits(),
                                icon_size_pt,
                                icon_data.viewbox.to_bits(),
                            );

                            if let Some(icon_id) = scene.alloc_node() {
                                let icon = scene.node_mut(icon_id);

                                icon.x = pt(icon_padding as i32);
                                icon.y = pt(icon_padding as i32);
                                icon.width = upt(icon_size_pt);
                                icon.height = upt(icon_size_pt);
                                icon.content = Content::Path {
                                    color: Color::TRANSPARENT,
                                    stroke_color: Color::rgba(255, 255, 255, 200),
                                    fill_rule: FillRule::Winding,
                                    stroke_width: icon_sw_fixed,
                                    contours: icon_data_ref,
                                };
                                icon.content_hash = icon_hash;

                                scene.add_child(btn_id, icon_id);
                            }
                        }
                    }
                }

                super::Space::Showcase => {
                    build_showcase_nodes(
                        &mut scene,
                        strip,
                        self.display_width,
                        content_h,
                        now_ns,
                        has_audio,
                        base_x,
                    );
                }
            }
        }

        self.swap_scene();

        self.last_line_count = line_count as u32;
        self.last_cursor_line = cursor_line_idx as u32;
        self.last_cursor_col = cursor_col_in_line as u32;
        self.last_content_len = content_len as u32;

        self.update_cursor_shape();
    }

    pub(crate) fn current_clock_secs(&self) -> u64 {
        let rtc = read_rtc_seconds(self.rtc_va);

        if rtc > 0 {
            rtc % 86400
        } else {
            let ns = abi::system::clock_read().unwrap_or(0);

            (ns / 1_000_000_000) % 86400
        }
    }

    pub(crate) fn update_clock(&mut self) -> bool {
        let clock_secs = self.current_clock_secs();

        if clock_secs == self.last_clock_secs {
            return false;
        }

        self.last_clock_secs = clock_secs;

        self.build_scene();

        true
    }

    pub(crate) fn make_info_reply(&self) -> presenter_service::InfoReply {
        let active = self.read_active_index();
        let scene = SceneWriter::from_existing(unsafe {
            core::slice::from_raw_parts_mut(self.scene_bufs[active].as_ptr() as *mut u8, SCENE_SIZE)
        });

        presenter_service::InfoReply {
            node_count: scene.node_count(),
            generation: self.swap_gen,
            line_count: self.last_line_count,
            cursor_line: self.last_cursor_line,
            cursor_col: self.last_cursor_col,
            content_len: self.last_content_len,
            scroll_y: self.scroll_y,
        }
    }

    // ── Cursor shape resolution ──────────────────────────────────

    pub(crate) fn resolve_cursor_shape(&self) -> u8 {
        let active = self.read_active_index();
        let scene = SceneWriter::from_existing(unsafe {
            core::slice::from_raw_parts_mut(self.scene_bufs[active].as_ptr() as *mut u8, SCENE_SIZE)
        });

        if let Some(hit_id) = scene.hit_test(pt(self.pointer_x), pt(self.pointer_y)) {
            let shape = scene.node(hit_id).cursor_shape;

            if shape != scene::CURSOR_INHERIT {
                return shape;
            }
        }

        if matches!(self.spaces.get(self.active_space), Some(super::Space::Text))
            && self.is_on_page(self.pointer_x as u32, self.pointer_y as u32)
        {
            return scene::CURSOR_TEXT;
        }

        scene::CURSOR_DEFAULT
    }

    pub(crate) fn update_cursor_shape(&mut self) {
        let shape = self.resolve_cursor_shape();

        if shape != self.cursor_shape_name {
            self.cursor_shape_name = shape;

            let payload = [shape];
            let _ =
                ipc::client::call_simple(self.render_ep, render::comp::SET_CURSOR_SHAPE, &payload);
        }
    }
}
