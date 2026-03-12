//! Compositor — multi-surface compositing model.
//!
//! Manages a set of independently-renderable surfaces, composited back-to-front
//! into the framebuffer using alpha blending each frame.
//!
//! # Surface layers (z-order bottom to top)
//!
//!   z=0:  Background    — full-screen solid color
//!   z=10: Content       — text editing area with cursor
//!   z=15: Title shadow  — gradient falloff beneath title bar
//!   z=15: Status shadow — gradient falloff above status bar
//!   z=20: Title bar     — translucent chrome overlay at top
//!   z=20: Status bar    — translucent chrome overlay at bottom
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
use alloc::vec;

const CHANNEL_SHM_BASE: usize = 0x4000_0000;
const FONT_SIZE: u32 = 16;
// Protocol message types.
const MSG_COMPOSITOR_CONFIG: u32 = 3;
const MSG_IMAGE_CONFIG: u32 = 6;
const MSG_KEY_EVENT: u32 = 10;
const MSG_PRESENT: u32 = 20;
const MSG_WRITE_INSERT: u32 = 30;
const MSG_WRITE_DELETE: u32 = 31;
const MSG_CURSOR_MOVE: u32 = 32;
// Linux evdev keycode for F1 — used as the context switch key.
// F1 is beyond the ASCII keymap (keycodes 0-57), so it won't produce
// a printable character and doesn't conflict with any editor keys.
const KEY_F1: u16 = 59;
// Handle indices (determined by the order init sends handles).
const INPUT_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;
const EDITOR_HANDLE: u8 = 3;
// Surface z-order constants.
const Z_BACKGROUND: u16 = 0;
const Z_CONTENT: u16 = 10;
const Z_SHADOW: u16 = 15;
const Z_CHROME: u16 = 20;
// Drop shadow configuration.
const SHADOW_DEPTH: u32 = 8;
const SHADOW_ALPHA_MAX: u8 = 80;
// Chrome dimensions.
const TITLE_BAR_H: u32 = 36;
const STATUS_BAR_H: u32 = 28;
// Content area insets (relative to framebuffer).
// The content surface now extends full-screen so that document content is
// visible through translucent chrome (title bar and status bar).
const CONTENT_MARGIN_X: u32 = 0;
const CONTENT_MARGIN_TOP: u32 = 0;
const CONTENT_MARGIN_BOTTOM: u32 = 0;
// Text insets within the content surface. Text starts near the top of the
// content surface so that document content is genuinely visible through
// the translucent chrome overlays — not just a background color.
const TEXT_INSET_X: u32 = 12;
const TEXT_INSET_TOP: u32 = 4;
const TEXT_INSET_BOTTOM: u32 = 4;
// Document header layout (first 64 bytes of shared buffer).
const DOC_HEADER_SIZE: usize = 64;

/// Whether the compositor is in image viewer mode (true) or text editor mode (false).
static mut IMAGE_MODE: bool = false;
/// Decoded image width (pixels). Set when PNG is decoded.
static mut IMAGE_WIDTH: u32 = 0;
/// Decoded image height (pixels). Set when PNG is decoded.
static mut IMAGE_HEIGHT: u32 = 0;
/// Counter value captured at boot for deriving elapsed wall-clock time.
static mut BOOT_COUNTER: u64 = 0;
/// Counter frequency in Hz (read once at boot).
static mut COUNTER_FREQ: u64 = 0;
/// Current timer handle for the 1-second periodic clock. 0 = no timer.
static mut TIMER_HANDLE: u8 = 0;
/// Whether a valid timer handle exists.
static mut TIMER_ACTIVE: bool = false;

static mut CHAR_W: u32 = 8;
static mut LINE_H: u32 = 20;
/// Pre-rasterized glyph cache for monospace font (heap-allocated, initialized at startup).
static mut GLYPH_CACHE: *const drawing::GlyphCache = core::ptr::null();
/// Pre-rasterized glyph cache for proportional font (chrome text).
static mut PROP_GLYPH_CACHE: *const drawing::GlyphCache = core::ptr::null();
/// Cursor byte offset in the document. Updated by write requests.
static mut CURSOR_POS: usize = 0;
/// Previous last-drawn pixel Y per content surface render (for clearing).
static mut PREV_LAST_Y: u32 = 0;
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
    mono_font_len: u32,
    mono_font_va: u64,
    prop_font_len: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct CursorMove {
    position: u32,
}
/// Payload for MSG_PRESENT — includes dirty rects for partial GPU transfer.
///
/// Layout (60 bytes total payload):
///   buffer_index: u32   (4 bytes) — which double-buffer to present
///   rect_count: u32     (4 bytes) — number of dirty rects (0 = full screen)
///   rects: [DirtyRect; 6] (48 bytes) — up to 6 dirty rects (8 bytes each)
///   _pad: [u8; 4]       (4 bytes) — padding to fill 60 bytes
///
/// When rect_count == 0, the GPU transfers the entire framebuffer.
/// When rect_count > 0, the GPU transfers only the specified rects.
#[repr(C)]
#[derive(Clone, Copy)]
struct PresentPayload {
    buffer_index: u32,
    rect_count: u32,
    rects: [drawing::DirtyRect; 6],
    _pad: [u8; 4],
}
/// Image configuration received from init. Contains the location of
/// raw PNG data in the shared memory buffer.
#[repr(C)]
#[derive(Clone, Copy)]
struct ImageConfig {
    image_va: u64,
    image_len: u32,
    _pad: u32,
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
/// Compute the pixel width of a string using proportional glyph advances.
fn proportional_string_width(text: &[u8], cache: &drawing::GlyphCache) -> u32 {
    let fallback = match cache.get(b' ') {
        Some((g, _)) => g.advance,
        None => 8,
    };
    let mut w = 0u32;

    for &byte in text {
        if let Some((glyph, _)) = cache.get(byte) {
            w += glyph.advance;
        } else {
            w += fallback;
        }
    }

    w
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
/// Build a TextLayout for the content surface.
fn content_text_layout(content_w: u32) -> drawing::TextLayout {
    let cache = unsafe { &*GLYPH_CACHE };

    drawing::TextLayout {
        char_width: unsafe { CHAR_W },
        line_height: cache.line_height,
        max_width: content_w - 2 * TEXT_INSET_X,
    }
}
/// Maximum Y coordinate for text within the content surface (local coords).
/// Text must stay above the status bar chrome area.
fn max_text_y_in_content(content_h: u32) -> u32 {
    content_h.saturating_sub(unsafe { LINE_H } + TEXT_INSET_BOTTOM)
}

// ---------------------------------------------------------------------------
// Surface rendering functions
// ---------------------------------------------------------------------------

/// Render the background surface: solid dark color, full screen.
fn render_background(surf: &mut drawing::Surface) {
    use drawing::Color;

    surf.clear(Color::rgb(18, 18, 26));
}

/// Render the content surface: text area background, text content, and cursor.
///
/// The content surface extends full-screen so that document content is
/// visible through the translucent chrome (title bar and status bar).
/// Text is rendered with margins that keep it below the title bar and
/// above the status bar, but the background fills the entire surface.
fn render_content_surface(
    surf: &mut drawing::Surface,
    text: &[u8],
) {
    use drawing::Color;

    let bg = Color::rgb(24, 24, 36);
    let cache = unsafe { &*GLYPH_CACHE };
    let cursor_pos = unsafe { CURSOR_POS };
    let prev_last_y = unsafe { PREV_LAST_Y };
    let content_w = surf.width;
    let content_h = surf.height;

    // Clear the text rendering area. We clear from the text top through
    // previous last rendered Y + some margin. On first render, clear everything.
    let clear_end_y = TEXT_INSET_TOP + prev_last_y + 2 * cache.line_height;
    let clear_end_y = if clear_end_y > content_h {
        content_h
    } else {
        clear_end_y
    };

    if TEXT_INSET_TOP < clear_end_y {
        surf.fill_rect(0, TEXT_INSET_TOP, content_w, clear_end_y - TEXT_INSET_TOP, bg);
    }

    // Fill the area above text (behind the title bar chrome).
    surf.fill_rect(0, 0, content_w, TEXT_INSET_TOP, bg);

    // Fill the area below text (behind the status bar chrome).
    let status_top = content_h.saturating_sub(TEXT_INSET_BOTTOM);
    if status_top < content_h {
        surf.fill_rect(0, status_top, content_w, TEXT_INSET_BOTTOM, bg);
    }

    let layout = content_text_layout(content_w);
    let my = max_text_y_in_content(content_h);

    let (_, _cursor_y) = layout.draw_tt(
        surf,
        text,
        TEXT_INSET_X,
        TEXT_INSET_TOP,
        cursor_pos,
        cache,
        Color::rgb(200, 210, 230),
        Color::rgb(100, 180, 255),
        my,
    );

    // Track last rendered Y for next frame's clear optimization.
    let (_, last_y) = layout.byte_to_xy(text, text.len());

    unsafe { PREV_LAST_Y = last_y };
}

/// Render the image viewer content surface: display a decoded PNG image
/// centered within the content area. If the image is larger than the
/// content area, it is clipped to fit (no scaling — clipping is simpler
/// and preserves pixel-perfect rendering).
fn render_image_content_surface(
    surf: &mut drawing::Surface,
    image_data: &[u8],
    image_w: u32,
    image_h: u32,
) {
    use drawing::Color;

    // Clear to background color.
    let bg = Color::rgb(24, 24, 36);
    surf.clear(bg);

    if image_w == 0 || image_h == 0 || image_data.is_empty() {
        return;
    }

    let content_w = surf.width;
    let content_h = surf.height;

    // Center the image within the content area.
    let dst_x = if image_w < content_w {
        (content_w - image_w) / 2
    } else {
        0
    };
    let dst_y = if image_h < content_h {
        (content_h - image_h) / 2
    } else {
        0
    };

    // Use blit_blend so alpha-transparent pixels composite correctly.
    let image_stride = image_w * 4;
    surf.blit_blend(image_data, image_w, image_h, image_stride, dst_x, dst_y);
}

/// Render the title bar drop shadow: gradient from opaque to transparent,
/// falling downward from the title bar's bottom edge.
fn render_title_shadow(surf: &mut drawing::Surface) {
    use drawing::Color;

    surf.clear(Color::TRANSPARENT);

    surf.fill_gradient_v(
        0, 0, surf.width, surf.height,
        Color::rgba(0, 0, 0, SHADOW_ALPHA_MAX),
        Color::rgba(0, 0, 0, 0),
    );
}

/// Render the status bar drop shadow: gradient from transparent to opaque,
/// falling upward from the status bar's top edge.
fn render_status_shadow(surf: &mut drawing::Surface) {
    use drawing::Color;

    surf.clear(Color::TRANSPARENT);

    surf.fill_gradient_v(
        0, 0, surf.width, surf.height,
        Color::rgba(0, 0, 0, 0),
        Color::rgba(0, 0, 0, SHADOW_ALPHA_MAX),
    );
}

/// Render the title bar chrome surface (translucent overlay).
/// Uses the proportional font (Nunito Sans) for chrome text.
fn render_title_bar(surf: &mut drawing::Surface) {
    use drawing::Color;

    let prop_cache = unsafe { &*PROP_GLYPH_CACHE };

    // Translucent background.
    surf.clear(Color::rgba(30, 30, 48, 220));

    // Title text (proportional font).
    drawing::draw_proportional_string(surf, 12, 10, b"Document OS", prop_cache, Color::rgb(200, 200, 220));

    // Subtitle on the right (proportional font).
    // Estimate width by summing per-glyph advances.
    let subtitle = b"Multi-Surface Compositor";
    let sub_w = proportional_string_width(subtitle, prop_cache);
    let sx = surf.width.saturating_sub(12 + sub_w);

    drawing::draw_proportional_string(surf, sx, 10, subtitle, prop_cache, Color::rgb(90, 90, 110));

    // Bottom edge line.
    surf.draw_hline(0, surf.height - 1, surf.width, Color::rgba(60, 60, 80, 200));
}

/// Render the status bar chrome surface (translucent overlay).
/// Uses the proportional font (Nunito Sans) for chrome text.
/// In image viewer mode, shows image dimensions instead of character count.
fn render_status_bar(surf: &mut drawing::Surface, text_len: usize) {
    use drawing::Color;

    let prop_cache = unsafe { &*PROP_GLYPH_CACHE };
    let in_image_mode = unsafe { IMAGE_MODE };

    // Translucent background.
    surf.clear(Color::rgba(30, 30, 48, 220));

    // Top edge line.
    surf.draw_hline(0, 0, surf.width, Color::rgba(60, 60, 80, 200));

    // Status text.
    let mut buf = [0u8; 64];
    let mut ci = 0;

    if in_image_mode {
        let prefix = b"Image viewer | ";
        for &b in prefix {
            if ci < buf.len() {
                buf[ci] = b;
                ci += 1;
            }
        }

        // Append image dimensions: WIDTHxHEIGHT px
        let img_w = unsafe { IMAGE_WIDTH };
        let img_h = unsafe { IMAGE_HEIGHT };

        ci = append_u32(&mut buf, ci, img_w);

        if ci < buf.len() {
            buf[ci] = b'x';
            ci += 1;
        }

        ci = append_u32(&mut buf, ci, img_h);

        let suffix = b" px";
        for &b in suffix {
            if ci < buf.len() {
                buf[ci] = b;
                ci += 1;
            }
        }
    } else {
        let prefix = b"Editor process active | ";
        for &b in prefix {
            if ci < buf.len() {
                buf[ci] = b;
                ci += 1;
            }
        }

        if text_len == 0 {
            buf[ci] = b'0';
            ci += 1;
        } else {
            ci = append_u32(&mut buf, ci, text_len as u32);
        }

        let suffix = b" chars";
        for &b in suffix {
            if ci < buf.len() {
                buf[ci] = b;
                ci += 1;
            }
        }
    }

    drawing::draw_proportional_string(
        surf,
        12,
        6,
        &buf[..ci],
        prop_cache,
        Color::rgb(130, 130, 150),
    );

    // Clock display (right-aligned): HH:MM:SS
    let mut time_buf = [0u8; 8];
    let secs = elapsed_seconds();

    format_time_hms(secs, &mut time_buf);

    let time_w = proportional_string_width(&time_buf, prop_cache);
    let time_x = surf.width.saturating_sub(12 + time_w);

    drawing::draw_proportional_string(
        surf,
        time_x,
        6,
        &time_buf,
        prop_cache,
        Color::rgb(160, 170, 190),
    );
}

/// Format total seconds into HH:MM:SS in the given 8-byte buffer.
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

/// Get elapsed seconds since boot using the ARM generic counter.
fn elapsed_seconds() -> u64 {
    let now = sys::counter();
    let boot = unsafe { BOOT_COUNTER };
    let freq = unsafe { COUNTER_FREQ };

    if freq == 0 {
        return 0;
    }

    (now - boot) / freq
}

/// Create a new 1-second periodic timer. Stores the handle in TIMER_HANDLE.
/// Returns true on success.
fn create_clock_timer() -> bool {
    match sys::timer_create(1_000_000_000) {
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

/// Append a u32 as decimal digits to a byte buffer. Returns the new index.
fn append_u32(buf: &mut [u8], start: usize, val: u32) -> usize {
    let mut ci = start;

    if val == 0 {
        if ci < buf.len() {
            buf[ci] = b'0';
            ci += 1;
        }
        return ci;
    }

    let mut digits = [0u8; 10];
    let mut di = 10;
    let mut n = val;

    while n > 0 {
        di -= 1;
        digits[di] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    while di < 10 && ci < buf.len() {
        buf[ci] = digits[di];
        ci += 1;
        di += 1;
    }

    ci
}

/// Allocate a pixel buffer for a surface with given dimensions.
fn alloc_surface_buf(width: u32, height: u32) -> alloc::vec::Vec<u8> {
    let stride = width * 4; // BGRA8888
    let size = (stride * height) as usize;

    vec![0u8; size]
}

/// Build a Surface from a mutable byte slice.
fn make_surf(buf: &mut [u8], w: u32, h: u32) -> drawing::Surface<'_> {
    drawing::Surface {
        data: buf,
        width: w,
        height: h,
        stride: w * 4,
        format: drawing::PixelFormat::Bgra8888,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Capture boot time counter for deriving wall-clock seconds since boot.
    unsafe {
        BOOT_COUNTER = sys::counter();
        COUNTER_FREQ = sys::counter_freq();
    }

    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting (multi-surface)\n");

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

    // Load monospace font from runtime buffer (shared by init via 9p driver).
    if config.mono_font_va == 0 || config.mono_font_len == 0 {
        sys::print(b"compositor: no monospace font data provided\n");
        sys::exit();
    }

    let mono_font_data = unsafe {
        core::slice::from_raw_parts(config.mono_font_va as *const u8, config.mono_font_len as usize)
    };
    let mono_ttf = drawing::TrueTypeFont::new(mono_font_data).unwrap_or_else(|| {
        sys::print(b"compositor: failed to parse monospace font\n");
        sys::exit();
    });
    let mut mono_cache: Box<drawing::GlyphCache> = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::GlyphCache>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;

        if ptr.is_null() {
            sys::print(b"compositor: mono glyph cache alloc failed\n");
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

    mono_cache.populate(&mono_ttf, FONT_SIZE, &mut scratch);

    // Read advance width from space glyph (monospace: all glyphs same width).
    if let Some((g, _)) = mono_cache.get(b' ') {
        unsafe { CHAR_W = g.advance };
    }

    unsafe {
        LINE_H = mono_cache.line_height;
        GLYPH_CACHE = Box::into_raw(mono_cache);
    }

    sys::print(b"     monospace font rasterized (Source Code Pro 16px)\n");

    // Load proportional font (Nunito Sans) for chrome text.
    // Proportional font is stored right after the monospace font in the same buffer.
    if config.prop_font_len > 0 {
        let prop_font_data = unsafe {
            let offset = config.mono_font_va as usize + config.mono_font_len as usize;
            core::slice::from_raw_parts(offset as *const u8, config.prop_font_len as usize)
        };

        if let Some(prop_ttf) = drawing::TrueTypeFont::new(prop_font_data) {
            let mut prop_cache: Box<drawing::GlyphCache> = unsafe {
                let layout = alloc::alloc::Layout::new::<drawing::GlyphCache>();
                let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::GlyphCache;

                if ptr.is_null() {
                    sys::print(b"compositor: prop glyph cache alloc failed\n");
                    sys::exit();
                }

                Box::from_raw(ptr)
            };

            prop_cache.populate(&prop_ttf, FONT_SIZE, &mut scratch);

            unsafe { PROP_GLYPH_CACHE = Box::into_raw(prop_cache) };

            sys::print(b"     proportional font rasterized (Nunito Sans 16px)\n");
        } else {
            sys::print(b"     warning: failed to parse proportional font, using monospace for chrome\n");
            // Fallback: use monospace cache for chrome text too.
            unsafe { PROP_GLYPH_CACHE = GLYPH_CACHE };
        }
    } else {
        sys::print(b"     no proportional font, using monospace for chrome\n");
        // Fallback: use monospace cache for chrome text.
        unsafe { PROP_GLYPH_CACHE = GLYPH_CACHE };
    }

    drop(scratch);

    // -----------------------------------------------------------------------
    // Check for image configuration (raw PNG data) from init.
    // If present, decode the PNG into a heap-allocated pixel buffer.
    // -----------------------------------------------------------------------
    let mut image_pixels: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    let mut image_w: u32 = 0;
    let mut image_h: u32 = 0;

    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMAGE_CONFIG {
        let img_config: ImageConfig = unsafe { msg.payload_as() };

        if img_config.image_va != 0 && img_config.image_len > 0 {
            sys::print(b"     decoding PNG image (");

            let png_data = unsafe {
                core::slice::from_raw_parts(
                    img_config.image_va as *const u8,
                    img_config.image_len as usize,
                )
            };

            // Parse header first to get dimensions.
            match drawing::png_header(png_data) {
                Ok(hdr) => {
                    sys::print(b"");
                    // Print dimensions.
                    let mut dim_buf = [0u8; 20];
                    let mut di = append_u32(&mut dim_buf, 0, hdr.width);
                    dim_buf[di] = b'x';
                    di += 1;
                    di = append_u32(&mut dim_buf, di, hdr.height);
                    sys::print(&dim_buf[..di]);
                    sys::print(b")\n");

                    let channels: u32 = if hdr.color_type == 6 { 4 } else { 3 };
                    let scanline_bytes = 1 + (hdr.width as usize) * (channels as usize);
                    let total_raw = scanline_bytes * (hdr.height as usize);
                    let out_size = (hdr.width * hdr.height * 4) as usize;
                    let decode_buf_size = if total_raw > out_size { total_raw } else { out_size };

                    let mut decode_buf = vec![0u8; decode_buf_size];

                    match drawing::png_decode(png_data, &mut decode_buf) {
                        Ok(_) => {
                            // Copy decoded BGRA pixels to final buffer.
                            image_pixels = vec![0u8; out_size];
                            image_pixels[..out_size].copy_from_slice(&decode_buf[..out_size]);
                            image_w = hdr.width;
                            image_h = hdr.height;

                            unsafe {
                                // Boot into editor mode; user switches
                                // to image viewer via F1.
                                IMAGE_MODE = false;
                                IMAGE_WIDTH = image_w;
                                IMAGE_HEIGHT = image_h;
                            }

                            sys::print(b"     PNG decoded successfully (F1 to view)\n");
                        }
                        Err(_) => {
                            sys::print(b"     PNG decode failed\n");
                        }
                    }
                }
                Err(_) => {
                    sys::print(b"invalid header)\n");
                }
            }
        }
    }

    // Channel 1: input events from input driver (endpoint 1 = recv).
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel 2: GPU present commands (endpoint 0 = send).
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    // Channel 3: editor (endpoint 0 = send input, recv write requests).
    let editor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(3), ipc::PAGE_SIZE, 0) };

    // -----------------------------------------------------------------------
    // Double buffering: two separate framebuffer allocations.
    // Buffer 0 at fb_va, buffer 1 at fb_va2.
    // The compositor composites surfaces into the back buffer, then presents.
    // -----------------------------------------------------------------------

    static mut FB_PTRS: [*mut u8; 2] = [core::ptr::null_mut(); 2];

    unsafe {
        FB_PTRS[0] = fb_va as *mut u8;
        FB_PTRS[1] = fb_va2 as *mut u8;
    }

    let make_fb_surface = |idx: usize| -> drawing::Surface<'static> {
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

    // -----------------------------------------------------------------------
    // Allocate surface pixel buffers.
    //
    // Each surface is an independently-renderable pixel buffer. On each
    // frame, all surfaces are composited back-to-front into the framebuffer.
    // -----------------------------------------------------------------------

    // Content area dimensions (inside the chrome margins).
    let content_w = fb_width - 2 * CONTENT_MARGIN_X;
    let content_h = fb_height.saturating_sub(CONTENT_MARGIN_TOP + CONTENT_MARGIN_BOTTOM);
    let content_x = CONTENT_MARGIN_X as i32;
    let content_y = CONTENT_MARGIN_TOP as i32;

    sys::print(b"     allocating surface buffers\n");

    // Background surface (z=0): full-screen solid color.
    let mut bg_buf = alloc_surface_buf(fb_width, fb_height);
    // Content surface (z=10): text editing area.
    let mut content_buf = alloc_surface_buf(content_w, content_h);
    // Title bar drop shadow (z=15): gradient beneath title bar.
    let mut title_shadow_buf = alloc_surface_buf(fb_width, SHADOW_DEPTH);
    // Status bar drop shadow (z=15): gradient above status bar.
    let mut status_shadow_buf = alloc_surface_buf(fb_width, SHADOW_DEPTH);
    // Title bar chrome (z=20): translucent overlay at top.
    let mut title_buf = alloc_surface_buf(fb_width, TITLE_BAR_H);
    // Status bar chrome (z=20): translucent overlay at bottom.
    let mut status_buf = alloc_surface_buf(fb_width, STATUS_BAR_H);

    sys::print(b"     surface buffers allocated\n");

    // -----------------------------------------------------------------------
    // Render initial surface contents.
    // -----------------------------------------------------------------------

    // Background: solid dark color.
    {
        let mut bg_surf = make_surf(&mut bg_buf, fb_width, fb_height);
        render_background(&mut bg_surf);
    }

    // Content: image viewer or text area background + cursor.
    {
        let mut content_surf = make_surf(&mut content_buf, content_w, content_h);
        if unsafe { IMAGE_MODE } && !image_pixels.is_empty() {
            render_image_content_surface(&mut content_surf, &image_pixels, image_w, image_h);
        } else {
            render_content_surface(&mut content_surf, doc_content());
        }
    }

    // Drop shadows (rendered once — static gradient, never re-rendered).
    {
        let mut title_shadow_surf = make_surf(&mut title_shadow_buf, fb_width, SHADOW_DEPTH);
        render_title_shadow(&mut title_shadow_surf);
    }
    {
        let mut status_shadow_surf = make_surf(&mut status_shadow_buf, fb_width, SHADOW_DEPTH);
        render_status_shadow(&mut status_shadow_surf);
    }

    // Title bar chrome.
    {
        let mut title_surf = make_surf(&mut title_buf, fb_width, TITLE_BAR_H);
        render_title_bar(&mut title_surf);
    }

    // Status bar chrome.
    {
        let mut status_surf = make_surf(&mut status_buf, fb_width, STATUS_BAR_H);
        render_status_bar(&mut status_surf, 0);
    }

    sys::print(b"     surfaces rendered, compositing initial frame\n");

    // -----------------------------------------------------------------------
    // Composite initial frame into buffer 0 and present.
    // -----------------------------------------------------------------------
    let status_y = (fb_height - STATUS_BAR_H) as i32;
    let title_shadow_y = TITLE_BAR_H as i32;
    let status_shadow_y = (fb_height - STATUS_BAR_H - SHADOW_DEPTH) as i32;

    {
        let mut fb0 = make_fb_surface(0);

        // Build composite surface references.
        let bg_cs = drawing::CompositeSurface {
            surface: make_surf(&mut bg_buf, fb_width, fb_height),
            x: 0,
            y: 0,
            z: Z_BACKGROUND,
            visible: true,
        };
        let content_cs = drawing::CompositeSurface {
            surface: make_surf(&mut content_buf, content_w, content_h),
            x: content_x,
            y: content_y,
            z: Z_CONTENT,
            visible: true,
        };
        let title_shadow_cs = drawing::CompositeSurface {
            surface: make_surf(&mut title_shadow_buf, fb_width, SHADOW_DEPTH),
            x: 0,
            y: title_shadow_y,
            z: Z_SHADOW,
            visible: true,
        };
        let status_shadow_cs = drawing::CompositeSurface {
            surface: make_surf(&mut status_shadow_buf, fb_width, SHADOW_DEPTH),
            x: 0,
            y: status_shadow_y,
            z: Z_SHADOW,
            visible: true,
        };
        let title_cs = drawing::CompositeSurface {
            surface: make_surf(&mut title_buf, fb_width, TITLE_BAR_H),
            x: 0,
            y: 0,
            z: Z_CHROME,
            visible: true,
        };
        let status_cs = drawing::CompositeSurface {
            surface: make_surf(&mut status_buf, fb_width, STATUS_BAR_H),
            x: 0,
            y: status_y,
            z: Z_CHROME,
            visible: true,
        };

        let surfaces: [&drawing::CompositeSurface; 6] = [
            &bg_cs, &content_cs, &title_shadow_cs, &status_shadow_cs, &title_cs, &status_cs,
        ];
        drawing::composite_surfaces(&mut fb0, &surfaces);
    }

    // Initial present: full screen (rect_count = 0 signals full transfer).
    let initial_payload = PresentPayload {
        buffer_index: 0,
        rect_count: 0,
        rects: [drawing::DirtyRect::new(0, 0, 0, 0); 6],
        _pad: [0; 4],
    };
    let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &initial_payload) };

    gpu_ch.send(&present_msg);

    let _ = sys::channel_signal(GPU_HANDLE);

    // Buffer 0 is now the front (being displayed). Next render goes to buffer 1.
    unsafe { BACK_BUF_IDX = 1 };

    // Create the initial 1-second periodic timer for the clock display.
    create_clock_timer();

    sys::print(b"     multi-surface compositor ready, entering event loop\n");

    // -----------------------------------------------------------------------
    // Event loop: wait for input, editor write requests, or timer.
    //
    // On each content change or timer tick:
    //   1. Re-render the content surface (only on content change)
    //   2. Re-render the status bar surface (char count + clock)
    //   3. Composite all surfaces into the back framebuffer
    //   4. Present the back buffer to the GPU
    //   5. Swap back/front buffers
    // -----------------------------------------------------------------------
    loop {
        // Build the wait handle set: input + editor + optional timer.
        let timer_active = unsafe { TIMER_ACTIVE };
        let timer_handle = unsafe { TIMER_HANDLE };

        let wait_result = if timer_active {
            sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle], u64::MAX)
        } else {
            sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], u64::MAX)
        };

        let _ = wait_result;
        let mut changed = false;
        let mut timer_fired = false;

        // Check if the timer fired: the timer handle becomes permanently
        // ready once its deadline passes (level-triggered). We detect this
        // by polling it — if it's active, close it and recreate.
        if timer_active {
            // Poll the timer handle to see if it fired (non-blocking).
            if let Ok(_) = sys::wait(&[timer_handle], 0) {
                timer_fired = true;

                // Close the expired one-shot timer handle.
                let _ = sys::handle_close(timer_handle);

                // Create a new 1-second timer for the next tick.
                create_clock_timer();
            }
        }

        // Forward input events to the editor (except context switch key).
        while input_ch.try_recv(&mut msg) {
            if msg.msg_type == MSG_KEY_EVENT {
                let key: KeyEvent = unsafe { msg.payload_as() };

                // F1 toggles between editor and image viewer contexts.
                if key.keycode == KEY_F1 && key.pressed == 1 {
                    let has_image = !image_pixels.is_empty();

                    if has_image {
                        let was_image = unsafe { IMAGE_MODE };

                        unsafe { IMAGE_MODE = !was_image };

                        // Re-render the content surface for the new mode.
                        {
                            let mut content_surf =
                                make_surf(&mut content_buf, content_w, content_h);

                            if unsafe { IMAGE_MODE } {
                                render_image_content_surface(
                                    &mut content_surf,
                                    &image_pixels,
                                    image_w,
                                    image_h,
                                );
                            } else {
                                // Switching back to editor: full clear + re-render
                                // to ensure no image artifacts remain.
                                unsafe { PREV_LAST_Y = content_h };
                                render_content_surface(&mut content_surf, doc_content());
                            }
                        }

                        changed = true;
                    }

                    continue; // Don't forward F1 to editor.
                }

                // In image mode, don't forward editing keys to the editor —
                // keyboard input only applies in editor mode.
                if !unsafe { IMAGE_MODE } {
                    editor_ch.send(&msg);

                    let _ = sys::channel_signal(EDITOR_HANDLE);
                }
            }
        }

        // Apply write requests from the editor (sole writer).
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

        if changed || timer_fired {
            let back = unsafe { BACK_BUF_IDX };
            let in_image_mode = unsafe { IMAGE_MODE };

            // 1. Re-render the content surface (only on actual content changes,
            //    not on timer ticks — avoids unnecessary re-rendering).
            if changed && !in_image_mode {
                let mut content_surf = make_surf(&mut content_buf, content_w, content_h);
                render_content_surface(&mut content_surf, doc_content());
            }
            // In image mode, content surface stays unchanged (showing the image).

            // 2. Re-render the status bar.
            {
                let mut status_surf = make_surf(&mut status_buf, fb_width, STATUS_BAR_H);
                render_status_bar(&mut status_surf, unsafe { DOC_LEN });
            }

            // 3. Composite all surfaces into the back framebuffer.
            {
                let mut fb = make_fb_surface(back);

                let bg_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut bg_buf, fb_width, fb_height),
                    x: 0,
                    y: 0,
                    z: Z_BACKGROUND,
                    visible: true,
                };
                let content_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut content_buf, content_w, content_h),
                    x: content_x,
                    y: content_y,
                    z: Z_CONTENT,
                    visible: true,
                };
                let title_shadow_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut title_shadow_buf, fb_width, SHADOW_DEPTH),
                    x: 0,
                    y: title_shadow_y,
                    z: Z_SHADOW,
                    visible: true,
                };
                let status_shadow_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut status_shadow_buf, fb_width, SHADOW_DEPTH),
                    x: 0,
                    y: status_shadow_y,
                    z: Z_SHADOW,
                    visible: true,
                };
                let title_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut title_buf, fb_width, TITLE_BAR_H),
                    x: 0,
                    y: 0,
                    z: Z_CHROME,
                    visible: true,
                };
                let status_cs = drawing::CompositeSurface {
                    surface: make_surf(&mut status_buf, fb_width, STATUS_BAR_H),
                    x: 0,
                    y: status_y,
                    z: Z_CHROME,
                    visible: true,
                };

                let surfaces: [&drawing::CompositeSurface; 6] = [
                    &bg_cs, &content_cs, &title_shadow_cs, &status_shadow_cs, &title_cs, &status_cs,
                ];
                drawing::composite_surfaces(&mut fb, &surfaces);
            }

            // 4. Present: full-screen transfer for now (multi-surface
            //    compositing touches most of the framebuffer anyway).
            let payload = PresentPayload {
                buffer_index: back as u32,
                rect_count: 0, // full screen
                rects: [drawing::DirtyRect::new(0, 0, 0, 0); 6],
                _pad: [0; 4],
            };
            let present_msg =
                unsafe { ipc::Message::from_payload(MSG_PRESENT, &payload) };

            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(GPU_HANDLE);

            // 5. Swap back/front buffers.
            unsafe { BACK_BUF_IDX = 1 - back };
        }
    }
}
