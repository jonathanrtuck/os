//! Core — the system's central process.
//!
//! Owns document state, understands content types, performs layout,
//! routes input, and builds the scene graph. The compositor is a
//! downstream consumer that renders the scene graph to pixels.
//!
//! # Responsibilities
//!
//! - Document buffer (sole writer)
//! - Text layout (line breaking, cursor/selection positioning)
//! - Scene graph building (writes to shared memory)
//! - Input routing (keyboard → editor, pointer → hit testing)
//! - Editor communication (receives write requests, sends input events)
//! - Clock / RTC
//! - Scroll management
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: input driver → core (keyboard events)
//! Handle 2: core → compositor (scene update signal)
//! Handle 3: core ↔ editor (input events out, write requests in)
//! Handle 4: second input device (tablet) → core (optional)

#![no_std]
#![no_main]

extern crate alloc;
extern crate animation;
extern crate fonts;
extern crate layout as layout_lib;
extern crate scene;

#[path = "fallback.rs"]
mod fallback;
#[path = "layout/mod.rs"]
mod layout;
#[path = "scene_state.rs"]
mod scene_state;
#[path = "test_gen.rs"]
mod test_gen;
#[path = "typography.rs"]
mod typography;

use protocol::{
    compose::{ImageConfig, RtcConfig, MSG_IMAGE_CONFIG, MSG_RTC_CONFIG},
    core_config::{CoreConfig, MSG_CORE_CONFIG, MSG_SCENE_UPDATED},
    edit::{
        CursorMove, SelectionUpdate, WriteDelete, WriteDeleteRange, WriteInsert, MSG_CURSOR_MOVE,
        MSG_SELECTION_UPDATE, MSG_SET_CURSOR, MSG_WRITE_DELETE, MSG_WRITE_DELETE_RANGE,
        MSG_WRITE_INSERT,
    },
    input::{
        KeyEvent, PointerAbs, PointerButton, MSG_KEY_EVENT, MSG_POINTER_ABS, MSG_POINTER_BUTTON,
    },
};

/// Clamp a float to [min, max]. Manual implementation for `no_std`.
#[inline]
fn clamp_f32(x: f32, min: f32, max: f32) -> f32 {
    if x < min {
        min
    } else if x > max {
        max
    } else {
        x
    }
}

/// Round a float to the nearest integer (round-half-away-from-zero).
/// Delegates to the canonical implementation in `drawing`.
#[inline]
fn round_f32(x: f32) -> i32 {
    drawing::round_f32(x)
}

const COMPOSITOR_HANDLE: u8 = 2;
const DOC_HEADER_SIZE: usize = 64;
const EDITOR_HANDLE: u8 = 3;
const FONT_SIZE: u32 = 18;
const INPUT_HANDLE: u8 = 1;
const INPUT2_HANDLE: u8 = 4;
// Keycodes (Linux evdev).
const KEY_BACKSPACE: u16 = 14;
const KEY_TAB: u16 = 15;
const KEY_A: u16 = 30;
const KEY_LEFTCTRL: u16 = 29;
const KEY_UP: u16 = 103;
const KEY_DOWN: u16 = 108;
const KEY_LEFT: u16 = 105;
const KEY_RIGHT: u16 = 106;
const KEY_HOME: u16 = 102;
const KEY_END: u16 = 107;
const KEY_PAGEUP: u16 = 104;
const KEY_PAGEDOWN: u16 = 109;
const KEY_DELETE: u16 = 111;
const SHADOW_DEPTH: u32 = 0;
const TEXT_INSET_BOTTOM: u32 = 8;
const TEXT_INSET_TOP: u32 = TITLE_BAR_H + SHADOW_DEPTH + 8;
const TEXT_INSET_X: u32 = 12;
const TITLE_BAR_H: u32 = 36;

// ── Cursor blink state machine ──────────────────────────────────────

/// Phase of the cursor blink cycle: visible hold → fade out → hidden hold → fade in.
#[derive(Clone, Copy, PartialEq)]
enum BlinkPhase {
    /// Cursor fully visible for 500ms.
    VisibleHold,
    /// Fading from opacity 255→0 over 150ms.
    FadeOut,
    /// Cursor fully hidden for 300ms.
    HiddenHold,
    /// Fading from opacity 0→255 over 150ms.
    FadeIn,
}

/// Duration of each blink phase in milliseconds.
const BLINK_VISIBLE_MS: u64 = 500;
const BLINK_FADE_OUT_MS: u64 = 150;
const BLINK_HIDDEN_MS: u64 = 300;
const BLINK_FADE_IN_MS: u64 = 150;

/// Advance the blink state machine. Returns `true` if `cursor_opacity` changed.
fn advance_blink(state: &mut CoreState, now_ms: u64) -> bool {
    let elapsed = now_ms.saturating_sub(state.blink_phase_start_ms);
    let mut changed = false;

    match state.blink_phase {
        BlinkPhase::VisibleHold => {
            state.cursor_opacity = 255;
            if elapsed >= BLINK_VISIBLE_MS {
                state.cursor_blink_id = state
                    .timeline
                    .start(255.0, 0.0, 150, animation::Easing::EaseInOut, now_ms)
                    .ok();
                state.blink_phase = BlinkPhase::FadeOut;
                state.blink_phase_start_ms = now_ms;
                changed = true;
            }
        }
        BlinkPhase::FadeOut => {
            if let Some(id) = state.cursor_blink_id {
                let new_opacity = if state.timeline.is_active(id) {
                    state.timeline.value(id) as u8
                } else {
                    0
                };
                if new_opacity != state.cursor_opacity {
                    state.cursor_opacity = new_opacity;
                    changed = true;
                }
            }
            if elapsed >= BLINK_FADE_OUT_MS {
                state.blink_phase = BlinkPhase::HiddenHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 0;
                changed = true;
            }
        }
        BlinkPhase::HiddenHold => {
            state.cursor_opacity = 0;
            if elapsed >= BLINK_HIDDEN_MS {
                state.cursor_blink_id = state
                    .timeline
                    .start(0.0, 255.0, 150, animation::Easing::EaseInOut, now_ms)
                    .ok();
                state.blink_phase = BlinkPhase::FadeIn;
                state.blink_phase_start_ms = now_ms;
                changed = true;
            }
        }
        BlinkPhase::FadeIn => {
            if let Some(id) = state.cursor_blink_id {
                let new_opacity = if state.timeline.is_active(id) {
                    state.timeline.value(id) as u8
                } else {
                    255
                };
                if new_opacity != state.cursor_opacity {
                    state.cursor_opacity = new_opacity;
                    changed = true;
                }
            }
            if elapsed >= BLINK_FADE_IN_MS {
                state.blink_phase = BlinkPhase::VisibleHold;
                state.blink_phase_start_ms = now_ms;
                state.cursor_opacity = 255;
                changed = true;
            }
        }
    }
    changed
}

/// Reset blink to fully visible (called on user input).
fn reset_blink(state: &mut CoreState, now_ms: u64) {
    if let Some(id) = state.cursor_blink_id {
        state.timeline.cancel(id);
    }
    state.blink_phase = BlinkPhase::VisibleHold;
    state.blink_phase_start_ms = now_ms;
    state.cursor_opacity = 255;
}

struct CoreState {
    blink_phase: BlinkPhase,
    blink_phase_start_ms: u64,
    boot_counter: u64,
    /// Character advance in 16.16 fixed-point points.
    /// Single source of truth — same precision as scene ShapedGlyph advances.
    char_w_fx: i32,
    counter_freq: u64,
    cursor_blink_id: Option<animation::AnimationId>,
    cursor_opacity: u8,
    cursor_pos: usize,
    doc_buf: *mut u8,
    doc_capacity: usize,
    doc_len: usize,
    /// Animation ID for the document switch fade-out (255→0).
    fade_out_id: Option<animation::AnimationId>,
    /// Animation ID for the document switch fade-in (0→255).
    fade_in_id: Option<animation::AnimationId>,
    font_data_ptr: *const u8,
    font_data_len: usize,
    font_upem: u16,
    image_mode: bool,
    line_h: u32,
    mouse_x: u32,
    mouse_y: u32,
    /// True while fading out before a document context switch.
    pending_context_switch: bool,
    /// Animation ID for the pointer fade-out (255→0, 300ms EaseOut).
    pointer_fade_id: Option<animation::AnimationId>,
    /// Timestamp (ms) of the last pointer movement event.
    pointer_last_event_ms: u64,
    /// Current pointer cursor opacity (0 = hidden, 255 = fully visible).
    pointer_opacity: u8,
    /// True when the pointer cursor is currently shown (recently moved).
    pointer_visible: bool,
    /// Root node opacity for document switch fade transitions.
    root_opacity: u8,
    rtc_mmio_va: usize,
    saved_editor_scroll: f32,
    scroll_animating: bool,
    scroll_offset: f32,
    scroll_spring: animation::Spring,
    scroll_target: f32,
    sel_end: usize,
    /// Selection anchor: the fixed end of a selection range. When
    /// `has_selection` is true, the visible range is
    /// `[min(anchor, cursor_pos), max(anchor, cursor_pos))`.
    anchor: usize,
    /// True when a selection is active.
    has_selection: bool,
    /// Sticky goal column for Up/Down navigation. Preserved across
    /// consecutive vertical moves, cleared on any horizontal move.
    goal_column: Option<usize>,
    /// Click state for double/triple-click detection.
    last_click_ms: u64,
    last_click_x: u32,
    last_click_y: u32,
    click_count: u8,
    /// Animation ID for the selection highlight fade-in (0→255).
    selection_fade_id: Option<animation::AnimationId>,
    /// Current selection highlight opacity (animated on selection change).
    selection_opacity: u8,
    sel_start: usize,
    timeline: animation::Timeline,
    timer_active: bool,
    timer_handle: u8,
}

impl CoreState {
    const fn new() -> Self {
        Self {
            blink_phase: BlinkPhase::VisibleHold,
            blink_phase_start_ms: 0,
            boot_counter: 0,
            char_w_fx: 8 * 65536,
            counter_freq: 0,
            cursor_blink_id: None,
            cursor_opacity: 255,
            cursor_pos: 0,
            doc_buf: core::ptr::null_mut(),
            doc_capacity: 0,
            doc_len: 0,
            fade_out_id: None,
            fade_in_id: None,
            font_data_ptr: core::ptr::null(),
            font_data_len: 0,
            font_upem: 1000,
            image_mode: false,
            line_h: 20,
            mouse_x: 0,
            mouse_y: 0,
            pending_context_switch: false,
            pointer_fade_id: None,
            pointer_last_event_ms: 0,
            pointer_opacity: 0,
            pointer_visible: false,
            root_opacity: 255,
            rtc_mmio_va: 0,
            saved_editor_scroll: 0.0,
            scroll_animating: false,
            scroll_offset: 0.0,
            scroll_spring: animation::Spring::snappy(0.0),
            scroll_target: 0.0,
            sel_end: 0,
            anchor: 0,
            has_selection: false,
            goal_column: None,
            last_click_ms: 0,
            last_click_x: 0,
            last_click_y: 0,
            click_count: 0,
            selection_fade_id: None,
            selection_opacity: 255,
            sel_start: 0,
            timeline: animation::Timeline::new(),
            timer_active: false,
            timer_handle: 0,
        }
    }
}

struct SyncState(core::cell::UnsafeCell<CoreState>);
// SAFETY: Single-threaded userspace process.
unsafe impl Sync for SyncState {}
static STATE: SyncState = SyncState(core::cell::UnsafeCell::new(CoreState::new()));

fn state() -> &'static mut CoreState {
    // SAFETY: Single-threaded userspace process. No concurrent access.
    unsafe { &mut *STATE.0.get() }
}

struct KeyAction {
    changed: bool,
    text_changed: bool,
    selection_changed: bool,
    context_switched: bool,
    consumed: bool,
}

/// Access the font data slice from shared memory.
fn font_data() -> &'static [u8] {
    let s = state();
    if s.font_data_ptr.is_null() || s.font_data_len == 0 {
        &[]
    } else {
        // SAFETY: font_data_ptr points to font_data_len bytes of shared memory.
        unsafe { core::slice::from_raw_parts(s.font_data_ptr, s.font_data_len) }
    }
}
fn clock_seconds() -> u64 {
    let s = state();
    let rtc_va = s.rtc_mmio_va;

    if rtc_va != 0 {
        // SAFETY: rtc_va points to memory-mapped PL031 RTC register.
        let epoch = unsafe { core::ptr::read_volatile(rtc_va as *const u32) };

        epoch as u64
    } else {
        let now = sys::counter();
        let boot = s.boot_counter;
        let freq = s.counter_freq;

        if freq == 0 {
            return 0;
        }

        (now - boot) / freq
    }
}
/// Fixed-pitch text layout engine.
///
/// Computes line breaks (hard newlines + soft wrap at max width), cursor
/// mapping (byte offset to/from pixel coordinates), and scroll management.
/// Pure computation — no allocations, no side effects.
struct TextLayout {
    /// Character advance in 16.16 fixed-point points.
    /// This is the single source of truth for character width — derived
    /// from the same formula as scene graph ShapedGlyph advances.
    char_width_fx: i32,
    line_height: u32,
    max_width: u32,
}

impl TextLayout {
    fn cols(&self) -> usize {
        if self.char_width_fx == 0 {
            return 0;
        }
        // chars_per_line = floor(max_width / char_width) using 16.16 math.
        ((self.max_width as i64 * 65536) / self.char_width_fx as i64) as usize
    }

    /// Character advance as f32 points (for APIs that need it).
    fn char_width_pt(&self) -> f32 {
        self.char_width_fx as f32 / 65536.0
    }

    /// Return the visual line number (0-based) for a given byte offset.
    /// Delegates to `byte_to_line_col` for a single wrapping implementation.
    fn byte_to_visual_line(&self, text: &[u8], offset: usize) -> u32 {
        let cols = self.cols();
        if cols == 0 || text.is_empty() {
            return 0;
        }
        let target = if offset > text.len() {
            text.len()
        } else {
            offset
        };
        let (line, _col) = scene_state::byte_to_line_col(text, target, cols);
        line as u32
    }

    /// Compute the scroll offset (in pixels) needed to keep the cursor visible.
    fn scroll_for_cursor(
        &self,
        text: &[u8],
        cursor_offset: usize,
        current_scroll: f32,
        viewport_lines: u32,
    ) -> f32 {
        if viewport_lines == 0 || self.line_height == 0 {
            return 0.0;
        }

        let cursor_line = self.byte_to_visual_line(text, cursor_offset);
        let cursor_pt = cursor_line as f32 * self.line_height as f32;
        let viewport_pt = viewport_lines as f32 * self.line_height as f32;

        if cursor_pt < current_scroll {
            return cursor_pt;
        }

        let last_visible_top = current_scroll + viewport_pt - self.line_height as f32;

        if cursor_pt > last_visible_top {
            return cursor_pt - viewport_pt + self.line_height as f32;
        }

        current_scroll
    }

    /// Map pixel coordinates to a byte offset (hit testing).
    fn xy_to_byte(&self, text: &[u8], x: u32, y: u32) -> usize {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return 0;
        }

        let target_row = y / self.line_height;
        // Use fractional char width for click-to-column mapping.
        let cw_pt = self.char_width_pt();
        let target_col = if cw_pt > 0.0 {
            ((x as f32 + cw_pt * 0.5) / cw_pt) as u32
        } else {
            0
        };
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                if row == target_row {
                    return i;
                }

                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            if row == target_row && col >= target_col as usize {
                return i;
            }
            if row > target_row {
                return i;
            }

            col += 1;
        }

        text.len()
    }
}

fn content_text_layout(content_w: u32) -> TextLayout {
    let s = state();
    TextLayout {
        char_width_fx: s.char_w_fx,
        line_height: s.line_h,
        max_width: content_w.saturating_sub(2 * TEXT_INSET_X),
    }
}
fn create_clock_timer() -> bool {
    let s = state();
    let freq = s.counter_freq;
    let timeout_ns = if freq > 0 {
        let now = sys::counter();
        let boot = s.boot_counter;
        let elapsed_ticks = now - boot;
        let ticks_this_second = elapsed_ticks % freq;
        let remaining_ticks = freq - ticks_this_second;

        (remaining_ticks as u128 * 1_000_000_000 / freq as u128) as u64
    } else {
        1_000_000_000
    };
    let timeout_ns = if timeout_ns < 10_000_000 {
        1_000_000_000
    } else {
        timeout_ns
    };

    match sys::timer_create(timeout_ns) {
        Ok(handle) => {
            let s = state();
            s.timer_handle = handle;
            s.timer_active = true;
            true
        }
        Err(_) => {
            state().timer_active = false;
            false
        }
    }
}
fn doc_content() -> &'static [u8] {
    let s = state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    // doc_len is always <= doc_capacity - DOC_HEADER_SIZE (maintained by
    // doc_insert/doc_delete/doc_delete_range). doc_buf is set once during
    // init and never null after that point.
    unsafe {
        debug_assert!(!s.doc_buf.is_null());
        debug_assert!(s.doc_len <= s.doc_capacity);
        core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), s.doc_len)
    }
}
fn doc_delete(pos: usize) -> bool {
    let s = state();
    if s.doc_len == 0 || pos >= s.doc_len {
        return false;
    }
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos + 1 < s.doc_len {
            core::ptr::copy(base.add(pos + 1), base.add(pos), s.doc_len - pos - 1);
        }
    }
    s.doc_len -= 1;
    doc_write_header();
    true
}
fn doc_delete_range(start: usize, end: usize) -> bool {
    let s = state();
    if start >= end || start >= s.doc_len || end > s.doc_len {
        return false;
    }
    let del_count = end - start;
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if end < s.doc_len {
            core::ptr::copy(base.add(end), base.add(start), s.doc_len - end);
        }
    }
    s.doc_len -= del_count;
    doc_write_header();
    true
}
fn doc_insert(pos: usize, byte: u8) -> bool {
    let s = state();
    if s.doc_len >= s.doc_capacity || pos > s.doc_len {
        return false;
    }
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        let base = s.doc_buf.add(DOC_HEADER_SIZE);
        if pos < s.doc_len {
            core::ptr::copy(base.add(pos), base.add(pos + 1), s.doc_len - pos);
        }
        *base.add(pos) = byte;
    }
    s.doc_len += 1;
    doc_write_header();
    true
}
fn doc_write_header() {
    let s = state();
    // SAFETY: doc_buf points to doc_capacity bytes of shared memory.
    unsafe {
        core::ptr::write_volatile(s.doc_buf as *mut u64, s.doc_len as u64);
        core::ptr::write_volatile(s.doc_buf.add(8) as *mut u64, s.cursor_pos as u64);
    }
}
fn format_time_hms(total_seconds: u64, buf: &mut [u8; 8]) {
    let hours = ((total_seconds / 3600) % 24) as u8;
    let minutes = ((total_seconds / 60) % 60) as u8;
    let seconds = (total_seconds % 60) as u8;

    buf[0] = b'0' + hours / 10;
    buf[1] = b'0' + hours % 10;
    buf[2] = b':';
    buf[3] = b'0' + minutes / 10;
    buf[4] = b'0' + minutes % 10;
    buf[5] = b':';
    buf[6] = b'0' + seconds / 10;
    buf[7] = b'0' + seconds % 10;
}
// ── Navigation helpers ─────────────────────────────────────────────

/// Find the previous word boundary (delegates to layout library).
fn word_boundary_backward(text: &[u8], pos: usize) -> usize {
    layout_lib::word_boundary_backward(text, pos)
}

/// Find the next word boundary (delegates to layout library).
fn word_boundary_forward(text: &[u8], pos: usize) -> usize {
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
fn visual_line_start(text: &[u8], pos: usize, cols: usize) -> usize {
    if cols == 0 || text.is_empty() {
        return 0;
    }
    let (line, _col) = layout::byte_to_line_col(text, pos, cols);
    line_col_to_byte(text, line, 0, cols)
}

/// Find byte offset of end of visual line containing `pos`.
/// Points to the last character on the line (or the newline).
fn visual_line_end(text: &[u8], pos: usize, cols: usize) -> usize {
    if cols == 0 || text.is_empty() {
        return 0;
    }
    let (line, _col) = layout::byte_to_line_col(text, pos, cols);
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
fn update_selection_from_anchor() -> bool {
    let s = state();
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
    changed
}

/// Clear selection state.
fn clear_selection() {
    let s = state();
    s.has_selection = false;
    s.anchor = 0;
    s.sel_start = 0;
    s.sel_end = 0;
}

/// Send MSG_SET_CURSOR to the editor to sync its local cursor.
fn sync_cursor_to_editor(editor_ch: &ipc::Channel) {
    let pos = state().cursor_pos;
    let cm = CursorMove {
        position: pos as u32,
    };
    // SAFETY: CursorMove is a plain data struct; from_payload copies it into payload region.
    let msg = unsafe { ipc::Message::from_payload(MSG_SET_CURSOR, &cm) };
    editor_ch.send(&msg);
}

fn process_key_event(
    key: &KeyEvent,
    has_image: bool,
    editor_ch: &ipc::Channel,
    content_w: u32,
    content_h: u32,
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
            let s = state();
            if !s.pending_context_switch {
                let now_ms = {
                    let freq = s.counter_freq;
                    if freq > 0 {
                        sys::counter() * 1000 / freq
                    } else {
                        0
                    }
                };
                s.fade_out_id = s
                    .timeline
                    .start(255.0, 0.0, 120, animation::Easing::EaseOut, now_ms)
                    .ok();
                s.pending_context_switch = true;
            }
            return KeyAction {
                changed: true,
                text_changed: false,
                selection_changed: false,
                context_switched: false,
                consumed: true,
            };
        }
        return no_change;
    }

    // In image mode, no editor keys apply.
    if state().image_mode {
        return no_change;
    }

    let text = doc_content();
    let len = text.len();
    let layout = content_text_layout(content_w);
    let cols = layout.cols();

    // ── Navigation helper: begin/extend selection ────────────────
    // If Shift is held, start or extend selection from current cursor.
    // If Shift is NOT held, clear any active selection (collapse).
    // Returns whether to proceed with cursor movement.
    macro_rules! nav_begin {
        () => {{
            let s = state();
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
                state().goal_column = None;
            }
            if !shift {
                clear_selection();
            } else {
                // Collapse selection if anchor == cursor.
                let s = state();
                if s.anchor == s.cursor_pos {
                    clear_selection();
                }
            }
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
        }};
    }

    match key.keycode {
        // ── Cmd+A: select all ───────────────────────────────────
        KEY_A if cmd => {
            let s = state();
            s.anchor = 0;
            s.cursor_pos = len;
            s.has_selection = len > 0;
            s.goal_column = None;
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

        // ── Left arrow ──────────────────────────────────────────
        KEY_LEFT => {
            nav_begin!();
            let s = state();
            if cmd {
                // Cmd+Left: move to start of visual line.
                s.cursor_pos = visual_line_start(text, s.cursor_pos, cols);
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
                s.cursor_pos -= 1;
            }
            nav_finish!(true)
        }

        // ── Right arrow ─────────────────────────────────────────
        KEY_RIGHT => {
            nav_begin!();
            let s = state();
            if cmd {
                // Cmd+Right: move to end of visual line.
                s.cursor_pos = visual_line_end(text, s.cursor_pos, cols);
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
                s.cursor_pos += 1;
            }
            nav_finish!(true)
        }

        // ── Up arrow ────────────────────────────────────────────
        KEY_UP => {
            nav_begin!();
            if cmd {
                // Cmd+Up: move to start of document.
                state().cursor_pos = 0;
                state().goal_column = None;
            } else {
                let s = state();
                let (line, col) = layout::byte_to_line_col(text, s.cursor_pos, cols);
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
                state().cursor_pos = len;
                state().goal_column = None;
            } else {
                let s = state();
                let (line, col) = layout::byte_to_line_col(text, s.cursor_pos, cols);
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
            state().cursor_pos = visual_line_start(text, state().cursor_pos, cols);
            nav_finish!(true)
        }

        // ── End ─────────────────────────────────────────────────
        KEY_END => {
            nav_begin!();
            state().cursor_pos = visual_line_end(text, state().cursor_pos, cols);
            nav_finish!(true)
        }

        // ── Page Up ─────────────────────────────────────────────
        KEY_PAGEUP => {
            nav_begin!();
            let s = state();
            let (line, col) = layout::byte_to_line_col(text, s.cursor_pos, cols);
            if s.goal_column.is_none() {
                s.goal_column = Some(col);
            }
            let gc = s.goal_column.unwrap_or(col);
            let vp = viewport_lines(content_h);
            let target_line = line.saturating_sub(vp as usize);
            state().cursor_pos = line_col_to_byte(text, target_line, gc, cols);
            nav_finish!(false)
        }

        // ── Page Down ───────────────────────────────────────────
        KEY_PAGEDOWN => {
            nav_begin!();
            let s = state();
            let (line, col) = layout::byte_to_line_col(text, s.cursor_pos, cols);
            if s.goal_column.is_none() {
                s.goal_column = Some(col);
            }
            let gc = s.goal_column.unwrap_or(col);
            let vp = viewport_lines(content_h);
            let target_line = line + vp as usize;
            state().cursor_pos = line_col_to_byte(text, target_line, gc, cols);
            nav_finish!(false)
        }

        // ── Backspace ───────────────────────────────────────────
        KEY_BACKSPACE => {
            let s = state();
            if s.has_selection {
                // Selection-delete: core handles directly.
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if doc_delete_range(lo, hi) {
                    state().cursor_pos = lo;
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
                let cursor = state().cursor_pos;
                let boundary = word_boundary_backward(text, cursor);
                if boundary < cursor && doc_delete_range(boundary, cursor) {
                    state().cursor_pos = boundary;
                    state().goal_column = None;
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
            let s = state();
            if s.has_selection {
                // Selection-delete: core handles directly.
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if doc_delete_range(lo, hi) {
                    state().cursor_pos = lo;
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
                let cursor = state().cursor_pos;
                let boundary = word_boundary_forward(text, cursor);
                if boundary > cursor && doc_delete_range(cursor, boundary) {
                    state().goal_column = None;
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
            let s = state();
            if s.has_selection && (key.ascii != 0 || key.keycode == KEY_TAB) {
                let lo = s.sel_start;
                let hi = s.sel_end;
                clear_selection();
                if doc_delete_range(lo, hi) {
                    state().cursor_pos = lo;
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

            state().goal_column = None;

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

/// Forward a key event to the editor process.
/// Forward a key event to the editor process and signal it to wake up.
fn forward_key_to_editor(key: &KeyEvent, editor_ch: &ipc::Channel) {
    // SAFETY: KeyEvent is a plain repr(C) struct; from_payload copies it into payload.
    let msg = unsafe { ipc::Message::from_payload(MSG_KEY_EVENT, key) };
    editor_ch.send(&msg);
    let _ = sys::channel_signal(EDITOR_HANDLE);
}
fn update_scroll_offset(content_w: u32, content_h: u32) {
    let vp_lines = viewport_lines(content_h);

    if vp_lines == 0 {
        return;
    }

    let layout = content_text_layout(content_w);
    let text = doc_content();
    let s = state();
    let cursor = s.cursor_pos;
    let current = s.scroll_offset;
    let new_scroll = layout.scroll_for_cursor(text, cursor, current, vp_lines);

    // Drive scroll changes through the spring instead of jumping instantly.
    // Clamp to valid scroll range (allow 50pt overscroll for bounce effect).
    let total_lines = layout.byte_to_visual_line(text, text.len()) + 1;
    let max_scroll = if total_lines > vp_lines {
        (total_lines - vp_lines) as f32 * s.line_h as f32
    } else {
        0.0
    };
    let clamped = clamp_f32(new_scroll, -50.0, max_scroll + 50.0);

    let diff = s.scroll_target - clamped;
    let abs_diff = if diff < 0.0 { -diff } else { diff };
    if abs_diff > 0.5 {
        s.scroll_target = clamped;
        s.scroll_spring.set_target(clamped);
        s.scroll_animating = true;
    }
}
fn viewport_lines(content_h: u32) -> u32 {
    let line_h = state().line_h;

    if line_h == 0 {
        return 0;
    }

    let usable = content_h.saturating_sub(TEXT_INSET_TOP + TEXT_INSET_BOTTOM);

    usable / line_h
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    {
        let s = state();
        s.boot_counter = sys::counter();
        s.counter_freq = sys::counter_freq();
    }

    sys::print(b"  \xF0\x9F\xA7\xA0 core - starting\n");

    // Read core config from init channel.
    // SAFETY: channel_shm_va(0) is the base of the init channel SHM region mapped by the kernel;
    // alignment guaranteed by page-boundary allocation.
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_CORE_CONFIG {
        sys::print(b"core: no config message\n");
        sys::exit();
    }

    // SAFETY: msg.msg_type is MSG_CORE_CONFIG; sender (init) guarantees payload is a valid CoreConfig.
    let config: CoreConfig = unsafe { msg.payload_as() };
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;

    if config.doc_va == 0 || config.scene_va == 0 {
        sys::print(b"core: bad config\n");
        sys::exit();
    }

    {
        let s = state();
        s.doc_buf = config.doc_va as *mut u8;
        s.doc_capacity = config.doc_capacity as usize;
        s.doc_len = 0;
    }
    doc_write_header();

    // Parse font to get metrics (char_width, line_height).
    // Core only needs metrics for layout, not the full glyph cache.
    if config.font_buf_va != 0 && config.mono_font_len > 0 {
        // SAFETY: font_buf_va..+mono_font_len is within the font shared memory region mapped
        // by init; alignment is 1 (u8 slice). Guarded by the non-null/non-zero checks above.
        let font_data = unsafe {
            core::slice::from_raw_parts(
                config.font_buf_va as *const u8,
                config.mono_font_len as usize,
            )
        };
        // Store font data pointer and length for shaping calls.
        {
            let s = state();
            s.font_data_ptr = config.font_buf_va as *const u8;
            s.font_data_len = config.mono_font_len as usize;
        }
        if let Some(fm) = fonts::rasterize::font_metrics(font_data) {
            let upem = fm.units_per_em;
            state().font_upem = upem;
            let asc = fm.ascent as i32;
            let desc = fm.descent as i32;
            let gap = fm.line_gap as i32;
            let size = FONT_SIZE;
            let ascent_pt = ((asc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let descent_pt = ((-desc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let gap_pt = if gap > 0 {
                (gap * size as i32 / upem as i32) as u32
            } else {
                0
            };
            let line_h = ascent_pt + descent_pt + gap_pt;
            // For monospace: use advance of space glyph in 16.16 fixed-point.
            // Same formula as shape_text: (advance_fu * point_size * 65536) / upem.
            let space_gid = fonts::rasterize::glyph_id_for_char(font_data, ' ').unwrap_or(0);
            let (advance_fu, _) =
                fonts::rasterize::glyph_h_metrics(font_data, space_gid).unwrap_or((0, 0));
            let char_w_fx = (advance_fu as i64 * size as i64 * 65536 / upem as i64) as i32;

            {
                let s = state();
                s.char_w_fx = if char_w_fx > 0 { char_w_fx } else { 8 * 65536 };
                s.line_h = if line_h > 0 { line_h } else { 20 };
            }

            sys::print(b"     font metrics loaded\n");
        } else {
            sys::print(b"     warning: font parse failed, using defaults\n");
        }
    }

    // Check for image data (used for Ctrl+Tab image viewer mode detection).
    let mut has_image = false;

    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMAGE_CONFIG {
        // SAFETY: msg.msg_type is MSG_IMAGE_CONFIG; sender (init) guarantees payload is a valid ImageConfig.
        let img_config: ImageConfig = unsafe { msg.payload_as() };

        if img_config.image_va != 0 && img_config.image_len > 0 {
            has_image = true;
        }
    }

    // Check for RTC config.
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_RTC_CONFIG {
        // SAFETY: msg.msg_type is MSG_RTC_CONFIG; sender (init) guarantees payload is a valid RtcConfig.
        let rtc_config: RtcConfig = unsafe { msg.payload_as() };

        if rtc_config.mmio_pa != 0 {
            match sys::device_map(rtc_config.mmio_pa, 4096) {
                Ok(va) => {
                    state().rtc_mmio_va = va;
                    sys::print(b"     pl031 rtc mapped\n");
                }
                Err(_) => {
                    sys::print(b"     pl031 rtc map failed\n");
                }
            }
        }
    }

    // Set up IPC channels.
    // SAFETY: channel_shm_va(1..3) are bases of channel SHM regions mapped by the kernel;
    // alignment guaranteed by page-boundary allocation.
    let input_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    let compositor_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    let editor_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(3), ipc::PAGE_SIZE, 0) };
    let has_input2 = match sys::wait(&[INPUT2_HANDLE], 0) {
        Ok(_) => true,
        Err(sys::SyscallError::WouldBlock) => true,
        _ => false,
    };
    let input2_ch = if has_input2 {
        sys::print(b"     tablet input channel detected\n");
        // SAFETY: same invariant as channel_shm_va(1..3) from_base above.
        Some(unsafe { ipc::Channel::from_base(protocol::channel_shm_va(4), ipc::PAGE_SIZE, 1) })
    } else {
        None
    };
    // Content area dimensions (for layout).
    let content_w = fb_width;
    let content_h = fb_height;
    // Scene graph in shared memory.
    // SAFETY: scene_va..+TRIPLE_SCENE_SIZE is within the scene SHM region mapped by init;
    // alignment is 1 (u8 slice). scene_va validated non-zero above.
    let scene_buf = unsafe {
        core::slice::from_raw_parts_mut(config.scene_va as *mut u8, scene::TRIPLE_SCENE_SIZE)
    };
    let mut scene = scene_state::SceneState::from_buf(scene_buf);
    // Build initial scene.
    let mut time_buf = [0u8; 8];

    format_time_hms(clock_seconds(), &mut time_buf);

    let scene_cfg = {
        let s = state();
        scene_state::SceneConfig {
            fb_width,
            fb_height,
            title_bar_h: TITLE_BAR_H,
            shadow_depth: SHADOW_DEPTH,
            text_inset_x: TEXT_INSET_X,
            chrome_bg: drawing::CHROME_BG,
            chrome_border: drawing::CHROME_BORDER,
            chrome_title_color: drawing::CHROME_TITLE,
            chrome_clock_color: drawing::CHROME_CLOCK,
            bg_color: drawing::BG_BASE,
            text_color: drawing::TEXT_PRIMARY,
            cursor_color: drawing::TEXT_CURSOR,
            sel_color: drawing::TEXT_SELECTION,
            font_size: FONT_SIZE as u16,
            char_width_fx: s.char_w_fx,
            line_height: s.line_h,
            font_data: font_data(),
            upem: s.font_upem,
            axes: &[],
        }
    };

    {
        let s = state();
        scene.build_editor_scene(
            &scene_cfg,
            doc_content(),
            s.cursor_pos as u32,
            s.sel_start as u32,
            s.sel_end as u32,
            b"Text",
            &time_buf,
            0.0,
            s.cursor_opacity,
            s.mouse_x,
            s.mouse_y,
            s.pointer_opacity,
            false,
        );
    }

    // Signal compositor that first frame is ready.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);

    compositor_ch.send(&scene_msg);

    let _ = sys::channel_signal(COMPOSITOR_HANDLE);

    create_clock_timer();

    // Track line count for incremental scene updates.
    let mut prev_line_count = scene_state::count_lines(doc_content());

    sys::print(b"     entering event loop\n");

    // ctrl_pressed removed — modifier state now in KeyEvent.modifiers.

    loop {
        let timer_active = state().timer_active;
        let timer_handle = state().timer_handle;
        // Compute wait timeout from active animations and blink phase.
        //
        // Scroll animation: 16ms (60fps) while active.
        // Blink fade (FadeOut/FadeIn): 16ms for smooth opacity changes.
        // Blink holds (VisibleHold/HiddenHold): sleep until next phase transition.
        let now_ms = {
            let s = state();
            let freq = s.counter_freq;
            if freq > 0 {
                sys::counter() * 1000 / freq
            } else {
                0
            }
        };
        let scroll_timeout_ns: u64 = if state().scroll_animating {
            16_000_000 // 16ms ~ 60fps
        } else {
            u64::MAX
        };
        let blink_timeout_ns: u64 = {
            let s = state();
            if s.timeline.any_active() {
                16_000_000 // 16ms for smooth fade animation
            } else {
                let elapsed = now_ms.saturating_sub(s.blink_phase_start_ms);
                let remaining_ms = match s.blink_phase {
                    BlinkPhase::VisibleHold => BLINK_VISIBLE_MS.saturating_sub(elapsed),
                    BlinkPhase::FadeOut => BLINK_FADE_OUT_MS.saturating_sub(elapsed),
                    BlinkPhase::HiddenHold => BLINK_HIDDEN_MS.saturating_sub(elapsed),
                    BlinkPhase::FadeIn => BLINK_FADE_IN_MS.saturating_sub(elapsed),
                };
                if remaining_ms == 0 {
                    1_000_000 // 1ms — transition imminent
                } else {
                    remaining_ms.saturating_mul(1_000_000)
                }
            }
        };
        let timeout_ns = scroll_timeout_ns.min(blink_timeout_ns);
        let _ = match (timer_active, has_input2) {
            (true, true) => sys::wait(
                &[INPUT_HANDLE, EDITOR_HANDLE, timer_handle, INPUT2_HANDLE],
                timeout_ns,
            ),
            (true, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle], timeout_ns),
            (false, true) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, INPUT2_HANDLE], timeout_ns),
            (false, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], timeout_ns),
        };
        let mut changed = false;
        let mut text_changed = false;
        let mut selection_changed = false;
        let mut context_switched = false;
        let mut timer_fired = false;
        let mut had_user_input = false;

        // Check timer.
        if timer_active {
            if let Ok(_) = sys::wait(&[timer_handle], 0) {
                timer_fired = true;

                let _ = sys::handle_close(timer_handle);

                create_clock_timer();
            }
        }

        // Process input events.
        while input_ch.try_recv(&mut msg) {
            if msg.msg_type == MSG_KEY_EVENT {
                // SAFETY: msg.msg_type is MSG_KEY_EVENT; sender (input driver) guarantees
                // payload is a valid KeyEvent.
                let key: KeyEvent = unsafe { msg.payload_as() };
                let action = process_key_event(&key, has_image, &editor_ch, content_w, content_h);

                if action.changed {
                    changed = true;
                }
                if action.text_changed {
                    text_changed = true;
                }
                if action.selection_changed {
                    selection_changed = true;
                }
                if action.context_switched {
                    context_switched = true;
                }
                had_user_input = true;
            }
        }

        // Drain second input channel.
        if let Some(ref ch2) = input2_ch {
            while ch2.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_KEY_EVENT => {
                        // SAFETY: same invariant as MSG_KEY_EVENT payload_as above.
                        let key: KeyEvent = unsafe { msg.payload_as() };
                        let action =
                            process_key_event(&key, has_image, &editor_ch, content_w, content_h);

                        if action.changed {
                            changed = true;
                        }
                        if action.text_changed {
                            text_changed = true;
                        }
                        if action.selection_changed {
                            selection_changed = true;
                        }
                        if action.context_switched {
                            context_switched = true;
                        }
                        had_user_input = true;
                    }
                    MSG_POINTER_ABS => {
                        // SAFETY: msg.msg_type is MSG_POINTER_ABS; sender guarantees
                        // payload is a valid PointerAbs.
                        let ptr: PointerAbs = unsafe { msg.payload_as() };
                        let s = state();
                        s.mouse_x = scale_pointer_coord(ptr.x, fb_width);
                        s.mouse_y = scale_pointer_coord(ptr.y, fb_height);

                        // Show pointer immediately (cancel any pending fade-out).
                        if let Some(id) = s.pointer_fade_id {
                            s.timeline.cancel(id);
                            s.pointer_fade_id = None;
                        }
                        s.pointer_visible = true;
                        s.pointer_opacity = 255;
                        s.pointer_last_event_ms = now_ms;

                        changed = true;
                    }
                    MSG_POINTER_BUTTON => {
                        // SAFETY: msg.msg_type is MSG_POINTER_BUTTON; sender guarantees
                        // payload is a valid PointerButton.
                        let btn: PointerButton = unsafe { msg.payload_as() };
                        if btn.button == 0 && btn.pressed == 1 {
                            let s = state();
                            let click_x = s.mouse_x;
                            let click_y = s.mouse_y;

                            if click_y >= TITLE_BAR_H && !s.image_mode {
                                let text_origin_x = TEXT_INSET_X;
                                let text_origin_y = TEXT_INSET_TOP;
                                let rel_x = click_x.saturating_sub(text_origin_x);
                                let rel_y = click_y.saturating_sub(text_origin_y);
                                let adjusted_y = rel_y + round_f32(s.scroll_offset) as u32;
                                let layout_info = content_text_layout(content_w);
                                let text = doc_content();
                                let byte_pos = layout_info.xy_to_byte(text, rel_x, adjusted_y);

                                // Double/triple-click detection.
                                // 400ms window, within 4pt of previous click.
                                let dx = if click_x > s.last_click_x {
                                    click_x - s.last_click_x
                                } else {
                                    s.last_click_x - click_x
                                };
                                let dy = if click_y > s.last_click_y {
                                    click_y - s.last_click_y
                                } else {
                                    s.last_click_y - click_y
                                };
                                let dt = now_ms.saturating_sub(s.last_click_ms);
                                let same_spot = dx <= 4 && dy <= 4 && dt <= 400;

                                let click_count = if same_spot {
                                    // Cycle: 1 → 2 → 3 → 1 → ...
                                    (s.click_count % 3) + 1
                                } else {
                                    1
                                };

                                {
                                    let s = state();
                                    s.last_click_ms = now_ms;
                                    s.last_click_x = click_x;
                                    s.last_click_y = click_y;
                                    s.click_count = click_count;
                                }

                                let cols = layout_info.cols();

                                match click_count {
                                    2 => {
                                        // Double-click: select word at click position.
                                        let lo = word_boundary_backward(text, byte_pos);
                                        let hi = word_boundary_forward(text, byte_pos);
                                        let s = state();
                                        s.anchor = lo;
                                        s.cursor_pos = hi;
                                        s.has_selection = hi > lo;
                                        update_selection_from_anchor();
                                    }
                                    3 => {
                                        // Triple-click: select entire visual line.
                                        let lo = visual_line_start(text, byte_pos, cols);
                                        let mut hi = visual_line_end(text, byte_pos, cols);
                                        // Include the newline if present.
                                        if hi < text.len() && text[hi] == b'\n' {
                                            hi += 1;
                                        }
                                        let s = state();
                                        s.anchor = lo;
                                        s.cursor_pos = hi;
                                        s.has_selection = hi > lo;
                                        update_selection_from_anchor();
                                    }
                                    _ => {
                                        // Single click: position cursor, clear selection.
                                        let s = state();
                                        s.cursor_pos = byte_pos;
                                        clear_selection();
                                    }
                                }

                                state().goal_column = None;
                                doc_write_header();
                                sync_cursor_to_editor(&editor_ch);

                                let _ = sys::channel_signal(EDITOR_HANDLE);

                                changed = true;
                                selection_changed = true;
                                had_user_input = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Process editor write requests.
        while editor_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_WRITE_INSERT => {
                    // SAFETY: msg.msg_type is MSG_WRITE_INSERT; sender (editor) guarantees
                    // payload is a valid WriteInsert.
                    let insert: WriteInsert = unsafe { msg.payload_as() };
                    let pos = insert.position as usize;

                    if doc_insert(pos, insert.byte) {
                        state().cursor_pos = pos + 1;

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    // SAFETY: same invariant as MSG_WRITE_INSERT payload_as above.
                    let del: WriteDelete = unsafe { msg.payload_as() };
                    let pos = del.position as usize;

                    if doc_delete(pos) {
                        state().cursor_pos = pos;

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
                    // SAFETY: same invariant as MSG_WRITE_INSERT payload_as above.
                    let cm: CursorMove = unsafe { msg.payload_as() };
                    let pos = cm.position as usize;

                    if pos <= state().doc_len {
                        state().cursor_pos = pos;

                        doc_write_header();

                        changed = true;
                        // Cursor-only move: no text change.
                    }
                }
                MSG_SELECTION_UPDATE => {
                    // SAFETY: same invariant as MSG_WRITE_INSERT payload_as above.
                    let su: SelectionUpdate = unsafe { msg.payload_as() };
                    let s = state();
                    s.sel_start = su.sel_start as usize;
                    s.sel_end = su.sel_end as usize;

                    changed = true;
                    selection_changed = true;
                }
                MSG_WRITE_DELETE_RANGE => {
                    // SAFETY: same invariant as MSG_WRITE_INSERT payload_as above.
                    let dr: WriteDeleteRange = unsafe { msg.payload_as() };
                    let start = dr.start as usize;
                    let end = dr.end as usize;

                    if doc_delete_range(start, end) {
                        state().cursor_pos = start;

                        changed = true;
                        text_changed = true;
                    }
                }
                _ => {}
            }
        }

        // Update scroll spring target for cursor/text changes.
        if (changed || text_changed) && !state().image_mode {
            update_scroll_offset(content_w, content_h);
        }

        // ── Cursor blink ─────────────────────────────────────────────
        //
        // Reset blink to fully visible on any user input (keystroke or
        // click). Then advance the blink state machine — may produce a
        // scene update even when no events arrived (phase transition or
        // fade frame).
        let now_ms = {
            let s = state();
            let freq = s.counter_freq;
            if freq > 0 {
                sys::counter() * 1000 / freq
            } else {
                0
            }
        };
        if had_user_input {
            reset_blink(state(), now_ms);
        }
        state().timeline.tick(now_ms);
        let blink_changed = advance_blink(state(), now_ms);
        if blink_changed {
            changed = true;
        }

        // ── Animation tick ───────────────────────────────────────────
        //
        // Advance the scroll spring toward its target. This must happen
        // after event processing (which may update the target) and before
        // scene dispatch (which reads scroll_offset).
        let mut scroll_changed = false;

        if state().scroll_animating {
            let old_scroll = state().scroll_offset;
            let dt = 1.0 / 60.0; // frame delta (TODO: use actual elapsed from sys::counter)
            let s = state();
            s.scroll_spring.tick(dt);
            s.scroll_offset = s.scroll_spring.value();

            if s.scroll_spring.settled() {
                // Snap to exact target (rounded to integer point) to avoid
                // persistent sub-pixel jitter.
                let target = s.scroll_target;
                let rounded = if target >= 0.0 {
                    ((target + 0.5) as i32) as f32
                } else {
                    ((target - 0.5) as i32) as f32
                };
                s.scroll_offset = rounded;
                s.scroll_animating = false;
            }

            let new_scroll = state().scroll_offset;
            let diff = old_scroll - new_scroll;
            let abs_diff = if diff < 0.0 { -diff } else { diff };
            if abs_diff > 0.5 {
                scroll_changed = true;

                if !text_changed {
                    text_changed = true;
                }
            }
            changed = true; // trigger scene update
        }

        // ── Selection fade animation ────────────────────────────────
        //
        // When the selection changes, start a fade-in animation from
        // opacity 0→255 over 100ms. The animation value is applied to
        // selection nodes after each scene build.
        if selection_changed {
            let s = state();
            // Cancel any previous selection fade in progress.
            if let Some(old_id) = s.selection_fade_id {
                s.timeline.cancel(old_id);
            }
            s.selection_fade_id = s
                .timeline
                .start(0.0, 255.0, 100, animation::Easing::EaseOut, now_ms)
                .ok();
            s.selection_opacity = 0;
        }
        // Tick the selection fade (if active).
        {
            let s = state();
            if let Some(id) = s.selection_fade_id {
                if s.timeline.is_active(id) {
                    let new_val = s.timeline.value(id) as u8;
                    if new_val != s.selection_opacity {
                        s.selection_opacity = new_val;
                        changed = true;
                    }
                } else {
                    s.selection_opacity = 255;
                    s.selection_fade_id = None;
                }
            }
        }

        // ── Document switch fade animation ──────────────────────────
        //
        // Ctrl+Tab starts a fade-out (255→0). When complete, the actual
        // context switch happens, followed by a fade-in (0→255). This
        // prevents the jarring instant switch between editor and image.
        if state().pending_context_switch {
            let s = state();
            if let Some(id) = s.fade_out_id {
                if s.timeline.is_active(id) {
                    let new_val = s.timeline.value(id) as u8;
                    if new_val != s.root_opacity {
                        s.root_opacity = new_val;
                        changed = true;
                    }
                } else {
                    // Fade out complete — do the actual switch.
                    s.root_opacity = 0;
                    s.fade_out_id = None;
                    s.pending_context_switch = false;

                    // Perform the context switch (same logic as the
                    // old immediate Ctrl+Tab path).
                    let was_image = s.image_mode;
                    if !was_image {
                        s.saved_editor_scroll = s.scroll_offset;
                    }
                    s.image_mode = !was_image;
                    if was_image {
                        s.scroll_offset = s.saved_editor_scroll;
                        s.scroll_target = s.saved_editor_scroll;
                        s.scroll_spring.reset_to(s.saved_editor_scroll);
                        s.scroll_animating = false;
                    }

                    // Start fade in.
                    s.fade_in_id = s
                        .timeline
                        .start(0.0, 255.0, 120, animation::Easing::EaseIn, now_ms)
                        .ok();

                    context_switched = true;
                    text_changed = true;
                    changed = true;
                }
            }
        }
        // Tick fade-in (runs independently of pending_context_switch).
        {
            let s = state();
            if let Some(id) = s.fade_in_id {
                if s.timeline.is_active(id) {
                    let new_val = s.timeline.value(id) as u8;
                    if new_val != s.root_opacity {
                        s.root_opacity = new_val;
                        changed = true;
                    }
                } else {
                    s.root_opacity = 255;
                    s.fade_in_id = None;
                }
            }
        }

        // ── Pointer auto-hide ─────────────────────────────────────
        //
        // After 3 s of inactivity, start a 300 ms EaseOut fade-out.
        // When the fade completes, mark the pointer hidden (opacity 0).
        // On any pointer move, the handler above cancels the fade and
        // restores full opacity immediately.
        {
            const POINTER_HIDE_MS: u64 = 3000;
            const POINTER_FADE_MS: u32 = 300;

            let s = state();

            // Start fade-out after 3 s of inactivity.
            if s.pointer_visible && s.pointer_fade_id.is_none() && s.pointer_opacity == 255 {
                let idle_ms = now_ms.saturating_sub(s.pointer_last_event_ms);
                if idle_ms >= POINTER_HIDE_MS {
                    s.pointer_fade_id = s
                        .timeline
                        .start(
                            255.0,
                            0.0,
                            POINTER_FADE_MS,
                            animation::Easing::EaseOut,
                            now_ms,
                        )
                        .ok();
                }
            }

            // Tick pointer fade animation.
            if let Some(id) = s.pointer_fade_id {
                if s.timeline.is_active(id) {
                    let new_opacity = s.timeline.value(id) as u8;
                    if new_opacity != s.pointer_opacity {
                        s.pointer_opacity = new_opacity;
                        changed = true;
                    }
                } else {
                    // Fade complete — pointer is now hidden.
                    s.pointer_opacity = 0;
                    s.pointer_visible = false;
                    s.pointer_fade_id = None;
                    changed = true;
                }
            }
        }

        // ── Scene update dispatch ──────────────────────────────────
        //
        // Use targeted updates for incremental changes instead of
        // rebuilding the entire scene graph every frame.
        //
        // Priority order (most-specific first):
        // 1. context_switched → full rebuild
        // 2. text_changed     → update_document_content (+ clock if timer)
        // 3. selection_changed → update_selection (+ clock if timer)
        // 4. changed (cursor/pointer only) → update_cursor (+ clock if timer)
        // 5. timer_fired only → update_clock
        //
        // When timer_fired coincides with an input change, the clock
        // is updated alongside the primary change within the same
        // copy/swap cycle — no full rebuild needed. The clock is just
        // another node to mark_dirty alongside the document nodes.

        let needs_scene_update = changed || text_changed || selection_changed || timer_fired;

        if needs_scene_update {
            // Prepare clock text if timer fired (needed by any path).
            if timer_fired {
                format_time_hms(clock_seconds(), &mut time_buf);
            }

            // Only context_switched requires a full rebuild. Timer+input
            // coincidence is handled incrementally by each targeted method.
            if context_switched {
                if !timer_fired {
                    format_time_hms(clock_seconds(), &mut time_buf);
                }

                let s = state();
                let title: &[u8] = if s.image_mode { b"Image" } else { b"Text" };
                scene.build_editor_scene(
                    &scene_cfg,
                    doc_content(),
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    title,
                    &time_buf,
                    s.scroll_offset,
                    s.cursor_opacity,
                    s.mouse_x,
                    s.mouse_y,
                    s.pointer_opacity,
                    s.image_mode,
                );
            } else if text_changed {
                // Document content changed (insert/delete/scroll).
                if !timer_fired {
                    format_time_hms(clock_seconds(), &mut time_buf);
                }

                let doc = doc_content();
                let new_line_count = scene_state::count_lines(doc);

                if scroll_changed {
                    // Scroll changed — visible lines differ, incremental
                    // paths would leave stale line-node y positions from
                    // the previous frame. Full rebuild.
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        timer_fired,
                        s.cursor_opacity,
                    );
                } else if new_line_count == prev_line_count {
                    // Same line count — incremental single-line update.
                    // Only reshapes the changed line, pushes new glyph data
                    // at the bump pointer, and updates cursor/selection.
                    let s = state();
                    let changed_line = scene_state::byte_to_line_col(
                        doc,
                        s.cursor_pos,
                        if s.char_w_fx > 0 {
                            ((scene_cfg.fb_width.saturating_sub(2 * TEXT_INSET_X) as i64 * 65536)
                                / s.char_w_fx as i64)
                                .max(1) as usize
                        } else {
                            80
                        },
                    )
                    .0;
                    scene.update_document_incremental(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        changed_line,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        timer_fired,
                        s.cursor_opacity,
                    );
                } else if new_line_count == prev_line_count + 1 {
                    // Single line inserted (Enter key) — incremental insert.
                    let s = state();
                    scene.update_document_insert_line(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        timer_fired,
                        s.cursor_opacity,
                    );
                } else if new_line_count + 1 == prev_line_count {
                    // Single line deleted (Backspace at BOL) — incremental delete.
                    let s = state();
                    scene.update_document_delete_line(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        timer_fired,
                        s.cursor_opacity,
                    );
                } else {
                    // Multi-line change (paste, delete selection spanning lines) —
                    // full rebuild (compaction).
                    let s = state();
                    scene.update_document_content(
                        &scene_cfg,
                        doc,
                        s.cursor_pos as u32,
                        s.sel_start as u32,
                        s.sel_end as u32,
                        b"Text",
                        &time_buf,
                        s.scroll_offset,
                        timer_fired,
                        s.cursor_opacity,
                    );
                }

                prev_line_count = new_line_count;
            } else if selection_changed {
                // Selection changed without text change (e.g., click
                // to clear selection, shift-arrow to extend selection).
                // Also updates cursor position in the scene graph so
                // that click-to-reposition is immediately visible.
                // Clock text is updated only by update_document_content
                // (timer-driven) to prevent data buffer leak.
                let s = state();
                let content_y = TITLE_BAR_H + SHADOW_DEPTH;
                let sel_content_h = fb_height.saturating_sub(content_y);
                let scroll_pt = round_f32(s.scroll_offset);

                scene.update_selection(
                    &scene_cfg,
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    doc_content(),
                    sel_content_h,
                    scroll_pt,
                    s.cursor_opacity,
                );
            } else if changed {
                // Cursor moved without text or selection change
                // (e.g., arrow keys producing a MSG_CURSOR_MOVE
                // that doesn't trigger scroll change).
                // When timer_fired, also updates clock in-place.
                let s = state();
                let doc_width = fb_width.saturating_sub(2 * TEXT_INSET_X);
                let chars_per_line = if s.char_w_fx > 0 {
                    ((doc_width as i64 * 65536) / s.char_w_fx as i64).max(1) as u32
                } else {
                    80
                };

                scene.update_cursor(
                    &scene_cfg,
                    s.cursor_pos as u32,
                    doc_content(),
                    chars_per_line,
                    if timer_fired { Some(&time_buf) } else { None },
                    s.cursor_opacity,
                );
            } else if timer_fired {
                // Timer only — just update the clock text.
                scene.update_clock(&scene_cfg, &time_buf);
            }

            // Apply post-build opacity adjustments (root fade for
            // document switch, selection fade-in for selection changes).
            {
                let s = state();
                scene.apply_opacity(s.root_opacity, s.selection_opacity);
            }

            // Apply pointer cursor position and opacity.
            {
                let s = state();
                scene.apply_pointer(s.mouse_x, s.mouse_y, s.pointer_opacity);
            }

            // Signal compositor.
            compositor_ch.send(&scene_msg);

            let _ = sys::channel_signal(COMPOSITOR_HANDLE);
        }
    }
}

/// Scale an absolute pointer coordinate from the [0, 32767] range to
/// [0, max_pixels). Uses integer math: `coord * max_pixels / 32768`.
/// The divisor is 32768 (not 32767) to ensure the result never equals
/// max_pixels (stays in [0, max_pixels-1]).
fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;

    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}
