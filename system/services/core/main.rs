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
extern crate scene;

#[path = "scene_state.rs"]
mod scene_state;

use protocol::core_config::{CoreConfig, MSG_CORE_CONFIG, MSG_SCENE_UPDATED};
use protocol::edit::{
    CursorMove, SelectionUpdate, WriteDelete, WriteDeleteRange, WriteInsert, MSG_CURSOR_MOVE,
    MSG_SELECTION_UPDATE, MSG_SET_CURSOR, MSG_WRITE_DELETE, MSG_WRITE_DELETE_RANGE,
    MSG_WRITE_INSERT,
};
use protocol::input::{
    KeyEvent, PointerAbs, PointerButton, MSG_KEY_EVENT, MSG_POINTER_ABS, MSG_POINTER_BUTTON,
};
use protocol::compose::{ImageConfig, RtcConfig, MSG_IMAGE_CONFIG, MSG_RTC_CONFIG};

const FONT_SIZE: u32 = 18;
const KEY_TAB: u16 = 15;
const KEY_LEFTCTRL: u16 = 29;

const INPUT_HANDLE: u8 = 1;
const COMPOSITOR_HANDLE: u8 = 2;
const EDITOR_HANDLE: u8 = 3;
const INPUT2_HANDLE: u8 = 4;

const SHADOW_DEPTH: u32 = 12;
const TITLE_BAR_H: u32 = 36;
const TEXT_INSET_X: u32 = 12;
const TEXT_INSET_TOP: u32 = TITLE_BAR_H + SHADOW_DEPTH + 8;
const TEXT_INSET_BOTTOM: u32 = 8;
const DOC_HEADER_SIZE: usize = 64;

static mut RTC_MMIO_VA: usize = 0;
static mut IMAGE_MODE: bool = false;
static mut BOOT_COUNTER: u64 = 0;
static mut COUNTER_FREQ: u64 = 0;
static mut TIMER_HANDLE: u8 = 0;
static mut TIMER_ACTIVE: bool = false;
static mut SEL_START: usize = 0;
static mut SEL_END: usize = 0;
static mut SCROLL_OFFSET: u32 = 0;
static mut SAVED_EDITOR_SCROLL: u32 = 0;
static mut MOUSE_X: u32 = 0;
static mut MOUSE_Y: u32 = 0;
static mut CURSOR_POS: usize = 0;
static mut CHAR_W: u32 = 8;
static mut LINE_H: u32 = 20;
static mut DOC_BUF: *mut u8 = core::ptr::null_mut();
static mut DOC_CAPACITY: usize = 0;
static mut DOC_LEN: usize = 0;

fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
}

fn content_text_layout(content_w: u32) -> drawing::TextLayout {
    drawing::TextLayout {
        char_width: unsafe { CHAR_W },
        line_height: unsafe { LINE_H },
        max_width: content_w - 2 * TEXT_INSET_X,
    }
}

fn doc_content() -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(DOC_BUF.add(DOC_HEADER_SIZE), DOC_LEN) }
}

fn doc_delete(pos: usize) -> bool {
    unsafe {
        if DOC_LEN == 0 || pos >= DOC_LEN {
            return false;
        }
        let base = DOC_BUF.add(DOC_HEADER_SIZE);
        if pos + 1 < DOC_LEN {
            core::ptr::copy(base.add(pos + 1), base.add(pos), DOC_LEN - pos - 1);
        }
        DOC_LEN -= 1;
        doc_write_header();
        true
    }
}

fn doc_delete_range(start: usize, end: usize) -> bool {
    unsafe {
        if start >= end || start >= DOC_LEN || end > DOC_LEN {
            return false;
        }
        let base = DOC_BUF.add(DOC_HEADER_SIZE);
        let del_count = end - start;
        if end < DOC_LEN {
            core::ptr::copy(base.add(end), base.add(start), DOC_LEN - end);
        }
        DOC_LEN -= del_count;
        doc_write_header();
        true
    }
}

fn doc_insert(pos: usize, byte: u8) -> bool {
    unsafe {
        if DOC_LEN >= DOC_CAPACITY || pos > DOC_LEN {
            return false;
        }
        let base = DOC_BUF.add(DOC_HEADER_SIZE);
        if pos < DOC_LEN {
            core::ptr::copy(base.add(pos), base.add(pos + 1), DOC_LEN - pos);
        }
        *base.add(pos) = byte;
        DOC_LEN += 1;
        doc_write_header();
        true
    }
}

fn doc_write_header() {
    unsafe {
        core::ptr::write_volatile(DOC_BUF as *mut u64, DOC_LEN as u64);
        core::ptr::write_volatile(DOC_BUF.add(8) as *mut u64, CURSOR_POS as u64);
    }
}

fn update_scroll_offset(content_w: u32, content_h: u32) {
    let vp_lines = viewport_lines(content_h);
    if vp_lines == 0 {
        return;
    }
    let layout = content_text_layout(content_w);
    let text = doc_content();
    let cursor = unsafe { CURSOR_POS };
    let current = unsafe { SCROLL_OFFSET };
    let new_scroll = layout.scroll_for_cursor(text, cursor, current, vp_lines);
    unsafe { SCROLL_OFFSET = new_scroll };
}

fn viewport_lines(content_h: u32) -> u32 {
    let line_h = unsafe { LINE_H };
    if line_h == 0 {
        return 0;
    }
    let usable = content_h.saturating_sub(TEXT_INSET_TOP + TEXT_INSET_BOTTOM);
    usable / line_h
}

fn clock_seconds() -> u64 {
    let rtc_va = unsafe { RTC_MMIO_VA };
    if rtc_va != 0 {
        let epoch = unsafe { core::ptr::read_volatile(rtc_va as *const u32) };
        epoch as u64
    } else {
        let now = sys::counter();
        let boot = unsafe { BOOT_COUNTER };
        let freq = unsafe { COUNTER_FREQ };
        if freq == 0 {
            return 0;
        }
        (now - boot) / freq
    }
}

fn create_clock_timer() -> bool {
    let freq = unsafe { COUNTER_FREQ };
    let timeout_ns = if freq > 0 {
        let now = sys::counter();
        let boot = unsafe { BOOT_COUNTER };
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
            unsafe {
                TIMER_HANDLE = handle;
                TIMER_ACTIVE = true;
            }
            true
        }
        Err(_) => {
            unsafe { TIMER_ACTIVE = false };
            false
        }
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

struct KeyAction {
    changed: bool,
    text_changed: bool,
    context_switched: bool,
    consumed: bool,
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
        return KeyAction { changed: false, text_changed: false, context_switched: false, consumed: true };
    }
    if key.keycode == KEY_TAB && key.pressed == 1 && *ctrl_pressed {
        if has_image {
            let was_image = unsafe { IMAGE_MODE };
            if !was_image {
                unsafe { SAVED_EDITOR_SCROLL = SCROLL_OFFSET };
            }
            unsafe { IMAGE_MODE = !was_image };
            if was_image {
                unsafe { SCROLL_OFFSET = SAVED_EDITOR_SCROLL };
            }
            return KeyAction { changed: true, text_changed: true, context_switched: true, consumed: true };
        }
        return KeyAction { changed: false, text_changed: false, context_switched: false, consumed: true };
    }
    if !unsafe { IMAGE_MODE } {
        editor_ch.send(msg);
        let _ = sys::channel_signal(EDITOR_HANDLE);
    }
    KeyAction { changed: false, text_changed: false, context_switched: false, consumed: false }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe {
        BOOT_COUNTER = sys::counter();
        COUNTER_FREQ = sys::counter_freq();
    }

    sys::print(b"  \xF0\x9F\xA7\xA0 core - starting\n");

    // Read core config from init channel.
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
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

    unsafe {
        DOC_BUF = config.doc_va as *mut u8;
        DOC_CAPACITY = config.doc_capacity as usize;
        DOC_LEN = 0;
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
        if let Some(ttf) = drawing::TrueTypeFont::new(font_data) {
            let upem = ttf.units_per_em();
            let asc = ttf.hhea_ascent() as i32;
            let desc = ttf.hhea_descent() as i32;
            let gap = ttf.hhea_line_gap() as i32;
            let size = FONT_SIZE;
            let ascent_px = ((asc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let descent_px = ((-desc * size as i32 + upem as i32 - 1) / upem as i32) as u32;
            let gap_px = if gap > 0 {
                (gap * size as i32 / upem as i32) as u32
            } else {
                0
            };
            let line_h = ascent_px + descent_px + gap_px;
            // For monospace: use hmtx advance of space glyph.
            let space_gid = ttf.glyph_index(' ').unwrap_or(0);
            let (advance_fu, _) = ttf.glyph_h_metrics(space_gid).unwrap_or((0, 0));
            let char_w = (advance_fu as u32 * size + upem as u32 / 2) / upem as u32;
            unsafe {
                CHAR_W = if char_w > 0 { char_w } else { 8 };
                LINE_H = if line_h > 0 { line_h } else { 20 };
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
                    unsafe { RTC_MMIO_VA = va };
                    sys::print(b"     pl031 rtc mapped\n");
                }
                Err(_) => {
                    sys::print(b"     pl031 rtc map failed\n");
                }
            }
        }
    }

    // Set up IPC channels.
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    let compositor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    let editor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(3), ipc::PAGE_SIZE, 0) };

    let has_input2 = match sys::wait(&[INPUT2_HANDLE], 0) {
        Ok(_) => true,
        Err(sys::SyscallError::WouldBlock) => true,
        _ => false,
    };
    let input2_ch = if has_input2 {
        sys::print(b"     tablet input channel detected\n");
        Some(unsafe { ipc::Channel::from_base(channel_shm_va(4), ipc::PAGE_SIZE, 1) })
    } else {
        None
    };

    // Content area dimensions (for layout).
    let content_w = fb_width;
    let content_h = fb_height;

    // Scene graph in shared memory.
    let scene_buf = unsafe {
        core::slice::from_raw_parts_mut(
            config.scene_va as *mut u8,
            scene::DOUBLE_SCENE_SIZE,
        )
    };
    let mut scene = scene_state::SceneState::from_buf(scene_buf);

    // Build initial scene.
    let mut time_buf = [0u8; 8];
    format_time_hms(clock_seconds(), &mut time_buf);

    scene.build_editor_scene(
        fb_width, fb_height, TITLE_BAR_H, SHADOW_DEPTH,
        TEXT_INSET_X, TEXT_INSET_TOP,
        drawing::CHROME_BG, drawing::CHROME_BORDER,
        drawing::CHROME_TITLE, drawing::CHROME_CLOCK,
        drawing::BG_BASE, drawing::TEXT_PRIMARY,
        drawing::TEXT_CURSOR, drawing::TEXT_SELECTION,
        FONT_SIZE as u16, unsafe { CHAR_W }, unsafe { LINE_H },
        doc_content(), unsafe { CURSOR_POS } as u32,
        unsafe { SEL_START } as u32, unsafe { SEL_END } as u32,
        b"Text", &time_buf, 0,
    );

    // Signal compositor that first frame is ready.
    let scene_msg = ipc::Message::new(MSG_SCENE_UPDATED);
    compositor_ch.send(&scene_msg);
    let _ = sys::channel_signal(COMPOSITOR_HANDLE);

    create_clock_timer();

    sys::print(b"     entering event loop\n");

    let mut ctrl_pressed = false;

    loop {
        let timer_active = unsafe { TIMER_ACTIVE };
        let timer_handle = unsafe { TIMER_HANDLE };
        let _ = match (timer_active, has_input2) {
            (true, true) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle, INPUT2_HANDLE], u64::MAX),
            (true, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle], u64::MAX),
            (false, true) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, INPUT2_HANDLE], u64::MAX),
            (false, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], u64::MAX),
        };

        let mut changed = false;
        let mut text_changed = false;
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
                let action = process_key_event(&key, &mut ctrl_pressed, has_image, &editor_ch, &msg);
                if action.changed { changed = true; }
                if action.text_changed { text_changed = true; }
            }
        }

        // Drain second input channel.
        if let Some(ref ch2) = input2_ch {
            while ch2.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_KEY_EVENT => {
                        let key: KeyEvent = unsafe { msg.payload_as() };
                        let action = process_key_event(&key, &mut ctrl_pressed, has_image, &editor_ch, &msg);
                        if action.changed { changed = true; }
                        if action.text_changed { text_changed = true; }
                    }
                    MSG_POINTER_ABS => {
                        let ptr: PointerAbs = unsafe { msg.payload_as() };
                        unsafe {
                            MOUSE_X = drawing::scale_pointer_coord(ptr.x, fb_width);
                            MOUSE_Y = drawing::scale_pointer_coord(ptr.y, fb_height);
                        }
                        changed = true;
                    }
                    MSG_POINTER_BUTTON => {
                        let btn: PointerButton = unsafe { msg.payload_as() };
                        if btn.button == 0 && btn.pressed == 1 {
                            let click_x = unsafe { MOUSE_X };
                            let click_y = unsafe { MOUSE_Y };
                            if click_y >= TITLE_BAR_H && !unsafe { IMAGE_MODE } {
                                let text_origin_x = TEXT_INSET_X;
                                let text_origin_y = TEXT_INSET_TOP;
                                let rel_x = click_x.saturating_sub(text_origin_x);
                                let rel_y = click_y.saturating_sub(text_origin_y);
                                let scroll = unsafe { SCROLL_OFFSET };
                                let line_h = unsafe { LINE_H };
                                let adjusted_y = rel_y + scroll * line_h;
                                let layout = content_text_layout(content_w);
                                let text = doc_content();
                                let byte_pos = layout.xy_to_byte(text, rel_x, adjusted_y);
                                unsafe {
                                    CURSOR_POS = byte_pos;
                                    SEL_START = 0;
                                    SEL_END = 0;
                                }
                                doc_write_header();
                                let cm = CursorMove { position: byte_pos as u32 };
                                let cm_msg = unsafe { ipc::Message::from_payload(MSG_SET_CURSOR, &cm) };
                                editor_ch.send(&cm_msg);
                                let _ = sys::channel_signal(EDITOR_HANDLE);
                                changed = true;
                                text_changed = true;
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
                        unsafe { CURSOR_POS = pos + 1 };
                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let del: WriteDelete = unsafe { msg.payload_as() };
                    let pos = del.position as usize;
                    if doc_delete(pos) {
                        unsafe { CURSOR_POS = pos };
                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
                    let cm: CursorMove = unsafe { msg.payload_as() };
                    let pos = cm.position as usize;
                    if pos <= unsafe { DOC_LEN } {
                        unsafe { CURSOR_POS = pos };
                        doc_write_header();
                        changed = true;
                        text_changed = true;
                    }
                }
                MSG_SELECTION_UPDATE => {
                    let su: SelectionUpdate = unsafe { msg.payload_as() };
                    unsafe {
                        SEL_START = su.sel_start as usize;
                        SEL_END = su.sel_end as usize;
                    }
                    changed = true;
                    text_changed = true;
                }
                MSG_WRITE_DELETE_RANGE => {
                    let dr: WriteDeleteRange = unsafe { msg.payload_as() };
                    let start = dr.start as usize;
                    let end = dr.end as usize;
                    if doc_delete_range(start, end) {
                        unsafe { CURSOR_POS = start };
                        changed = true;
                        text_changed = true;
                    }
                }
                _ => {}
            }
        }

        if text_changed && !unsafe { IMAGE_MODE } {
            update_scroll_offset(content_w, content_h);
        }

        if changed || timer_fired {
            format_time_hms(clock_seconds(), &mut time_buf);

            scene.build_editor_scene(
                fb_width, fb_height, TITLE_BAR_H, SHADOW_DEPTH,
                TEXT_INSET_X, TEXT_INSET_TOP,
                drawing::CHROME_BG, drawing::CHROME_BORDER,
                drawing::CHROME_TITLE, drawing::CHROME_CLOCK,
                drawing::BG_BASE, drawing::TEXT_PRIMARY,
                drawing::TEXT_CURSOR, drawing::TEXT_SELECTION,
                FONT_SIZE as u16, unsafe { CHAR_W }, unsafe { LINE_H },
                doc_content(), unsafe { CURSOR_POS } as u32,
                unsafe { SEL_START } as u32, unsafe { SEL_END } as u32,
                b"Text", &time_buf,
                unsafe { SCROLL_OFFSET } as i32,
            );

            // Signal compositor.
            compositor_ch.send(&scene_msg);
            let _ = sys::channel_signal(COMPOSITOR_HANDLE);
        }
    }
}
