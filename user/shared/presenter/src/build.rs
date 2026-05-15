//! Scene building — compiles viewer subtrees into a scene graph via write_subtree.

use scene::{SCENE_SIZE, SceneWriter, pt, upt};
use view_tree::Viewer;

use super::{parse_visible_run_at, read_rtc_seconds};

pub(crate) fn build_font_axes(
    weight: u16,
    font_size: u16,
) -> ([fonts::metrics::AxisValue; 2], usize) {
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

// ── Rich text measurement helpers ───────────────────────────────

pub(crate) fn byte_to_x_rich(
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

        let font_data = super::font_data_for_style(vr.font_family, vr.flags);
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

// ── Rich text cursor ────────────────────────────────────────────

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

    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);
        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        if cursor_pos < run_start || cursor_pos > run_end {
            continue;
        }

        let font_data = super::font_data_for_style(vr.font_family, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, axis_count) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..axis_count];
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

    let line_count = layout_header.line_count as usize;

    for i in 0..line_count {
        let line = super::parse_line_at(results_buf, i);
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

    if line_count > 0 {
        let last = super::parse_line_at(results_buf, line_count - 1);
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

// ── Rich text hit testing ───────────────────────────────────────

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

    let mut target_line = line_count - 1;

    for i in 0..line_count {
        let next_y = if i + 1 < line_count {
            super::parse_line_at(results_buf, i + 1).y
        } else {
            i32::MAX
        };

        if rel_y < next_y {
            target_line = i;

            break;
        }
    }

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

    let mut best_pos = super::parse_line_at(results_buf, target_line).byte_offset as usize;

    for run_i in 0..run_count {
        let vr = parse_visible_run_at(results_buf, run_i);

        if vr.line_index as usize != target_line {
            continue;
        }

        let font_data = super::font_data_for_style(vr.font_family, vr.flags);
        let upem = fonts::metrics::font_metrics(font_data)
            .map(|m| m.units_per_em)
            .unwrap_or(1000);
        let (axes_buf, axis_count) = build_font_axes(vr.weight, vr.font_size);
        let axes = &axes_buf[..axis_count];
        let run_start = vr.byte_offset as usize;
        let run_end = run_start + vr.byte_length as usize;

        if rel_x < vr.x {
            best_pos = run_start;

            break;
        }

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

// ── Presenter scene methods ──────────────────────────────────────

impl super::Presenter {
    pub(crate) fn build_scene(&mut self) {
        if self.layout_dirty {
            let _ = ipc::client::call_simple(self.layout_ep, layout_service::RECOMPUTE, &[]);
            self.layout_dirty = false;
        }

        let (content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };

        self.results_reader.read(&mut self.results_buf);

        let layout_header = super::parse_layout_header(&self.results_buf);
        let line_count = layout_header.line_count as usize;
        let is_rich = layout_header.format == 1;
        let (cursor_line_idx, cursor_col_in_line) = self.find_cursor_line(cursor_pos, line_count);

        if is_rich {
            let ci =
                compute_rich_cursor(&self.results_buf, &layout_header, cursor_pos, self.doc_va);

            self.ensure_cursor_visible_at(ci.y, ci.height as i32);
        } else {
            self.ensure_cursor_visible(cursor_line_idx as u32);
        }

        self.clamp_scroll();
        self.write_viewport();

        let content = unsafe { document_service::doc_content_slice(self.doc_va, content_len) };
        let now_ns = abi::system::clock_read().unwrap_or(0);
        let title_bar_h = presenter_service::TITLE_BAR_H;
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let page_padding = presenter_service::PAGE_PADDING;
        let content_h = self.display_height.saturating_sub(title_bar_h);
        let page_h = content_h.saturating_sub(2 * page_margin);
        let page_w = (page_h as u64 * 210 / 297) as u32;
        let page_w = page_w.min(self.display_width.saturating_sub(2 * page_margin));
        let max_w = self.display_width.saturating_sub(2 * page_margin);
        let max_h = content_h.saturating_sub(2 * page_margin);
        let (vis_lo, vis_hi) = self.workspace.visible_range(self.display_width);

        for i in vis_lo..=vis_hi {
            match &mut self.workspace.children[i].viewer {
                super::handlers::ViewerKind::Text(tv) => {
                    tv.rebuild(&super::handlers::TextRebuildContext {
                        content,
                        doc_va: self.doc_va,
                        results_buf: &self.results_buf,
                        layout_header,
                        page_w,
                        page_h,
                        page_padding,
                        cursor_pos,
                        sel_anchor,
                        content_len,
                        display_width: self.display_width,
                    });
                }
                super::handlers::ViewerKind::Image(iv) => {
                    iv.rebuild(&view_tree::Constraints {
                        available_width: upt(max_w),
                        available_height: upt(max_h),
                        now_ns,
                    });
                }
                super::handlers::ViewerKind::Video(vv) => {
                    vv.rebuild(&view_tree::Constraints {
                        available_width: upt(max_w),
                        available_height: upt(max_h),
                        now_ns,
                    });
                }
            }
        }

        let active_mimetype = self
            .workspace
            .children
            .get(self.workspace.active)
            .and_then(|c| core::str::from_utf8(c.mimetype).ok());

        self.workspace
            .rebuild(&super::handlers::WorkspaceRebuildContext {
                display_width: self.display_width,
                display_height: self.display_height,
                now_ns,
                rtc_secs: self.current_clock_secs(),
                active_mimetype,
            });

        let back_idx = 1 - self.read_active_index();
        let back_buf = unsafe {
            core::slice::from_raw_parts_mut(self.scene_bufs[back_idx].as_mut_ptr(), SCENE_SIZE)
        };
        let mut scene = SceneWriter::from_existing(back_buf);

        scene.reset();

        let ws_subtree = self.workspace.subtree();
        let child_subtrees = self.workspace.child_subtrees();
        let child_refs: alloc::vec::Vec<&view_tree::ViewSubtree> = child_subtrees.to_vec();

        super::handlers::write_subtree_as_root(ws_subtree, &child_refs, &mut scene, pt(0), pt(0));

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
            scroll_y: self.scroll_y(),
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

        if self.active_is_text() && self.is_on_page(self.pointer_x as u32, self.pointer_y as u32) {
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
