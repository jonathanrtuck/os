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
//!   z=20: Title bar     — translucent chrome overlay at top
//!   z=30: Mouse cursor  — procedural arrow, tracks pointer position
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
use protocol::compose::{
    CompositorConfig, IconConfig, ImageConfig, RtcConfig,
    MSG_COMPOSITOR_CONFIG, MSG_ICON_CONFIG, MSG_IMAGE_CONFIG, MSG_IMG_ICON_CONFIG, MSG_RTC_CONFIG,
};
use protocol::edit::{
    CursorMove, SelectionUpdate, WriteDelete, WriteDeleteRange, WriteInsert,
    MSG_CURSOR_MOVE, MSG_SELECTION_UPDATE, MSG_SET_CURSOR, MSG_WRITE_DELETE,
    MSG_WRITE_DELETE_RANGE, MSG_WRITE_INSERT,
};
use protocol::input::{KeyEvent, PointerAbs, PointerButton, MSG_KEY_EVENT, MSG_POINTER_ABS, MSG_POINTER_BUTTON};
use protocol::present::{PresentPayload, MSG_PRESENT};

const FONT_SIZE: u32 = 18;
// Linux evdev keycodes for Ctrl+Tab — used as the context switch combo.
// Tab (keycode 15) alone produces '\t' and is forwarded to the editor.
// Only Tab while Left Ctrl is held triggers context switching.
const KEY_TAB: u16 = 15;
const KEY_LEFTCTRL: u16 = 29;
// Handle indices (determined by the order init sends handles).
const INPUT_HANDLE: u8 = 1;
const GPU_HANDLE: u8 = 2;
const EDITOR_HANDLE: u8 = 3;
// Additional input devices (e.g. virtio-tablet) get handles starting at 4.
// Handle 4 is the second input device if present.
const INPUT2_HANDLE: u8 = 4;
// Surface z-order constants.
const Z_BACKGROUND: u16 = 0;
const Z_CONTENT: u16 = 10;
const Z_SHADOW: u16 = 15;
const Z_CHROME: u16 = 20;
const Z_CURSOR: u16 = 30;
// Drop shadow configuration — 12px depth for visible darkening.
const SHADOW_DEPTH: u32 = 12;
// Shadow alpha max is defined in drawing::SHADOW_PEAK.
// Chrome dimensions.
const TITLE_BAR_H: u32 = 36;
// Content area insets (relative to framebuffer).
// The content surface now extends full-screen so that document content is
// visible through translucent chrome (title bar).
const CONTENT_MARGIN_X: u32 = 0;
const CONTENT_MARGIN_TOP: u32 = 0;
const CONTENT_MARGIN_BOTTOM: u32 = 0;
// Text insets within the content surface. The top inset places text below
// the title bar with adequate margin; the bottom inset keeps text above the
// bottom edge with a small margin.
const TEXT_INSET_X: u32 = 12;
const TEXT_INSET_TOP: u32 = TITLE_BAR_H + SHADOW_DEPTH + 8;
const TEXT_INSET_BOTTOM: u32 = 8;
// Document header layout (first 64 bytes of shared buffer).
const DOC_HEADER_SIZE: usize = 64;

/// Mapped VA of the PL031 RTC MMIO page. 0 = not mapped / not available.
/// The Data Register at offset 0x000 contains Unix epoch seconds (read-only u32).
static mut RTC_MMIO_VA: usize = 0;
/// Whether the compositor is in image viewer mode (true) or text editor mode (false).
static mut IMAGE_MODE: bool = false;
/// Counter value captured at boot for deriving elapsed wall-clock time.
static mut BOOT_COUNTER: u64 = 0;
/// Counter frequency in Hz (read once at boot).
static mut COUNTER_FREQ: u64 = 0;
/// Current timer handle for the 1-second periodic clock. 0 = no timer.
static mut TIMER_HANDLE: u8 = 0;
/// Whether a valid timer handle exists.
static mut TIMER_ACTIVE: bool = false;
/// Selection start byte offset (0 = no selection when equal to sel_end).
static mut SEL_START: usize = 0;
/// Selection end byte offset (0 = no selection when equal to sel_start).
static mut SEL_END: usize = 0;
/// Vertical scroll offset in visual lines. Lines above this offset are
/// not rendered. Updated automatically when the cursor moves outside the
/// visible viewport.
static mut SCROLL_OFFSET: u32 = 0;
/// Saved scroll offset for the text editor when switching to image viewer.
/// Restored when switching back.
static mut SAVED_EDITOR_SCROLL: u32 = 0;
/// Whether the content surface has been fully rendered at least once.
/// First frame always requires a full clear+render.
static mut CONTENT_FIRST_RENDER: bool = true;
/// Previous frame's cursor position (for computing which lines changed).
static mut PREV_CURSOR_POS: usize = 0;
/// Previous frame's document length.
static mut PREV_DOC_LEN: usize = 0;
/// Previous frame's total visual line count (for accurate dirty rect
/// computation when newlines are deleted — we can't recompute old line
/// count from new text content).
static mut PREV_TOTAL_LINES: u32 = 1;
/// Previous frame's selection start.
static mut PREV_SEL_START: usize = 0;
/// Previous frame's selection end.
static mut PREV_SEL_END: usize = 0;
/// Current mouse cursor X position in framebuffer pixels.
static mut MOUSE_X: u32 = 0;
/// Current mouse cursor Y position in framebuffer pixels.
static mut MOUSE_Y: u32 = 0;
/// Previous frame's cursor X (for dirty-rect generation).
static mut PREV_MOUSE_X: u32 = 0;
/// Previous frame's cursor Y (for dirty-rect generation).
static mut PREV_MOUSE_Y: u32 = 0;
/// Whether a pointer event has been received (cursor becomes visible).
static mut CURSOR_VISIBLE: bool = false;
/// Whether cursor position changed this frame (needs dirty rects).
static mut CURSOR_MOVED: bool = false;
static mut CHAR_W: u32 = 8;
static mut LINE_H: u32 = 20;
/// Content surface dimensions (set once during initialization).
static mut CONTENT_W: u32 = 0;
static mut CONTENT_H: u32 = 0;
/// Pre-rasterized glyph cache for monospace font (heap-allocated, initialized at startup).
static mut GLYPH_CACHE: *const drawing::GlyphCache = core::ptr::null();
/// Pre-rasterized glyph cache for proportional font (chrome text).
static mut PROP_GLYPH_CACHE: *const drawing::GlyphCache = core::ptr::null();
/// Raw font data pointer and length for the proportional font (used for kerning lookups).
static mut PROP_FONT_DATA: *const u8 = core::ptr::null();
static mut PROP_FONT_LEN: usize = 0;
/// Cached parsed proportional TrueTypeFont (heap-allocated at startup, avoids
/// re-parsing on every `render_title_bar()` call). Null if no proportional font.
static mut PROP_TTF: *const drawing::TrueTypeFont<'static> = core::ptr::null();
/// Pre-rasterized SVG document icon coverage map (heap-allocated, initialized at startup).
/// Null if no icon was loaded. Width and height stored alongside.
static mut ICON_COVERAGE: *const u8 = core::ptr::null();
static mut ICON_W: u32 = 0;
static mut ICON_H: u32 = 0;
/// Pre-rasterized SVG image icon coverage map (for image viewer mode).
/// Null if no icon was loaded.
static mut IMG_ICON_COVERAGE: *const u8 = core::ptr::null();
static mut IMG_ICON_W: u32 = 0;
static mut IMG_ICON_H: u32 = 0;
/// Cursor byte offset in the document. Updated by write requests.
static mut CURSOR_POS: usize = 0;
/// Current back buffer index (0 or 1). Swapped after each present.
static mut BACK_BUF_IDX: usize = 0;
// Document shared buffer — owned exclusively by the compositor (sole writer).
// Set from config message; editor has a read-only mapping of the same pages.
static mut DOC_BUF: *mut u8 = core::ptr::null_mut();
static mut DOC_CAPACITY: usize = 0;
static mut DOC_LEN: usize = 0;

/// Compositor configuration received from init via IPC.
///
/// Layout: all u64 fields first, then all u32 fields, so that
/// `size_of::<CompositorConfig>() == 56` (no trailing alignment padding)
/// and the struct fits within the 60-byte IPC payload.
///
/// `fb_size` is intentionally omitted — the compositor computes it as
fn channel_shm_va(idx: usize) -> usize {
    protocol::channel_shm_va(idx)
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
/// Delete a range of bytes [start..end), shifting subsequent bytes left.
/// Returns true if deletion was performed.
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
/// Draw a byte string using the glyph cache (simple helper, no wrapping).
fn draw_string(
    fb: &mut drawing::Surface,
    x: u32,
    y: u32,
    text: &[u8],
    cache: &drawing::GlyphCache,
    color: drawing::Color,
) {
    let baseline_y = y as i32 + cache.ascent as i32;
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
/// Maximum Y coordinate for text within the content surface (local coords).
/// Text must stay above the bottom edge margin.
fn max_text_y_in_content(content_h: u32) -> u32 {
    content_h.saturating_sub(unsafe { LINE_H } + TEXT_INSET_BOTTOM)
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
/// Update SCROLL_OFFSET so that the cursor remains visible in the viewport.
/// Uses the stored CONTENT_W and CONTENT_H dimensions.
fn update_scroll_offset() {
    let content_w = unsafe { CONTENT_W };
    let content_h = unsafe { CONTENT_H };
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
/// Number of visible text lines in the content viewport.
fn viewport_lines(content_h: u32) -> u32 {
    let line_h = unsafe { LINE_H };

    if line_h == 0 {
        return 0;
    }

    // Usable vertical space: content height minus top and bottom insets.
    let usable = content_h.saturating_sub(TEXT_INSET_TOP + TEXT_INSET_BOTTOM);

    usable / line_h
}

// ---------------------------------------------------------------------------
// Surface rendering functions
// ---------------------------------------------------------------------------

// Deterministic PRNG seed for the background gradient noise.
const BG_GRADIENT_SEED: u32 = 0xDEAD_BEEF;
// Noise amplitude for background gradient (±3 RGB units per pixel).
const BG_NOISE_AMP: u32 = 3;

/// Result of processing a single key event. Used by `process_key_event()`
/// to communicate state changes back to the event loop caller.
struct KeyAction {
    /// Whether the display changed and needs a present.
    changed: bool,
    /// Whether text/content was modified (forces content dirty rects).
    text_changed: bool,
    /// Whether a Ctrl+Tab context switch occurred.
    context_switched: bool,
    /// Whether the event was fully consumed (should not be forwarded to editor).
    consumed: bool,
}

/// Allocate a pixel buffer for a surface with given dimensions.
fn alloc_surface_buf(width: u32, height: u32) -> alloc::vec::Vec<u8> {
    let stride = width * 4; // BGRA8888
    let size = (stride * height) as usize;

    vec![0u8; size]
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
/// Get the current time in seconds for the clock display.
///
/// If the PL031 RTC is mapped, reads Unix epoch seconds from the Data
/// Register (offset 0x000). Otherwise, falls back to elapsed seconds
/// since boot using the ARM generic counter.
fn clock_seconds() -> u64 {
    let rtc_va = unsafe { RTC_MMIO_VA };

    if rtc_va != 0 {
        // PL031 Data Register at offset 0x000: read-only 32-bit Unix epoch seconds.
        let epoch = unsafe { core::ptr::read_volatile(rtc_va as *const u32) };

        epoch as u64
    } else {
        // Fallback: elapsed seconds since boot.
        let now = sys::counter();
        let boot = unsafe { BOOT_COUNTER };
        let freq = unsafe { COUNTER_FREQ };

        if freq == 0 {
            return 0;
        }

        (now - boot) / freq
    }
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
/// Handle a single key event from any input channel. Encapsulates the Ctrl
/// modifier tracking, Ctrl+Tab context switch, and editor forwarding logic
/// that is shared across all input channels.
///
/// Returns a `KeyAction` describing what happened. If `consumed` is true,
/// the caller should not forward the event further.
fn process_key_event(
    key: &KeyEvent,
    ctrl_pressed: &mut bool,
    has_image: bool,
    content_buf: &mut [u8],
    title_buf: &mut [u8],
    content_w: u32,
    content_h: u32,
    fb_width: u32,
    image_pixels: &[u8],
    image_w: u32,
    image_h: u32,
    editor_ch: &ipc::Channel,
    msg: &ipc::Message,
) -> KeyAction {
    // Track Left Ctrl modifier state.
    if key.keycode == KEY_LEFTCTRL {
        *ctrl_pressed = key.pressed == 1;

        return KeyAction {
            changed: false,
            text_changed: false,
            context_switched: false,
            consumed: true,
        };
    }

    // Ctrl+Tab toggles between editor and image viewer contexts.
    if key.keycode == KEY_TAB && key.pressed == 1 && *ctrl_pressed {
        if has_image {
            let was_image = unsafe { IMAGE_MODE };

            if !was_image {
                // Switching TO image: save editor scroll offset.
                unsafe { SAVED_EDITOR_SCROLL = SCROLL_OFFSET };
            }

            unsafe { IMAGE_MODE = !was_image };

            if was_image {
                // Switching BACK to editor: restore scroll offset.
                unsafe { SCROLL_OFFSET = SAVED_EDITOR_SCROLL };
            }

            // Re-render the content surface for the new mode.
            {
                let mut content_surf = make_surf(content_buf, content_w, content_h);

                if unsafe { IMAGE_MODE } {
                    render_image_content_surface(&mut content_surf, image_pixels, image_w, image_h);
                } else {
                    // Switching back to editor: full clear + re-render
                    // to ensure no image artifacts remain.
                    render_content_surface(&mut content_surf, doc_content(), true);
                }
            }

            // Re-render the title bar to switch the icon.
            {
                let mut title_surf = make_surf(title_buf, fb_width, TITLE_BAR_H);

                render_title_bar(&mut title_surf);
            }

            return KeyAction {
                changed: true,
                text_changed: true,
                context_switched: true,
                consumed: true,
            };
        }

        return KeyAction {
            changed: false,
            text_changed: false,
            context_switched: false,
            consumed: true,
        };
    }

    // Forward non-modifier keys to editor in text mode.
    if !unsafe { IMAGE_MODE } {
        editor_ch.send(msg);

        let _ = sys::channel_signal(EDITOR_HANDLE);
    }

    KeyAction {
        changed: false,
        text_changed: false,
        context_switched: false,
        consumed: false,
    }
}
/// Parse SVG path data and rasterize into a leaked coverage map.
///
/// Heap-allocates both `SvgPath` and `SvgRasterScratch` to avoid blowing the
/// 16 KiB userspace stack (~16 KiB + ~64 KiB respectively). On success,
/// returns the coverage pointer, width, and height as a leaked allocation
/// (caller stores the pointer in a static). On failure (OOM or parse/raster
/// error), returns `None` and all temporary allocations are freed.
fn rasterize_svg_icon(
    svg_data: &[u8],
    label: &[u8],
    icon_w: u32,
    icon_h: u32,
) -> Option<(*const u8, u32, u32)> {
    sys::print(label);

    let path_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::SvgPath>();
        let ptr = alloc::alloc::alloc_zeroed(layout) as *mut drawing::SvgPath;
        ptr
    };

    if path_ptr.is_null() {
        sys::print(b"compositor: SVG path alloc failed (OOM)\n");

        return None;
    }

    let scratch_ptr = unsafe {
        let layout = alloc::alloc::Layout::new::<drawing::SvgRasterScratch>();
        alloc::alloc::alloc_zeroed(layout) as *mut drawing::SvgRasterScratch
    };

    if scratch_ptr.is_null() {
        sys::print(b"compositor: SVG scratch alloc failed (OOM)\n");

        unsafe {
            let layout = alloc::alloc::Layout::new::<drawing::SvgPath>();
            alloc::alloc::dealloc(path_ptr as *mut u8, layout);
        }

        return None;
    }

    let result = match drawing::svg_parse_path_into(svg_data, unsafe { &mut *path_ptr }) {
        Ok(()) => {
            let icon_size = (icon_w * icon_h) as usize;
            let mut icon_cov = vec![0u8; icon_size];

            match drawing::svg_rasterize(
                unsafe { &*path_ptr },
                unsafe { &mut *scratch_ptr },
                &mut icon_cov,
                icon_w,
                icon_h,
                drawing::SVG_FP_ONE,
                0,
                0,
            ) {
                Ok(()) => {
                    // Convert single-channel SVG coverage to 3-channel (RGB)
                    // for draw_coverage() which expects subpixel format.
                    let mut rgb_cov = vec![0u8; icon_size * 3];

                    for i in 0..icon_size {
                        let c = icon_cov[i];
                        rgb_cov[i * 3] = c;
                        rgb_cov[i * 3 + 1] = c;
                        rgb_cov[i * 3 + 2] = c;
                    }

                    let leaked = rgb_cov.leak();

                    Some((leaked.as_ptr(), icon_w, icon_h))
                }
                Err(_) => {
                    sys::print(b"     SVG icon rasterize failed\n");
                    None
                }
            }
        }
        Err(_) => {
            sys::print(b"     SVG icon parse failed\n");
            None
        }
    };

    // Free temporary heap allocations.
    unsafe {
        let path_layout = alloc::alloc::Layout::new::<drawing::SvgPath>();

        alloc::alloc::dealloc(path_ptr as *mut u8, path_layout);

        let scratch_layout = alloc::alloc::Layout::new::<drawing::SvgRasterScratch>();

        alloc::alloc::dealloc(scratch_ptr as *mut u8, scratch_layout);
    }

    result
}
/// Render the background surface: radial gradient (lighter center, darker
/// edges) with subtle per-pixel noise to break up banding.
///
/// The gradient runs from BG_CENTER (center of screen) to BG_BASE (corners).
/// A deterministic xorshift32 PRNG adds ±3 RGB units of noise per pixel so
/// the pattern is reproducible across boots.
fn render_background(surf: &mut drawing::Surface) {
    drawing::fill_radial_gradient_noise(
        surf,
        drawing::BG_CENTER,
        drawing::BG_BASE,
        BG_NOISE_AMP,
        BG_GRADIENT_SEED,
    );
}
/// Render the content surface: text area background, text content, and cursor.
///
/// The content surface extends full-screen so that document content is
/// visible through the translucent chrome (title bar).
/// Text is rendered with margins that keep it below the title bar and
/// above the bottom edge, but the background fills the entire surface.
///
/// When `force_full` is true (first frame, context switch, scroll change,
/// selection change), the entire surface is cleared and re-rendered.
/// Otherwise, only the lines that changed (based on cursor movement and
/// content changes) are cleared and re-rendered — an incremental update.
fn render_content_surface(surf: &mut drawing::Surface, text: &[u8], force_full: bool) {
    let cache = unsafe { &*GLYPH_CACHE };
    let cursor_pos = unsafe { CURSOR_POS };
    let content_w = surf.width;
    let content_h = surf.height;
    let scroll_offset = unsafe { SCROLL_OFFSET };
    let layout = content_text_layout(content_w);
    let my = max_text_y_in_content(content_h);
    let sel_start = unsafe { SEL_START };
    let sel_end = unsafe { SEL_END };
    let line_h = unsafe { LINE_H };

    if force_full || line_h == 0 {
        // Full clear with gradient background (matches the bg surface gradient
        // so that the gradient is visible through the translucent title bar chrome).
        drawing::fill_radial_gradient_noise(
            surf,
            drawing::BG_CENTER,
            drawing::BG_BASE,
            BG_NOISE_AMP,
            BG_GRADIENT_SEED,
        );

        let (_, _cursor_y) = layout.draw_tt_sel_scroll(
            surf,
            text,
            TEXT_INSET_X,
            TEXT_INSET_TOP,
            cursor_pos,
            cache,
            drawing::TEXT_PRIMARY,
            drawing::TEXT_CURSOR,
            my,
            sel_start,
            sel_end,
            drawing::TEXT_SELECTION,
            scroll_offset,
        );
    } else {
        // Incremental render: only clear+redraw changed lines.
        let prev_cursor = unsafe { PREV_CURSOR_POS };
        let prev_doc_len = unsafe { PREV_DOC_LEN };
        let doc_len = text.len();
        // Compute which visual lines are affected.
        // The cursor line always needs re-rendering (cursor bar moved).
        let new_cursor_line = layout.byte_to_visual_line(text, cursor_pos);
        let prev_cursor_line = layout.byte_to_visual_line(text, prev_cursor.min(doc_len));
        // Determine the range of lines to re-render.
        // For simple insertions/deletions at the cursor, the changed content
        // starts at the cursor line. All lines from the cursor to the end of
        // text may have shifted (e.g., inserting a newline pushes everything
        // down). We also need to cover the previous cursor line (to erase
        // the old cursor bar).
        let first_changed = prev_cursor_line.min(new_cursor_line);
        // Compute the last line we need to re-render.
        // For a single character insert/delete on the same line (no reflow),
        // we only need the cursor line. But if lines reflow (e.g., soft wrap
        // changes, newline insert/delete), we need everything from the
        // changed line to the end. We detect reflow by checking if the
        // total line count changed.
        let new_total_lines = if doc_len == 0 {
            1u32
        } else {
            layout.byte_to_visual_line(text, doc_len) + 1
        };
        // Use the stored previous total line count — computing it from
        // the new text with the old byte offset gives wrong results when
        // a newline was deleted (the old text had more lines than the new
        // text at that position).
        let prev_total_lines = unsafe { PREV_TOTAL_LINES };
        // If the cursor stayed on the same line and total line count didn't
        // change, only re-render the cursor line (+ previous if different).
        let last_changed =
            if new_total_lines != prev_total_lines || new_cursor_line != prev_cursor_line {
                // Lines reflowed or cursor moved between lines — re-render from
                // first_changed to the end of visible text.
                let max_total = if new_total_lines > prev_total_lines {
                    new_total_lines
                } else {
                    prev_total_lines
                };

                max_total.saturating_sub(1)
            } else {
                // Same line, no reflow — only the cursor line.
                new_cursor_line
            };
        // Convert to viewport-relative visual lines (after scroll).
        let vp_lines = viewport_lines(content_h);
        let first_vis = first_changed.saturating_sub(scroll_offset);
        let last_vis = last_changed
            .saturating_sub(scroll_offset)
            .min(if vp_lines > 0 { vp_lines - 1 } else { 0 });

        if first_vis <= last_vis {
            // Clear only the affected lines in the content surface.
            let clear_y = TEXT_INSET_TOP + first_vis * line_h;
            let clear_h = (last_vis - first_vis + 1) * line_h;
            // Clamp to surface bounds.
            let clamped_h = if clear_y + clear_h > content_h {
                content_h.saturating_sub(clear_y)
            } else {
                clear_h
            };

            if clamped_h > 0 {
                // Re-render the exact gradient+dither pixels for these rows.
                // Uses fill_radial_gradient_rows which produces output
                // identical to fill_radial_gradient_noise for the same
                // coordinates — no visible flat rectangles behind text.
                drawing::fill_radial_gradient_rows(
                    surf,
                    drawing::BG_CENTER,
                    drawing::BG_BASE,
                    clear_y,
                    clamped_h,
                );
            }

            // Re-render only the affected lines.
            let (_, _cursor_y) = layout.draw_tt_sel_scroll_lines(
                surf,
                text,
                TEXT_INSET_X,
                TEXT_INSET_TOP,
                cursor_pos,
                cache,
                drawing::TEXT_PRIMARY,
                drawing::TEXT_CURSOR,
                my,
                sel_start,
                sel_end,
                drawing::TEXT_SELECTION,
                scroll_offset,
                first_vis,
                last_vis,
            );
        }
    }

    // Update previous frame state for next incremental render.
    // Compute current total line count for the next frame's dirty tracking.
    let doc_len = text.len();
    let current_total_lines = if doc_len == 0 {
        1u32
    } else {
        layout.byte_to_visual_line(text, doc_len) + 1
    };

    unsafe {
        PREV_CURSOR_POS = cursor_pos;
        PREV_DOC_LEN = doc_len;
        PREV_TOTAL_LINES = current_total_lines;
        PREV_SEL_START = sel_start;
        PREV_SEL_END = sel_end;
    }
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
    // Clear with gradient background (matches the bg surface gradient so
    // gradient is visible through translucent title bar chrome).
    drawing::fill_radial_gradient_noise(
        surf,
        drawing::BG_CENTER,
        drawing::BG_BASE,
        BG_NOISE_AMP,
        BG_GRADIENT_SEED,
    );

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
/// Render the title bar chrome surface (translucent overlay).
/// Layout: [icon] Text/Image on the left, HH:MM:SS clock on the right.
/// Uses the proportional font (Nunito Sans) for all chrome text.
/// The icon and document name switch between text editor ("Text" + doc
/// icon) and image viewer ("Image" + img icon) based on IMAGE_MODE.
fn render_title_bar(surf: &mut drawing::Surface) {
    let prop_cache = unsafe { &*PROP_GLYPH_CACHE };
    let in_image_mode = unsafe { IMAGE_MODE };

    // Translucent background.
    surf.clear(drawing::CHROME_BG);

    // Select the correct icon based on the current context.
    let (icon_ptr, icon_w, icon_h) = if in_image_mode {
        let ptr = unsafe { IMG_ICON_COVERAGE };
        let w = unsafe { IMG_ICON_W };
        let h = unsafe { IMG_ICON_H };

        if !ptr.is_null() && w > 0 && h > 0 {
            (ptr, w, h)
        } else {
            // Fallback to doc icon if image icon not loaded.
            (unsafe { ICON_COVERAGE }, unsafe { ICON_W }, unsafe {
                ICON_H
            })
        }
    } else {
        (unsafe { ICON_COVERAGE }, unsafe { ICON_W }, unsafe {
            ICON_H
        })
    };

    let text_x: u32;
    // Vertically center text within the title bar (line_height centered).
    let text_y = (TITLE_BAR_H.saturating_sub(prop_cache.line_height)) / 2;

    if !icon_ptr.is_null() && icon_w > 0 && icon_h > 0 {
        let icon_coverage =
            unsafe { core::slice::from_raw_parts(icon_ptr, (icon_w * icon_h * 3) as usize) };
        // Position icon vertically centered in the title bar, left margin = 10.
        let icon_x: i32 = 10;
        let icon_y: i32 = ((TITLE_BAR_H as i32 - icon_h as i32) / 2).max(0);

        surf.draw_coverage(
            icon_x,
            icon_y,
            icon_coverage,
            icon_w,
            icon_h,
            drawing::CHROME_ICON,
        );

        // Title text starts after the icon with a small gap.
        text_x = icon_x as u32 + icon_w + 8;
    } else {
        text_x = 12;
    }

    // Use the cached parsed proportional TrueTypeFont for kerning (avoids
    // re-parsing font tables on every title bar render).
    let prop_font: Option<&drawing::TrueTypeFont<'static>> = unsafe {
        if !PROP_TTF.is_null() {
            Some(&*PROP_TTF)
        } else {
            None
        }
    };

    // Document name (proportional font with kerning) — "Text" or "Image"
    // depending on the active context (text editor vs image viewer).
    let doc_name: &[u8] = if in_image_mode { b"Image" } else { b"Text" };
    drawing::draw_proportional_string_kerned(
        surf,
        text_x,
        text_y,
        doc_name,
        prop_cache,
        drawing::CHROME_TITLE,
        prop_font,
    );

    // Clock on the right side — HH:MM:SS format.
    let mut time_buf = [0u8; 8];
    let total_seconds = clock_seconds();

    format_time_hms(total_seconds, &mut time_buf);

    let clock_w = proportional_string_width(&time_buf, prop_cache);
    let clock_x = surf.width.saturating_sub(12 + clock_w);

    drawing::draw_proportional_string_kerned(
        surf,
        clock_x,
        text_y,
        &time_buf,
        prop_cache,
        drawing::CHROME_CLOCK,
        prop_font,
    );

    // Bottom edge line.
    surf.draw_hline(0, surf.height - 1, surf.width, drawing::CHROME_BORDER);
}
/// Render the title bar drop shadow: gradient from opaque to transparent,
/// falling downward from the title bar's bottom edge.
fn render_title_shadow(surf: &mut drawing::Surface) {
    surf.clear(drawing::Color::TRANSPARENT);
    surf.fill_gradient_v(
        0,
        0,
        surf.width,
        surf.height,
        drawing::SHADOW_PEAK,
        drawing::SHADOW_ZERO,
    );
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
    let fb_size = fb_stride * fb_height;

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
        core::slice::from_raw_parts(
            config.mono_font_va as *const u8,
            config.mono_font_len as usize,
        )
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

    sys::print(b"     monospace font rasterized (Source Code Pro 18px)\n");

    // Load proportional font (Nunito Sans) for chrome text.
    // Proportional font is stored right after the monospace font in the same buffer.
    if config.prop_font_len > 0 {
        let prop_font_data = unsafe {
            let offset = config.mono_font_va as usize + config.mono_font_len as usize;
            core::slice::from_raw_parts(offset as *const u8, config.prop_font_len as usize)
        };

        // Store raw font data pointer for kerning lookups at render time.
        unsafe {
            PROP_FONT_DATA = prop_font_data.as_ptr();
            PROP_FONT_LEN = prop_font_data.len();
        }

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

            // Cache the parsed TrueTypeFont so render_title_bar() can use it
            // for kerning without re-parsing the font tables on every call.
            // Safety: prop_font_data is backed by PROP_FONT_DATA which lives
            // for the entire process lifetime. We transmute the lifetime to
            // 'static since the data will never be freed.
            let boxed_ttf: Box<drawing::TrueTypeFont<'static>> = unsafe {
                Box::new(core::mem::transmute::<
                    drawing::TrueTypeFont<'_>,
                    drawing::TrueTypeFont<'static>,
                >(prop_ttf))
            };

            unsafe { PROP_TTF = Box::into_raw(boxed_ttf) };

            sys::print(b"     proportional font rasterized (Nunito Sans 18px)\n");
        } else {
            sys::print(
                b"     warning: failed to parse proportional font, using monospace for chrome\n",
            );
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
    // Init→compositor message ordering invariant.
    //
    // Init enqueues configuration messages into the ring buffer in a fixed
    // order before the compositor process starts executing. The compositor
    // drains them via sequential `try_recv` calls and **must** read them in
    // the same order init writes them:
    //
    //   1. MSG_COMPOSITOR_CONFIG  — framebuffer + document + font pointers (required)
    //   2. MSG_IMAGE_CONFIG       — raw PNG data for image viewer (optional)
    //   3. MSG_ICON_CONFIG        — SVG document icon path data (optional)
    //   4. MSG_IMG_ICON_CONFIG    — SVG image icon path data (optional)
    //   5. MSG_RTC_CONFIG         — PL031 RTC MMIO address (optional)
    //
    // If init omits an optional message, the corresponding `try_recv` will
    // either see the next message type (and skip processing) or return false
    // (ring empty). Reordering the reads here would silently mis-parse
    // payloads because messages are identified by position, not type lookup.
    // -----------------------------------------------------------------------

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
            let png_data = unsafe {
                core::slice::from_raw_parts(
                    img_config.image_va as *const u8,
                    img_config.image_len as usize,
                )
            };

            // Parse header first to get dimensions.
            match drawing::png_header(png_data) {
                Ok(hdr) => {
                    // Print dimensions as a single line.
                    let mut dim_buf = [0u8; 40];
                    let prefix = b"     decoding PNG image (";

                    dim_buf[..prefix.len()].copy_from_slice(prefix);

                    let mut di = prefix.len();

                    di = append_u32(&mut dim_buf, di, hdr.width);
                    dim_buf[di] = b'x';
                    di += 1;
                    di = append_u32(&mut dim_buf, di, hdr.height);
                    dim_buf[di] = b')';
                    di += 1;
                    dim_buf[di] = b'\n';
                    di += 1;

                    sys::print(&dim_buf[..di]);

                    let channels: u32 = if hdr.color_type == 6 { 4 } else { 3 };
                    let scanline_bytes = 1 + (hdr.width as usize) * (channels as usize);
                    let total_raw = scanline_bytes * (hdr.height as usize);
                    let out_size = (hdr.width * hdr.height * 4) as usize;
                    let decode_buf_size = if total_raw > out_size {
                        total_raw
                    } else {
                        out_size
                    };
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
                                // to image viewer via Ctrl+Tab.
                                IMAGE_MODE = false;
                            }

                            sys::print(b"     PNG decoded successfully (Ctrl+Tab to view)\n");
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

    // -----------------------------------------------------------------------
    // Check for SVG icon configuration from init.
    // If present, parse the SVG path data and rasterize into a coverage map.
    // -----------------------------------------------------------------------
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_ICON_CONFIG {
        let icn_config: IconConfig = unsafe { msg.payload_as() };

        if icn_config.icon_va != 0 && icn_config.icon_len > 0 {
            let svg_data = unsafe {
                core::slice::from_raw_parts(
                    icn_config.icon_va as *const u8,
                    icn_config.icon_len as usize,
                )
            };

            if let Some((ptr, w, h)) =
                rasterize_svg_icon(svg_data, b"     parsing SVG doc icon\n", 20, 24)
            {
                sys::print(b"     SVG icon rasterized (20x24)\n");

                unsafe {
                    ICON_COVERAGE = ptr;
                    ICON_W = w;
                    ICON_H = h;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Check for image icon configuration from init (second SVG icon).
    // Used in image viewer mode; switches with doc icon on Ctrl+Tab.
    // -----------------------------------------------------------------------
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_IMG_ICON_CONFIG {
        let icn_config: IconConfig = unsafe { msg.payload_as() };

        if icn_config.icon_va != 0 && icn_config.icon_len > 0 {
            let svg_data = unsafe {
                core::slice::from_raw_parts(
                    icn_config.icon_va as *const u8,
                    icn_config.icon_len as usize,
                )
            };

            if let Some((ptr, w, h)) =
                rasterize_svg_icon(svg_data, b"     parsing image icon SVG\n", 20, 24)
            {
                sys::print(b"     image icon rasterized (20x24)\n");

                unsafe {
                    IMG_ICON_COVERAGE = ptr;
                    IMG_ICON_W = w;
                    IMG_ICON_H = h;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Check for RTC configuration from init (PL031 physical address).
    // If present, map the MMIO page and read the Data Register for wall-clock time.
    // -----------------------------------------------------------------------
    if init_ch.try_recv(&mut msg) && msg.msg_type == MSG_RTC_CONFIG {
        let rtc_config: RtcConfig = unsafe { msg.payload_as() };

        if rtc_config.mmio_pa != 0 {
            match sys::device_map(rtc_config.mmio_pa, 4096) {
                Ok(va) => {
                    unsafe { RTC_MMIO_VA = va };
                    sys::print(b"     pl031 rtc mapped\n");
                }
                Err(_) => {
                    sys::print(b"     pl031 rtc device_map failed\n");
                }
            }
        }
    }

    // Channel 1: input events from keyboard driver (endpoint 1 = recv).
    let input_ch = unsafe { ipc::Channel::from_base(channel_shm_va(1), ipc::PAGE_SIZE, 1) };
    // Channel 2: GPU present commands (endpoint 0 = send).
    let gpu_ch = unsafe { ipc::Channel::from_base(channel_shm_va(2), ipc::PAGE_SIZE, 0) };
    // Channel 3: editor (endpoint 0 = send input, recv write requests).
    let editor_ch = unsafe { ipc::Channel::from_base(channel_shm_va(3), ipc::PAGE_SIZE, 0) };
    // Channel 4: input events from tablet driver (endpoint 1 = recv).
    // This channel exists only when a second input device (virtio-tablet) is
    // present. Probe whether handle 4 was sent by init: poll with timeout 0.
    // WouldBlock means the handle exists (just no events yet); InvalidHandle
    // means no second input device.
    let has_input2 = match sys::wait(&[INPUT2_HANDLE], 0) {
        Ok(_) => true,
        Err(sys::SyscallError::WouldBlock) => true,
        _ => false,
    };
    let input2_ch = if has_input2 {
        sys::print(b"     tablet input channel detected\n");
        let ch = unsafe { ipc::Channel::from_base(channel_shm_va(4), ipc::PAGE_SIZE, 1) };
        Some(ch)
    } else {
        None
    };

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

    // Store content dimensions for scroll offset calculations.
    unsafe {
        CONTENT_W = content_w;
        CONTENT_H = content_h;
    }

    sys::print(b"     allocating surface buffers\n");

    // Background surface (z=0): full-screen solid color.
    let mut bg_buf = alloc_surface_buf(fb_width, fb_height);
    // Content surface (z=10): text editing area.
    let mut content_buf = alloc_surface_buf(content_w, content_h);
    // Title bar drop shadow (z=15): gradient beneath title bar.
    let mut title_shadow_buf = alloc_surface_buf(fb_width, SHADOW_DEPTH);
    // Title bar chrome (z=20): translucent overlay at top.
    let mut title_buf = alloc_surface_buf(fb_width, TITLE_BAR_H);
    // Mouse cursor (z=30): procedural arrow cursor, highest z-order.
    let mut cursor_buf = alloc_surface_buf(drawing::CURSOR_W, drawing::CURSOR_H);

    drawing::render_cursor(&mut cursor_buf);
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
            render_content_surface(&mut content_surf, doc_content(), true);
        }
    }
    // Drop shadow (rendered once — static gradient, never re-rendered).
    {
        let mut title_shadow_surf = make_surf(&mut title_shadow_buf, fb_width, SHADOW_DEPTH);

        render_title_shadow(&mut title_shadow_surf);
    }
    // Title bar chrome.
    {
        let mut title_surf = make_surf(&mut title_buf, fb_width, TITLE_BAR_H);

        render_title_bar(&mut title_surf);
    }

    sys::print(b"     surfaces rendered, compositing initial frame\n");

    // -----------------------------------------------------------------------
    // Composite initial frame into buffer 0 and present.
    // -----------------------------------------------------------------------
    let title_shadow_y = TITLE_BAR_H as i32;

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
        let title_cs = drawing::CompositeSurface {
            surface: make_surf(&mut title_buf, fb_width, TITLE_BAR_H),
            x: 0,
            y: 0,
            z: Z_CHROME,
            visible: true,
        };
        let cursor_cs = drawing::CompositeSurface {
            surface: make_surf(&mut cursor_buf, drawing::CURSOR_W, drawing::CURSOR_H),
            x: 0,
            y: 0,
            z: Z_CURSOR,
            visible: false, // Hidden until first pointer event.
        };
        let surfaces: [&drawing::CompositeSurface; 5] =
            [&bg_cs, &content_cs, &title_shadow_cs, &title_cs, &cursor_cs];

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
    //   1. Compute dirty rects (which framebuffer regions changed)
    //   2. Re-render the affected surfaces
    //   3. Composite only the dirty regions into the back framebuffer
    //   4. Present the back buffer with dirty rects for partial GPU transfer
    //   5. Swap back/front buffers
    // -----------------------------------------------------------------------
    let mut first_present_done = false;
    let mut ctrl_pressed = false;

    loop {
        // Build the wait handle set: input + editor + optional timer.
        let timer_active = unsafe { TIMER_ACTIVE };
        let timer_handle = unsafe { TIMER_HANDLE };
        let wait_result = match (timer_active, has_input2) {
            (true, true) => sys::wait(
                &[INPUT_HANDLE, EDITOR_HANDLE, timer_handle, INPUT2_HANDLE],
                u64::MAX,
            ),
            (true, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, timer_handle], u64::MAX),
            (false, true) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE, INPUT2_HANDLE], u64::MAX),
            (false, false) => sys::wait(&[INPUT_HANDLE, EDITOR_HANDLE], u64::MAX),
        };
        let _ = wait_result;
        let mut changed = false;
        let mut text_changed = false; // Text/editor content actually modified.
        let mut timer_fired = false;
        let mut context_switched = false;
        // Snapshot cursor/text state before processing events so we can
        // compute which visual lines changed afterward.
        let old_cursor = unsafe { CURSOR_POS };
        let old_doc_len = unsafe { DOC_LEN };
        let old_scroll = unsafe { SCROLL_OFFSET };
        let old_total_lines = unsafe { PREV_TOTAL_LINES };

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

        // Forward input events to the editor (except context switch combo).
        while input_ch.try_recv(&mut msg) {
            if msg.msg_type == MSG_KEY_EVENT {
                let key: KeyEvent = unsafe { msg.payload_as() };
                let action = process_key_event(
                    &key,
                    &mut ctrl_pressed,
                    !image_pixels.is_empty(),
                    &mut content_buf,
                    &mut title_buf,
                    content_w,
                    content_h,
                    fb_width,
                    &image_pixels,
                    image_w,
                    image_h,
                    &editor_ch,
                    &msg,
                );

                if action.changed {
                    changed = true;
                }
                if action.text_changed {
                    text_changed = true;
                }
                if action.context_switched {
                    context_switched = true;
                }
            }
        }

        // Drain second input channel (tablet/keyboard) if present.
        // NOTE: QEMU may enumerate virtio devices in reverse command-line order,
        // so the keyboard can end up on the second channel. We must handle
        // Ctrl+Tab context switching on BOTH input channels.
        if let Some(ref ch2) = input2_ch {
            while ch2.try_recv(&mut msg) {
                match msg.msg_type {
                    MSG_KEY_EVENT => {
                        let key: KeyEvent = unsafe { msg.payload_as() };
                        let action = process_key_event(
                            &key,
                            &mut ctrl_pressed,
                            !image_pixels.is_empty(),
                            &mut content_buf,
                            &mut title_buf,
                            content_w,
                            content_h,
                            fb_width,
                            &image_pixels,
                            image_w,
                            image_h,
                            &editor_ch,
                            &msg,
                        );

                        if action.changed {
                            changed = true;
                        }
                        if action.text_changed {
                            text_changed = true;
                        }
                        if action.context_switched {
                            context_switched = true;
                        }
                    }
                    MSG_POINTER_ABS => {
                        let ptr: PointerAbs = unsafe { msg.payload_as() };
                        let new_x = drawing::scale_pointer_coord(ptr.x, fb_width);
                        let new_y = drawing::scale_pointer_coord(ptr.y, fb_height);

                        unsafe {
                            PREV_MOUSE_X = MOUSE_X;
                            PREV_MOUSE_Y = MOUSE_Y;
                            MOUSE_X = new_x;
                            MOUSE_Y = new_y;
                            CURSOR_MOVED = true;

                            if !CURSOR_VISIBLE {
                                CURSOR_VISIBLE = true;
                            }
                        }

                        changed = true;
                    }
                    MSG_POINTER_BUTTON => {
                        let btn: PointerButton = unsafe { msg.payload_as() };

                        // Only process left button press (not release).
                        if btn.button == 0 && btn.pressed == 1 {
                            let click_x = unsafe { MOUSE_X };
                            let click_y = unsafe { MOUSE_Y };

                            // Ignore clicks in the title bar region.
                            if click_y < TITLE_BAR_H {
                                continue;
                            }
                            // Only process clicks in text editor mode.
                            if unsafe { IMAGE_MODE } {
                                continue;
                            }

                            // Convert screen coordinates to content-relative
                            // text coordinates. The content surface starts at
                            // (content_x, content_y) and text within it has
                            // insets TEXT_INSET_X and TEXT_INSET_TOP.
                            let text_origin_x = content_x as u32 + TEXT_INSET_X;
                            let text_origin_y = content_y as u32 + TEXT_INSET_TOP;
                            // If click is above the text area (in shadow region
                            // between title bar and text start), clamp to line 0.
                            let rel_x = click_x.saturating_sub(text_origin_x);
                            let rel_y = click_y.saturating_sub(text_origin_y);
                            // Account for scroll offset: add scroll_offset
                            // visual lines worth of pixels to the y coordinate.
                            let scroll = unsafe { SCROLL_OFFSET };
                            let line_h = unsafe { LINE_H };
                            let adjusted_y = rel_y + scroll * line_h;
                            // Use TextLayout::xy_to_byte to convert pixel
                            // position to byte offset.
                            let layout = content_text_layout(content_w);
                            let text = doc_content();
                            let byte_pos = layout.xy_to_byte(text, rel_x, adjusted_y);

                            // Update cursor position (compositor is sole writer).
                            unsafe {
                                CURSOR_POS = byte_pos;
                                // Clear selection on click.
                                SEL_START = 0;
                                SEL_END = 0;
                            }

                            doc_write_header();

                            // Notify the editor of the new cursor position
                            // so its local cursor variable stays in sync.
                            let cm = CursorMove {
                                position: byte_pos as u32,
                            };
                            let cm_msg = unsafe { ipc::Message::from_payload(MSG_SET_CURSOR, &cm) };

                            editor_ch.send(&cm_msg);

                            let _ = sys::channel_signal(EDITOR_HANDLE);

                            changed = true;
                            text_changed = true;
                        }
                    }
                    _ => {}
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
                    let len = unsafe { DOC_LEN };

                    if pos <= len {
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

        // Update scroll offset after processing all editor messages so that
        // the cursor remains visible in the viewport.
        if text_changed && !unsafe { IMAGE_MODE } {
            update_scroll_offset();
        }

        if changed || timer_fired {
            let back = unsafe { BACK_BUF_IDX };
            let in_image_mode = unsafe { IMAGE_MODE };

            // 1. Re-render the content surface (only on actual text/editor
            //    changes, not on timer ticks or mouse moves).
            if text_changed && !in_image_mode {
                let mut content_surf = make_surf(&mut content_buf, content_w, content_h);
                let new_scroll = unsafe { SCROLL_OFFSET };
                let sel_start = unsafe { SEL_START };
                let sel_end = unsafe { SEL_END };
                let prev_sel_start = unsafe { PREV_SEL_START };
                let prev_sel_end = unsafe { PREV_SEL_END };
                let first_render = unsafe { CONTENT_FIRST_RENDER };
                // Full clear is needed when:
                // - First render of the content surface
                // - Context switch (already handled above with force_full: true)
                // - Scroll offset changed (entire viewport shifted)
                // - Selection changed (highlight may span many lines)
                let force_full = first_render
                    || context_switched
                    || old_scroll != new_scroll
                    || sel_start != prev_sel_start
                    || sel_end != prev_sel_end;

                render_content_surface(&mut content_surf, doc_content(), force_full);

                if first_render {
                    unsafe { CONTENT_FIRST_RENDER = false };
                }
            }
            // In image mode, content surface stays unchanged (showing the image).

            // 2. Re-render title bar if timer fired (clock update) or context
            //    switched (icon change). Context switch already re-rendered
            //    above, but timer + content change needs handling here.
            if timer_fired && !context_switched {
                let mut title_surf = make_surf(&mut title_buf, fb_width, TITLE_BAR_H);

                render_title_bar(&mut title_surf);
            }

            // ---------------------------------------------------------------
            // 3. Compute dirty rects based on what changed this frame.
            // ---------------------------------------------------------------
            let mut damage = drawing::DamageTracker::new(fb_width as u16, fb_height as u16);

            if !first_present_done || context_switched {
                // First frame after init or context switch: full screen.
                damage.mark_full_screen();

                first_present_done = true;
            } else if text_changed && !in_image_mode {
                // Content change in text editor mode: compute which visual
                // lines changed and add dirty rects for those lines.
                let new_cursor = unsafe { CURSOR_POS };
                let new_doc_len = unsafe { DOC_LEN };
                let new_scroll = unsafe { SCROLL_OFFSET };
                let layout = content_text_layout(content_w);
                let text = doc_content();
                let line_h = unsafe { LINE_H };

                if old_scroll != new_scroll {
                    // Scroll offset changed — the entire content area shifted.
                    // Dirty the full content region.
                    damage.add(
                        content_x as u16,
                        content_y as u16,
                        content_w as u16,
                        content_h as u16,
                    );
                } else if line_h > 0 {
                    // Compute the visual line range that changed.
                    // Old cursor line (before scroll adjustment):
                    let (_, old_cy) = layout.byte_to_xy(text, old_cursor);
                    let old_line = old_cy / line_h;
                    // New cursor line:
                    let (_, new_cy) = layout.byte_to_xy(text, new_cursor);
                    let new_line = new_cy / line_h;
                    // The changed region spans from the earliest affected line
                    // down to the last line of text (insertions/deletions shift
                    // all subsequent lines).
                    let first_changed = if old_line < new_line {
                        old_line
                    } else {
                        new_line
                    };
                    // Compute the last visible line of text.
                    let total_text_lines = if new_doc_len == 0 {
                        1
                    } else {
                        let (_, end_cy) = layout.byte_to_xy(text, new_doc_len);
                        end_cy / line_h + 1
                    };
                    // Use the total line count captured before
                    // render_content_surface (which updates PREV_TOTAL_LINES)
                    // so we get the pre-render line count for accurate dirty
                    // tracking when newlines are deleted.
                    let old_total = old_total_lines;
                    let last_line = if total_text_lines > old_total {
                        total_text_lines
                    } else {
                        old_total
                    };
                    // Convert visual lines to content-surface Y coordinates,
                    // accounting for scroll offset.
                    let vis_first = first_changed.saturating_sub(new_scroll);
                    let vis_last = last_line.saturating_sub(new_scroll);
                    let vp = viewport_lines(content_h);
                    // Clamp to the visible viewport.
                    let draw_first = vis_first;
                    let draw_last = if vis_last > vp { vp } else { vis_last };

                    if draw_last > draw_first {
                        // Convert to framebuffer coordinates.
                        let dirty_y = content_y as u32 + TEXT_INSET_TOP + draw_first * line_h;
                        let dirty_h = (draw_last - draw_first) * line_h;
                        // Clamp to framebuffer height.
                        let clamped_h = if dirty_y + dirty_h > fb_height {
                            fb_height - dirty_y
                        } else {
                            dirty_h
                        };

                        damage.add(0, dirty_y as u16, fb_width as u16, clamped_h as u16);
                    }

                    // Also dirty the old cursor line if different from new
                    // (cursor bar moved between lines).
                    if old_line != new_line {
                        let vis_old = old_line.saturating_sub(new_scroll);

                        if vis_old < vp {
                            let old_dirty_y = content_y as u32 + TEXT_INSET_TOP + vis_old * line_h;

                            damage.add(0, old_dirty_y as u16, fb_width as u16, line_h as u16);
                        }
                    }
                }

                // If timer also fired, dirty the title bar for clock update.
                if timer_fired {
                    damage.add(0, 0, fb_width as u16, TITLE_BAR_H as u16);
                }
            } else if text_changed && in_image_mode {
                // Image mode content change (context switch already handled
                // above via context_switched). Fall back to full screen.
                damage.mark_full_screen();
            }

            // Timer-only tick or timer+cursor: re-render title bar for clock.
            if timer_fired && !context_switched && !damage.full_screen {
                damage.add(0, 0, fb_width as u16, TITLE_BAR_H as u16);
            }

            // 3b. Cursor dirty rects: old position + new position.
            let cursor_moved = unsafe { CURSOR_MOVED };
            let cursor_vis = unsafe { CURSOR_VISIBLE };

            if cursor_moved && cursor_vis {
                let old_mx = unsafe { PREV_MOUSE_X };
                let old_my = unsafe { PREV_MOUSE_Y };
                let new_mx = unsafe { MOUSE_X };
                let new_my = unsafe { MOUSE_Y };
                let cw = drawing::CURSOR_W as u16;
                let ch = drawing::CURSOR_H as u16;

                // Dirty the old cursor position (erase).
                damage.add(old_mx as u16, old_my as u16, cw, ch);
                // Dirty the new cursor position (draw).
                damage.add(new_mx as u16, new_my as u16, cw, ch);

                unsafe { CURSOR_MOVED = false };
            }

            // ---------------------------------------------------------------
            // 4. Composite dirty regions into the back framebuffer.
            // ---------------------------------------------------------------
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
            let title_cs = drawing::CompositeSurface {
                surface: make_surf(&mut title_buf, fb_width, TITLE_BAR_H),
                x: 0,
                y: 0,
                z: Z_CHROME,
                visible: true,
            };
            let cursor_cs = drawing::CompositeSurface {
                surface: make_surf(&mut cursor_buf, drawing::CURSOR_W, drawing::CURSOR_H),
                x: unsafe { MOUSE_X } as i32,
                y: unsafe { MOUSE_Y } as i32,
                z: Z_CURSOR,
                visible: cursor_vis,
            };
            let surfaces: [&drawing::CompositeSurface; 5] =
                [&bg_cs, &content_cs, &title_shadow_cs, &title_cs, &cursor_cs];

            {
                let mut fb = make_fb_surface(back);

                if let Some(rects) = damage.dirty_rects() {
                    // Partial composite: only re-composite dirty regions.
                    for r in rects {
                        drawing::composite_surfaces_rect(
                            &mut fb, &surfaces, r.x as u32, r.y as u32, r.w as u32, r.h as u32,
                        );
                    }
                } else {
                    // Full-screen composite.
                    drawing::composite_surfaces(&mut fb, &surfaces);
                }
            }

            // ---------------------------------------------------------------
            // 5. Present with dirty rects for partial GPU transfer.
            // ---------------------------------------------------------------
            let payload = if let Some(rects) = damage.dirty_rects() {
                let n = rects.len();
                let n = rects.len();
                let mut pr = [drawing::DirtyRect::new(0, 0, 0, 0); 6];
                let mut i = 0;

                while i < n && i < 6 {
                    pr[i] = rects[i];
                    i += 1;
                }

                PresentPayload {
                    buffer_index: back as u32,
                    rect_count: n as u32,
                    rects: pr,
                    _pad: [0; 4],
                }
            } else {
                // Full-screen present.
                PresentPayload {
                    buffer_index: back as u32,
                    rect_count: 0,
                    rects: [drawing::DirtyRect::new(0, 0, 0, 0); 6],
                    _pad: [0; 4],
                }
            };
            let present_msg = unsafe { ipc::Message::from_payload(MSG_PRESENT, &payload) };

            gpu_ch.send(&present_msg);

            let _ = sys::channel_signal(GPU_HANDLE);

            // 6. Swap back/front buffers.
            unsafe { BACK_BUF_IDX = 1 - back };

            // 7. Synchronize: copy dirty regions from the just-presented
            //    buffer to the new back buffer so both buffers stay identical.
            //    Without this, the new back buffer contains stale content from
            //    two frames ago in non-dirty regions, causing visual glitches
            //    when the GPU transfers only dirty rects.
            if let Some(rects) = damage.dirty_rects() {
                let new_back = unsafe { BACK_BUF_IDX };
                let src_ptr = unsafe { FB_PTRS[back] };
                let dst_ptr = unsafe { FB_PTRS[new_back] };
                let stride = fb_stride as usize;

                for r in rects {
                    let rx = r.x as usize;
                    let ry = r.y as usize;
                    let rw = r.w as usize;
                    let rh = r.h as usize;
                    let bpp = 4usize; // BGRA8888

                    for row in 0..rh {
                        let y = ry + row;

                        if y >= fb_height as usize {
                            break;
                        }

                        let offset = y * stride + rx * bpp;
                        let bytes = rw * bpp;

                        if offset + bytes <= fb_size as usize {
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    src_ptr.add(offset),
                                    dst_ptr.add(offset),
                                    bytes,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
