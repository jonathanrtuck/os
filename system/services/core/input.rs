//! Keyboard input processing and cursor navigation.
//!
//! Owns the key event dispatch loop (`process_key_event`), all cursor
//! movement helpers (word boundaries, visual line start/end, scroll),
//! and selection management. Accesses document state via `super::state()`.

use protocol::{
    edit::{CursorMove, MSG_SET_CURSOR},
    input::KeyEvent,
};

use super::{
    clamp_f32, content_text_layout,
    documents::{doc_content, doc_delete_range, doc_write_header},
    EDITOR_HANDLE, KEY_1, KEY_2, KEY_A, KEY_B, KEY_BACKSPACE, KEY_DELETE, KEY_DOWN, KEY_END,
    KEY_HOME, KEY_I, KEY_LEFT, KEY_PAGEDOWN, KEY_PAGEUP, KEY_RIGHT, KEY_TAB, KEY_UP,
};

/// Delete a byte range using the correct path for the current document format.
/// Rich text documents route through the piece table; plain text through the flat buffer.
fn delete_range_for_format(start: usize, end: usize) -> bool {
    let s = super::state();
    if s.doc_format == super::DocumentFormat::Rich {
        super::documents::rich_delete_range(start, end)
    } else {
        doc_delete_range(start, end)
    }
}

pub(crate) struct KeyAction {
    pub(crate) changed: bool,
    pub(crate) text_changed: bool,
    pub(crate) selection_changed: bool,
    pub(crate) context_switched: bool,
    pub(crate) consumed: bool,
}

// ── Navigation helpers ─────────────────────────────────────────────

/// Find the previous word boundary (delegates to layout library).
pub(crate) fn word_boundary_backward(text: &[u8], pos: usize) -> usize {
    layout_lib::word_boundary_backward(text, pos)
}

/// Find the next word boundary (delegates to layout library).
pub(crate) fn word_boundary_forward(text: &[u8], pos: usize) -> usize {
    layout_lib::word_boundary_forward(text, pos)
}

/// Convert (visual_line, column) to byte offset (delegates to layout library).
fn line_col_to_byte(text: &[u8], target_line: usize, target_col: usize, cols: usize) -> usize {
    /// Unit-width metrics: every character has width 1.0, so max_width = chars_per_line.
    struct UnitM;
    impl layout_lib::FontMetrics for UnitM {
        fn char_width(&self, _ch: char) -> f32 {
            1.0
        }
        fn line_height(&self) -> f32 {
            1.0
        }
    }
    let max_width = cols as f32;
    layout_lib::line_col_to_byte(
        text,
        target_line,
        target_col,
        &UnitM,
        max_width,
        &layout_lib::CharBreaker,
    )
}

/// Find byte offset of start of visual line containing `pos`.
pub(crate) fn visual_line_start(text: &[u8], pos: usize, cols: usize) -> usize {
    if cols == 0 || text.is_empty() {
        return 0;
    }
    let (line, _col) = super::layout::byte_to_line_col(text, pos, cols);
    line_col_to_byte(text, line, 0, cols)
}

/// Find byte offset of end of visual line containing `pos`.
/// Points to the last character on the line (or the newline).
pub(crate) fn visual_line_end(text: &[u8], pos: usize, cols: usize) -> usize {
    if cols == 0 || text.is_empty() {
        return 0;
    }
    let (line, _col) = super::layout::byte_to_line_col(text, pos, cols);
    // Walk to start of next line and back up, or to end of text.
    let next_start = line_col_to_byte(text, line + 1, 0, cols);
    if next_start > 0 && next_start <= text.len() && text[next_start - 1] == b'\n' {
        // Line ends with newline — point to the newline position.
        next_start - 1
    } else {
        // Wrapped line or last line — point past last char.
        next_start
    }
}

/// Update selection state in CoreState from anchor + cursor_pos.
/// Returns true if sel_start/sel_end changed.
pub(crate) fn update_selection_from_anchor() -> bool {
    let s = super::state();
    let (new_start, new_end) = if s.has_selection {
        let lo = if s.anchor < s.cursor_pos {
            s.anchor
        } else {
            s.cursor_pos
        };
        let hi = if s.anchor > s.cursor_pos {
            s.anchor
        } else {
            s.cursor_pos
        };
        (lo, hi)
    } else {
        (0, 0)
    };
    let changed = s.sel_start != new_start || s.sel_end != new_end;
    s.sel_start = new_start;
    s.sel_end = new_end;
    if changed && s.doc_format == super::DocumentFormat::Rich {
        super::documents::rich_set_selection(new_start, new_end);
    }
    changed
}

/// Clear selection state.
pub(crate) fn clear_selection() {
    let s = super::state();
    s.has_selection = false;
    s.anchor = 0;
    s.sel_start = 0;
    s.sel_end = 0;
    if s.doc_format == super::DocumentFormat::Rich {
        super::documents::rich_set_selection(0, 0);
    }
}

/// Send MSG_SET_CURSOR to the editor to sync its local cursor.
pub(crate) fn sync_cursor_to_editor(editor_ch: &ipc::Channel) {
    let pos = super::state().cursor_pos;
    let cm = CursorMove {
        position: pos as u32,
    };
    // SAFETY: CursorMove is a plain data struct; from_payload copies it into payload region.
    let msg = unsafe { ipc::Message::from_payload(MSG_SET_CURSOR, &cm) };
    editor_ch.send(&msg);
}

pub(crate) fn process_key_event(
    key: &KeyEvent,
    has_image: bool,
    editor_ch: &ipc::Channel,
    fb_width: u32,
    page_w: u32,
    page_h: u32,
    page_pad: u32,
) -> KeyAction {
    use protocol::input::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};

    let no_change = KeyAction {
        changed: false,
        text_changed: false,
        selection_changed: false,
        context_switched: false,
        consumed: true,
    };

    // Ignore modifier-only key events (tracked by input driver).
    match key.keycode {
        42 | 54 | 29 | 97 | 56 | 100 | 125 | 126 | 58 => return no_change,
        _ => {}
    }

    // Only handle key presses (not releases).
    if key.pressed != 1 {
        return no_change;
    }

    let mods = key.modifiers;
    let shift = mods & MOD_SHIFT != 0;
    let ctrl = mods & MOD_CTRL != 0;
    let alt = mods & MOD_ALT != 0;
    let cmd = mods & MOD_SUPER != 0;

    // ── System keys ─────────────────────────────────────────────
    if key.keycode == KEY_TAB && ctrl {
        if has_image {
            let s = super::state();
            // Toggle active space.
            let new_space = if s.active_space == 0 { 1u8 } else { 0u8 };
            s.active_space = new_space;
            let target_pt = new_space as f32 * fb_width as f32;
            s.slide_target = scene::f32_to_mpt(target_pt);
            s.slide_spring.set_target(target_pt);
            s.slide_animating = true;
            s.slide_first_frame = true;
            return KeyAction {
                changed: true,
                text_changed: false,
                selection_changed: false,
                context_switched: true,
                consumed: true,
            };
        }
        return no_change;
    }

    // In image space, no editor keys apply.
    if super::state().active_space != 0 {
        return no_change;
    }

    // For rich text the raw buffer holds piece table bytes, not text.
    // Use logical text length and extracted text for navigation.
    let raw = doc_content();
    let is_rich = super::state().doc_format == super::DocumentFormat::Rich;
    let mut rich_scratch = alloc::vec::Vec::new();
    let (text, len): (&[u8], usize) = if is_rich {
        let tl = super::documents::rich_text_len();
        rich_scratch.resize(tl, 0u8);
        super::documents::rich_copy_text(&mut rich_scratch);
        (&rich_scratch, tl)
    } else {
        (raw, raw.len())
    };
    let layout = content_text_layout(page_w, page_pad);
    let cols = layout.cols();

    // ── Navigation helper: begin/extend selection ────────────────
    // If Shift is held, start or extend selection from current cursor.
    // If Shift is NOT held, clear any active selection (collapse).
    // Returns whether to proceed with cursor movement.
    macro_rules! nav_begin {
        () => {{
            let s = super::state();
            if shift {
                if !s.has_selection {
                    s.anchor = s.cursor_pos;
                    s.has_selection = true;
                }
            } else if s.has_selection {
                // Non-shift navigation clears selection.
                // For Left: collapse to left edge. For Right: collapse to right edge.
                // The specific collapse behavior is handled per-key below.
            }
        }};
    }

    // After cursor movement, update selection and sync editor.
    macro_rules! nav_finish {
        ($clear_goal:expr) => {{
            if $clear_goal {
                super::state().goal_column = None;
                super::state().goal_x = None;
            }
            if !shift {
                clear_selection();
            } else {
                // Collapse selection if anchor == cursor.
                let s = super::state();
                if s.anchor == s.cursor_pos {
                    clear_selection();
                }
            }
            update_selection_from_anchor();
            // Auto-update insertion style to match the style at the new cursor position
            // so typed characters inherit the surrounding style.
            if is_rich {
                let buf = super::documents::rich_buf_ref();
                let pos = super::state().cursor_pos;
                // Use style at cursor, or at cursor-1 if cursor is at a boundary.
                let at = if pos > 0 {
                    piecetable::style_at(buf, (pos - 1) as u32).unwrap_or(0)
                } else {
                    piecetable::style_at(buf, 0).unwrap_or(0)
                };
                super::documents::rich_set_current_style(at);
            }
            doc_write_header();
            sync_cursor_to_editor(editor_ch);
            let _ = sys::channel_signal(EDITOR_HANDLE);
            KeyAction {
                changed: true,
                text_changed: false,
                selection_changed: true,
                context_switched: false,
                consumed: true,
            }
        }};
    }

    match key.keycode {
        // ── Cmd+A: select all ───────────────────────────────────
        KEY_A if cmd => {
            let s = super::state();
            s.anchor = 0;
            s.cursor_pos = len;
            s.has_selection = len > 0;
            s.goal_column = None;
            s.goal_x = None;
            update_selection_from_anchor();
            doc_write_header();
            sync_cursor_to_editor(editor_ch);
            let _ = sys::channel_signal(EDITOR_HANDLE);
            KeyAction {
                changed: true,
                text_changed: false,
                selection_changed: true,
                context_switched: false,
                consumed: true,
            }
        }

        // ── Cmd+B: toggle bold ──────────────────────────────────
        KEY_B if cmd => {
            let s = super::state();
            if s.doc_format != super::DocumentFormat::Rich {
                return no_change;
            }
            let buf = super::documents::rich_buf_ref();
            let bold_id = piecetable::find_style_by_role(buf, piecetable::ROLE_STRONG).unwrap_or(0);
            if s.has_selection {
                let lo = s.sel_start;
                let hi = s.sel_end;
                let cur = piecetable::style_at(buf, lo as u32).unwrap_or(0);
                let target = if cur == bold_id { 0u8 } else { bold_id };
                super::documents::rich_apply_style(lo, hi, target);
            } else {
                let cur = piecetable::current_style(buf);
                let target = if cur == bold_id { 0u8 } else { bold_id };
                super::documents::rich_set_current_style(target);
            }
            KeyAction {
                changed: true,
                text_changed: true,
                selection_changed: false,
                context_switched: false,
                consumed: true,
            }
        }

        // ── Cmd+I: toggle italic ────────────────────────────────
        KEY_I if cmd => {
            let s = super::state();
            if s.doc_format != super::DocumentFormat::Rich {
                return no_change;
            }
            let buf = super::documents::rich_buf_ref();
            let italic_id =
                piecetable::find_style_by_role(buf, piecetable::ROLE_EMPHASIS).unwrap_or(0);
            if s.has_selection {
                let lo = s.sel_start;
                let hi = s.sel_end;
                let cur = piecetable::style_at(buf, lo as u32).unwrap_or(0);
                let target = if cur == italic_id { 0u8 } else { italic_id };
                super::documents::rich_apply_style(lo, hi, target);
            } else {
                let cur = piecetable::current_style(buf);
                let target = if cur == italic_id { 0u8 } else { italic_id };
                super::documents::rich_set_current_style(target);
            }
            KeyAction {
                changed: true,
                text_changed: true,
                selection_changed: false,
                context_switched: false,
                consumed: true,
            }
        }

        // ── Cmd+1: toggle heading1 ──────────────────────────────
        KEY_1 if cmd => {
            let s = super::state();
            if s.doc_format != super::DocumentFormat::Rich {
                return no_change;
            }
            let buf = super::documents::rich_buf_ref();
            let h1_id = piecetable::find_style_by_role(buf, piecetable::ROLE_HEADING1).unwrap_or(0);
            if s.has_selection {
                let lo = s.sel_start;
                let hi = s.sel_end;
                let cur = piecetable::style_at(buf, lo as u32).unwrap_or(0);
                let target = if cur == h1_id { 0u8 } else { h1_id };
                super::documents::rich_apply_style(lo, hi, target);
            } else {
                let cur = piecetable::current_style(buf);
                let target = if cur == h1_id { 0u8 } else { h1_id };
                super::documents::rich_set_current_style(target);
            }
            KeyAction {
                changed: true,
                text_changed: true,
                selection_changed: false,
                context_switched: false,
                consumed: true,
            }
        }

        // ── Cmd+2: toggle heading2 ──────────────────────────────
        KEY_2 if cmd => {
            let s = super::state();
            if s.doc_format != super::DocumentFormat::Rich {
                return no_change;
            }
            let buf = super::documents::rich_buf_ref();
            let h2_id = piecetable::find_style_by_role(buf, piecetable::ROLE_HEADING2).unwrap_or(0);
            if s.has_selection {
                let lo = s.sel_start;
                let hi = s.sel_end;
                let cur = piecetable::style_at(buf, lo as u32).unwrap_or(0);
                let target = if cur == h2_id { 0u8 } else { h2_id };
                super::documents::rich_apply_style(lo, hi, target);
            } else {
                let cur = piecetable::current_style(buf);
                let target = if cur == h2_id { 0u8 } else { h2_id };
                super::documents::rich_set_current_style(target);
            }
            KeyAction {
                changed: true,
                text_changed: true,
                selection_changed: false,
                context_switched: false,
                consumed: true,
            }
        }

        // ── Left arrow ──────────────────────────────────────────
        KEY_LEFT => {
            nav_begin!();
            let s = super::state();
            if cmd {
                // Cmd+Left: move to start of visual line.
                if is_rich {
                    let rl = &s.rich_lines;
                    let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                    if line < rl.len() {
                        s.cursor_pos = super::layout::rich_line_start(rl, line);
                    }
                } else {
                    s.cursor_pos = visual_line_start(text, s.cursor_pos, cols);
                }
            } else if alt {
                // Opt+Left: move to previous word boundary.
                s.cursor_pos = word_boundary_backward(text, s.cursor_pos);
            } else if !shift && s.has_selection {
                // Plain Left with selection: collapse to left edge.
                let lo = if s.anchor < s.cursor_pos {
                    s.anchor
                } else {
                    s.cursor_pos
                };
                s.cursor_pos = lo;
                clear_selection();
                doc_write_header();
                sync_cursor_to_editor(editor_ch);
                let _ = sys::channel_signal(EDITOR_HANDLE);
                return KeyAction {
                    changed: true,
                    text_changed: false,
                    selection_changed: true,
                    context_switched: false,
                    consumed: true,
                };
            } else if s.cursor_pos > 0 {
                // Move back one character (UTF-8 aware for rich text).
                if is_rich {
                    let mut p = s.cursor_pos - 1;
                    while p > 0 && text[p] & 0xC0 == 0x80 {
                        p -= 1; // Skip continuation bytes.
                    }
                    s.cursor_pos = p;
                } else {
                    s.cursor_pos -= 1;
                }
            }
            nav_finish!(true)
        }

        // ── Right arrow ─────────────────────────────────────────
        KEY_RIGHT => {
            nav_begin!();
            let s = super::state();
            if cmd {
                // Cmd+Right: move to end of visual line.
                if is_rich {
                    let rl = &s.rich_lines;
                    let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                    if line < rl.len() {
                        s.cursor_pos = super::layout::rich_line_end(rl, line);
                    }
                } else {
                    s.cursor_pos = visual_line_end(text, s.cursor_pos, cols);
                }
            } else if alt {
                // Opt+Right: move to next word boundary.
                s.cursor_pos = word_boundary_forward(text, s.cursor_pos);
            } else if !shift && s.has_selection {
                // Plain Right with selection: collapse to right edge.
                let hi = if s.anchor > s.cursor_pos {
                    s.anchor
                } else {
                    s.cursor_pos
                };
                s.cursor_pos = hi;
                clear_selection();
                doc_write_header();
                sync_cursor_to_editor(editor_ch);
                let _ = sys::channel_signal(EDITOR_HANDLE);
                return KeyAction {
                    changed: true,
                    text_changed: false,
                    selection_changed: true,
                    context_switched: false,
                    consumed: true,
                };
            } else if s.cursor_pos < len {
                // Move forward one character (UTF-8 aware for rich text).
                if is_rich {
                    let mut p = s.cursor_pos + 1;
                    while p < len && text[p] & 0xC0 == 0x80 {
                        p += 1; // Skip continuation bytes.
                    }
                    s.cursor_pos = p;
                } else {
                    s.cursor_pos += 1;
                }
            }
            nav_finish!(true)
        }

        // ── Up arrow ────────────────────────────────────────────
        KEY_UP => {
            nav_begin!();
            if cmd {
                // Cmd+Up: move to start of document.
                super::state().cursor_pos = 0;
                super::state().goal_column = None;
                super::state().goal_x = None;
            } else if is_rich {
                let rich_fonts = super::make_rich_fonts();
                let pt_buf = super::documents::rich_buf_ref();
                let s = super::state();
                let rl = &s.rich_lines;
                let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Trailing empty line: go to last real line end.
                if line >= rl.len() {
                    if !rl.is_empty() {
                        let last = rl.len() - 1;
                        s.cursor_pos = super::layout::rich_line_end(rl, last);
                    }
                } else {
                    if s.goal_x.is_none() {
                        s.goal_x = Some(super::layout::rich_cursor_x(
                            rl,
                            pt_buf,
                            text,
                            s.cursor_pos,
                            &rich_fonts,
                        ));
                    }
                    if line > 0 {
                        let gx = s.goal_x.unwrap_or(0.0);
                        s.cursor_pos = super::layout::rich_x_to_byte(
                            rl,
                            pt_buf,
                            text,
                            line - 1,
                            gx,
                            &rich_fonts,
                        );
                    }
                }
            } else {
                let s = super::state();
                let (line, col) = super::layout::byte_to_line_col(text, s.cursor_pos, cols);
                if s.goal_column.is_none() {
                    s.goal_column = Some(col);
                }
                if line > 0 {
                    let gc = s.goal_column.unwrap_or(col);
                    s.cursor_pos = line_col_to_byte(text, line - 1, gc, cols);
                }
            }
            nav_finish!(false)
        }

        // ── Down arrow ──────────────────────────────────────────
        KEY_DOWN => {
            nav_begin!();
            if cmd {
                // Cmd+Down: move to end of document.
                super::state().cursor_pos = len;
                super::state().goal_column = None;
                super::state().goal_x = None;
            } else if is_rich {
                let rich_fonts = super::make_rich_fonts();
                let pt_buf = super::documents::rich_buf_ref();
                let s = super::state();
                let rl = &s.rich_lines;
                let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Past-end or last line: move to trailing position (len).
                if line >= rl.len() {
                    // Already past end — no-op.
                } else if line + 1 >= rl.len() {
                    // On last line: move to trailing position (past all text).
                    s.cursor_pos = len;
                } else {
                    if s.goal_x.is_none() {
                        s.goal_x = Some(super::layout::rich_cursor_x(
                            rl,
                            pt_buf,
                            text,
                            s.cursor_pos,
                            &rich_fonts,
                        ));
                    }
                    let gx = s.goal_x.unwrap_or(0.0);
                    s.cursor_pos =
                        super::layout::rich_x_to_byte(rl, pt_buf, text, line + 1, gx, &rich_fonts);
                }
            } else {
                let s = super::state();
                let (line, col) = super::layout::byte_to_line_col(text, s.cursor_pos, cols);
                if s.goal_column.is_none() {
                    s.goal_column = Some(col);
                }
                let gc = s.goal_column.unwrap_or(col);
                let new_pos = line_col_to_byte(text, line + 1, gc, cols);
                // Only move if we actually reached a different line.
                if new_pos != s.cursor_pos || new_pos == len {
                    s.cursor_pos = new_pos;
                }
            }
            nav_finish!(false)
        }

        // ── Home ────────────────────────────────────────────────
        KEY_HOME => {
            nav_begin!();
            if is_rich {
                let s = super::state();
                let rl = &s.rich_lines;
                let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Past-end: no-op (cursor is on trailing empty line).
                if line < rl.len() {
                    s.cursor_pos = super::layout::rich_line_start(rl, line);
                }
            } else {
                super::state().cursor_pos =
                    visual_line_start(text, super::state().cursor_pos, cols);
            }
            nav_finish!(true)
        }

        // ── End ─────────────────────────────────────────────────
        KEY_END => {
            nav_begin!();
            if is_rich {
                let s = super::state();
                let rl = &s.rich_lines;
                let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Past-end: no-op.
                if line < rl.len() {
                    s.cursor_pos = super::layout::rich_line_end(rl, line);
                }
            } else {
                super::state().cursor_pos = visual_line_end(text, super::state().cursor_pos, cols);
            }
            nav_finish!(true)
        }

        // ── Page Up ─────────────────────────────────────────────
        KEY_PAGEUP => {
            nav_begin!();
            if is_rich {
                let rich_fonts = super::make_rich_fonts();
                let pt_buf = super::documents::rich_buf_ref();
                let s = super::state();
                let rl = &s.rich_lines;
                let mut line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Clamp past-end sentinel to last real line.
                if line >= rl.len() && !rl.is_empty() {
                    line = rl.len() - 1;
                }
                if line < rl.len() {
                    if s.goal_x.is_none() {
                        s.goal_x = Some(super::layout::rich_cursor_x(
                            rl,
                            pt_buf,
                            text,
                            s.cursor_pos,
                            &rich_fonts,
                        ));
                    }
                    let vp_h = page_h.saturating_sub(2 * page_pad) as i32;
                    let vp = super::layout::rich_viewport_lines(rl, vp_h);
                    let target = line.saturating_sub(vp);
                    let gx = s.goal_x.unwrap_or(0.0);
                    s.cursor_pos =
                        super::layout::rich_x_to_byte(rl, pt_buf, text, target, gx, &rich_fonts);
                }
            } else {
                let s = super::state();
                let (line, col) = super::layout::byte_to_line_col(text, s.cursor_pos, cols);
                if s.goal_column.is_none() {
                    s.goal_column = Some(col);
                }
                let gc = s.goal_column.unwrap_or(col);
                let vp = viewport_lines(page_h, page_pad);
                let target_line = line.saturating_sub(vp as usize);
                super::state().cursor_pos = line_col_to_byte(text, target_line, gc, cols);
            }
            nav_finish!(false)
        }

        // ── Page Down ───────────────────────────────────────────
        KEY_PAGEDOWN => {
            nav_begin!();
            if is_rich {
                let rich_fonts = super::make_rich_fonts();
                let pt_buf = super::documents::rich_buf_ref();
                let s = super::state();
                let rl = &s.rich_lines;
                let line = super::layout::rich_byte_to_line(rl, s.cursor_pos);
                // Past-end: no-op.
                if line < rl.len() {
                    if s.goal_x.is_none() {
                        s.goal_x = Some(super::layout::rich_cursor_x(
                            rl,
                            pt_buf,
                            text,
                            s.cursor_pos,
                            &rich_fonts,
                        ));
                    }
                    let vp_h = page_h.saturating_sub(2 * page_pad) as i32;
                    let vp = super::layout::rich_viewport_lines(rl, vp_h);
                    let target = (line + vp).min(rl.len().saturating_sub(1));
                    let gx = s.goal_x.unwrap_or(0.0);
                    s.cursor_pos =
                        super::layout::rich_x_to_byte(rl, pt_buf, text, target, gx, &rich_fonts);
                }
            } else {
                let s = super::state();
                let (line, col) = super::layout::byte_to_line_col(text, s.cursor_pos, cols);
                if s.goal_column.is_none() {
                    s.goal_column = Some(col);
                }
                let gc = s.goal_column.unwrap_or(col);
                let vp = viewport_lines(page_h, page_pad);
                let target_line = line + vp as usize;
                super::state().cursor_pos = line_col_to_byte(text, target_line, gc, cols);
            }
            nav_finish!(false)
        }

        // ── Backspace ───────────────────────────────────────────
        KEY_BACKSPACE => {
            let s = super::state();
            if s.has_selection {
                // Selection-delete: core handles directly.
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if delete_range_for_format(lo, hi) {
                    super::state().cursor_pos = lo;
                    doc_write_header();
                    sync_cursor_to_editor(editor_ch);
                    let _ = sys::channel_signal(EDITOR_HANDLE);
                    return KeyAction {
                        changed: true,
                        text_changed: true,
                        selection_changed: true,
                        context_switched: false,
                        consumed: true,
                    };
                }
                return no_change;
            }
            if alt {
                // Opt+Backspace: word-delete backward.
                let cursor = super::state().cursor_pos;
                let boundary = word_boundary_backward(text, cursor);
                if boundary < cursor && delete_range_for_format(boundary, cursor) {
                    super::state().cursor_pos = boundary;
                    super::state().goal_column = None;
                    super::state().goal_x = None;
                    doc_write_header();
                    sync_cursor_to_editor(editor_ch);
                    let _ = sys::channel_signal(EDITOR_HANDLE);
                    return KeyAction {
                        changed: true,
                        text_changed: true,
                        selection_changed: false,
                        context_switched: false,
                        consumed: true,
                    };
                }
                return no_change;
            }
            // Single backspace: forward to editor.
            forward_key_to_editor(key, editor_ch);
            no_change
        }

        // ── Delete (forward) ────────────────────────────────────
        KEY_DELETE => {
            let s = super::state();
            if s.has_selection {
                // Selection-delete: core handles directly.
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if delete_range_for_format(lo, hi) {
                    super::state().cursor_pos = lo;
                    doc_write_header();
                    sync_cursor_to_editor(editor_ch);
                    let _ = sys::channel_signal(EDITOR_HANDLE);
                    return KeyAction {
                        changed: true,
                        text_changed: true,
                        selection_changed: true,
                        context_switched: false,
                        consumed: true,
                    };
                }
                return no_change;
            }
            if alt {
                // Opt+Delete: word-delete forward.
                let cursor = super::state().cursor_pos;
                let boundary = word_boundary_forward(text, cursor);
                if boundary > cursor && delete_range_for_format(cursor, boundary) {
                    super::state().goal_column = None;
                    super::state().goal_x = None;
                    doc_write_header();
                    sync_cursor_to_editor(editor_ch);
                    let _ = sys::channel_signal(EDITOR_HANDLE);
                    return KeyAction {
                        changed: true,
                        text_changed: true,
                        selection_changed: false,
                        context_switched: false,
                        consumed: true,
                    };
                }
                return no_change;
            }
            // Single forward-delete: forward to editor.
            forward_key_to_editor(key, editor_ch);
            no_change
        }

        // ── All other keys: editing ─────────────────────────────
        _ => {
            // If selection is active and this is a printable char or tab,
            // delete the selection first, then forward the key.
            let s = super::state();
            if s.has_selection && (key.ascii != 0 || key.keycode == KEY_TAB) {
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if delete_range_for_format(lo, hi) {
                    super::state().cursor_pos = lo;
                    doc_write_header();
                    sync_cursor_to_editor(editor_ch);
                    // Now forward the key so editor inserts at the new cursor.
                    forward_key_to_editor(key, editor_ch);
                    return KeyAction {
                        changed: true,
                        text_changed: true,
                        selection_changed: true,
                        context_switched: false,
                        consumed: true,
                    };
                }
            }

            super::state().goal_column = None;
            super::state().goal_x = None;

            // Forward printable characters and tab to editor.
            if key.ascii != 0 || key.keycode == KEY_TAB {
                forward_key_to_editor(key, editor_ch);
            }

            KeyAction {
                changed: false,
                text_changed: false,
                selection_changed: false,
                context_switched: false,
                consumed: false,
            }
        }
    }
}

/// Forward a key event to the editor process and signal it to wake up.
pub(crate) fn forward_key_to_editor(key: &KeyEvent, editor_ch: &ipc::Channel) {
    // SAFETY: KeyEvent is a plain repr(C) struct; from_payload copies it into payload.
    let msg = unsafe { ipc::Message::from_payload(protocol::input::MSG_KEY_EVENT, key) };
    editor_ch.send(&msg);
    let _ = sys::channel_signal(EDITOR_HANDLE);
}

pub(crate) fn update_scroll_offset(page_w: u32, page_h: u32, page_pad: u32) {
    let s = super::state();
    if s.doc_format == super::DocumentFormat::Rich {
        rich_scroll_for_cursor(page_h, page_pad);
        return;
    }

    let vp_lines = viewport_lines(page_h, page_pad);

    if vp_lines == 0 {
        return;
    }

    let layout = content_text_layout(page_w, page_pad);
    let text = doc_content();
    let s = super::state();
    let cursor = s.cursor_pos;
    let current = scene::mpt_to_f32(s.scroll_offset);
    let new_scroll = layout.scroll_for_cursor(text, cursor, current, vp_lines);

    // Jump instantly to the target scroll position. Cursor-driven scroll
    // must be immediate so the eye can track the new line without chasing
    // a moving target. Spring animation reserved for future trackpad/wheel
    // inertial scrolling.
    let total_lines = layout.byte_to_visual_line(text, text.len()) + 1;
    let max_scroll = if total_lines > vp_lines {
        (total_lines - vp_lines) as f32 * s.line_h as f32
    } else {
        0.0
    };
    let clamped = clamp_f32(new_scroll, 0.0, max_scroll);

    s.scroll_offset = scene::f32_to_mpt(clamped);
    s.scroll_target = scene::f32_to_mpt(clamped);
    s.scroll_spring.reset_to(clamped);
}

/// Scroll offset for rich text — uses proportional line positions.
fn rich_scroll_for_cursor(page_h: u32, page_pad: u32) {
    let vp_h = page_h.saturating_sub(2 * page_pad) as i32;
    if vp_h <= 0 {
        return;
    }

    let s = super::state();
    let rl = &s.rich_lines;
    if rl.is_empty() {
        return;
    }
    let cursor = s.cursor_pos;
    let line_idx = super::layout::rich_byte_to_line(rl, cursor);

    // Trailing empty line: use the position just below the last real line.
    let last = &rl[rl.len() - 1];
    let (cursor_y_top, cursor_y_bottom) = if line_idx >= rl.len() {
        let y = (last.y + last.line_height) as f32;
        (y, y + last.line_height as f32)
    } else {
        let line = &rl[line_idx];
        let top = line.y as f32;
        (top, top + line.line_height as f32)
    };
    let current = scene::mpt_to_f32(s.scroll_offset);

    // Ensure cursor line is visible in viewport.
    let new_scroll = if cursor_y_top < current {
        // Cursor above viewport — scroll up.
        cursor_y_top
    } else if cursor_y_bottom > current + vp_h as f32 {
        // Cursor below viewport — scroll down so cursor bottom is at viewport bottom.
        cursor_y_bottom - vp_h as f32
    } else {
        current
    };

    // Compute max scroll from total content height.
    let total_h = (last.y + last.line_height) as f32;
    let max_scroll = (total_h - vp_h as f32).max(0.0);
    let clamped = clamp_f32(new_scroll, 0.0, max_scroll);

    let s = super::state();
    s.scroll_offset = scene::f32_to_mpt(clamped);
    s.scroll_target = scene::f32_to_mpt(clamped);
    s.scroll_spring.reset_to(clamped);
}

pub(crate) fn viewport_lines(page_h: u32, page_pad: u32) -> u32 {
    let line_h = super::state().line_h;

    if line_h == 0 {
        return 0;
    }

    let usable = page_h.saturating_sub(2 * page_pad);

    usable / line_h
}
