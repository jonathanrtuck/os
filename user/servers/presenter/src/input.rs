//! Keyboard input handling — navigation, selection, and key dispatch.

use super::*;
use crate::build::compute_rich_cursor;

impl Presenter {
    // ── Navigation helpers ─────────────────────────────────────

    pub(crate) fn find_cursor_line(&self, cursor_pos: usize, line_count: usize) -> (usize, usize) {
        for i in 0..line_count {
            let line = parse_line_at(&self.results_buf, i);
            let start = line.byte_offset as usize;
            let next_start = if i + 1 < line_count {
                parse_line_at(&self.results_buf, i + 1).byte_offset as usize
            } else {
                usize::MAX
            };

            if cursor_pos < next_start {
                return (i, cursor_pos.saturating_sub(start));
            }
        }

        if line_count > 0 {
            let last = parse_line_at(&self.results_buf, line_count - 1);

            (
                line_count - 1,
                cursor_pos.saturating_sub(last.byte_offset as usize),
            )
        } else {
            (0, 0)
        }
    }

    pub(crate) fn line_start_byte(&self, line_idx: usize) -> usize {
        parse_line_at(&self.results_buf, line_idx).byte_offset as usize
    }

    pub(crate) fn line_end_byte(&self, line_idx: usize) -> usize {
        let line = parse_line_at(&self.results_buf, line_idx);

        (line.byte_offset + line.byte_length) as usize
    }

    fn logical_text(&self, _content_len_hint: usize) -> alloc::vec::Vec<u8> {
        let (raw_len, _, _, _) = unsafe { document_service::read_doc_header(self.doc_va) };
        let header = parse_layout_header(&self.results_buf);

        if header.format == 1 {
            let doc_buf = unsafe {
                core::slice::from_raw_parts(
                    (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                    raw_len,
                )
            };

            if piecetable::validate(doc_buf) {
                let tl = piecetable::text_len(doc_buf) as usize;
                let mut buf = alloc::vec![0u8; tl];
                let copied = piecetable::text_slice(doc_buf, 0, tl as u32, &mut buf);

                buf.truncate(copied);

                return buf;
            }
        }

        unsafe { document_service::doc_content_slice(self.doc_va, raw_len) }.to_vec()
    }

    fn visible_lines(&self) -> usize {
        let viewport_height = self
            .display_height
            .saturating_sub(presenter_service::MARGIN_TOP as u32 * 2);

        (viewport_height / presenter_service::LINE_HEIGHT).max(1) as usize
    }

    pub(crate) fn nav_target(
        &self,
        key_code: u16,
        modifiers: u8,
        cursor_pos: usize,
        content_len: usize,
    ) -> Option<usize> {
        let alt = modifiers & text_editor::MOD_ALT != 0;
        let cmd = modifiers & text_editor::MOD_SUPER != 0;
        let header = parse_layout_header(&self.results_buf);
        let line_count = header.line_count as usize;

        if line_count == 0 {
            return Some(0);
        }

        let (cur_line, cur_col) = self.find_cursor_line(cursor_pos, line_count);

        match key_code {
            text_editor::HID_KEY_LEFT => {
                if cmd {
                    Some(self.line_start_byte(cur_line))
                } else if alt {
                    let text = self.logical_text(content_len);

                    Some(layout::word_boundary_backward(&text, cursor_pos))
                } else {
                    Some(cursor_pos.saturating_sub(1))
                }
            }
            text_editor::HID_KEY_RIGHT => {
                if cmd {
                    Some(self.line_end_byte(cur_line))
                } else if alt {
                    let text = self.logical_text(content_len);

                    Some(layout::word_boundary_forward(&text, cursor_pos))
                } else {
                    Some((cursor_pos + 1).min(content_len))
                }
            }
            text_editor::HID_KEY_UP => {
                if cmd {
                    Some(0)
                } else if cur_line == 0 {
                    Some(self.line_start_byte(0))
                } else {
                    Some(self.byte_at_sticky_x(cur_line - 1, cur_col, content_len, &header))
                }
            }
            text_editor::HID_KEY_DOWN => {
                if cmd {
                    Some(content_len)
                } else if cur_line + 1 >= line_count {
                    Some(self.line_end_byte(cur_line))
                } else {
                    Some(self.byte_at_sticky_x(cur_line + 1, cur_col, content_len, &header))
                }
            }
            text_editor::HID_KEY_HOME => Some(self.line_start_byte(cur_line)),
            text_editor::HID_KEY_END => Some(self.line_end_byte(cur_line)),
            text_editor::HID_KEY_PAGE_UP => {
                let page = self.visible_lines();

                if cur_line == 0 {
                    return Some(self.line_start_byte(0));
                }

                let target_line = cur_line.saturating_sub(page);

                Some(self.byte_at_sticky_x(target_line, cur_col, content_len, &header))
            }
            text_editor::HID_KEY_PAGE_DOWN => {
                let page = self.visible_lines();

                if cur_line + 1 >= line_count {
                    return Some(self.line_end_byte(cur_line));
                }

                let target_line = (cur_line + page).min(line_count - 1);

                Some(self.byte_at_sticky_x(target_line, cur_col, content_len, &header))
            }
            _ => None,
        }
    }

    fn byte_at_sticky_x(
        &self,
        target_line: usize,
        cur_col: usize,
        content_len: usize,
        header: &layout_service::LayoutHeader,
    ) -> usize {
        let target_info = parse_line_at(&self.results_buf, target_line);

        if header.format == 1 {
            let cursor_x_fp = self.sticky_col.unwrap_or(u32::MAX);

            if cursor_x_fp == u32::MAX {
                let col = cur_col.min(target_info.byte_length as usize);

                return target_info.byte_offset as usize + col;
            }

            let target_x = cursor_x_fp as f32 / 256.0;
            let (text_x, text_y) = self.text_origin();
            let line_y = target_info.y + 1;
            let abs_x = text_x + target_x as u32;
            let abs_y = (text_y as i32 + line_y - self.scroll_y).max(0) as u32;

            self.xy_to_byte(abs_x, abs_y, content_len)
        } else {
            let col = self.sticky_col.unwrap_or(cur_col as u32) as usize;
            let target_col = col.min(target_info.byte_length as usize);

            target_info.byte_offset as usize + target_col
        }
    }

    pub(crate) fn is_vertical_nav(key_code: u16) -> bool {
        matches!(
            key_code,
            text_editor::HID_KEY_UP
                | text_editor::HID_KEY_DOWN
                | text_editor::HID_KEY_PAGE_UP
                | text_editor::HID_KEY_PAGE_DOWN
        )
    }

    // ── Document commands ──────────────────────────────────────

    pub(crate) fn doc_select(&self, anchor: usize, cursor: usize) {
        let sel = document_service::Selection {
            anchor: anchor as u64,
            cursor: cursor as u64,
        };
        let mut data = [0u8; document_service::Selection::SIZE];

        sel.write_to(&mut data);

        let _ = ipc::client::call_simple(self.doc_ep, document_service::SELECT, &data);
    }

    pub(crate) fn doc_cursor_move(&self, pos: usize) {
        let req = document_service::CursorMove {
            position: pos as u64,
        };
        let mut data = [0u8; document_service::CursorMove::SIZE];

        req.write_to(&mut data);

        let _ = ipc::client::call_simple(self.doc_ep, document_service::CURSOR_MOVE, &data);
    }

    // ── Main key event handler ─────────────────────────────────

    pub(crate) fn handle_key_event(&mut self, dispatch: text_editor::KeyDispatch) {
        let ctrl = dispatch.modifiers & text_editor::MOD_CONTROL != 0;

        // Ctrl+W: close the active space.
        if ctrl && dispatch.character == b'w' {
            if self.spaces.len() <= 1 {
                return;
            }

            let _ = self.spaces.remove(self.active_space);

            if self.active_space >= self.spaces.len() {
                self.active_space = self.spaces.len() - 1;
            }

            let target = self.active_space as f32 * self.display_width as f32;

            self.slide_spring.reset_to(target);
            self.slide_animating = false;
            self.build_scene();

            return;
        }

        // Ctrl+Tab: cycle to next space.
        if dispatch.key_code == text_editor::HID_KEY_TAB && ctrl {
            if self.spaces.len() <= 1 {
                return;
            }

            self.active_space = (self.active_space + 1) % self.spaces.len();

            let target = self.active_space as f32 * self.display_width as f32;

            self.slide_spring.set_target(target);
            self.slide_animating = true;
            self.last_anim_tick = abi::system::clock_read().unwrap_or(0);
            self.blink_start = self.last_anim_tick;
            self.build_scene();

            return;
        }

        if !matches!(self.spaces.get(self.active_space), Some(super::Space::Text)) {
            return;
        }

        // SAFETY: doc_va is a valid RO mapping of the document buffer.
        let (raw_content_len, cursor_pos, sel_anchor, _) =
            unsafe { document_service::read_doc_header(self.doc_va) };
        let header = parse_layout_header(&self.results_buf);
        let content_len = if header.format == 1 {
            let doc_buf = unsafe {
                core::slice::from_raw_parts(
                    (self.doc_va + document_service::DOC_HEADER_SIZE) as *const u8,
                    raw_content_len,
                )
            };

            if piecetable::validate(doc_buf) {
                piecetable::text_len(doc_buf) as usize
            } else {
                raw_content_len
            }
        } else {
            raw_content_len
        };
        let has_selection = sel_anchor != cursor_pos;
        let shift = dispatch.modifiers & text_editor::MOD_SHIFT != 0;
        let cmd = dispatch.modifiers & text_editor::MOD_SUPER != 0;

        // Cmd+A: select all.
        if cmd && dispatch.character == b'a' {
            self.doc_select(0, content_len);
            self.sticky_col = None;
            self.blink_start = abi::system::clock_read().unwrap_or(0);
            self.build_scene();

            return;
        }

        // Compute sticky_col BEFORE nav_target so byte_at_sticky_x has
        // the correct proportional x on the very first vertical press.
        if Self::is_vertical_nav(dispatch.key_code) && self.sticky_col.is_none() {
            if header.format == 1 {
                let ci = compute_rich_cursor(&self.results_buf, &header, cursor_pos, self.doc_va);

                self.sticky_col = Some((ci.x * 256.0) as u32);
            } else {
                let (_, col) = self.find_cursor_line(cursor_pos, header.line_count as usize);

                self.sticky_col = Some(col as u32);
            }
        }

        if let Some(mut target) = self.nav_target(
            dispatch.key_code,
            dispatch.modifiers,
            cursor_pos,
            content_len,
        ) {
            let is_vertical = Self::is_vertical_nav(dispatch.key_code);

            if !shift && has_selection {
                let sel_start = sel_anchor.min(cursor_pos);
                let sel_end = sel_anchor.max(cursor_pos);

                match dispatch.key_code {
                    text_editor::HID_KEY_LEFT => target = sel_start,
                    text_editor::HID_KEY_RIGHT => target = sel_end,
                    _ => {}
                }
            }

            if shift {
                let anchor = if has_selection {
                    sel_anchor
                } else {
                    cursor_pos
                };

                self.doc_select(anchor, target);
            } else {
                self.doc_cursor_move(target);
            }

            if !is_vertical {
                self.sticky_col = None;
            }
        } else {
            let mut data = [0u8; text_editor::KeyDispatch::SIZE];

            dispatch.write_to(&mut data);

            let _ = ipc::client::call_simple(self.editor_ep, text_editor::DISPATCH_KEY, &data);

            self.sticky_col = None;
        }

        self.blink_start = abi::system::clock_read().unwrap_or(0);
        self.build_scene();
    }
}
