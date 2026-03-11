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
// Handle indices (determined by the order init sends handles).
const INPUT_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;
const EDITOR_HANDLE: u8 = 3;
// Text area layout constants.
const TEXT_X: u32 = 24;
const TEXT_Y: u32 = 48;
const CHAR_W: u32 = 8;
const LINE_H: u32 = 20;
const TEXT_BUF_SIZE: usize = 2048;

// Cursor position for rendering.
static mut CURSOR_COL: usize = 0;
static mut CURSOR_ROW: u32 = 0;
// Document state — owned exclusively by the compositor (sole writer).
static mut TEXT_BUF: [u8; TEXT_BUF_SIZE] = [0u8; TEXT_BUF_SIZE];
static mut TEXT_LEN: usize = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositorConfig {
    fb_va: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_size: u32,
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
    byte: u8,
}

fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
fn draw_cursor(fb: &mut drawing::Surface, col: usize, row: u32) {
    let x = TEXT_X + col as u32 * CHAR_W;
    let y = TEXT_Y + row * LINE_H;

    if y <= max_text_y(fb.height) {
        fb.fill_rect(x, y, CHAR_W, 16, drawing::Color::rgb(100, 180, 255));
    }
}
fn erase_cursor(fb: &mut drawing::Surface, col: usize, row: u32) {
    let x = TEXT_X + col as u32 * CHAR_W;
    let y = TEXT_Y + row * LINE_H;

    if y <= max_text_y(fb.height) {
        fb.fill_rect(x, y, CHAR_W, 16, drawing::Color::rgb(24, 24, 36));
    }
}
fn max_cols(fb_width: u32) -> usize {
    ((fb_width - 48) / CHAR_W) as usize
}
fn max_text_y(fb_height: u32) -> u32 {
    let text_area_h = fb_height.saturating_sub(48 + 32);

    TEXT_Y + text_area_h - LINE_H - 8
}
fn render_backspace(fb: &mut drawing::Surface) {
    let (col, row) = unsafe { (CURSOR_COL, CURSOR_ROW) };
    let cols = max_cols(fb.width);

    erase_cursor(fb, col, row);

    let (new_col, new_row) = if col > 0 {
        (col - 1, row)
    } else if row > 0 {
        (cols - 1, row - 1)
    } else {
        (0, 0)
    };

    erase_cursor(fb, new_col, new_row);

    unsafe {
        CURSOR_COL = new_col;
        CURSOR_ROW = new_row;
    }

    draw_cursor(fb, new_col, new_row);
}
fn render_char(fb: &mut drawing::Surface, byte: u8) {
    use drawing::{Color, FONT_8X16 as F};

    let (col, row) = unsafe { (CURSOR_COL, CURSOR_ROW) };
    let cols = max_cols(fb.width);
    let my = max_text_y(fb.height);

    erase_cursor(fb, col, row);

    if byte == b'\n' {
        unsafe {
            CURSOR_COL = 0;
            CURSOR_ROW = row + 1;
        }
    } else {
        if byte >= 0x20 && byte < 0x7F && row * LINE_H + TEXT_Y <= my {
            let ch = [byte];
            let s = unsafe { core::str::from_utf8_unchecked(&ch) };

            fb.draw_text(
                TEXT_X + col as u32 * CHAR_W,
                TEXT_Y + row * LINE_H,
                s,
                &F,
                Color::rgb(200, 210, 230),
            );
        }

        let new_col = col + 1;

        if new_col >= cols {
            unsafe {
                CURSOR_COL = 0;
                CURSOR_ROW = row + 1;
            }
        } else {
            unsafe {
                CURSOR_COL = new_col;
            }
        }
    }

    let (nc, nr) = unsafe { (CURSOR_COL, CURSOR_ROW) };

    draw_cursor(fb, nc, nr);
}
fn render_full(fb: &mut drawing::Surface, text: &[u8]) {
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

    let cols = max_cols(fb.width);
    let my = max_text_y(fb.height);
    let tc = Color::rgb(200, 210, 230);
    let mut col = 0usize;
    let mut row = 0u32;

    for &byte in text {
        if row * LINE_H + TEXT_Y > my {
            break;
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

        if byte >= 0x20 && byte < 0x7F {
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

    unsafe {
        CURSOR_COL = col;
        CURSOR_ROW = row;
    }

    draw_cursor(fb, col, row);
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
    let text = unsafe { &TEXT_BUF[..TEXT_LEN] };

    render_full(&mut fb, text);

    let present_msg = ipc::Message::new(MSG_PRESENT);

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(GPU_HANDLE);

    sys::print(b"     initial frame rendered, entering event loop\n");

    // -----------------------------------------------------------------------
    // Event loop: wait for input or editor write requests.
    //
    // Input events from the input driver are forwarded to the editor.
    // Write requests from the editor are applied to the document (sole writer)
    // and the display is updated.
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
        while editor_ch.try_recv(&mut msg) {
            match msg.msg_type {
                MSG_WRITE_INSERT => {
                    let insert: WriteInsert = unsafe { msg.payload_as() };
                    let text_len = unsafe { TEXT_LEN };

                    if text_len < TEXT_BUF_SIZE {
                        unsafe {
                            TEXT_BUF[text_len] = insert.byte;
                            TEXT_LEN += 1;
                        }

                        render_char(&mut fb, insert.byte);

                        changed = true;
                    }
                }
                MSG_WRITE_DELETE => {
                    let text_len = unsafe { TEXT_LEN };

                    if text_len > 0 {
                        unsafe { TEXT_LEN -= 1 };
                        render_backspace(&mut fb);
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        if changed {
            render_status(&mut fb, unsafe { TEXT_LEN });

            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(GPU_HANDLE);
        }
    }
}
