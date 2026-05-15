//! Pointer interaction — click, double/triple-click, drag selection,
//! and coordinate hit-testing for the page area.

use super::*;
use crate::build::xy_to_byte_rich;

// ── Page geometry & hit testing ──────────────────────────────────

impl Presenter {
    pub(crate) fn page_rect(&self) -> (u32, u32, u32, u32) {
        let title_bar_h = presenter_service::TITLE_BAR_H;
        let page_margin = presenter_service::PAGE_MARGIN_V;
        let content_h = self.display_height.saturating_sub(title_bar_h);
        let page_h = content_h.saturating_sub(2 * page_margin);
        let page_w = ((page_h as u64 * 210 / 297) as u32)
            .min(self.display_width.saturating_sub(2 * page_margin));
        let page_x = (self.display_width - page_w) / 2;
        let page_y = title_bar_h + page_margin;

        (page_x, page_y, page_w, page_h)
    }

    pub(crate) fn is_on_page(&self, px: u32, py: u32) -> bool {
        let (page_x, page_y, page_w, page_h) = self.page_rect();

        px >= page_x && px < page_x + page_w && py >= page_y && py < page_y + page_h
    }

    pub(crate) fn text_origin(&self) -> (u32, u32) {
        let (page_x, page_y, _, _) = self.page_rect();
        let page_padding = presenter_service::PAGE_PADDING;

        (page_x + page_padding, page_y + page_padding)
    }

    pub(crate) fn xy_to_byte(&self, px: u32, py: u32, content_len: usize) -> usize {
        let (text_x, text_y) = self.text_origin();
        let rel_x = px.saturating_sub(text_x) as i32;
        let rel_y = py.saturating_sub(text_y) as i32 + self.scroll_y();

        if rel_y < 0 {
            return 0;
        }

        let header = parse_layout_header(&self.results_buf);
        let line_count = header.line_count as usize;

        if line_count == 0 {
            return 0;
        }

        if header.format == 1 {
            return xy_to_byte_rich(
                &self.results_buf,
                &header,
                rel_x as f32,
                rel_y,
                self.doc_va,
                content_len,
            );
        }

        let line_h = presenter_service::LINE_HEIGHT as i32;
        let target_line = (rel_y / line_h) as usize;
        let cw_mpt = self.char_width_mpt();
        let rel_x_mpt = rel_x * scene::MPT_PER_PT;
        let target_col = if cw_mpt > 0 {
            ((rel_x_mpt + cw_mpt / 2) / cw_mpt) as usize
        } else {
            0
        };

        if target_line >= line_count {
            return content_len;
        }

        let line = parse_line_at(&self.results_buf, target_line);
        let line_start = line.byte_offset as usize;
        let line_len = line.byte_length as usize;
        let pos = line_start + target_col.min(line_len);

        pos.min(content_len)
    }

    // ── Pointer button handling ──────────────────────────────────

    pub(crate) fn handle_pointer_button(&mut self, btn: presenter_service::PointerButton) {
        self.layout_dirty = true;

        if btn.button != 0 {
            return;
        }

        if btn.pressed == 0 {
            self.dragging = false;

            return;
        }

        let click_x = (btn.abs_x as u64 * self.display_width as u64 / 32768) as u32;
        let click_y = (btn.abs_y as u64 * self.display_height as u64 / 32768) as u32;

        {
            let mut dbg = [0u8; 60];
            let mut p = 0;

            p += super::copy_into(&mut dbg[p..], b"click: x=");
            p += console::format_u32(click_x, &mut dbg[p..]);
            p += super::copy_into(&mut dbg[p..], b" y=");
            p += console::format_u32(click_y, &mut dbg[p..]);
            p += super::copy_into(&mut dbg[p..], b" sp=");
            p += console::format_u32(self.workspace.active as u32, &mut dbg[p..]);

            dbg[p] = b'\n';

            p += 1;

            console::write(self.console_ep, &dbg[..p]);
        }
        {
            let active = self.read_active_index();
            let scene = scene::SceneWriter::from_existing(unsafe {
                core::slice::from_raw_parts_mut(
                    self.scene_bufs[active].as_ptr() as *mut u8,
                    scene::SCENE_SIZE,
                )
            });

            if let Some(hit_id) =
                scene.hit_test(scene::pt(click_x as i32), scene::pt(click_y as i32))
            {
                let node = scene.node(hit_id);

                console::write(
                    self.console_ep,
                    if node.role == scene::ROLE_BUTTON {
                        b"click: HIT BUTTON\n"
                    } else {
                        b"click: hit non-btn\n"
                    },
                );

                if node.role == scene::ROLE_BUTTON {
                    if self.active_is_video() {
                        self.toggle_video_playback();
                    } else {
                        self.play_audio_clip();
                    }

                    return;
                }
            } else {
                console::write(self.console_ep, b"click: no hit\n");
            }
        }

        if !self.active_is_text() {
            return;
        }

        if !self.is_on_page(click_x, click_y) {
            return;
        }

        let (content_len, _cursor_pos, _sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let byte_pos = self.xy_to_byte(click_x, click_y, content_len);
        let now_ns = abi::system::clock_read().unwrap_or(0);
        let now_ms = now_ns / 1_000_000;
        let mpt = scene::MPT_PER_PT as u32;
        let dx_mpt = click_x.abs_diff(self.last_click_x) * mpt;
        let dy_mpt = click_y.abs_diff(self.last_click_y) * mpt;
        let dt = now_ms.saturating_sub(self.last_click_ms);
        let click_tolerance_mpt = 4 * mpt;
        let same_spot = dx_mpt <= click_tolerance_mpt && dy_mpt <= click_tolerance_mpt && dt <= 400;
        let click_count = if same_spot {
            (self.click_count % 3) + 1
        } else {
            1
        };

        self.last_click_ms = now_ms;
        self.last_click_x = click_x;
        self.last_click_y = click_y;
        self.click_count = click_count;

        let header = parse_layout_header(&self.results_buf);
        let is_rich = header.format == 1;
        let mut rich_text_buf = alloc::vec::Vec::new();
        let content: &[u8] = if is_rich {
            let doc_buf = unsafe {
                core::slice::from_raw_parts(
                    (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                    content_len,
                )
            };
            if piecetable::validate(doc_buf) {
                let tl = piecetable::text_len(doc_buf) as usize;

                rich_text_buf.resize(tl, 0u8);

                let copied = piecetable::text_slice(doc_buf, 0, tl as u32, &mut rich_text_buf);

                &rich_text_buf[..copied]
            } else {
                unsafe { document_service::doc_content_slice(self.doc_va, content_len) }
            }
        } else {
            unsafe { document_service::doc_content_slice(self.doc_va, content_len) }
        };
        let text_len = content.len();

        match click_count {
            2 => {
                let at_word = byte_pos < text_len && !layout::is_whitespace(content[byte_pos]);
                let back_pos = if at_word { byte_pos + 1 } else { byte_pos };
                let lo = layout::word_boundary_backward(content, back_pos);
                let mut hi = byte_pos;

                while hi < text_len && !layout::is_whitespace(content[hi]) {
                    hi += 1;
                }

                if hi > lo {
                    self.doc_select(lo, hi);
                } else {
                    self.doc_cursor_move(byte_pos);
                }
            }
            3 => {
                let line_count = header.line_count as usize;
                let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                let lo = self.line_start_byte(line_idx);
                let mut hi = self.line_end_byte(line_idx);

                if hi < text_len && content[hi] == b'\n' {
                    hi += 1;
                }

                self.doc_select(lo, hi);
            }
            _ => {
                self.doc_select(byte_pos, byte_pos);
            }
        }

        self.dragging = true;

        let (_cl, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };

        self.drag_origin_start = sel_anchor;
        self.drag_origin_end = cursor_pos;
        self.sticky_col = None;

        self.set_blink_start(abi::system::clock_read().unwrap_or(0));
        self.build_scene();
    }

    pub(crate) fn handle_pointer_drag(&mut self, abs_x: u32, abs_y: u32) {
        self.layout_dirty = true;

        if !self.dragging {
            return;
        }

        let drag_x = (abs_x as u64 * self.display_width as u64 / 32768) as u32;
        let drag_y = (abs_y as u64 * self.display_height as u64 / 32768) as u32;

        if !self.is_on_page(drag_x, drag_y) {
            return;
        }

        let (content_len, _, _, _) = unsafe { document_service::read_doc_header(self.doc_va) };
        let byte_pos = self.xy_to_byte(drag_x, drag_y, content_len);
        let header = parse_layout_header(&self.results_buf);
        let is_rich = header.format == 1;
        let mut rich_text_buf = alloc::vec::Vec::new();
        let content: &[u8] = if is_rich {
            let doc_buf = unsafe {
                core::slice::from_raw_parts(
                    (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                    content_len,
                )
            };

            if piecetable::validate(doc_buf) {
                let tl = piecetable::text_len(doc_buf) as usize;

                rich_text_buf.resize(tl, 0u8);

                let copied = piecetable::text_slice(doc_buf, 0, tl as u32, &mut rich_text_buf);

                &rich_text_buf[..copied]
            } else {
                unsafe { document_service::doc_content_slice(self.doc_va, content_len) }
            }
        } else {
            unsafe { document_service::doc_content_slice(self.doc_va, content_len) }
        };
        let text_len = content.len();

        match self.click_count {
            2 => {
                if byte_pos < self.drag_origin_start {
                    let lo = layout::word_boundary_backward(content, byte_pos);

                    self.doc_select(self.drag_origin_end, lo);
                } else if byte_pos >= self.drag_origin_end {
                    let mut hi = byte_pos;

                    while hi < text_len && !layout::is_whitespace(content[hi]) {
                        hi += 1;
                    }

                    self.doc_select(self.drag_origin_start, hi);
                } else {
                    self.doc_select(self.drag_origin_start, self.drag_origin_end);
                }
            }
            3 => {
                let header = parse_layout_header(&self.results_buf);
                let line_count = header.line_count as usize;

                if byte_pos < self.drag_origin_start {
                    let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                    let lo = self.line_start_byte(line_idx);

                    self.doc_select(self.drag_origin_end, lo);
                } else if byte_pos >= self.drag_origin_end {
                    let (line_idx, _) = self.find_cursor_line(byte_pos, line_count);
                    let mut hi = self.line_end_byte(line_idx);

                    if hi < content_len && content[hi] == b'\n' {
                        hi += 1;
                    }

                    self.doc_select(self.drag_origin_start, hi);
                } else {
                    self.doc_select(self.drag_origin_start, self.drag_origin_end);
                }
            }
            _ => {
                self.doc_select(self.drag_origin_start, byte_pos);
            }
        }

        self.set_blink_start(abi::system::clock_read().unwrap_or(0));
        self.build_scene();
    }
}
