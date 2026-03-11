//! Compositor — interactive display with keyboard input.
//!
//! Receives framebuffer config from init, keyboard events from the input
//! driver, and sends present commands to the GPU driver. Runs a continuous
//! event loop: wait for input → update display → present.
//!
//! # Rendering strategy
//!
//! Full re-render on startup only. On each keystroke, only the affected
//! character cell and cursor are redrawn (~256 bytes vs ~6 MiB). This is
//! critical for acceptable performance under TCG (software emulation).
//!
//! # Architecture note
//!
//! In the real OS, this process would be the OS service (renderer + input
//! router + compositor). Input would be routed to the active editor, which
//! would modify document state via the edit protocol. The OS service would
//! then re-render based on document state. For this demo, the compositor
//! plays both roles: it interprets keyboard input (editor role) and renders
//! the result (OS service role).

#![no_std]
#![no_main]

extern crate alloc;

/// Channel shared memory base (first channel in our address space).
const CHANNEL_SHM_BASE: usize = 0x4000_0000;
// Protocol message types (must match init/driver definitions).
const MSG_COMPOSITOR_CONFIG: u32 = 3;
const MSG_KEY_EVENT: u32 = 10;
const MSG_PRESENT: u32 = 20;
/// Maximum text buffer size (characters).
const TEXT_BUF_SIZE: usize = 2048;
// Handle indices (determined by the order init sends handles).
const INPUT_HANDLE: u8 = 1;
// Text area layout constants.
const TEXT_X: u32 = 24;
const TEXT_Y: u32 = 48;
const CHAR_W: u32 = 8;
const LINE_H: u32 = 20;
const BG_COLOR: u32 = 0xFF1A1218; // BGRA for rgb(18, 18, 26)
const TEXT_AREA_BG: u32 = 0xFF241824; // BGRA for rgb(24, 24, 36)

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositorConfig {
    fb_va: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_size: u32,
}
/// Key event received from the input driver.
#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEvent {
    keycode: u16,
    pressed: u8,
    ascii: u8,
}

/// Mutable text state — accumulated keystrokes.
static mut TEXT_BUF: [u8; TEXT_BUF_SIZE] = [0u8; TEXT_BUF_SIZE];
static mut TEXT_LEN: usize = 0;
/// Cursor position (column, row) for incremental rendering.
static mut CURSOR_COL: usize = 0;
static mut CURSOR_ROW: u32 = 0;

/// Compute the base VA of channel N's shared pages.
fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
/// Draw the block cursor at (col, row).
fn draw_cursor(fb: &mut drawing::Surface, col: usize, row: u32) {
    let x = TEXT_X + col as u32 * CHAR_W;
    let y = TEXT_Y + row * LINE_H;

    if y <= max_text_y(fb.height) {
        fb.fill_rect(x, y, CHAR_W, 16, drawing::Color::rgb(100, 180, 255));
    }
}
/// Erase the block cursor at (col, row) by filling with text area background.
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
/// Incremental render: erase last character, move cursor back.
fn render_backspace(fb: &mut drawing::Surface) {
    let (col, row) = unsafe { (CURSOR_COL, CURSOR_ROW) };
    let cols = max_cols(fb.width);

    // Erase current cursor.
    erase_cursor(fb, col, row);

    // Move cursor back.
    let (new_col, new_row) = if col > 0 {
        (col - 1, row)
    } else if row > 0 {
        // Backspace at start of line: go to end of previous line.
        // For simplicity, go to max_cols - 1 (might not be exact for
        // short lines, but good enough for the demo).
        (cols - 1, row - 1)
    } else {
        (0, 0)
    };

    // Erase the character at the new position.
    erase_cursor(fb, new_col, new_row);

    unsafe {
        CURSOR_COL = new_col;
        CURSOR_ROW = new_row;
    }

    // Draw cursor at new position.
    draw_cursor(fb, new_col, new_row);
}
/// Incremental render: draw one character at cursor, advance cursor.
fn render_char(fb: &mut drawing::Surface, byte: u8) {
    use drawing::{Color, FONT_8X16 as F};

    let (col, row) = unsafe { (CURSOR_COL, CURSOR_ROW) };
    let cols = max_cols(fb.width);
    let my = max_text_y(fb.height);

    // Erase old cursor.
    erase_cursor(fb, col, row);

    if byte == b'\n' {
        unsafe {
            CURSOR_COL = 0;
            CURSOR_ROW = row + 1;
        }
    } else {
        // Draw the character.
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

        // Advance cursor.
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

    // Draw new cursor.
    let (nc, nr) = unsafe { (CURSOR_COL, CURSOR_ROW) };

    draw_cursor(fb, nc, nr);
}
/// Full render — called once at startup.
fn render_full(fb: &mut drawing::Surface, text: &[u8]) {
    use drawing::{Color, FONT_8X16 as F};

    fb.clear(Color::rgb(18, 18, 26));
    // Header bar.
    fb.fill_rect(0, 0, fb.width, 36, Color::rgb(30, 30, 48));
    fb.draw_text(12, 10, "Document OS", &F, Color::rgb(200, 200, 220));

    let subtitle = "Interactive Demo";
    let sx = fb.width.saturating_sub(12 + subtitle.len() as u32 * 8);

    fb.draw_text(sx, 10, subtitle, &F, Color::rgb(90, 90, 110));
    fb.draw_hline(0, 36, fb.width, Color::rgb(60, 60, 80));

    // Text area.
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

    // Render existing text.
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

    // Save cursor position.
    unsafe {
        CURSOR_COL = col;
        CURSOR_ROW = row;
    }

    // Draw cursor.
    draw_cursor(fb, col, row);
    // Status bar.
    render_status(fb, text.len());
}
/// Render status bar (small area — always fast).
fn render_status(fb: &mut drawing::Surface, len: usize) {
    use drawing::{Color, FONT_8X16 as F};

    let bar_y = fb.height.saturating_sub(28);

    fb.fill_rect(0, bar_y, fb.width, 28, Color::rgb(30, 30, 48));
    fb.draw_hline(0, bar_y, fb.width, Color::rgb(60, 60, 80));

    let mut buf = [0u8; 64];
    let mut ci = 0;
    let prefix = b"Type to enter text | Backspace to delete | ";

    for &b in prefix {
        buf[ci] = b;
        ci += 1;
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

    let status = unsafe { core::str::from_utf8_unchecked(&buf[..ci]) };

    fb.draw_text(12, bar_y + 6, status, &F, Color::rgb(130, 130, 150));
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    // Read compositor config from ring buffer (first message on channel 0).
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

    // Channel 1: input events (endpoint 1 = receive side).
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel 2: GPU present commands (endpoint 0 = send side).
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    // Create framebuffer surface.
    let fb_slice = unsafe { core::slice::from_raw_parts_mut(fb_va as *mut u8, fb_size as usize) };
    let mut fb = drawing::Surface {
        data: fb_slice,
        width: fb_width,
        height: fb_height,
        stride: fb_stride,
        format: drawing::PixelFormat::Bgra8888,
    };
    // Full initial render.
    let text = unsafe { &TEXT_BUF[..TEXT_LEN] };

    render_full(&mut fb, text);

    // Signal GPU to present the initial frame.
    let present_msg = ipc::Message::new(MSG_PRESENT);

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(2);

    sys::print(b"     initial frame rendered, entering event loop\n");

    // -----------------------------------------------------------------------
    // Event loop: wait for input → incremental update → present
    // -----------------------------------------------------------------------
    loop {
        let _ = sys::wait(&[INPUT_HANDLE], u64::MAX);

        let mut changed = false;

        while input_ch.try_recv(&mut msg) {
            if msg.msg_type != MSG_KEY_EVENT {
                continue;
            }

            let key: KeyEvent = unsafe { msg.payload_as() };

            if key.pressed != 1 {
                continue;
            }

            let text_len = unsafe { TEXT_LEN };

            if key.ascii == 0x08 {
                // Backspace.
                if text_len > 0 {
                    unsafe { TEXT_LEN -= 1 };
                    render_backspace(&mut fb);
                    changed = true;
                }
            } else if key.ascii != 0 && text_len < TEXT_BUF_SIZE {
                // Regular character.
                unsafe {
                    TEXT_BUF[text_len] = key.ascii;
                    TEXT_LEN += 1;
                }
                render_char(&mut fb, key.ascii);
                changed = true;
            }
        }

        if changed {
            // Update status bar (small, always fast).
            render_status(&mut fb, unsafe { TEXT_LEN });

            // Present.
            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(2);
        }
    }
}
