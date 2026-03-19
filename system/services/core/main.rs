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
extern crate fonts;
extern crate scene;

#[path = "fallback.rs"]
mod fallback;
#[path = "scene_state.rs"]
mod scene_state;
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

const COMPOSITOR_HANDLE: u8 = 2;
const DOC_HEADER_SIZE: usize = 64;
const EDITOR_HANDLE: u8 = 3;
const FONT_SIZE: u32 = 18;
const INPUT_HANDLE: u8 = 1;
const INPUT2_HANDLE: u8 = 4;
const KEY_LEFTCTRL: u16 = 29;
const KEY_TAB: u16 = 15;
const SHADOW_DEPTH: u32 = 12;
const TEXT_INSET_BOTTOM: u32 = 8;
const TEXT_INSET_TOP: u32 = TITLE_BAR_H + SHADOW_DEPTH + 8;
const TEXT_INSET_X: u32 = 12;
const TITLE_BAR_H: u32 = 36;

struct CoreState {
    boot_counter: u64,
    char_w: u32,
    counter_freq: u64,
    cursor_pos: usize,
    doc_buf: *mut u8,
    doc_capacity: usize,
    doc_len: usize,
    font_data_ptr: *const u8,
    font_data_len: usize,
    font_upem: u16,
    image_mode: bool,
    line_h: u32,
    mouse_x: u32,
    mouse_y: u32,
    rtc_mmio_va: usize,
    saved_editor_scroll: u32,
    scroll_offset: u32,
    sel_end: usize,
    sel_start: usize,
    timer_active: bool,
    timer_handle: u8,
}

impl CoreState {
    const fn new() -> Self {
        Self {
            boot_counter: 0,
            char_w: 8,
            counter_freq: 0,
            cursor_pos: 0,
            doc_buf: core::ptr::null_mut(),
            doc_capacity: 0,
            doc_len: 0,
            font_data_ptr: core::ptr::null(),
            font_data_len: 0,
            font_upem: 1000,
            image_mode: false,
            line_h: 20,
            mouse_x: 0,
            mouse_y: 0,
            rtc_mmio_va: 0,
            saved_editor_scroll: 0,
            scroll_offset: 0,
            sel_end: 0,
            sel_start: 0,
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
    char_width: u32,
    line_height: u32,
    max_width: u32,
}

impl TextLayout {
    fn cols(&self) -> usize {
        if self.char_width == 0 {
            return 0;
        }

        (self.max_width / self.char_width) as usize
    }

    /// Return the visual line number (0-based) for a given byte offset.
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
        let mut col = 0usize;
        let mut row = 0u32;

        for (i, &byte) in text.iter().enumerate() {
            if i == target {
                return row;
            }

            if byte == b'\n' {
                row += 1;
                col = 0;

                continue;
            }

            if col >= cols {
                row += 1;
                col = 0;
            }

            col += 1;
        }

        row
    }

    /// Compute the scroll offset needed to keep the cursor visible.
    fn scroll_for_cursor(
        &self,
        text: &[u8],
        cursor_offset: usize,
        current_scroll: u32,
        viewport_lines: u32,
    ) -> u32 {
        if viewport_lines == 0 {
            return 0;
        }

        let cursor_line = self.byte_to_visual_line(text, cursor_offset);

        if cursor_line < current_scroll {
            return cursor_line;
        }

        let last_visible = current_scroll + viewport_lines - 1;

        if cursor_line > last_visible {
            return cursor_line - (viewport_lines - 1);
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
        let half_char = self.char_width / 2;
        let target_col = (x + half_char) / self.char_width;
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
        char_width: s.char_w,
        line_height: s.line_h,
        max_width: content_w - 2 * TEXT_INSET_X,
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
    unsafe { core::slice::from_raw_parts(s.doc_buf.add(DOC_HEADER_SIZE), s.doc_len) }
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
fn process_key_event(
    key: &KeyEvent,
    ctrl_pressed: &mut bool,
    has_image: bool,
    editor_ch: &ipc::Channel,
    msg: &ipc::Message,
) -> KeyAction {
    if key.keycode == KEY_LEFTCTRL {
        *ctrl_pressed = key.pressed == 1;

        return KeyAction {
            changed: false,
            text_changed: false,
            selection_changed: false,
            context_switched: false,
            consumed: true,
        };
    }
    if key.keycode == KEY_TAB && key.pressed == 1 && *ctrl_pressed {
        if has_image {
            let s = state();
            let was_image = s.image_mode;

            if !was_image {
                s.saved_editor_scroll = s.scroll_offset;
            }

            s.image_mode = !was_image;

            if was_image {
                s.scroll_offset = s.saved_editor_scroll;
            }

            return KeyAction {
                changed: true,
                text_changed: true,
                selection_changed: false,
                context_switched: true,
                consumed: true,
            };
        }
        return KeyAction {
            changed: false,
            text_changed: false,
            selection_changed: false,
            context_switched: false,
            consumed: true,
        };
    }

    if !state().image_mode {
        editor_ch.send(msg);

        let _ = sys::channel_signal(EDITOR_HANDLE);
    }

    KeyAction {
        changed: false,
        text_changed: false,
        selection_changed: false,
        context_switched: false,
        consumed: false,
    }
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

    state().scroll_offset = new_scroll;
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
    let init_ch =
        unsafe { ipc::Channel::from_base(protocol::channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_CORE_CONFIG {
        sys::print(b"core: no config message\n");
        sys::exit();
    }

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
    if config.mono_font_va != 0 && config.mono_font_len > 0 {
        let font_data = unsafe {
            core::slice::from_raw_parts(
                config.mono_font_va as *const u8,
                config.mono_font_len as usize,
            )
        };
        // Store font data pointer and length for shaping calls.
        {
            let s = state();
            s.font_data_ptr = config.mono_font_va as *const u8;
            s.font_data_len = config.mono_font_len as usize;
        }
        if let Some(fm) = fonts::rasterize::font_metrics(font_data) {
            let upem = fm.units_per_em;
            state().font_upem = upem;
            let asc = fm.ascent as i32;
            let desc = fm.descent as i32;
            let gap = fm.line_gap as i32;
            let size = FONT_SIZE;
            let ascent_px = ((asc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let descent_px = ((-desc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let gap_px = if gap > 0 {
                (gap * size as i32 / upem as i32) as u32
            } else {
                0
            };
            let line_h = ascent_px + descent_px + gap_px;
            // For monospace: use axis-adjusted advance of space glyph (MONO=1).
            let space_gid = fonts::rasterize::glyph_id_for_char(font_data, ' ').unwrap_or(0);
            let mono_axes = [fonts::rasterize::AxisValue {
                tag: *b"MONO",
                value: 1.0,
            }];
            let char_w = fonts::rasterize::glyph_advance_with_axes(
                font_data,
                space_gid,
                size as u16,
                &mono_axes,
            )
            .unwrap_or_else(|| {
                let (advance_fu, _) =
                    fonts::rasterize::glyph_h_metrics(font_data, space_gid).unwrap_or((0, 0));
                (advance_fu as u32 * size + upem as u32 / 2) / upem as u32
            });

            {
                let s = state();
                s.char_w = if char_w > 0 { char_w } else { 8 };
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
        let img_config: ImageConfig = unsafe { msg.payload_as() };

        if img_config.image_va != 0 && img_config.image_len > 0 {
            has_image = true;
        }
    }

    // Check for RTC config.
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_RTC_CONFIG {
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
        Some(unsafe { ipc::Channel::from_base(protocol::channel_shm_va(4), ipc::PAGE_SIZE, 1) })
    } else {
        None
    };
    // Content area dimensions (for layout).
    let content_w = fb_width;
    let content_h = fb_height;
    // Scene graph in shared memory.
    let scene_buf = unsafe {
        core::slice::from_raw_parts_mut(config.scene_va as *mut u8, scene::TRIPLE_SCENE_SIZE)
    };
    let mut scene = scene_state::SceneState::from_buf(scene_buf);
    // Build initial scene.
    let mut time_buf = [0u8; 8];

    format_time_hms(clock_seconds(), &mut time_buf);

    // Axis values for monospace shaping (MONO=1).
    let mono_shape_axes = [fonts::rasterize::AxisValue {
        tag: *b"MONO",
        value: 1.0,
    }];

    {
        let s = state();
        scene.build_editor_scene(
            fb_width,
            fb_height,
            TITLE_BAR_H,
            SHADOW_DEPTH,
            TEXT_INSET_X,
            TEXT_INSET_TOP,
            drawing::CHROME_BG,
            drawing::CHROME_BORDER,
            drawing::CHROME_TITLE,
            drawing::CHROME_CLOCK,
            drawing::BG_BASE,
            drawing::TEXT_PRIMARY,
            drawing::TEXT_CURSOR,
            drawing::TEXT_SELECTION,
            FONT_SIZE as u16,
            s.char_w,
            s.line_h,
            doc_content(),
            s.cursor_pos as u32,
            s.sel_start as u32,
            s.sel_end as u32,
            b"Text",
            &time_buf,
            0,
            font_data(),
            s.font_upem,
            &mono_shape_axes,
        );
    }

    // Signal compositor that first frame is ready.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);

    compositor_ch.send(&scene_msg);

    let _ = sys::channel_signal(COMPOSITOR_HANDLE);

    create_clock_timer();

    sys::print(b"     entering event loop\n");

    let mut ctrl_pressed = false;

    loop {
        let timer_active = state().timer_active;
        let timer_handle = state().timer_handle;
        let _ = match (timer_active, has_input2) {
            (true, true) => sys::wait(
                &[INPUT_HANDLE, EDITOR_HANDLE, timer_handle, INPUT2_HANDLE],
                u64::MAX,
            ),
            (true, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle], u64::MAX),
            (false, true) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, INPUT2_HANDLE], u64::MAX),
            (false, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], u64::MAX),
        };
        let mut changed = false;
        let mut text_changed = false;
        let mut selection_changed = false;
        let mut context_switched = false;
        let mut timer_fired = false;

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
                let key: KeyEvent = unsafe { msg.payload_as() };
                let action =
                    process_key_event(&key, &mut ctrl_pressed, has_image, &editor_ch, &msg);

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
            }
        }

        // Drain second input channel.
        if let Some(ref ch2) = input2_ch {
            while ch2.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_KEY_EVENT => {
                        let key: KeyEvent = unsafe { msg.payload_as() };
                        let action =
                            process_key_event(&key, &mut ctrl_pressed, has_image, &editor_ch, &msg);

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
                    }
                    MSG_POINTER_ABS => {
                        let ptr: PointerAbs = unsafe { msg.payload_as() };
                        let s = state();
                        s.mouse_x = scale_pointer_coord(ptr.x, fb_width);
                        s.mouse_y = scale_pointer_coord(ptr.y, fb_height);

                        changed = true;
                    }
                    MSG_POINTER_BUTTON => {
                        let btn: PointerButton = unsafe { msg.payload_as() };
                        // TODO: Handle right-click, middle-click, and button
                        // release events (review 6.9). Currently only left-press.
                        if btn.button == 0 && btn.pressed == 1 {
                            let s = state();
                            let click_x = s.mouse_x;
                            let click_y = s.mouse_y;

                            if click_y >= TITLE_BAR_H && !s.image_mode {
                                let text_origin_x = TEXT_INSET_X;
                                let text_origin_y = TEXT_INSET_TOP;
                                let rel_x = click_x.saturating_sub(text_origin_x);
                                let rel_y = click_y.saturating_sub(text_origin_y);
                                let scroll = s.scroll_offset;
                                let line_h = s.line_h;
                                let adjusted_y = rel_y + scroll * line_h;
                                let layout = content_text_layout(content_w);
                                let text = doc_content();
                                let byte_pos = layout.xy_to_byte(text, rel_x, adjusted_y);

                                {
                                    let s = state();
                                    s.cursor_pos = byte_pos;
                                    s.sel_start = 0;
                                    s.sel_end = 0;
                                }

                                doc_write_header();

                                let cm = CursorMove {
                                    position: byte_pos as u32,
                                };
                                let cm_msg =
                                    unsafe { ipc::Message::from_payload(MSG_SET_CURSOR, &cm) };

                                editor_ch.send(&cm_msg);

                                let _ = sys::channel_signal(EDITOR_HANDLE);

                                changed = true;
                                // Click moves cursor, clears selection.
                                // Treat as cursor-move + selection clear.
                                selection_changed = true;
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
                    let insert: WriteInsert = unsafe { msg.payload_as() };
                    let pos = insert.position as usize;

                    if doc_insert(pos, insert.byte) {
                        state().cursor_pos = pos + 1;

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let del: WriteDelete = unsafe { msg.payload_as() };
                    let pos = del.position as usize;

                    if doc_delete(pos) {
                        state().cursor_pos = pos;

                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
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
                    let su: SelectionUpdate = unsafe { msg.payload_as() };
                    let s = state();
                    s.sel_start = su.sel_start as usize;
                    s.sel_end = su.sel_end as usize;

                    changed = true;
                    selection_changed = true;
                }
                MSG_WRITE_DELETE_RANGE => {
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

        // Update scroll offset for cursor/text changes.
        if (changed || text_changed) && !state().image_mode {
            let old_scroll = state().scroll_offset;

            update_scroll_offset(content_w, content_h);

            // If scroll changed, we need a full document content update
            // (visible lines changed) regardless of whether text changed.
            let new_scroll = state().scroll_offset;

            if old_scroll != new_scroll && !text_changed {
                text_changed = true;
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
        // another node to mark_changed alongside the document nodes.

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
                scene.build_editor_scene(
                    fb_width,
                    fb_height,
                    TITLE_BAR_H,
                    SHADOW_DEPTH,
                    TEXT_INSET_X,
                    TEXT_INSET_TOP,
                    drawing::CHROME_BG,
                    drawing::CHROME_BORDER,
                    drawing::CHROME_TITLE,
                    drawing::CHROME_CLOCK,
                    drawing::BG_BASE,
                    drawing::TEXT_PRIMARY,
                    drawing::TEXT_CURSOR,
                    drawing::TEXT_SELECTION,
                    FONT_SIZE as u16,
                    s.char_w,
                    s.line_h,
                    doc_content(),
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    b"Text",
                    &time_buf,
                    s.scroll_offset as i32,
                    font_data(),
                    s.font_upem,
                    &mono_shape_axes,
                );
            } else if text_changed {
                // Document content changed (insert/delete/scroll).
                // update_document_content handles doc text, cursor, and
                // selection. Compacts the data buffer on each call so
                // data_used stays proportional to visible content.
                // When timer_fired, also marks N_CLOCK_TEXT changed so
                // both document and clock update in one frame.
                if !timer_fired {
                    format_time_hms(clock_seconds(), &mut time_buf);
                }

                let s = state();
                scene.update_document_content(
                    fb_width,
                    fb_height,
                    TITLE_BAR_H,
                    SHADOW_DEPTH,
                    TEXT_INSET_X,
                    TEXT_INSET_TOP,
                    drawing::CHROME_BG,
                    drawing::CHROME_BORDER,
                    drawing::CHROME_TITLE,
                    drawing::CHROME_CLOCK,
                    drawing::BG_BASE,
                    drawing::TEXT_PRIMARY,
                    drawing::TEXT_CURSOR,
                    drawing::TEXT_SELECTION,
                    FONT_SIZE as u16,
                    s.char_w,
                    s.line_h,
                    doc_content(),
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    b"Text",
                    &time_buf,
                    s.scroll_offset as i32,
                    timer_fired,
                    font_data(),
                    s.font_upem,
                    &mono_shape_axes,
                );
            } else if selection_changed {
                // Selection changed without text change (e.g., click
                // to clear selection, shift-arrow to extend selection).
                // Also updates cursor position in the scene graph so
                // that click-to-reposition is immediately visible.
                // When timer_fired, also updates clock in-place.
                let s = state();
                let content_y = TITLE_BAR_H + SHADOW_DEPTH;
                let sel_content_h = fb_height.saturating_sub(content_y);
                let scroll_px = s.scroll_offset as i32 * s.line_h as i32;
                let dc =
                    |c: drawing::Color| -> scene::Color { scene::Color::rgba(c.r, c.g, c.b, c.a) };
                let chars_per_line = {
                    let doc_width = fb_width.saturating_sub(2 * TEXT_INSET_X);
                    if s.char_w > 0 {
                        (doc_width / s.char_w).max(1)
                    } else {
                        80
                    }
                };

                scene.update_selection(
                    s.cursor_pos as u32,
                    s.sel_start as u32,
                    s.sel_end as u32,
                    doc_content(),
                    chars_per_line,
                    s.char_w,
                    s.line_h,
                    dc(drawing::TEXT_SELECTION),
                    sel_content_h,
                    scroll_px,
                    if timer_fired { Some(&time_buf) } else { None },
                    font_data(),
                    FONT_SIZE as u16,
                    s.font_upem,
                    &mono_shape_axes,
                );
            } else if changed {
                // Cursor moved without text or selection change
                // (e.g., arrow keys producing a MSG_CURSOR_MOVE
                // that doesn't trigger scroll change).
                // When timer_fired, also updates clock in-place.
                let s = state();
                let doc_width = fb_width.saturating_sub(2 * TEXT_INSET_X);
                let chars_per_line = if s.char_w > 0 {
                    (doc_width / s.char_w).max(1)
                } else {
                    80
                };
                let scroll_px = s.scroll_offset as i32 * s.line_h as i32;

                scene.update_cursor(
                    s.cursor_pos as u32,
                    doc_content(),
                    chars_per_line,
                    s.char_w,
                    s.line_h,
                    scroll_px,
                    if timer_fired { Some(&time_buf) } else { None },
                    font_data(),
                    FONT_SIZE as u16,
                    s.font_upem,
                    &mono_shape_axes,
                );
            } else if timer_fired {
                // Timer only — just update the clock text.
                scene.update_clock(
                    &time_buf,
                    font_data(),
                    FONT_SIZE as u16,
                    state().font_upem,
                    &mono_shape_axes,
                );
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
