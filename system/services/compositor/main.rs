//! Compositor — OS service with editor process separation.
//!
//! Receives framebuffer config from init, keyboard events from the input
//! driver, and write requests from the text editor. Routes input to the
//! editor, applies write requests to the document (sole writer), and
//! renders the result.
//!
//! # Architecture
//!
//! This demonstrates the settled edit protocol: the compositor (proto-OS
//! service) is the sole writer to document state. The editor receives
//! input events, decides what they mean, and sends write requests back.
//! The compositor applies writes and re-renders. The editor never touches
//! the document directly.
//!
//! # IPC channels (handle indices)
//!
//! Handle 1: input driver → compositor (keyboard events)
//! Handle 2: compositor → GPU driver (present commands)
//! Handle 3: compositor ↔ editor (input events out, write requests in)

#![no_std]
#![no_main]

extern crate alloc;

const CHANNEL_SHM_BASE: usize = 0x4000_0000;
// Protocol message types.
const MSG_COMPOSITOR_CONFIG: u32 = 3;
const MSG_KEY_EVENT: u32 = 10;
const MSG_PRESENT: u32 = 20;
const MSG_WRITE_INSERT: u32 = 30;
const MSG_WRITE_DELETE: u32 = 31;
const MSG_CURSOR_MOVE: u32 = 32;
// Handle indices (determined by the order init sends handles).
const INPUT_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;
const EDITOR_HANDLE: u8 = 3;
// Text area layout constants.
const TEXT_X: u32 = 24;
const TEXT_Y: u32 = 48;
const CHAR_W: u32 = 8;
const LINE_H: u32 = 20;
// Document header layout (first 64 bytes of shared buffer).
const DOC_HEADER_SIZE: usize = 64;

// Cursor position for rendering.
static mut CURSOR_COL: usize = 0;
static mut CURSOR_ROW: u32 = 0;
/// Cursor byte offset in the document. Updated by write requests.
static mut CURSOR_POS: usize = 0;
/// Highest text row drawn in the previous render (for dirty-rect clearing).
static mut LAST_TEXT_ROW: u32 = 0;
// Document shared buffer — owned exclusively by the compositor (sole writer).
// Set from config message; editor has a read-only mapping of the same pages.
static mut DOC_BUF: *mut u8 = core::ptr::null_mut();
static mut DOC_CAPACITY: usize = 0;
static mut DOC_LEN: usize = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositorConfig {
    fb_va: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_size: u32,
    doc_va: u64,
    doc_capacity: u32,
    _pad2: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEvent {
    keycode: u16,
    pressed: u8,
    ascii: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteInsert {
    position: u32,
    byte: u8,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteDelete {
    position: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CursorMove {
    position: u32,
}

fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
/// Get a slice of the document content (read from shared buffer).
fn doc_content() -> &'static [u8] {
    unsafe { core::slice::from_raw_parts(DOC_BUF.add(DOC_HEADER_SIZE), DOC_LEN) }
}
/// Delete a byte at position, shifting subsequent bytes left.
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
/// Insert a byte at position, shifting subsequent bytes right.
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
/// Write the document length to the shared buffer header (offset 0, u64).
/// The editor reads this atomically to know how much content is present.
fn doc_write_header() {
    unsafe {
        core::ptr::write_volatile(DOC_BUF as *mut u64, DOC_LEN as u64);
        core::ptr::write_volatile(DOC_BUF.add(8) as *mut u64, CURSOR_POS as u64);
    }
}
fn max_cols(fb_width: u32) -> usize {
    ((fb_width - 48) / CHAR_W) as usize
}
fn max_text_y(fb_height: u32) -> u32 {
    let text_area_h = fb_height.saturating_sub(48 + 32);

    TEXT_Y + text_area_h - LINE_H - 8
}
/// Draw static chrome (title bar, text area background/border). Called once.
fn render_chrome(fb: &mut drawing::Surface) {
    use drawing::{Color, FONT_8X16 as F};

    fb.clear(Color::rgb(18, 18, 26));
    fb.fill_rect(0, 0, fb.width, 36, Color::rgb(30, 30, 48));
    fb.draw_text(12, 10, "Document OS", &F, Color::rgb(200, 200, 220));

    let subtitle = "Editor Separation Demo";
    let sx = fb.width.saturating_sub(12 + subtitle.len() as u32 * 8);

    fb.draw_text(sx, 10, subtitle, &F, Color::rgb(90, 90, 110));
    fb.draw_hline(0, 36, fb.width, Color::rgb(60, 60, 80));

    let text_area_h = fb.height.saturating_sub(48 + 32);

    fb.fill_rect(
        12,
        TEXT_Y - 4,
        fb.width - 24,
        text_area_h,
        Color::rgb(24, 24, 36),
    );
    fb.draw_rect(
        12,
        TEXT_Y - 4,
        fb.width - 24,
        text_area_h,
        Color::rgb(50, 50, 70),
    );
}
/// Redraw text content, cursor, and status bar. Only touches the text area
/// interior and status bar — leaves the static chrome untouched.
///
/// Optimized: clears and redraws only from the previous cursor row downward,
/// since characters before the edit point haven't changed. For the common
/// case (typing at end of text), this means clearing ~1-2 lines instead
/// of the entire text area (~34x fewer pixels).
fn render_content(fb: &mut drawing::Surface, text: &[u8]) {
    use drawing::{Color, FONT_8X16 as F};

    let bg = Color::rgb(24, 24, 36);
    let cols = max_cols(fb.width);
    let my = max_text_y(fb.height);
    let tc = Color::rgb(200, 210, 230);
    let cursor_pos = unsafe { CURSOR_POS };
    let prev_row = unsafe { CURSOR_ROW };
    let prev_last = unsafe { LAST_TEXT_ROW };
    // Clear only the dirty rows: from the previous cursor row to one row
    // past the previous last text row (handles both growing and shrinking).
    // For appending at end of single-line text, this clears ~1 line.
    let clear_start_y = TEXT_Y + prev_row * LINE_H;
    let clear_end_y = TEXT_Y + (prev_last + 2) * LINE_H;
    let text_bottom = fb.height.saturating_sub(32) - 1;
    let clear_end_y = if clear_end_y > text_bottom {
        text_bottom
    } else {
        clear_end_y
    };

    if clear_start_y < clear_end_y {
        fb.fill_rect(
            13,
            clear_start_y,
            fb.width - 26,
            clear_end_y - clear_start_y,
            bg,
        );
    }

    // Walk text to find the byte offset where prev_row starts, then draw
    // only from that row onward.
    let mut col = 0usize;
    let mut row = 0u32;
    let mut cursor_col = 0usize;
    let mut cursor_row = 0u32;

    for (i, &byte) in text.iter().enumerate() {
        if row * LINE_H + TEXT_Y > my {
            break;
        }

        if i == cursor_pos {
            cursor_col = col;
            cursor_row = row;
        }

        if byte == b'\n' {
            col = 0;
            row += 1;

            continue;
        }

        if col >= cols {
            col = 0;
            row += 1;

            if row * LINE_H + TEXT_Y > my {
                break;
            }
        }

        // Only draw characters on or after the dirty row.
        if row >= prev_row && byte >= 0x20 && byte < 0x7F {
            let ch = [byte];
            let s = unsafe { core::str::from_utf8_unchecked(&ch) };

            fb.draw_text(
                TEXT_X + col as u32 * CHAR_W,
                TEXT_Y + row * LINE_H,
                s,
                &F,
                tc,
            );
        }

        col += 1;
    }

    if cursor_pos >= text.len() {
        cursor_col = col;
        cursor_row = row;
    }

    let cx = TEXT_X + cursor_col as u32 * CHAR_W;
    let cy = TEXT_Y + cursor_row * LINE_H;

    if cy <= my {
        fb.fill_rect(cx, cy, CHAR_W, 16, Color::rgb(100, 180, 255));
    }

    unsafe {
        CURSOR_COL = cursor_col;
        CURSOR_ROW = cursor_row;
        LAST_TEXT_ROW = row;
    }

    render_status(fb, text.len());
}
fn render_status(fb: &mut drawing::Surface, len: usize) {
    use drawing::{Color, FONT_8X16 as F};

    let bar_y = fb.height.saturating_sub(28);

    fb.fill_rect(0, bar_y, fb.width, 28, Color::rgb(30, 30, 48));
    fb.draw_hline(0, bar_y, fb.width, Color::rgb(60, 60, 80));

    let mut buf = [0u8; 64];
    let mut ci = 0;
    let prefix = b"Editor process active | ";

    for &b in prefix {
        if ci < buf.len() {
            buf[ci] = b;
            ci += 1;
        }
    }

    if len == 0 {
        buf[ci] = b'0';
        ci += 1;
    } else {
        let mut digits = [0u8; 6];
        let mut di = 6;
        let mut n = len;

        while n > 0 {
            di -= 1;
            digits[di] = b'0' + (n % 10) as u8;
            n /= 10;
        }

        while di < 6 && ci < buf.len() {
            buf[ci] = digits[di];
            ci += 1;
            di += 1;
        }
    }

    let suffix = b" chars";

    for &b in suffix {
        if ci < buf.len() {
            buf[ci] = b;
            ci += 1;
        }
    }

    let status = unsafe { core::str::from_utf8_unchecked(&buf[..ci]) };

    fb.draw_text(12, bar_y + 6, status, &F, Color::rgb(130, 130, 150));
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    // Read compositor config from ring buffer (channel 0 = init).
    let init_ch = unsafe { ipc::Channel::from_base(channel_shm_va(0), ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !init_ch.try_recv(&mut msg) || msg.msg_type != MSG_COMPOSITOR_CONFIG {
        sys::print(b"compositor: no config message\n");
        sys::exit();
    }

    let config: CompositorConfig = unsafe { msg.payload_as() };
    let fb_va = config.fb_va as usize;
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;
    let fb_stride = config.fb_stride;
    let fb_size = config.fb_size;

    if fb_va == 0 || fb_width == 0 || fb_height == 0 {
        sys::print(b"compositor: bad framebuffer info\n");
        sys::exit();
    }
    // Initialize shared document buffer pointers.
    if config.doc_va == 0 {
        sys::print(b"compositor: no document buffer\n");
        sys::exit();
    }

    unsafe {
        DOC_BUF = config.doc_va as *mut u8;
        DOC_CAPACITY = config.doc_capacity as usize;
        DOC_LEN = 0;
    }

    doc_write_header();

    // Channel 1: input events from input driver (endpoint 1 = recv).
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel 2: GPU present commands (endpoint 0 = send).
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    // Channel 3: editor (endpoint 0 = send input, recv write requests).
    let editor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(3), ipc::PAGE_SIZE, 0) };
    let fb_slice = unsafe { core::slice::from_raw_parts_mut(fb_va as *mut u8, fb_size as usize) };
    let mut fb = drawing::Surface {
        data: fb_slice,
        width: fb_width,
        height: fb_height,
        stride: fb_stride,
        format: drawing::PixelFormat::Bgra8888,
    };

    render_chrome(&mut fb);
    render_content(&mut fb, doc_content());

    let present_msg = ipc::Message::new(MSG_PRESENT);

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(GPU_HANDLE);

    sys::print(b"     initial frame rendered, entering event loop\n");

    // -----------------------------------------------------------------------
    // Event loop: wait for input or editor write requests.
    //
    // Input events from the input driver are forwarded to the editor.
    // Write requests from the editor are applied to the document (sole writer)
    // and the display is re-rendered once per batch (not per message).
    // -----------------------------------------------------------------------
    loop {
        let _ = sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], u64::MAX);
        let mut changed = false;

        // Forward input events to the editor.
        while input_ch.try_recv(&mut msg) {
            if msg.msg_type == MSG_KEY_EVENT {
                editor_ch.send(&msg);

                let _ = sys::channel_signal(EDITOR_HANDLE);
            }
        }

        // Apply write requests from the editor (sole writer).
        // Document mutations are applied immediately; rendering is deferred
        // until all pending messages are drained (batch rendering).
        while editor_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_WRITE_INSERT => {
                    let insert: WriteInsert = unsafe { msg.payload_as() };
                    let pos = insert.position as usize;

                    if doc_insert(pos, insert.byte) {
                        unsafe { CURSOR_POS = pos + 1 };

                        changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let del: WriteDelete = unsafe { msg.payload_as() };
                    let pos = del.position as usize;

                    if doc_delete(pos) {
                        unsafe { CURSOR_POS = pos };

                        changed = true;
                    }
                }
                MSG_CURSOR_MOVE => {
                    let cm: CursorMove = unsafe { msg.payload_as() };
                    let pos = cm.position as usize;
                    let len = unsafe { DOC_LEN };

                    if pos <= len {
                        unsafe { CURSOR_POS = pos };

                        doc_write_header();

                        changed = true;
                    }
                }
                _ => {}
            }
        }

        if changed {
            render_content(&mut fb, doc_content());
            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(GPU_HANDLE);
        }
    }
}
