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

use alloc::boxed::Box;

const CHANNEL_SHM_BASE: usize = 0x4000_0000;
const FONT_SIZE: u32 = 16;
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
// Document header layout (first 64 bytes of shared buffer).
const DOC_HEADER_SIZE: usize = 64;

static mut CHAR_W: u32 = 8;
static mut LINE_H: u32 = 20;
/// Pre-rasterized glyph cache (heap-allocated, initialized at startup).
static mut GLYPH_CACHE: *const drawing::GlyphCache = core::ptr::null();
/// Cursor byte offset in the document. Updated by write requests.
static mut CURSOR_POS: usize = 0;
/// Previous cursor pixel Y (for dirty-rect clearing).
static mut PREV_CURSOR_Y: [u32; 2] = [0; 2];
/// Previous last-drawn pixel Y (for dirty-rect clearing).
static mut PREV_LAST_Y: [u32; 2] = [0; 2];
/// Current back buffer index (0 or 1). Swapped after each present.
static mut BACK_BUF_IDX: usize = 0;
// Document shared buffer — owned exclusively by the compositor (sole writer).
// Set from config message; editor has a read-only mapping of the same pages.
static mut DOC_BUF: *mut u8 = core::ptr::null_mut();
static mut DOC_CAPACITY: usize = 0;
static mut DOC_LEN: usize = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositorConfig {
    fb_va: u64,
    fb_va2: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_size: u32,
    doc_va: u64,
    doc_capacity: u32,
    font_len: u32,
    font_va: u64,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CursorMove {
    position: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct PresentInfo {
    buffer_index: u32,
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
struct WriteDelete {
    position: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct WriteInsert {
    position: u32,
    byte: u8,
}

fn channel_shm_va(idx: usize) -> usize {
    CHANNEL_SHM_BASE + idx * 2 * 4096
}
/// Draw a byte string using the glyph cache (simple helper, no wrapping).
fn draw_string(
    fb: &mut drawing::Surface,
    x: u32,
    y: u32,
    text: &[u8],
    cache: &drawing::GlyphCache,
    color: drawing::Color,
) {
    let baseline_y = y as i32 + (cache.line_height * 3 / 4) as i32;
    let mut cx = x as i32;

    for &byte in text {
        if let Some((glyph, coverage)) = cache.get(byte) {
            if glyph.width > 0 && glyph.height > 0 {
                let gx = cx + glyph.bearing_x;
                let gy = baseline_y - glyph.bearing_y;

                fb.draw_coverage(gx, gy, coverage, glyph.width, glyph.height, color);
            }

            cx += glyph.advance as i32;
        } else {
            cx += unsafe { CHAR_W } as i32;
        }
    }
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
fn text_layout(fb_width: u32) -> drawing::TextLayout {
    let cache = unsafe { &*GLYPH_CACHE };

    drawing::TextLayout {
        char_width: unsafe { CHAR_W },
        line_height: cache.line_height,
        max_width: fb_width - 48,
    }
}
fn max_text_y(fb_height: u32) -> u32 {
    let text_area_h = fb_height.saturating_sub(48 + 32);

    TEXT_Y + text_area_h - unsafe { LINE_H } - 8
}
/// Draw static chrome (title bar, text area background/border). Called once.
fn render_chrome(fb: &mut drawing::Surface, cache: &drawing::GlyphCache) {
    use drawing::Color;

    fb.clear(Color::rgb(18, 18, 26));
    fb.fill_rect(0, 0, fb.width, 36, Color::rgb(30, 30, 48));

    draw_string(fb, 12, 10, b"Document OS", cache, Color::rgb(200, 200, 220));

    let subtitle = b"Editor Separation Demo";
    let sub_w = subtitle.len() as u32 * unsafe { CHAR_W };
    let sx = fb.width.saturating_sub(12 + sub_w);

    draw_string(fb, sx, 10, subtitle, cache, Color::rgb(90, 90, 110));

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
/// Dirty-rect optimization: clears from the previous cursor Y downward,
/// then delegates to TextLayout::draw for positioning and rendering.
fn render_content(fb: &mut drawing::Surface, text: &[u8], buf_idx: usize) {
    use drawing::Color;

    let bg = Color::rgb(24, 24, 36);
    let my = max_text_y(fb.height);
    let layout = text_layout(fb.width);
    let cache = unsafe { &*GLYPH_CACHE };
    let cursor_pos = unsafe { CURSOR_POS };
    let prev_last_y = unsafe { PREV_LAST_Y[buf_idx] };
    // Clear the entire text region that was previously drawn. draw_tt()
    // re-renders ALL text from the top, so we must clear from the top of
    // the text area down through the previous last row (plus cursor height).
    // Without this, lines above the cursor would accumulate alpha blending
    // across frames, producing progressively thicker/bolder strokes.
    let clear_end_y = TEXT_Y + prev_last_y + 2 * cache.line_height;
    let text_bottom = fb.height.saturating_sub(32) - 1;
    let clear_end_y = if clear_end_y > text_bottom {
        text_bottom
    } else {
        clear_end_y
    };

    if TEXT_Y < clear_end_y {
        fb.fill_rect(13, TEXT_Y, fb.width - 26, clear_end_y - TEXT_Y, bg);
    }

    let (_, cursor_y) = layout.draw_tt(
        fb,
        text,
        TEXT_X,
        TEXT_Y,
        cursor_pos,
        cache,
        Color::rgb(200, 210, 230),
        Color::rgb(100, 180, 255),
        my,
    );
    // Track dirty state for next frame (per-buffer for double buffering).
    let (_, last_y) = layout.byte_to_xy(text, text.len());

    unsafe {
        PREV_CURSOR_Y[buf_idx] = cursor_y - TEXT_Y;
        PREV_LAST_Y[buf_idx] = last_y;
    }

    render_status(fb, text.len());
}
fn render_status(fb: &mut drawing::Surface, len: usize) {
    use drawing::Color;

    let cache = unsafe { &*GLYPH_CACHE };
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

    draw_string(
        fb,
        12,
        bar_y + 6,
        &buf[..ci],
        cache,
        Color::rgb(130, 130, 150),
    );
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
    let fb_va2 = config.fb_va2 as usize;
    let fb_width = config.fb_width;
    let fb_height = config.fb_height;
    let fb_stride = config.fb_stride;
    let fb_size = config.fb_size;

    if fb_va == 0 || fb_va2 == 0 || fb_width == 0 || fb_height == 0 {
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

    // Load font from runtime buffer (shared by init via 9p driver).
    if config.font_va == 0 || config.font_len == 0 {
        sys::print(b"compositor: no font data provided\n");
        sys::exit();
    }

    let font_data = unsafe {
        core::slice::from_raw_parts(config.font_va as *const u8, config.font_len as usize)
    };
    let ttf = drawing::TrueTypeFont::new(font_data).unwrap_or_else(|| {
        sys::print(b"compositor: failed to parse font\n");
        sys::exit();
    });
    let mut cache: Box<drawing::GlyphCache> = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::GlyphCache>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;

        if ptr.is_null() {
            sys::print(b"compositor: glyph cache alloc failed\n");
            sys::exit();
        }

        Box::from_raw(ptr)
    };
    let mut scratch: Box<drawing::RasterScratch> = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::RasterScratch>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::RasterScratch;

        if ptr.is_null() {
            sys::print(b"compositor: scratch alloc failed\n");
            sys::exit();
        }

        Box::from_raw(ptr)
    };

    cache.populate(&ttf, FONT_SIZE, &mut scratch);

    drop(scratch);

    // Read advance width from space glyph (monospace: all glyphs same width).
    if let Some((g, _)) = cache.get(b' ') {
        unsafe { CHAR_W = g.advance };
    }

    unsafe {
        LINE_H = cache.line_height;
        GLYPH_CACHE = Box::into_raw(cache);
    }

    sys::print(b"     font rasterized (Source Code Pro 16px)\n");

    // Channel 1: input events from input driver (endpoint 1 = recv).
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel 2: GPU present commands (endpoint 0 = send).
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    // Channel 3: editor (endpoint 0 = send input, recv write requests).
    let editor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(3), ipc::PAGE_SIZE, 0) };

    // -----------------------------------------------------------------------
    // Double buffering: two separate framebuffer allocations.
    // Buffer 0 at fb_va, buffer 1 at fb_va2.
    // The compositor renders into the back buffer, then presents it.
    // -----------------------------------------------------------------------

    // Static storage for the two surface base pointers. We need stable
    // references across iterations, so we store the raw pointers and
    // rebuild Surfaces on demand.
    static mut FB_PTRS: [*mut u8; 2] = [core::ptr::null_mut(); 2];

    unsafe {
        FB_PTRS[0] = fb_va as *mut u8;
        FB_PTRS[1] = fb_va2 as *mut u8;
    }

    let make_surface = |idx: usize| -> drawing::Surface<'static> {
        let ptr = unsafe { FB_PTRS[idx] };
        let data = unsafe { core::slice::from_raw_parts_mut(ptr, fb_size as usize) };

        drawing::Surface {
            data,
            width: fb_width,
            height: fb_height,
            stride: fb_stride,
            format: drawing::PixelFormat::Bgra8888,
        }
    };

    // Render chrome on BOTH buffers (chrome is static, drawn once).
    {
        let mut fb0 = make_surface(0);

        render_chrome(&mut fb0, unsafe { &*GLYPH_CACHE });

        let mut fb1 = make_surface(1);

        render_chrome(&mut fb1, unsafe { &*GLYPH_CACHE });
    }

    // Render initial content into buffer 0 and present it.
    {
        let mut fb0 = make_surface(0);

        render_content(&mut fb0, doc_content(), 0);
    }

    let present_info = PresentInfo { buffer_index: 0 };
    let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &present_info) };

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(GPU_HANDLE);

    // Buffer 0 is now the front (being displayed). Next render goes to buffer 1.
    unsafe { BACK_BUF_IDX = 1 };

    sys::print(b"     double-buffered, initial frame rendered, entering event loop\n");

    // -----------------------------------------------------------------------
    // Event loop: wait for input or editor write requests.
    //
    // Input events from the input driver are forwarded to the editor.
    // Write requests from the editor are applied to the document (sole writer)
    // and the display is re-rendered once per batch (not per message).
    //
    // Double buffering: render into back buffer, present it, swap.
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
            let back = unsafe { BACK_BUF_IDX };
            let mut fb = make_surface(back);

            render_content(&mut fb, doc_content(), back);

            // Present the back buffer (tell GPU which buffer to transfer).
            let info = PresentInfo {
                buffer_index: back as u32,
            };
            let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &info) };

            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(GPU_HANDLE);

            // Swap: the just-presented buffer becomes front, the other becomes back.
            unsafe { BACK_BUF_IDX = 1 - back };
        }
    }
}
