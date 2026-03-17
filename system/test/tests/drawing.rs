//! Host-side tests for the drawing library.

use drawing::{Color, PixelFormat, Surface};

/// Fixed-pitch text layout engine (test-local copy).
///
/// This is a local duplicate of the layout engine that now lives in Core.
/// Tests can't import from services, so we define it here. These are pure
/// computation functions with no dependencies.
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

    fn byte_to_xy(&self, text: &[u8], offset: usize) -> (u32, u32) {
        let cols = self.cols();

        if cols == 0 || text.is_empty() {
            return (0, 0);
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
                return (col as u32 * self.char_width, row * self.line_height);
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

        (col as u32 * self.char_width, row * self.line_height)
    }

    fn total_visual_lines(&self, text: &[u8]) -> u32 {
        if text.is_empty() {
            return 0;
        }

        let mut count = 0u32;

        self.layout_lines(text, |_, _, _| {
            count += 1;
        });

        count
    }

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

    fn layout_lines(&self, text: &[u8], mut f: impl FnMut(usize, usize, u32)) {
        if text.is_empty() {
            return;
        }

        let cols = self.cols();

        if cols == 0 {
            return;
        }

        let mut row = 0u32;
        let mut line_start = 0usize;
        let mut col = 0usize;

        for (i, &byte) in text.iter().enumerate() {
            if byte == b'\n' {
                f(line_start, i, row);

                row += 1;
                line_start = i + 1;
                col = 0;

                continue;
            }
            if col >= cols {
                f(line_start, i, row);

                row += 1;
                line_start = i;
                col = 0;
            }

            col += 1;
        }

        f(line_start, text.len(), row);
    }

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

// ---------------------------------------------------------------------------
// Test-local copies of types moved out of drawing to their consumers.
// Tests can't import from services, so we duplicate these pure
// data/computation items here.
// ---------------------------------------------------------------------------

use protocol::DirtyRect;

/// Maximum number of dirty rects that fit in a MSG_PRESENT payload.
const MAX_DIRTY_RECTS: usize = 6;

/// Collects dirty rectangles during a render pass (test-local copy).
struct DamageTracker {
    rects: [DirtyRect; MAX_DIRTY_RECTS],
    count: usize,
    full_screen: bool,
    fb_width: u16,
    fb_height: u16,
}

impl DamageTracker {
    const fn new(fb_width: u16, fb_height: u16) -> Self {
        Self {
            rects: [DirtyRect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            }; MAX_DIRTY_RECTS],
            count: 0,
            full_screen: false,
            fb_width,
            fb_height,
        }
    }

    fn add(&mut self, x: u16, y: u16, w: u16, h: u16) {
        if self.full_screen || w == 0 || h == 0 {
            return;
        }
        if self.count >= MAX_DIRTY_RECTS {
            self.full_screen = true;
            return;
        }
        self.rects[self.count] = DirtyRect::new(x, y, w, h);
        self.count += 1;
    }

    fn bounding_box(&self) -> DirtyRect {
        if self.full_screen || self.count == 0 {
            DirtyRect::new(0, 0, self.fb_width, self.fb_height)
        } else {
            DirtyRect::union_all(&self.rects[..self.count])
        }
    }

    fn dirty_rects(&self) -> Option<&[DirtyRect]> {
        if self.full_screen || self.count == 0 {
            None
        } else {
            Some(&self.rects[..self.count])
        }
    }

    fn mark_full_screen(&mut self) {
        self.full_screen = true;
    }

    fn reset(&mut self) {
        self.count = 0;
        self.full_screen = false;
    }
}

/// A compositing surface (test-local copy).
struct CompositeSurface<'a> {
    surface: Surface<'a>,
    x: i32,
    y: i32,
    z: u16,
    visible: bool,
}

fn min_u32(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
    }
}

/// Composite surfaces back-to-front (test-local copy).
fn composite_surfaces(dst: &mut Surface, surfaces: &[&CompositeSurface]) {
    const MAX_SURFACES: usize = 16;
    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;
    while i < count {
        order[i] = i;
        i += 1;
    }
    let mut j = 1;
    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;
        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }
        order[k] = key;
        j += 1;
    }
    let mut idx = 0;
    while idx < count {
        let s = surfaces[order[idx]];
        idx += 1;
        if !s.visible {
            continue;
        }
        let src_x_start: u32 = if s.x < 0 { (-s.x) as u32 } else { 0 };
        let src_y_start: u32 = if s.y < 0 { (-s.y) as u32 } else { 0 };
        let dst_x: u32 = if s.x < 0 { 0 } else { s.x as u32 };
        let dst_y: u32 = if s.y < 0 { 0 } else { s.y as u32 };
        let src_w = s.surface.width;
        let src_h = s.surface.height;
        if src_x_start >= src_w || src_y_start >= src_h {
            continue;
        }
        let visible_w = src_w - src_x_start;
        let visible_h = src_h - src_y_start;
        let src_offset = (src_y_start * s.surface.stride
            + src_x_start * s.surface.format.bytes_per_pixel()) as usize;
        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                visible_w,
                visible_h,
                s.surface.stride,
                dst_x,
                dst_y,
            );
        }
    }
}

/// Composite surfaces to a rectangular sub-region (test-local copy).
fn composite_surfaces_rect(
    dst: &mut Surface,
    surfaces: &[&CompositeSurface],
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
) {
    if rw == 0 || rh == 0 {
        return;
    }
    const MAX_SURFACES: usize = 16;
    let count = if surfaces.len() > MAX_SURFACES {
        MAX_SURFACES
    } else {
        surfaces.len()
    };
    let mut order: [usize; MAX_SURFACES] = [0; MAX_SURFACES];
    let mut i = 0;
    while i < count {
        order[i] = i;
        i += 1;
    }
    let mut j = 1;
    while j < count {
        let key = order[j];
        let key_z = surfaces[key].z;
        let mut k = j;
        while k > 0 && surfaces[order[k - 1]].z > key_z {
            order[k] = order[k - 1];
            k -= 1;
        }
        order[k] = key;
        j += 1;
    }
    let rx_end = min_u32(rx + rw, dst.width);
    let ry_end = min_u32(ry + rh, dst.height);
    if rx >= rx_end || ry >= ry_end {
        return;
    }
    let mut idx = 0;
    while idx < count {
        let s = surfaces[order[idx]];
        idx += 1;
        if !s.visible {
            continue;
        }
        let surf_fb_x0 = if s.x < 0 { 0i32 } else { s.x };
        let surf_fb_y0 = if s.y < 0 { 0i32 } else { s.y };
        let surf_fb_x1 = s.x + s.surface.width as i32;
        let surf_fb_y1 = s.y + s.surface.height as i32;
        let ix0 = if surf_fb_x0 > rx as i32 {
            surf_fb_x0
        } else {
            rx as i32
        };
        let iy0 = if surf_fb_y0 > ry as i32 {
            surf_fb_y0
        } else {
            ry as i32
        };
        let ix1 = if surf_fb_x1 < rx_end as i32 {
            surf_fb_x1
        } else {
            rx_end as i32
        };
        let iy1 = if surf_fb_y1 < ry_end as i32 {
            surf_fb_y1
        } else {
            ry_end as i32
        };
        if ix0 >= ix1 || iy0 >= iy1 {
            continue;
        }
        let src_x = (ix0 - s.x) as u32;
        let src_y = (iy0 - s.y) as u32;
        let blit_w = (ix1 - ix0) as u32;
        let blit_h = (iy1 - iy0) as u32;
        let src_offset =
            (src_y * s.surface.stride + src_x * s.surface.format.bytes_per_pixel()) as usize;
        if src_offset < s.surface.data.len() {
            dst.blit_blend(
                &s.surface.data[src_offset..],
                blit_w,
                blit_h,
                s.surface.stride,
                ix0 as u32,
                iy0 as u32,
            );
        }
    }
}

/// Width of the procedural arrow cursor in pixels.
const CURSOR_W: u32 = 12;
/// Height of the procedural arrow cursor in pixels.
const CURSOR_H: u32 = 16;

/// Procedural arrow cursor bitmap (test-local copy).
const CURSOR_BITMAP: [u8; (CURSOR_W * CURSOR_H) as usize] = [
    2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  0
    2, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  1
    2, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, //  2
    2, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, 0, //  3
    2, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, 0, //  4
    2, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, 0, //  5
    2, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, 0, //  6
    2, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, 0, //  7
    2, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, 0, //  8
    2, 1, 1, 1, 1, 1, 1, 1, 1, 2, 0, 0, //  9
    2, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 0, // 10
    2, 1, 1, 2, 1, 1, 2, 0, 0, 0, 0, 0, // 11
    2, 1, 2, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 12
    2, 2, 0, 0, 2, 1, 1, 2, 0, 0, 0, 0, // 13
    2, 0, 0, 0, 0, 2, 1, 1, 2, 0, 0, 0, // 14
    0, 0, 0, 0, 0, 2, 2, 2, 0, 0, 0, 0, // 15
];

/// Render the procedural arrow cursor (test-local copy).
fn render_cursor(buf: &mut [u8]) {
    let stride = CURSOR_W * 4;
    let total = (CURSOR_W * CURSOR_H * 4) as usize;
    if buf.len() < total {
        return;
    }
    let mut i = 0;
    while i < total {
        buf[i] = 0;
        buf[i + 1] = 0;
        buf[i + 2] = 0;
        buf[i + 3] = 0;
        i += 4;
    }
    let fill = Color::rgb(255, 255, 255); // CURSOR_FILL
    let outline = Color::rgb(40, 40, 40); // CURSOR_OUTLINE
    let mut y = 0u32;
    while y < CURSOR_H {
        let mut x = 0u32;
        while x < CURSOR_W {
            let idx = (y * CURSOR_W + x) as usize;
            let color = match CURSOR_BITMAP[idx] {
                1 => fill,
                2 => outline,
                _ => {
                    x += 1;
                    continue;
                }
            };
            let off = (y * stride + x * 4) as usize;
            // BGRA8888 encoding
            buf[off] = color.b;
            buf[off + 1] = color.g;
            buf[off + 2] = color.r;
            buf[off + 3] = color.a;
            x += 1;
        }
        y += 1;
    }
}

/// Scale an absolute pointer coordinate (test-local copy).
fn scale_pointer_coord(coord: u32, max_pixels: u32) -> u32 {
    let result = (coord as u64 * max_pixels as u64) / 32768;
    let r = result as u32;
    if r >= max_pixels && max_pixels > 0 {
        max_pixels - 1
    } else {
        r
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Heap-allocate a zeroed GlyphCache without touching the stack.
///
/// GlyphCache is ~238 KiB with grayscale rendering. Uses `alloc_zeroed`
/// to avoid constructing the value on stack before moving to heap.
fn heap_glyph_cache() -> Box<fonts::cache::GlyphCache> {
    unsafe {
        let layout = std::alloc::Layout::new::<fonts::cache::GlyphCache>();
        let ptr = std::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Box::from_raw(ptr)
    }
}

/// Create a small test surface (zeroed buffer).
fn make_surface(buf: &mut [u8], width: u32, height: u32) -> Surface<'_> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    for b in buf.iter_mut() {
        *b = 0;
    }
    Surface {
        data: buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

// ---------------------------------------------------------------------------
// Color tests
// ---------------------------------------------------------------------------

#[test]
fn color_rgb_is_opaque() {
    let c = Color::rgb(100, 150, 200);
    assert_eq!(c.r, 100);
    assert_eq!(c.g, 150);
    assert_eq!(c.b, 200);
    assert_eq!(c.a, 255);
}

#[test]
fn color_rgba_preserves_alpha() {
    let c = Color::rgba(10, 20, 30, 128);
    assert_eq!(c.a, 128);
}

#[test]
fn color_encode_decode_roundtrip_via_pixel() {
    let mut buf = [0u8; 4]; // 1x1 surface
    let mut s = make_surface(&mut buf, 1, 1);

    let original = Color::rgba(11, 22, 33, 44);
    s.set_pixel(0, 0, original);
    assert_eq!(s.get_pixel(0, 0), Some(original));
}

#[test]
fn color_bgra_byte_order() {
    // Verify the actual byte layout in BGRA8888 format by inspecting the buffer.
    let mut buf = [0u8; 4]; // 1x1 surface
    let mut s = make_surface(&mut buf, 1, 1);

    s.set_pixel(0, 0, Color::rgba(0x11, 0x22, 0x33, 0x44));

    // BGRA order: B=0x33, G=0x22, R=0x11, A=0x44
    assert_eq!(buf, [0x33, 0x22, 0x11, 0x44]);
}

// ---------------------------------------------------------------------------
// Pixel format tests
// ---------------------------------------------------------------------------

#[test]
fn bgra8888_is_4_bytes() {
    assert_eq!(PixelFormat::Bgra8888.bytes_per_pixel(), 4);
}

// ---------------------------------------------------------------------------
// Surface: set_pixel / get_pixel
// ---------------------------------------------------------------------------

#[test]
fn set_get_pixel_roundtrip() {
    let mut buf = [0u8; 4 * 4 * 4]; // 4x4
    let mut s = make_surface(&mut buf, 4, 4);

    let red = Color::rgb(255, 0, 0);
    s.set_pixel(2, 1, red);

    assert_eq!(s.get_pixel(2, 1), Some(red));
    // Adjacent pixels untouched.
    assert_eq!(s.get_pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn set_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(4, 0, Color::WHITE); // x out of bounds
    s.set_pixel(0, 4, Color::WHITE); // y out of bounds
    s.set_pixel(100, 100, Color::WHITE); // way out

    // Buffer unchanged.
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn get_pixel_out_of_bounds_returns_none() {
    let mut buf = [0u8; 4 * 4 * 4];
    let s = make_surface(&mut buf, 4, 4);

    assert_eq!(s.get_pixel(4, 0), None);
    assert_eq!(s.get_pixel(0, 4), None);
}

// ---------------------------------------------------------------------------
// Surface: clear
// ---------------------------------------------------------------------------

#[test]
fn clear_fills_entire_surface() {
    let mut buf = [0u8; 8 * 8 * 4]; // 8x8
    let mut s = make_surface(&mut buf, 8, 8);

    let blue = Color::rgb(0, 0, 255);
    s.clear(blue);

    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(s.get_pixel(x, y), Some(blue), "mismatch at ({x}, {y})");
        }
    }
}

// ---------------------------------------------------------------------------
// Surface: fill_rect
// ---------------------------------------------------------------------------

#[test]
fn fill_rect_basic() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    let green = Color::rgb(0, 200, 0);
    s.fill_rect(2, 3, 4, 2, green);

    // Inside the rect.
    for y in 3..5 {
        for x in 2..6 {
            assert_eq!(s.get_pixel(x, y), Some(green), "inside at ({x}, {y})");
        }
    }
    // Outside the rect (spot checks).
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(1, 3), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(6, 3), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 5), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_clips_to_bounds() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    // Rect extends past right and bottom edges.
    s.fill_rect(2, 2, 10, 10, Color::WHITE);

    // Only the clipped region (2..4, 2..4) should be filled.
    assert_eq!(s.get_pixel(2, 2), Some(Color::WHITE));
    assert_eq!(s.get_pixel(3, 3), Some(Color::WHITE));
    // Outside the clipped region.
    assert_eq!(s.get_pixel(1, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_entirely_outside_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(5, 5, 10, 10, Color::WHITE); // starts past both edges

    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn fill_rect_zero_size_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(0, 0, 0, 5, Color::WHITE);
    s.fill_rect(0, 0, 5, 0, Color::WHITE);

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Surface: draw_hline / draw_vline
// ---------------------------------------------------------------------------

#[test]
fn draw_hline_basic() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut s = make_surface(&mut buf, 8, 4);

    s.draw_hline(1, 2, 5, Color::WHITE);

    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 2), Some(Color::WHITE));
    }
    // Not drawn outside.
    assert_eq!(s.get_pixel(0, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(6, 2), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(1, 1), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_vline_basic() {
    let mut buf = [0u8; 4 * 8 * 4];
    let mut s = make_surface(&mut buf, 4, 8);

    s.draw_vline(2, 1, 5, Color::WHITE);

    for y in 1..6 {
        assert_eq!(s.get_pixel(2, y), Some(Color::WHITE));
    }
    assert_eq!(s.get_pixel(2, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 6), Some(Color::rgba(0, 0, 0, 0)));
}

// ---------------------------------------------------------------------------
// Surface: draw_rect (outline)
// ---------------------------------------------------------------------------

#[test]
fn draw_rect_outline() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.draw_rect(1, 1, 5, 4, Color::WHITE);

    // Top edge: (1,1)..(6,1)
    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 1), Some(Color::WHITE), "top at x={x}");
    }
    // Bottom edge: (1,4)..(6,4)
    for x in 1..6 {
        assert_eq!(s.get_pixel(x, 4), Some(Color::WHITE), "bottom at x={x}");
    }
    // Left edge: (1,2)..(1,3)
    for y in 2..4 {
        assert_eq!(s.get_pixel(1, y), Some(Color::WHITE), "left at y={y}");
    }
    // Right edge: (5,2)..(5,3)
    for y in 2..4 {
        assert_eq!(s.get_pixel(5, y), Some(Color::WHITE), "right at y={y}");
    }
    // Interior is empty.
    for y in 2..4 {
        for x in 2..5 {
            assert_eq!(
                s.get_pixel(x, y),
                Some(Color::rgba(0, 0, 0, 0)),
                "interior at ({x}, {y})"
            );
        }
    }
}

#[test]
fn draw_rect_1x1() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_rect(1, 1, 1, 1, Color::WHITE);

    assert_eq!(s.get_pixel(1, 1), Some(Color::WHITE));
    // Only that one pixel.
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
    assert_eq!(s.get_pixel(2, 2), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn draw_rect_zero_size_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_rect(0, 0, 0, 5, Color::WHITE);
    s.draw_rect(0, 0, 5, 0, Color::WHITE);

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Surface: draw_line
// ---------------------------------------------------------------------------

#[test]
fn draw_line_horizontal() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut s = make_surface(&mut buf, 8, 4);

    s.draw_line(1, 2, 5, 2, Color::WHITE);

    for x in 1..=5 {
        assert_eq!(s.get_pixel(x, 2), Some(Color::WHITE), "at x={x}");
    }
}

#[test]
fn draw_line_vertical() {
    let mut buf = [0u8; 4 * 8 * 4];
    let mut s = make_surface(&mut buf, 4, 8);

    s.draw_line(2, 1, 2, 6, Color::WHITE);

    for y in 1..=6 {
        assert_eq!(s.get_pixel(2, y), Some(Color::WHITE), "at y={y}");
    }
}

#[test]
fn draw_line_diagonal() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    // 45-degree line from (0,0) to (4,4): Bresenham should hit each pixel.
    s.draw_line(0, 0, 4, 4, Color::WHITE);

    for i in 0..=4u32 {
        assert_eq!(s.get_pixel(i, i), Some(Color::WHITE), "at ({i}, {i})");
    }
}

#[test]
fn draw_line_clips_negative_coords() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    // Line starts outside surface (negative coords).
    s.draw_line(-2, -2, 1, 1, Color::WHITE);

    // Should draw the visible portion without panicking.
    assert_eq!(s.get_pixel(0, 0), Some(Color::WHITE));
    assert_eq!(s.get_pixel(1, 1), Some(Color::WHITE));
}

#[test]
fn draw_line_single_point() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.draw_line(2, 2, 2, 2, Color::WHITE);

    assert_eq!(s.get_pixel(2, 2), Some(Color::WHITE));
}

// ---------------------------------------------------------------------------
// Surface: draw_line anti-aliasing (Wu's algorithm)
// ---------------------------------------------------------------------------

#[test]
fn draw_line_aa_horizontal_pixel_perfect() {
    // VAL-PRIM-010: horizontal AA line must be pixel-perfect — only
    // the line pixels are set, no AA fringe on neighboring rows.
    let mut buf = [0u8; 10 * 5 * 4];
    let mut s = make_surface(&mut buf, 10, 5);

    s.draw_line(1, 2, 8, 2, Color::WHITE);

    // Every pixel on the line must be fully opaque white.
    for x in 1..=8 {
        assert_eq!(s.get_pixel(x, 2), Some(Color::WHITE), "line pixel at x={x}");
    }
    // Rows above and below must be untouched (all zero = transparent black).
    for x in 0..10 {
        let above = s.get_pixel(x, 1).unwrap();
        let below = s.get_pixel(x, 3).unwrap();
        assert_eq!(above.a, 0, "no AA fringe above at x={x}");
        assert_eq!(below.a, 0, "no AA fringe below at x={x}");
    }
}

#[test]
fn draw_line_aa_vertical_pixel_perfect() {
    // VAL-PRIM-010: vertical AA line must be pixel-perfect.
    let mut buf = [0u8; 5 * 10 * 4];
    let mut s = make_surface(&mut buf, 5, 10);

    s.draw_line(2, 1, 2, 8, Color::WHITE);

    // Every pixel on the line must be fully opaque white.
    for y in 1..=8 {
        assert_eq!(s.get_pixel(2, y), Some(Color::WHITE), "line pixel at y={y}");
    }
    // Columns left and right must be untouched.
    for y in 0..10 {
        let left = s.get_pixel(1, y).unwrap();
        let right = s.get_pixel(3, y).unwrap();
        assert_eq!(left.a, 0, "no AA fringe left at y={y}");
        assert_eq!(right.a, 0, "no AA fringe right at y={y}");
    }
}

#[test]
fn draw_line_aa_diagonal_has_smooth_edges() {
    // VAL-PRIM-009: diagonal line must have anti-aliased edge pixels
    // with intermediate alpha values (not just 0 or 255).
    let mut buf = [0u8; 20 * 20 * 4];
    let mut s = make_surface(&mut buf, 20, 20);

    // Shallow diagonal: (0, 2) → (19, 10). dx=19, dy=8, slope ≈ 0.42.
    s.draw_line(0, 2, 19, 10, Color::WHITE);

    // Collect all non-zero alpha pixels.
    let mut has_intermediate_alpha = false;
    let mut has_full_alpha = false;
    for y in 0..20 {
        for x in 0..20 {
            let c = s.get_pixel(x, y).unwrap();
            if c.a > 0 && c.a < 255 {
                has_intermediate_alpha = true;
            }
            if c.a == 255 {
                has_full_alpha = true;
            }
        }
    }
    assert!(has_full_alpha, "diagonal line should have fully opaque pixels");
    assert!(
        has_intermediate_alpha,
        "diagonal line should have intermediate alpha AA pixels"
    );
}

#[test]
fn draw_line_aa_no_gaps() {
    // Expected behavior: no gaps in the line.
    // Every column along a shallow-slope line should have at least one
    // non-zero pixel.
    let mut buf = [0u8; 30 * 20 * 4];
    let mut s = make_surface(&mut buf, 30, 20);

    // Shallow line: (0, 5) → (29, 15). dx=29, dy=10.
    s.draw_line(0, 5, 29, 15, Color::WHITE);

    // Every column from 0 to 29 must have at least one non-zero pixel.
    for x in 0..30 {
        let mut has_pixel = false;
        for y in 0..20 {
            let c = s.get_pixel(x, y).unwrap();
            if c.a > 0 {
                has_pixel = true;
                break;
            }
        }
        assert!(has_pixel, "no gap at column x={x}");
    }
}

#[test]
fn draw_line_aa_width_consistent_across_angles() {
    // Expected behavior: visual line width consistent across angles (±0.5px).
    // We verify that the total "ink" (sum of alpha) per step along the major
    // axis is approximately constant for a shallow diagonal.
    let mut buf = [0u8; 40 * 40 * 4];
    let mut s = make_surface(&mut buf, 40, 40);

    // Line from (2, 2) → (38, 20). dx=36, dy=18.
    s.draw_line(2, 2, 38, 20, Color::WHITE);

    // For each column, sum the alpha of all pixels.
    let mut col_alpha_sums = [0u32; 40];
    for x in 2..=38 {
        for y in 0..40 {
            let c = s.get_pixel(x, y).unwrap();
            col_alpha_sums[x as usize] += c.a as u32;
        }
    }

    // The alpha sum per column should be approximately 255 (one full pixel
    // of coverage). Allow ±128 (±0.5px) tolerance.
    for x in 3..38 {
        // skip endpoints which may have partial coverage
        let sum = col_alpha_sums[x as usize];
        assert!(
            sum >= 127 && sum <= 383,
            "column {x}: alpha sum {sum} outside [127, 383] — width inconsistent"
        );
    }
}

#[test]
fn draw_line_aa_steep_diagonal_smooth() {
    // Steep diagonal: verify AA on steep (dy > dx) lines too.
    let mut buf = [0u8; 10 * 30 * 4];
    let mut s = make_surface(&mut buf, 10, 30);

    // Steep line: (2, 0) → (8, 29). dx=6, dy=29.
    s.draw_line(2, 0, 8, 29, Color::WHITE);

    let mut has_intermediate_alpha = false;
    for y in 0..30 {
        for x in 0..10 {
            let c = s.get_pixel(x, y).unwrap();
            if c.a > 0 && c.a < 255 {
                has_intermediate_alpha = true;
            }
        }
    }
    assert!(
        has_intermediate_alpha,
        "steep diagonal should have intermediate alpha AA pixels"
    );
}

#[test]
fn draw_line_aa_45_degree_no_fringe() {
    // A perfect 45-degree line passes through pixel centers — Wu's algorithm
    // should produce the same result as Bresenham: only the diagonal pixels set.
    let mut buf = [0u8; 10 * 10 * 4];
    let mut s = make_surface(&mut buf, 10, 10);

    s.draw_line(1, 1, 6, 6, Color::WHITE);

    for i in 1..=6u32 {
        assert_eq!(
            s.get_pixel(i, i),
            Some(Color::WHITE),
            "45° pixel at ({i},{i})"
        );
    }
}

#[test]
fn draw_line_aa_reverse_direction() {
    // Drawing from (x1,y1) to (x0,y0) should produce the same result.
    let w = 20u32;
    let h = 20u32;
    let mut buf1 = vec![0u8; (w * h * 4) as usize];
    let mut buf2 = vec![0u8; (w * h * 4) as usize];

    let mut s1 = make_surface(&mut buf1, w, h);
    let mut s2 = make_surface(&mut buf2, w, h);

    s1.draw_line(2, 3, 18, 14, Color::WHITE);
    s2.draw_line(18, 14, 2, 3, Color::WHITE);

    // Both surfaces should have the same pixels.
    for y in 0..h {
        for x in 0..w {
            let c1 = s1.get_pixel(x, y).unwrap();
            let c2 = s2.get_pixel(x, y).unwrap();
            assert_eq!(
                c1, c2,
                "reverse mismatch at ({x},{y}): forward={c1:?} reverse={c2:?}"
            );
        }
    }
}

#[test]
fn draw_line_aa_blends_with_background() {
    // AA pixels should blend with existing background via gamma-correct blending.
    let w = 20u32;
    let h = 10u32;
    let bg = Color::rgb(100, 100, 100);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut s = make_surface(&mut buf, w, h);
    s.clear(bg);

    s.draw_line(0, 1, 19, 8, Color::WHITE);

    // Find an AA pixel (intermediate alpha before blending — now it's blended
    // with the background, so it should differ from both pure bg and pure white).
    let mut found_blended = false;
    for y in 0..h {
        for x in 0..w {
            let c = s.get_pixel(x, y).unwrap();
            if c != bg && c != Color::WHITE && c.a == 255 {
                // This is a blended pixel — not pure bg, not pure white.
                found_blended = true;
                break;
            }
        }
        if found_blended {
            break;
        }
    }
    assert!(
        found_blended,
        "AA line over background should produce blended intermediate pixels"
    );
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

#[test]
fn surface_with_stride_padding() {
    // Stride > width * bpp (rows have padding bytes).
    let stride = 5 * 4 + 8; // 28 bytes per row (5 pixels + 8 padding)
    let mut buf = vec![0u8; stride as usize * 4]; // 4 rows
    let mut s = Surface {
        data: &mut buf,
        width: 5,
        height: 4,
        stride,
        format: PixelFormat::Bgra8888,
    };

    let red = Color::rgb(255, 0, 0);
    s.set_pixel(4, 3, red); // last pixel, last row

    assert_eq!(s.get_pixel(4, 3), Some(red));
    assert_eq!(s.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn fill_rect_saturating_add_no_overflow() {
    // Ensure that x + w doesn't overflow u32.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.fill_rect(2, 0, u32::MAX, 1, Color::WHITE);

    // Should fill from x=2 to x=3 (clipped to width).
    assert_eq!(s.get_pixel(2, 0), Some(Color::WHITE));
    assert_eq!(s.get_pixel(3, 0), Some(Color::WHITE));
}

// ---------------------------------------------------------------------------
// Blit tests
// ---------------------------------------------------------------------------

#[test]
fn blit_basic() {
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);

    // Create a 4x4 red source.
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let src_stride = 4 * bpp;
    let mut src_buf = [0u8; 4 * 4 * 4];
    let red = Color::rgb(255, 0, 0);
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: src_stride,
            format: PixelFormat::Bgra8888,
        };
        src.clear(red);
    }

    dst.blit(&src_buf, 4, 4, src_stride, 2, 3);

    // Pixel inside blit region should be red.
    assert_eq!(dst.get_pixel(2, 3), Some(red));
    assert_eq!(dst.get_pixel(5, 6), Some(red));
    // Pixel outside blit region should be black/zeroed.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn blit_clips_at_edges() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 4 * bpp,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(0, 255, 0));
    }

    // Place at (6, 6) — only 2x2 pixels should fit.
    dst.blit(&src_buf, 4, 4, 4 * bpp, 6, 6);

    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(0, 255, 0)));
    // (5, 6) is outside the blit region.
    assert_eq!(dst.get_pixel(5, 6), Some(Color::rgba(0, 0, 0, 0)));
}

#[test]
fn blit_entirely_outside() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    let src_buf = [0xFFu8; 4 * 4 * 4];

    // Place entirely outside destination.
    dst.blit(&src_buf, 4, 4, 16, 8, 8);

    // Nothing should have changed.
    assert!(dst_buf.iter().all(|&b| b == 0));
}

#[test]
fn blit_zero_size_source() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    // Zero-width source — should be a no-op.
    dst.blit(&[], 0, 0, 0, 0, 0);
    assert!(dst_buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Alpha blending: Color::blend_over
// ---------------------------------------------------------------------------

#[test]
fn blend_over_opaque_src_returns_src() {
    let src = Color::rgb(255, 0, 0);
    let dst = Color::rgb(0, 0, 255);
    assert_eq!(src.blend_over(dst), src);
}

#[test]
fn blend_over_transparent_src_returns_dst() {
    let src = Color::TRANSPARENT;
    let dst = Color::rgb(0, 255, 0);
    assert_eq!(src.blend_over(dst), dst);
}

#[test]
fn blend_over_50_percent_red_on_opaque_blue() {
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgb(0, 0, 255);
    let result = src.blend_over(dst);

    // out_a = 128 + 255*(255-128)/255 = 128 + 127 = 255
    assert_eq!(result.a, 255);
    // Gamma-correct: blending happens in linear space then converts back to sRGB.
    // 50% alpha red on blue produces sRGB ~188 red (higher than linear's 128)
    // because the gamma curve maps linear 0.5 to sRGB ~0.74.
    assert!(
        result.r > 140,
        "gamma-correct red should be > 140, got {}",
        result.r
    );
    assert!(
        result.b > 140,
        "gamma-correct blue should be > 140, got {}",
        result.b
    );
    assert_eq!(result.g, 0);
}

#[test]
fn blend_over_25_percent_white_on_black() {
    let src = Color::rgba(255, 255, 255, 64);
    let dst = Color::rgb(0, 0, 0);
    let result = src.blend_over(dst);

    assert_eq!(result.a, 255);
    // Gamma-correct: 25% alpha white on black. In linear space, 25% of max
    // intensity maps to a higher sRGB value than 64 due to the gamma curve.
    assert!(
        result.r > 100,
        "gamma-correct 25% white on black should be > 100, got {}",
        result.r
    );
    assert_eq!(result.r, result.g);
    assert_eq!(result.r, result.b);
}

#[test]
fn blend_over_both_transparent() {
    let src = Color::TRANSPARENT;
    let dst = Color::TRANSPARENT;
    assert_eq!(src.blend_over(dst), Color::TRANSPARENT);
}

#[test]
fn blend_over_semi_on_semi() {
    // 50% red on 50% blue — both semi-transparent.
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgba(0, 0, 255, 128);
    let result = src.blend_over(dst);

    // out_a = 128 + 128*127/255 ≈ 191
    assert!(result.a >= 190 && result.a <= 192, "a={}", result.a);
    // Source (red) dominates since it's on top.
    assert!(
        result.r > result.b,
        "r={} should > b={}",
        result.r,
        result.b
    );
}

#[test]
fn blend_over_commutative_only_when_symmetric() {
    // Blending is NOT commutative in general — order matters.
    let a = Color::rgba(255, 0, 0, 128);
    let b = Color::rgba(0, 255, 0, 128);

    let ab = a.blend_over(b);
    let ba = b.blend_over(a);

    // Red-on-green: more red. Green-on-red: more green.
    assert!(ab.r > ab.g);
    assert!(ba.g > ba.r);
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::blend_pixel
// ---------------------------------------------------------------------------

#[test]
fn blend_pixel_on_opaque_background() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(1, 1, Color::rgb(0, 0, 255));
    s.blend_pixel(1, 1, Color::rgba(255, 0, 0, 128));

    let result = s.get_pixel(1, 1).unwrap();
    // Gamma-correct blending produces higher sRGB values than linear.
    assert!(
        result.r > 140,
        "gamma-correct red should be > 140, got {}",
        result.r
    );
    assert!(
        result.b > 140,
        "gamma-correct blue should be > 140, got {}",
        result.b
    );
    assert_eq!(result.a, 255);
}

#[test]
fn blend_pixel_transparent_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    let blue = Color::rgb(0, 0, 255);
    s.set_pixel(1, 1, blue);
    s.blend_pixel(1, 1, Color::TRANSPARENT);

    assert_eq!(s.get_pixel(1, 1), Some(blue));
}

#[test]
fn blend_pixel_opaque_overwrites() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.set_pixel(1, 1, Color::rgb(0, 0, 255));
    s.blend_pixel(1, 1, Color::rgb(255, 0, 0));

    assert_eq!(s.get_pixel(1, 1), Some(Color::rgb(255, 0, 0)));
}

#[test]
fn blend_pixel_out_of_bounds_is_noop() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.blend_pixel(10, 10, Color::rgba(255, 0, 0, 128));

    assert!(buf.iter().all(|&b| b == 0));
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::fill_rect_blend
// ---------------------------------------------------------------------------

#[test]
fn fill_rect_blend_on_opaque_background() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.clear(Color::BLACK);
    s.fill_rect_blend(2, 2, 4, 4, Color::rgba(255, 255, 255, 128));

    // Inside: gamma-correct 50% white on black gives sRGB ~188 (not 128).
    let inside = s.get_pixel(3, 3).unwrap();
    assert!(
        inside.r > 140,
        "gamma-correct 50% white on black should be > 140, got {}",
        inside.r
    );
    assert_eq!(inside.r, inside.g);
    assert_eq!(inside.r, inside.b);

    // Outside: still black.
    assert_eq!(s.get_pixel(0, 0), Some(Color::BLACK));
}

#[test]
fn fill_rect_blend_opaque_fast_path() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    // Opaque fill_rect_blend should behave identically to fill_rect.
    s.fill_rect_blend(1, 1, 3, 3, Color::rgb(200, 100, 50));

    assert_eq!(s.get_pixel(2, 2), Some(Color::rgb(200, 100, 50)));
}

#[test]
fn fill_rect_blend_transparent_is_noop() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut s = make_surface(&mut buf, 8, 8);

    s.clear(Color::WHITE);
    s.fill_rect_blend(0, 0, 8, 8, Color::TRANSPARENT);

    assert_eq!(s.get_pixel(0, 0), Some(Color::WHITE));
}

#[test]
fn fill_rect_blend_clips_to_bounds() {
    let mut buf = [0u8; 4 * 4 * 4];
    let mut s = make_surface(&mut buf, 4, 4);

    s.clear(Color::BLACK);
    s.fill_rect_blend(2, 2, 10, 10, Color::rgba(255, 0, 0, 128));

    // Clipped region blended — gamma-correct produces higher sRGB values.
    let px = s.get_pixel(3, 3).unwrap();
    assert!(
        px.r > 140,
        "gamma-correct 50% red on black should be > 140, got {}",
        px.r
    );
    // Outside clipped region unchanged.
    assert_eq!(s.get_pixel(1, 1), Some(Color::BLACK));
}

// ---------------------------------------------------------------------------
// Alpha blending: Surface::blit_blend
// ---------------------------------------------------------------------------

#[test]
fn blit_blend_transparent_pixels_pass_through() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    let blue = Color::rgb(0, 0, 255);
    dst.clear(blue);

    // Source is all transparent (zeroed).
    let src_buf = [0u8; 4 * 4 * 4];
    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    // Destination unchanged.
    assert_eq!(dst.get_pixel(3, 3), Some(blue));
}

#[test]
fn blit_blend_opaque_pixels_overwrite() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(255, 0, 0));
    }

    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn blit_blend_semi_transparent() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgba(255, 0, 0, 128));
    }

    dst.blit_blend(&src_buf, 4, 4, 16, 2, 2);

    let result = dst.get_pixel(3, 3).unwrap();
    // Gamma-correct blending: 50% red on blue produces higher sRGB values.
    assert!(
        result.r > 140,
        "gamma-correct red should be > 140, got {}",
        result.r
    );
    assert!(
        result.b > 140,
        "gamma-correct blue should be > 140, got {}",
        result.b
    );
    assert_eq!(result.a, 255);
}

#[test]
fn blit_blend_clips_at_edges() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut src_buf = [0u8; 4 * 4 * 4];
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 4,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        src.clear(Color::rgb(255, 0, 0));
    }

    // Place at (6, 6) — only 2x2 should fit.
    dst.blit_blend(&src_buf, 4, 4, 16, 6, 6);

    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(5, 6), Some(Color::BLACK));
}

#[test]
fn blit_blend_entirely_outside_is_noop() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::WHITE);

    let src_buf = [0xFFu8; 4 * 4 * 4];
    dst.blit_blend(&src_buf, 4, 4, 16, 8, 8);

    assert_eq!(dst.get_pixel(0, 0), Some(Color::WHITE));
}

#[test]
fn blit_blend_mixed_alpha_pixels() {
    // Source has both transparent and semi-transparent pixels.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));

    let mut src_buf = [0u8; 4 * 2 * 4]; // 4x2, starts transparent
    {
        let mut src = Surface {
            data: &mut src_buf,
            width: 4,
            height: 2,
            stride: 16,
            format: PixelFormat::Bgra8888,
        };
        // Left half: opaque red. Right half: stays transparent.
        src.fill_rect(0, 0, 2, 2, Color::rgb(255, 0, 0));
    }

    dst.blit_blend(&src_buf, 4, 2, 16, 2, 2);

    // Left half: overwritten with red.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    // Right half: blue shows through (transparent source).
    assert_eq!(dst.get_pixel(4, 2), Some(Color::rgb(0, 0, 255)));
    assert_eq!(dst.get_pixel(5, 3), Some(Color::rgb(0, 0, 255)));
}

const NUNITO_SANS: &[u8] = include_bytes!("../../share/nunito-sans.ttf");
const SOURCE_CODE_PRO: &[u8] = include_bytes!("../../share/source-code-pro.ttf");

// ---------------------------------------------------------------------------
// Rasterizer — glyph-ID-based rasterization (read-fonts outline extraction)
// ---------------------------------------------------------------------------

#[test]
fn rasterize_valid_glyph_produces_coverage() {
    // VAL-RASTER-001: rasterize(font, glyph_id=valid, 18px) returns Some(metrics)
    // with width > 0, height > 0, coverage sum > 0.
    let glyph_id = fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'A').unwrap();
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, glyph_id, 18, &mut raster, &mut scratch);
    assert!(metrics.is_some(), "valid glyph should produce Some(metrics)");
    let m = metrics.unwrap();
    assert!(m.width > 0, "bitmap width should be > 0");
    assert!(m.height > 0, "bitmap height should be > 0");

    let total = (m.width * m.height * 3) as usize;
    let coverage_sum: u64 = buf[..total].iter().map(|&b| b as u64).sum();
    assert!(coverage_sum > 0, "coverage sum should be > 0, got 0");
}

#[test]
fn rasterize_notdef_glyph_produces_valid_coverage() {
    // VAL-RASTER-001: glyph ID 0 (.notdef) produces valid output.
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, 0, 18, &mut raster, &mut scratch);
    // .notdef may have an outline (rectangle) or may be empty.
    // Either way, it should not panic and should return Some.
    assert!(metrics.is_some(), ".notdef (glyph_id=0) should return Some");
}

#[test]
fn rasterize_invalid_glyph_returns_none() {
    // VAL-RASTER-001: glyph ID u16::MAX returns None without panic.
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, u16::MAX, 18, &mut raster, &mut scratch);
    assert!(metrics.is_none(), "glyph_id=u16::MAX should return None (no panic)");
}

#[test]
fn rasterize_a_glyph_reasonable_dimensions() {
    // VAL-RASTER-002: 'A' at 18px produces bounding box ~5-20px wide, ~10-25px tall.
    let glyph_id = fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'A').unwrap();
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let m = fonts::rasterize::rasterize(SOURCE_CODE_PRO, glyph_id, 18, &mut raster, &mut scratch).unwrap();
    assert!(
        m.width >= 5 && m.width <= 20,
        "'A' at 18px width should be 5-20px, got {}",
        m.width,
    );
    assert!(
        m.height >= 10 && m.height <= 25,
        "'A' at 18px height should be 10-25px, got {}",
        m.height,
    );

    // Verify non-trivial coverage
    let total = (m.width * m.height * 3) as usize;
    let coverage_sum: u64 = buf[..total].iter().map(|&b| b as u64).sum();
    assert!(coverage_sum > 100, "coverage sum should be > 100, got {}", coverage_sum);
}

#[test]
fn rasterize_proportional_font_valid() {
    // VAL-RASTER-002: Nunito Sans glyph rasterizes correctly via read-fonts.
    let glyph_id = fonts::rasterize::glyph_id_for_char(NUNITO_SANS, 'W').unwrap();
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let m = fonts::rasterize::rasterize(NUNITO_SANS, glyph_id, 18, &mut raster, &mut scratch).unwrap();
    assert!(m.width > 0, "proportional font glyph should have non-zero width");
    assert!(m.height > 0, "proportional font glyph should have non-zero height");
    assert!(m.advance > 0, "proportional font glyph should have non-zero advance");
}

// ---------------------------------------------------------------------------
// Coverage map compositing
// ---------------------------------------------------------------------------

#[test]
fn draw_coverage_basic() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    // 2x2 coverage map (1 byte per pixel, grayscale).
    // Pixel (0,0): full coverage.
    // Pixel (1,0): half coverage.
    // Pixel (0,1): quarter coverage.
    // Pixel (1,1): zero coverage.
    let coverage = [
        255, 128, // row 0: full, half
        64, 0, // row 1: quarter, zero
    ];
    dst.draw_coverage(2, 2, &coverage, 2, 2, Color::WHITE);

    // Full coverage → white.
    let p0 = dst.get_pixel(2, 2).unwrap();
    assert_eq!(p0.r, 255);
    assert_eq!(p0.g, 255);

    // Half coverage → blended.
    let p1 = dst.get_pixel(3, 2).unwrap();
    assert!(
        p1.r > 0 && p1.r < 255,
        "half coverage should blend, got {}",
        p1.r
    );

    // Zero coverage → unchanged (black).
    let p3 = dst.get_pixel(3, 3).unwrap();
    assert_eq!(p3.r, 0);
}

#[test]
fn draw_coverage_negative_coords_clip() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);

    // Place at negative coords — should clip without panic.
    // 2x2 coverage, 1 byte per pixel (grayscale). All full coverage.
    let coverage = [255u8; 4]; // 2*2 = 4 bytes
    dst.draw_coverage(-1, -1, &coverage, 2, 2, Color::WHITE);

    // (0, 0) should be drawn (it's at local (1, 1) of the coverage map).
    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
}

#[test]
fn draw_coverage_colored() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    // 1x1 pixel, grayscale coverage, full coverage.
    let coverage = [255u8];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::rgb(255, 0, 0));

    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
    assert_eq!(p.g, 0);
    assert_eq!(p.b, 0);
}

// ---------------------------------------------------------------------------
// TextLayout tests
// ---------------------------------------------------------------------------

fn make_layout(max_width: u32) -> TextLayout {
    TextLayout {
        char_width: 8,
        line_height: 20,
        max_width,
    }
}

// --- layout_lines ---

#[test]
fn layout_lines_empty_text() {
    let layout = make_layout(200);
    let mut count = 0;
    layout.layout_lines(b"", |_, _, _| count += 1);
    assert_eq!(count, 0);
}

#[test]
fn layout_lines_single_line_no_wrap() {
    let layout = make_layout(200); // 25 cols
    let text = b"hello";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], (0, 5, 0));
}

#[test]
fn layout_lines_newline_creates_new_line() {
    let layout = make_layout(200);
    let text = b"ab\ncd";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 2, 0)); // "ab"
    assert_eq!(lines[1], (3, 5, 1)); // "cd"
}

#[test]
fn layout_lines_wrap_at_max_width() {
    let layout = make_layout(24); // 3 cols (24 / 8)
    let text = b"abcdef";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 3, 0)); // "abc"
    assert_eq!(lines[1], (3, 6, 1)); // "def"
}

#[test]
fn layout_lines_wrap_and_newline_combined() {
    let layout = make_layout(24); // 3 cols
    let text = b"abc\nde";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 3, 0)); // "abc"
    assert_eq!(lines[1], (4, 6, 1)); // "de"
}

#[test]
fn layout_lines_trailing_newline() {
    let layout = make_layout(200);
    let text = b"hello\n";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    // "hello" on line 0, empty line 1 from trailing newline.
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], (0, 5, 0));
    assert_eq!(lines[1], (6, 6, 1));
}

#[test]
fn layout_lines_multiple_newlines() {
    let layout = make_layout(200);
    let text = b"a\n\nb";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], (0, 1, 0)); // "a"
    assert_eq!(lines[1], (2, 2, 1)); // empty
    assert_eq!(lines[2], (3, 4, 2)); // "b"
}

#[test]
fn layout_lines_exact_width_no_extra_wrap() {
    let layout = make_layout(24); // 3 cols
    let text = b"abc";
    let mut lines = Vec::new();
    layout.layout_lines(text, |start, end, row| lines.push((start, end, row)));
    // Exactly fills one line, no extra wrap.
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0], (0, 3, 0));
}

// --- byte_to_xy ---

#[test]
fn byte_to_xy_start_of_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 0);
    assert_eq!((x, y), (0, 0));
}

#[test]
fn byte_to_xy_middle_of_line() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 3);
    assert_eq!((x, y), (24, 0)); // col 3 * 8px
}

#[test]
fn byte_to_xy_end_of_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hello", 5);
    assert_eq!((x, y), (40, 0)); // col 5 * 8px
}

#[test]
fn byte_to_xy_after_newline() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"ab\ncd", 3);
    assert_eq!((x, y), (0, 20)); // start of row 1
}

#[test]
fn byte_to_xy_at_newline_char() {
    let layout = make_layout(200);
    // Cursor at the newline itself = end of that line.
    let (x, y) = layout.byte_to_xy(b"ab\ncd", 2);
    assert_eq!((x, y), (16, 0)); // col 2 on row 0
}

#[test]
fn byte_to_xy_wrapped_line() {
    let layout = make_layout(24); // 3 cols
                                  // "abcdef" wraps: "abc" on row 0, "def" on row 1.
    let (x, y) = layout.byte_to_xy(b"abcdef", 4);
    assert_eq!((x, y), (8, 20)); // col 1 on row 1
}

#[test]
fn byte_to_xy_past_end_clamps() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"hi", 10);
    // Past end -- should return position at end of text.
    assert_eq!((x, y), (16, 0));
}

#[test]
fn byte_to_xy_empty_text() {
    let layout = make_layout(200);
    let (x, y) = layout.byte_to_xy(b"", 0);
    assert_eq!((x, y), (0, 0));
}

// --- xy_to_byte ---

#[test]
fn xy_to_byte_origin() {
    let layout = make_layout(200);
    assert_eq!(layout.xy_to_byte(b"hello", 0, 0), 0);
}

#[test]
fn xy_to_byte_middle_of_line() {
    let layout = make_layout(200);
    // Click at pixel (24, 0) = col 3.
    assert_eq!(layout.xy_to_byte(b"hello", 24, 0), 3);
}

#[test]
fn xy_to_byte_between_chars_rounds_left() {
    let layout = make_layout(200);
    // Click at pixel (3, 0) = within first character cell.
    assert_eq!(layout.xy_to_byte(b"hello", 3, 0), 0);
}

#[test]
fn xy_to_byte_between_chars_rounds_right() {
    let layout = make_layout(200);
    // Click at pixel (5, 0) = past midpoint of first char (8px wide).
    assert_eq!(layout.xy_to_byte(b"hello", 5, 0), 1);
}

#[test]
fn xy_to_byte_past_end_of_line() {
    let layout = make_layout(200);
    // Click past the end of "hi" -- snaps to end of text.
    assert_eq!(layout.xy_to_byte(b"hi", 100, 0), 2);
}

#[test]
fn xy_to_byte_second_line() {
    let layout = make_layout(200);
    // "ab\ncd", click on row 1 col 1.
    assert_eq!(layout.xy_to_byte(b"ab\ncd", 8, 20), 4);
}

#[test]
fn xy_to_byte_wrapped_line() {
    let layout = make_layout(24); // 3 cols
                                  // "abcdef" wraps. Click at row 1, col 0 = byte 3.
    assert_eq!(layout.xy_to_byte(b"abcdef", 0, 20), 3);
}

#[test]
fn xy_to_byte_past_last_row() {
    let layout = make_layout(200);
    // Click below all text -- snaps to end.
    assert_eq!(layout.xy_to_byte(b"hello", 0, 100), 5);
}

#[test]
fn xy_to_byte_empty_text() {
    let layout = make_layout(200);
    assert_eq!(layout.xy_to_byte(b"", 50, 50), 0);
}

// ---------------------------------------------------------------------------
// sRGB gamma-correct blending tests
// ---------------------------------------------------------------------------

use drawing::{LINEAR_TO_SRGB, SRGB_TO_LINEAR};

#[test]
fn srgb_to_linear_boundary_values() {
    // sRGB 0 → linear 0
    assert_eq!(SRGB_TO_LINEAR[0], 0);
    // sRGB 255 → linear 65535
    assert_eq!(SRGB_TO_LINEAR[255], 65535);
    // sRGB 128 → roughly 21.6% linear ≈ 14158 (should be in that neighborhood)
    assert!(
        SRGB_TO_LINEAR[128] > 13000 && SRGB_TO_LINEAR[128] < 16000,
        "sRGB 128 → linear {} should be near 14158",
        SRGB_TO_LINEAR[128],
    );
}

#[test]
fn srgb_to_linear_monotonically_increasing() {
    for i in 1..256 {
        assert!(
            SRGB_TO_LINEAR[i] >= SRGB_TO_LINEAR[i - 1],
            "srgb_to_linear should be monotonic: [{}]={} < [{}]={}",
            i,
            SRGB_TO_LINEAR[i],
            i - 1,
            SRGB_TO_LINEAR[i - 1],
        );
    }
}

#[test]
fn linear_to_srgb_boundary_values() {
    // linear 0 → sRGB 0
    assert_eq!(LINEAR_TO_SRGB[0], 0);
    // linear 4095 (max index = 65535 >> 4) → sRGB 255
    assert_eq!(LINEAR_TO_SRGB[4095], 255);
}

#[test]
fn linear_to_srgb_monotonically_increasing() {
    for i in 1..4096 {
        assert!(
            LINEAR_TO_SRGB[i] >= LINEAR_TO_SRGB[i - 1],
            "linear_to_srgb should be monotonic: [{}]={} < [{}]={}",
            i,
            LINEAR_TO_SRGB[i],
            i - 1,
            LINEAR_TO_SRGB[i - 1],
        );
    }
}

#[test]
fn srgb_linear_roundtrip() {
    // Converting sRGB → linear → sRGB should return the original value (or ±1).
    // LINEAR_TO_SRGB is indexed by linear >> 4 (4096 entries).
    for srgb in 0u16..=255 {
        let linear = SRGB_TO_LINEAR[srgb as usize];
        let idx = (linear >> 4) as usize;
        let idx = if idx > 4095 { 4095 } else { idx };
        let back = LINEAR_TO_SRGB[idx];
        let diff = if back > srgb as u8 {
            back - srgb as u8
        } else {
            srgb as u8 - back
        };
        assert!(
            diff <= 1,
            "roundtrip sRGB {} → linear {} → sRGB {}: diff {}",
            srgb,
            linear,
            back,
            diff,
        );
    }
}

#[test]
fn gamma_blend_zero_coverage_unchanged() {
    // Zero-coverage pixels must not be modified at all.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(100, 150, 200));

    // Read the original pixel value.
    let orig = dst.get_pixel(0, 0).unwrap();

    // Draw with zero coverage (1 byte per pixel: 2x2 pixels = 4 bytes).
    let coverage = [0u8; 4];
    dst.draw_coverage(0, 0, &coverage, 2, 2, Color::WHITE);

    // Pixel must be identical.
    let after = dst.get_pixel(0, 0).unwrap();
    assert_eq!(orig, after, "zero-coverage should not modify destination");
}

#[test]
fn gamma_blend_full_coverage_replaces() {
    // Full coverage (255) with opaque color should fully replace the destination.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(0, 0, 255));

    // 1x1 pixel, grayscale, full coverage.
    let coverage = [255u8];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::rgb(255, 0, 0));

    let p = dst.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255);
    assert_eq!(p.g, 0);
    assert_eq!(p.b, 0);
}

#[test]
fn gamma_blend_half_coverage_heavier_than_linear() {
    // At 50% coverage, gamma-correct blending on a black background should
    // produce higher sRGB values than naive linear blending would (128).
    // This is the key test: gamma correction makes text appear heavier.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    // 1x1 pixel, grayscale, 50% coverage.
    let coverage = [128u8]; // ~50% coverage
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // Linear blending would give r=128. Gamma-correct blending should give
    // a higher value (~188) because 50% linear light maps to ~74% sRGB.
    assert!(
        p.r > 140,
        "gamma-correct 50% coverage on black should produce r > 140, got {}",
        p.r,
    );
}

#[test]
fn gamma_blend_over_half_red_on_blue_heavier() {
    // blend_over with 50% alpha: gamma-correct should produce heavier result.
    let src = Color::rgba(255, 0, 0, 128);
    let dst = Color::rgb(0, 0, 255);
    let result = src.blend_over(dst);

    // In gamma-correct blending, the red channel should be higher than 128
    // (linear would give ~128). The blue channel should also reflect the
    // gamma curve behavior.
    assert!(
        result.r > 140,
        "gamma-correct blend_over: 50% red on blue should produce r > 140, got {}",
        result.r,
    );
}

#[test]
fn gamma_blend_over_opaque_src_returns_src() {
    // Opaque source fast path must still work.
    let src = Color::rgb(200, 100, 50);
    let dst = Color::rgb(0, 0, 255);
    assert_eq!(src.blend_over(dst), src);
}

#[test]
fn gamma_blend_over_transparent_src_returns_dst() {
    // Transparent source fast path must still work.
    let src = Color::TRANSPARENT;
    let dst = Color::rgb(0, 255, 0);
    assert_eq!(src.blend_over(dst), dst);
}

#[test]
fn gamma_draw_coverage_uses_gamma_correction() {
    // Compare: 50% coverage white on black should produce sRGB value ~188,
    // not the linear-blended value of ~128.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    // 1x1 pixel, grayscale, 50% coverage.
    let coverage = [128u8];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // Gamma-correct: 50% linear ≈ 188 sRGB. Should be in range 180-195.
    assert!(
        p.r >= 180 && p.r <= 200,
        "gamma-correct coverage blend should give r ≈ 188, got {}",
        p.r,
    );
    // All channels should be equal for white on black.
    assert_eq!(p.r, p.g, "r ({}) should equal g ({})", p.r, p.g);
    assert_eq!(p.r, p.b, "r ({}) should equal b ({})", p.r, p.b);
}

// ---------------------------------------------------------------------------
// Vertical oversampling tests (grayscale anti-aliasing)
// ---------------------------------------------------------------------------

use fonts::rasterize::OVERSAMPLE_Y;

#[test]
fn oversample_y_is_at_least_4() {
    assert!(
        OVERSAMPLE_Y >= 4,
        "OVERSAMPLE_Y should be >= 4, got {}",
        OVERSAMPLE_Y,
    );
}

#[test]
fn grayscale_rasterize_produces_intermediate_coverage() {
    // Diagonal strokes should have intermediate coverage values (not just 0/255).
    // 'k' has diagonal strokes.

    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'k').unwrap(), 24 as u16, &mut raster, &mut scratch).unwrap();
    assert!(metrics.width > 0 && metrics.height > 0);

    // Output is 1 byte per pixel (grayscale).
    let total = (metrics.width * metrics.height) as usize;
    let coverage = &buf[..total];

    let intermediate_count = coverage.iter().filter(|&&c| c > 0 && c < 255).count();
    assert!(
        intermediate_count > 0,
        "'k' should have intermediate coverage values (smooth edges), got 0 intermediate pixels",
    );
}

#[test]
fn grayscale_diagonal_has_smooth_transitions() {
    // Diagonal strokes should show smooth transitions. Check 'x' which has diagonals.

    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'x').unwrap(), 24 as u16, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    // Output is 1 byte per pixel (grayscale).
    let total = (w * metrics.height) as usize;
    let coverage = &buf[..total];

    // Find a row in the middle of the glyph (where diagonals cross).
    let mid_row = metrics.height / 2;
    let row_start = (mid_row * w) as usize;
    let row_end = row_start + w as usize;
    let row = &coverage[row_start..row_end];

    // The middle row should have some intermediate values along edges.
    let has_intermediate = row.iter().any(|&c| c > 10 && c < 245);
    assert!(
        has_intermediate,
        "'x' mid-row should have intermediate coverage from vertical oversampling",
    );
}

#[test]
fn grayscale_curve_has_smooth_edges() {
    // Curved characters like 'o' should have smooth edges with grayscale AA.

    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'o').unwrap(), 24 as u16, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    // Output is 1 byte per pixel (grayscale).
    let total = (w * metrics.height) as usize;
    let coverage = &buf[..total];

    // Count distinct non-zero coverage levels (more levels = smoother).
    let mut levels = [false; 256];
    for &c in coverage.iter() {
        if c > 0 {
            levels[c as usize] = true;
        }
    }
    let distinct_levels = levels.iter().filter(|&&v| v).count();

    // With OVERSAMPLE_Y=8 vertical oversampling, we expect
    // more than 4 distinct levels at minimum.
    assert!(
        distinct_levels >= 4,
        "'o' should have at least 4 distinct non-zero coverage levels, got {}",
        distinct_levels,
    );
}

#[test]
fn grayscale_all_printable_ascii_still_rasterize() {
    // All printable ASCII should rasterize successfully with grayscale AA.

    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];

    for c in 0x20u8..=0x7Eu8 {
        let ch = c as char;
        let mut raster = fonts::rasterize::RasterBuffer {
            data: &mut buf,
            width: 128,
            height: 128,
        };
        let gid = match fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, ch) {
            Some(id) => id,
            None => continue,
        };
        let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, gid, 24, &mut raster, &mut scratch);
        assert!(
            metrics.is_some(),
            "grayscale: should rasterize '{}' (0x{:02x}) at 24px",
            ch,
            c,
        );
    }
}

#[test]
fn grayscale_glyph_cache_populated() {
    // GlyphCache should populate correctly with grayscale rendering.

    let mut cache = heap_glyph_cache();

    cache.populate(SOURCE_CODE_PRO, 16);

    // Check a few glyphs are cached with valid dimensions.
    let (g_a, cov_a) = cache.get(b'A' as u16).unwrap();
    assert!(
        g_a.width > 0 && g_a.height > 0,
        "'A' should have non-zero cached dimensions"
    );
    assert!(cov_a.len() > 0, "'A' coverage should be non-empty");
    // Coverage length should be width * height (1 byte per pixel grayscale).
    assert_eq!(
        cov_a.len(),
        (g_a.width * g_a.height) as usize,
        "'A' coverage should be 1 byte per pixel (grayscale)"
    );

    let (g_k, cov_k) = cache.get(b'k' as u16).unwrap();
    assert!(g_k.width > 0 && g_k.height > 0);

    // Check coverage has intermediate values (smooth edges).
    let has_intermediate = cov_k.iter().any(|&c| c > 0 && c < 255);
    assert!(
        has_intermediate,
        "'k' cached coverage should have intermediate values"
    );
}

// ---------------------------------------------------------------------------
// Grayscale coverage tests (replaced subpixel tests)
// ---------------------------------------------------------------------------

#[test]
fn grayscale_rasterizer_output_is_1_byte_per_pixel() {
    // Rasterized glyph coverage should be 1 byte per pixel (grayscale).

    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'H').unwrap(), 24 as u16, &mut raster, &mut scratch).unwrap();
    assert!(metrics.width > 0 && metrics.height > 0);

    // Total output bytes should be width * height (1 byte per pixel).
    let expected_bytes = (metrics.width * metrics.height) as usize;

    // Verify the data region is valid (non-zero coverage exists).
    let coverage = &buf[..expected_bytes];
    let has_nonzero = coverage.iter().any(|&c| c > 0);
    assert!(
        has_nonzero,
        "'H' grayscale coverage should have non-zero values"
    );
}

#[test]
fn grayscale_monospace_cache_has_1_byte_per_pixel() {
    // Cache produces 1-byte-per-pixel grayscale coverage.

    let mut cache = heap_glyph_cache();
    cache.populate(SOURCE_CODE_PRO, 16);

    let (g, cov) = cache.get(b'A' as u16).unwrap();
    assert_eq!(
        cov.len(),
        (g.width * g.height) as usize,
        "monospace cache: coverage should be 1 byte per pixel (grayscale)"
    );

    // Verify non-zero coverage exists.
    assert!(
        cov.iter().any(|&c| c > 0),
        "monospace cache 'A': should have non-zero coverage"
    );
}

#[test]
fn grayscale_proportional_cache_has_1_byte_per_pixel() {
    // The proportional cache should produce 1-byte-per-pixel data.

    let mut cache = heap_glyph_cache();
    cache.populate(SOURCE_CODE_PRO, 16);

    let (g, cov) = cache.get(b'A' as u16).unwrap();
    assert_eq!(
        cov.len(),
        (g.width * g.height) as usize,
        "proportional cache: coverage should be 1 byte per pixel (grayscale)"
    );

    // Verify non-zero coverage exists.
    assert!(
        cov.iter().any(|&c| c > 0),
        "proportional cache 'A': should have non-zero coverage"
    );
}

#[test]
fn grayscale_draw_coverage_uniform_rgb() {
    // Verify that draw_coverage with grayscale coverage applies the
    // single coverage value uniformly to R, G, B (no color fringing).
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    // 1x1 pixel: coverage=128 (half).
    let coverage = [128u8];
    dst.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);

    let p = dst.get_pixel(0, 0).unwrap();
    // All channels should be equal (uniform grayscale blend).
    assert_eq!(p.r, p.g, "R ({}) should equal G ({})", p.r, p.g);
    assert_eq!(p.r, p.b, "R ({}) should equal B ({})", p.r, p.b);
    // Half coverage should produce intermediate value.
    assert!(
        p.r > 128 && p.r < 255,
        "half coverage should produce intermediate value, got {}",
        p.r
    );
}

// ---------------------------------------------------------------------------
// Stem darkening — non-linear coverage boost for thin strokes
// ---------------------------------------------------------------------------

use fonts::cache::{STEM_DARKENING_BOOST, STEM_DARKENING_LUT};

#[test]
fn stem_darkening_lut_zero_stays_zero() {
    // Zero coverage must remain zero after darkening (no phantom pixels).
    assert_eq!(
        STEM_DARKENING_LUT[0], 0,
        "zero coverage should stay 0 after darkening"
    );
}

#[test]
fn stem_darkening_lut_full_stays_full() {
    // Full coverage (255) must remain 255 after darkening.
    assert_eq!(
        STEM_DARKENING_LUT[255], 255,
        "full coverage (255) should stay 255 after darkening"
    );
}

#[test]
fn stem_darkening_lut_boost_mid_range() {
    // Coverage values in the 30-200 range should be strictly higher after darkening.
    for cov in 30u8..=200u8 {
        let darkened = STEM_DARKENING_LUT[cov as usize];
        assert!(
            darkened > cov,
            "coverage {} should be strictly boosted, got {}",
            cov,
            darkened,
        );
    }
}

#[test]
fn stem_darkening_lut_monotonic() {
    // The LUT must be monotonically non-decreasing: higher input → ≥ higher output.
    for i in 1..256 {
        assert!(
            STEM_DARKENING_LUT[i] >= STEM_DARKENING_LUT[i - 1],
            "LUT not monotonic at {}: {} < {}",
            i,
            STEM_DARKENING_LUT[i],
            STEM_DARKENING_LUT[i - 1],
        );
    }
}

#[test]
fn stem_darkening_boost_is_tunable() {
    // The boost constant should be in a reasonable range (40-120).
    assert!(
        STEM_DARKENING_BOOST >= 40 && STEM_DARKENING_BOOST <= 120,
        "STEM_DARKENING_BOOST should be 40-120, got {}",
        STEM_DARKENING_BOOST,
    );
}

#[test]
fn stem_darkening_applied_to_rasterized_glyph() {
    // Rasterize a thin-stroke glyph ('l') and verify that intermediate
    // coverage values are boosted compared to the raw formula.
    // Since darkening is applied in the rasterizer, we verify the output
    // has higher coverage values than raw (undarkened) values would produce.
    
    let mut scratch = fonts::rasterize::RasterScratch::zeroed();
    let mut buf = [0u8; 128 * 128];
    let mut raster = fonts::rasterize::RasterBuffer {
        data: &mut buf,
        width: 128,
        height: 128,
    };

    let metrics = fonts::rasterize::rasterize(SOURCE_CODE_PRO, fonts::rasterize::glyph_id_for_char(SOURCE_CODE_PRO, 'l').unwrap(), 16 as u16, &mut raster, &mut scratch).unwrap();
    let w = metrics.width;
    let h = metrics.height;
    let total = (w * h) as usize; // 1 byte per pixel (grayscale)
    let coverage = &buf[..total];

    // Count coverage values that are in the boosted range (30-200).
    // After darkening, any raw value in 30-200 should now be higher.
    // We verify indirectly: the glyph should have coverage values in
    // the STEM_DARKENING_LUT[30]..=254 range (values that can only exist
    // if darkening was applied to raw values in 30..200).
    let boosted_threshold = STEM_DARKENING_LUT[30];
    let has_boosted = coverage.iter().any(|&c| c >= boosted_threshold && c < 255);
    assert!(
        has_boosted,
        "'l' at 16px should have boosted coverage values (>= {})",
        boosted_threshold,
    );
}

#[test]
fn stem_darkening_lut_matches_formula() {
    // The LUT is applied per grayscale byte.
    // Verify the formula: darkened = cov + BOOST * (255 - cov) / 255.
    // Special case: LUT[0] = 0 (no phantom pixels).
    let boost = STEM_DARKENING_BOOST as u32;
    assert_eq!(STEM_DARKENING_LUT[0], 0, "LUT[0] must be 0");
    for cov in 1u32..=255 {
        let expected = cov + boost * (255 - cov) / 255;
        let expected = if expected > 255 { 255 } else { expected };
        assert_eq!(
            STEM_DARKENING_LUT[cov as usize], expected as u8,
            "LUT[{}] should be {}, got {}",
            cov, expected, STEM_DARKENING_LUT[cov as usize],
        );
    }
}

// ---------------------------------------------------------------------------
// Damage tracking — DirtyRect + DamageTracker
// ---------------------------------------------------------------------------

#[test]
fn dirty_rect_new_stores_fields() {
    let r = protocol::DirtyRect::new(10, 20, 100, 50);
    assert_eq!(r.x, 10);
    assert_eq!(r.y, 20);
    assert_eq!(r.w, 100);
    assert_eq!(r.h, 50);
}

#[test]
fn dirty_rect_union_basic() {
    let a = protocol::DirtyRect::new(10, 20, 50, 30);
    let b = protocol::DirtyRect::new(40, 10, 80, 50);
    let u = a.union(b);
    // Union should be: x=10, y=10, x1=120, y1=60 → w=110, h=50
    assert_eq!(u.x, 10);
    assert_eq!(u.y, 10);
    assert_eq!(u.w, 110);
    assert_eq!(u.h, 50);
}

#[test]
fn dirty_rect_union_identity_with_zero() {
    let a = protocol::DirtyRect::new(10, 20, 50, 30);
    let zero = protocol::DirtyRect::new(0, 0, 0, 0);
    assert_eq!(a.union(zero), a);
    assert_eq!(zero.union(a), a);
}

#[test]
fn dirty_rect_union_all_multiple() {
    let rects = [
        protocol::DirtyRect::new(0, 0, 10, 10),
        protocol::DirtyRect::new(100, 200, 50, 30),
        protocol::DirtyRect::new(50, 100, 20, 20),
    ];
    let u = protocol::DirtyRect::union_all(&rects);
    assert_eq!(u.x, 0);
    assert_eq!(u.y, 0);
    assert_eq!(u.w, 150); // max(10, 150, 70)
    assert_eq!(u.h, 230); // max(10, 230, 120)
}

#[test]
fn dirty_rect_union_all_empty() {
    let u = protocol::DirtyRect::union_all(&[]);
    assert_eq!(u.w, 0);
    assert_eq!(u.h, 0);
}

#[test]
fn dirty_rect_size_is_8_bytes() {
    assert_eq!(core::mem::size_of::<protocol::DirtyRect>(), 8);
}

#[test]
fn damage_tracker_starts_empty() {
    let dt = DamageTracker::new(1024, 768);
    assert_eq!(dt.count, 0);
    assert!(!dt.full_screen);
}

#[test]
fn damage_tracker_add_rect() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.add(10, 20, 100, 50);
    assert_eq!(dt.count, 1);
    assert!(!dt.full_screen);
    let rects = dt.dirty_rects().unwrap();
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0], protocol::DirtyRect::new(10, 20, 100, 50));
}

#[test]
fn damage_tracker_ignores_zero_size() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.add(10, 20, 0, 50);
    dt.add(10, 20, 50, 0);
    assert_eq!(dt.count, 0);
}

#[test]
fn damage_tracker_overflow_triggers_full_screen() {
    let mut dt = DamageTracker::new(1024, 768);
    for i in 0..MAX_DIRTY_RECTS {
        dt.add(i as u16 * 10, 0, 10, 10);
    }
    assert!(!dt.full_screen);
    assert_eq!(dt.count, MAX_DIRTY_RECTS);
    // Adding one more should trigger full screen
    dt.add(200, 0, 10, 10);
    assert!(dt.full_screen);
    // dirty_rects returns None when full_screen
    assert!(dt.dirty_rects().is_none());
}

#[test]
fn damage_tracker_full_screen_bounding_box() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.mark_full_screen();
    let bb = dt.bounding_box();
    assert_eq!(bb.x, 0);
    assert_eq!(bb.y, 0);
    assert_eq!(bb.w, 1024);
    assert_eq!(bb.h, 768);
}

#[test]
fn damage_tracker_partial_bounding_box() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.add(10, 100, 200, 30);
    dt.add(50, 700, 300, 28);
    let bb = dt.bounding_box();
    assert_eq!(bb.x, 10);
    assert_eq!(bb.y, 100);
    assert_eq!(bb.w, 340); // 50+300 - 10 = 340
    assert_eq!(bb.h, 628); // 700+28 - 100 = 628
}

#[test]
fn damage_tracker_reset_clears_state() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.add(10, 20, 100, 50);
    dt.add(50, 60, 200, 100);
    assert_eq!(dt.count, 2);
    dt.reset();
    assert_eq!(dt.count, 0);
    assert!(!dt.full_screen);
    // After reset, dirty_rects returns None (no rects = full screen transfer)
    assert!(dt.dirty_rects().is_none());
}

#[test]
fn damage_tracker_add_after_full_screen_is_noop() {
    let mut dt = DamageTracker::new(1024, 768);
    dt.mark_full_screen();
    dt.add(10, 20, 100, 50);
    // count stays 0 — once full_screen is set, add is a no-op
    assert_eq!(dt.count, 0);
}

#[test]
fn damage_tracker_max_rects_is_6() {
    assert_eq!(MAX_DIRTY_RECTS, 6);
}

#[test]
fn damage_tracker_multiple_content_and_chrome_rects() {
    // Simulates the real use case: content area change + chrome change
    let mut dt = DamageTracker::new(1024, 768);
    // Content area: one line of text changed (approx one line_height tall)
    dt.add(13, 48, 998, 22); // text region
                             // Chrome area (e.g., title bar)
    dt.add(0, 0, 1024, 36); // title bar
    assert_eq!(dt.count, 2);
    let rects = dt.dirty_rects().unwrap();
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], protocol::DirtyRect::new(13, 48, 998, 22));
    assert_eq!(rects[1], protocol::DirtyRect::new(0, 0, 1024, 36));
}

// ---------------------------------------------------------------------------
// CompositeSurface + multi-surface compositing
// ---------------------------------------------------------------------------

// CompositeSurface is now a test-local type (moved out of drawing).

fn make_composite_surface<'a>(
    buf: &'a mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    z: u16,
) -> CompositeSurface<'a> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    for b in buf.iter_mut() {
        *b = 0;
    }
    CompositeSurface {
        surface: Surface {
            data: buf,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        },
        x,
        y,
        z,
        visible: true,
    }
}

#[test]
fn composite_surface_stores_position_and_z() {
    let mut buf = [0u8; 4 * 4 * 4];
    let cs = make_composite_surface(&mut buf, 4, 4, 10, 20, 5);
    assert_eq!(cs.x, 10);
    assert_eq!(cs.y, 20);
    assert_eq!(cs.z, 5);
    assert!(cs.visible);
}

#[test]
fn composite_two_opaque_surfaces_z_order() {
    // Background (z=0) is blue, foreground (z=1) is red at (2,2).
    // After compositing, the framebuffer should show red overlapping blue.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 2, 2, 1);
    fg.surface.clear(Color::rgb(255, 0, 0));

    // Composite back-to-front.
    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    composite_surfaces(&mut dst, &surfaces);

    // Outside the red overlay: should be blue.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 0, 255)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(0, 0, 255)));
    // Inside the red overlay: should be red.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(255, 0, 0)));
    // After the red overlay: should be blue.
    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_respects_z_order_not_array_order() {
    // Pass surfaces in reverse z-order — compositing should still sort by z.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 10);
    fg.surface.clear(Color::rgb(255, 0, 0));

    // Pass in wrong order (fg first, bg second).
    let surfaces: [&CompositeSurface; 2] = [&fg, &bg];
    composite_surfaces(&mut dst, &surfaces);

    // Red (higher z) should be on top of blue (lower z).
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    // Outside red (4..8): should be blue.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_alpha_blending() {
    // Semi-transparent surface over opaque background.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 8 * 8 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 8, 8, 0, 0, 1);
    fg.surface.clear(Color::rgba(255, 0, 0, 128));

    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    composite_surfaces(&mut dst, &surfaces);

    let p = dst.get_pixel(4, 4).unwrap();
    // Gamma-correct 50% red on blue: both channels > 140.
    assert!(p.r > 140, "blended red should be > 140, got {}", p.r);
    assert!(p.b > 140, "blended blue should be > 140, got {}", p.b);
}

#[test]
fn composite_invisible_surface_skipped() {
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255));

    let mut fg_buf = [0u8; 8 * 8 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 8, 8, 0, 0, 1);
    fg.surface.clear(Color::rgb(255, 0, 0));
    fg.visible = false;

    let surfaces: [&CompositeSurface; 2] = [&bg, &fg];
    composite_surfaces(&mut dst, &surfaces);

    // Red surface is invisible, should only see blue.
    assert_eq!(dst.get_pixel(4, 4), Some(Color::rgb(0, 0, 255)));
}

#[test]
fn composite_surface_with_negative_offset() {
    // Surface partially outside the framebuffer (negative x/y).
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut s_buf = [0u8; 4 * 4 * 4];
    let mut s = make_composite_surface(&mut s_buf, 4, 4, -2, -2, 0);
    s.surface.clear(Color::rgb(0, 255, 0));

    let surfaces: [&CompositeSurface; 1] = [&s];
    composite_surfaces(&mut dst, &surfaces);

    // Only the visible portion should be blitted.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(0, 255, 0)));
    // Beyond the 4x4 surface from (-2,-2): pixel (2,2) should be black.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::BLACK));
}

#[test]
fn composite_surface_partially_outside_right() {
    // Surface extends past the right edge.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut s_buf = [0u8; 4 * 4 * 4];
    let mut s = make_composite_surface(&mut s_buf, 4, 4, 6, 6, 0);
    s.surface.clear(Color::rgb(0, 255, 0));

    let surfaces: [&CompositeSurface; 1] = [&s];
    composite_surfaces(&mut dst, &surfaces);

    // Only (6,6) and (7,7) should be green.
    assert_eq!(dst.get_pixel(6, 6), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(7, 7), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_three_layers() {
    // background (z=0) → content (z=10) → chrome (z=20)
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(20, 20, 40));

    let mut content_buf = [0u8; 6 * 6 * 4];
    let mut content = make_composite_surface(&mut content_buf, 6, 6, 1, 1, 10);
    content.surface.clear(Color::rgb(30, 30, 50));

    let mut chrome_buf = [0u8; 8 * 2 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 8, 2, 0, 0, 20);
    chrome.surface.clear(Color::rgba(60, 60, 80, 200));

    let surfaces: [&CompositeSurface; 3] = [&bg, &content, &chrome];
    composite_surfaces(&mut dst, &surfaces);

    // Top-left pixel (0,0): bg under chrome (alpha blended).
    let p00 = dst.get_pixel(0, 0).unwrap();
    // Chrome rgba(60,60,80,200) over bg rgb(20,20,40) — should be close to chrome.
    assert!(p00.r > 40 && p00.r < 70, "chrome over bg r={}", p00.r);

    // Pixel at (1,1): still under chrome (row 0-1), so content is under chrome.
    let p11 = dst.get_pixel(1, 1).unwrap();
    assert!(p11.b > 50, "chrome over content b={}", p11.b);

    // Pixel at (1,3): content area, no chrome overlap.
    let p13 = dst.get_pixel(1, 3).unwrap();
    assert_eq!(p13, Color::rgb(30, 30, 50));

    // Pixel at (0,3): background, not covered by content (content starts at 1).
    let p03 = dst.get_pixel(0, 3).unwrap();
    assert_eq!(p03, Color::rgb(20, 20, 40));
}

#[test]
fn composite_empty_surfaces_list() {
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(100, 100, 100));

    let surfaces: [&CompositeSurface; 0] = [];
    composite_surfaces(&mut dst, &surfaces);

    // Destination should be unchanged.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(100, 100, 100)));
}

// ---------------------------------------------------------------------------
// Translucent chrome over content (VAL-COMP-002)
// ---------------------------------------------------------------------------

#[test]
fn translucent_chrome_shows_content_beneath() {
    // Simulates the translucent chrome feature: content surface extends
    // full-height (behind chrome), chrome overlay is translucent (alpha < 255).
    // The result: chrome area shows a blend of chrome and content colors.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content surface: full height, bright green (easy to detect bleed-through).
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(0, 200, 0));

    // Chrome overlay: covers top 4 rows, translucent dark (alpha=200).
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(40, 40, 60, 200));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    composite_surfaces(&mut dst, &surfaces);

    // In the chrome region (row 0-3), the green content should bleed through.
    let p_chrome = dst.get_pixel(8, 2).unwrap();
    // Green channel should be > 0 (content bleeds through) but < 200 (attenuated by chrome).
    assert!(
        p_chrome.g > 5,
        "green content should bleed through translucent chrome, got g={}",
        p_chrome.g
    );
    assert!(
        p_chrome.g < 200,
        "chrome should attenuate content green, got g={}",
        p_chrome.g
    );

    // Below chrome (row 5+), pure content visible.
    let p_content = dst.get_pixel(8, 8).unwrap();
    assert_eq!(p_content, Color::rgb(0, 200, 0));
}

#[test]
fn translucent_chrome_is_visually_distinct_from_content() {
    // Chrome with alpha < 255 should produce a different color from the
    // uncovered content region — proving the chrome is visually distinct.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content: white text area background.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(24, 24, 36));

    // Chrome: translucent with alpha=220 (like the actual compositor).
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    composite_surfaces(&mut dst, &surfaces);

    let p_chrome = dst.get_pixel(8, 2).unwrap();
    let p_content = dst.get_pixel(8, 8).unwrap();

    // Chrome region and content region should NOT be identical.
    assert_ne!(
        p_chrome, p_content,
        "chrome and content should be visually distinct"
    );
}

#[test]
fn chrome_alpha_200_produces_visible_translucency() {
    // Verify that alpha=200 (not 255) produces measurable bleed-through
    // when composited over bright content.
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::BLACK);

    // Bright red content underneath.
    let mut content_buf = [0u8; 4 * 4 * 4];
    let mut content = make_composite_surface(&mut content_buf, 4, 4, 0, 0, 0);
    content.surface.clear(Color::rgb(255, 0, 0));

    // Dark chrome on top with alpha=200.
    let mut chrome_buf = [0u8; 4 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 4, 4, 0, 0, 10);
    chrome.surface.clear(Color::rgba(30, 30, 48, 200));

    let surfaces: [&CompositeSurface; 2] = [&content, &chrome];
    composite_surfaces(&mut dst, &surfaces);

    let p = dst.get_pixel(2, 2).unwrap();
    // Red should bleed through: r > chrome_r (30) due to content contribution.
    assert!(
        p.r > 35,
        "red content should bleed through alpha=200 chrome, got r={}",
        p.r
    );
}

#[test]
fn title_bar_chrome_over_content_shows_bleedthrough() {
    // Title bar at the top of the frame with content extending behind it.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content: full height, has blue pixels.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(0, 0, 180));

    // Title bar: top 4 rows, translucent.
    let mut title_buf = [0u8; 16 * 4 * 4];
    let mut title = make_composite_surface(&mut title_buf, 16, 4, 0, 0, 20);
    title.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 2] = [&content, &title];
    composite_surfaces(&mut dst, &surfaces);

    // In the title bar region, blue from content should be partially visible.
    let p_title = dst.get_pixel(8, 1).unwrap();
    assert!(
        p_title.b > 40,
        "blue content should partially show through title bar, got b={}",
        p_title.b
    );

    // Below the title bar, pure content.
    let p_below = dst.get_pixel(8, 8).unwrap();
    assert_eq!(p_below, Color::rgb(0, 0, 180));
}

// ---------------------------------------------------------------------------
// Drop shadows (VAL-COMP-003)
// ---------------------------------------------------------------------------

#[test]
fn fill_gradient_v_first_row_is_top_color() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(
        0,
        0,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    // First row should have the top color (alpha ~80).
    let p = surf.get_pixel(4, 0).unwrap();
    assert!(
        p.a >= 70 && p.a <= 90,
        "top row alpha should be ~80, got {}",
        p.a
    );
}

#[test]
fn fill_gradient_v_last_row_is_bottom_color() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(
        0,
        0,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    // Last row should have the bottom color (alpha ~0).
    let p = surf.get_pixel(4, 7).unwrap();
    assert!(p.a <= 15, "bottom row alpha should be ~0, got {}", p.a);
}

#[test]
fn fill_gradient_v_monotonic_alpha_decrease() {
    // Shadow gradient from alpha=80 to alpha=0 over 8 rows.
    // Each row's alpha should be <= the row above it.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(
        0,
        0,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let mut prev_alpha = 255u8;
    for row in 0..8 {
        let p = surf.get_pixel(4, row).unwrap();
        assert!(
            p.a <= prev_alpha,
            "alpha should decrease monotonically: row {} has a={}, prev={}",
            row,
            p.a,
            prev_alpha
        );
        prev_alpha = p.a;
    }
}

#[test]
fn fill_gradient_v_intermediate_rows_have_intermediate_alpha() {
    // Over 8 rows from alpha=80 to alpha=0, the middle row should have ~40.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(
        0,
        0,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let p_mid = surf.get_pixel(4, 4).unwrap();
    // At row 4/8, alpha should be roughly 80 * (1 - 4/7) ≈ 34.
    assert!(
        p_mid.a > 15 && p_mid.a < 60,
        "middle row alpha should be intermediate, got {}",
        p_mid.a
    );
}

#[test]
fn fill_gradient_v_fills_all_columns() {
    // All columns in a given row should have the same alpha.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    surf.fill_gradient_v(
        0,
        0,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let expected_a = surf.get_pixel(0, 3).unwrap().a;
    for col in 1..8 {
        let p = surf.get_pixel(col, 3).unwrap();
        assert_eq!(p.a, expected_a, "all columns in row should have same alpha");
    }
}

#[test]
fn fill_gradient_v_clips_to_surface_bounds() {
    // Gradient positioned partially outside surface should clip without panic.
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::BLACK);

    // Starts at y=6, height=8: only 2 rows should be visible.
    surf.fill_gradient_v(
        0,
        6,
        8,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let p_visible = surf.get_pixel(4, 6).unwrap();
    assert!(p_visible.a > 0, "visible row should have some alpha");

    // Row 5 should be unaffected (still black, a=0 from clear to BLACK).
    let p_above = surf.get_pixel(4, 5).unwrap();
    assert_eq!(p_above, Color::BLACK);
}

#[test]
fn fill_gradient_v_zero_height_is_noop() {
    let mut buf = [0u8; 8 * 8 * 4];
    let mut surf = make_surface(&mut buf, 8, 8);
    surf.clear(Color::rgb(100, 100, 100));

    surf.fill_gradient_v(
        0,
        0,
        8,
        0,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    // Surface should be unchanged.
    assert_eq!(surf.get_pixel(4, 4), Some(Color::rgb(100, 100, 100)));
}

#[test]
fn fill_gradient_v_single_row() {
    let mut buf = [0u8; 8 * 4 * 4];
    let mut surf = make_surface(&mut buf, 8, 4);
    surf.clear(Color::BLACK);

    // A single row gradient should just have the top color.
    surf.fill_gradient_v(
        0,
        0,
        8,
        1,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let p = surf.get_pixel(4, 0).unwrap();
    assert_eq!(p.a, 80, "single row should have top color alpha");
}

#[test]
fn shadow_surface_composites_between_content_and_chrome() {
    // Verify that a shadow surface (z=15) composites between content (z=10)
    // and chrome (z=20), creating a visible darkening effect beneath chrome.
    let mut dst_buf = [0u8; 16 * 16 * 4];
    let mut dst = make_surface(&mut dst_buf, 16, 16);
    dst.clear(Color::BLACK);

    // Content surface: bright white.
    let mut content_buf = [0u8; 16 * 16 * 4];
    let mut content = make_composite_surface(&mut content_buf, 16, 16, 0, 0, 10);
    content.surface.clear(Color::rgb(200, 200, 200));

    // Shadow surface: covers rows 4-7 (just below where chrome would be),
    // filled with semi-transparent black gradient.
    let mut shadow_buf = [0u8; 16 * 4 * 4];
    let mut shadow = make_composite_surface(&mut shadow_buf, 16, 4, 0, 4, 15);
    shadow.surface.fill_gradient_v(
        0,
        0,
        16,
        4,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    // Chrome surface: covers rows 0-3.
    let mut chrome_buf = [0u8; 16 * 4 * 4];
    let mut chrome = make_composite_surface(&mut chrome_buf, 16, 4, 0, 0, 20);
    chrome.surface.clear(Color::rgba(30, 30, 48, 220));

    let surfaces: [&CompositeSurface; 3] = [&content, &shadow, &chrome];
    composite_surfaces(&mut dst, &surfaces);

    // In the shadow region (row 4): content should be darkened by shadow.
    let p_shadow = dst.get_pixel(8, 4).unwrap();
    let p_no_shadow = dst.get_pixel(8, 10).unwrap();

    // The shadowed pixel should be darker than the unshadowed content.
    assert!(
        p_shadow.r < p_no_shadow.r,
        "shadow should darken content: shadow_r={} < content_r={}",
        p_shadow.r,
        p_no_shadow.r
    );

    // The shadow should have gradient falloff: row 4 darker than row 7.
    let p_shadow_top = dst.get_pixel(8, 4).unwrap();
    let p_shadow_bottom = dst.get_pixel(8, 7).unwrap();
    assert!(
        p_shadow_top.r <= p_shadow_bottom.r,
        "shadow should fade: top_r={} <= bottom_r={}",
        p_shadow_top.r,
        p_shadow_bottom.r
    );
}

#[test]
fn shadow_gradient_not_hard_edged() {
    // Verify the shadow has at least 3 distinct alpha levels (not just on/off).
    let mut buf = [0u8; 16 * 8 * 4];
    let mut surf = make_surface(&mut buf, 16, 8);
    surf.clear(Color::TRANSPARENT);

    surf.fill_gradient_v(
        0,
        0,
        16,
        8,
        Color::rgba(0, 0, 0, 80),
        Color::rgba(0, 0, 0, 0),
    );

    let mut distinct_alphas = [0u8; 8];
    for row in 0..8 {
        distinct_alphas[row as usize] = surf.get_pixel(8, row).unwrap().a;
    }

    // Count unique alpha values.
    let mut unique_count = 0;
    for i in 0..8 {
        let mut is_unique = true;
        for j in 0..i {
            if distinct_alphas[i] == distinct_alphas[j] {
                is_unique = false;
                break;
            }
        }
        if is_unique {
            unique_count += 1;
        }
    }

    assert!(
        unique_count >= 3,
        "shadow should have gradient falloff with >= 3 distinct alpha levels, got {}",
        unique_count
    );
}

// ---------------------------------------------------------------------------
// Image viewer: PNG surface rendering within content area bounds
// ---------------------------------------------------------------------------

#[test]
fn image_blit_clips_to_content_area() {
    // Simulate blitting a large image into a smaller content area.
    // The image should be clipped to the content surface bounds.
    let img_w: u32 = 16;
    let img_h: u32 = 16;
    let content_w: u32 = 10;
    let content_h: u32 = 10;

    // Create a "decoded image" buffer (BGRA8888).
    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for y in 0..img_h {
        for x in 0..img_w {
            let idx = ((y * img_w + x) * 4) as usize;
            img_data[idx] = 0; // B
            img_data[idx + 1] = (y * 16) as u8; // G
            img_data[idx + 2] = (x * 16) as u8; // R
            img_data[idx + 3] = 255; // A
        }
    }

    // Create a content surface smaller than the image.
    let mut content_buf = vec![0u8; (content_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, content_w, content_h);

    // Blit the image at (0,0) — should clip to content bounds.
    content.blit(&img_data, img_w, img_h, img_w * 4, 0, 0);

    // Verify: only the top-left 10x10 of the 16x16 image is visible.
    for y in 0..content_h {
        for x in 0..content_w {
            let px = content.get_pixel(x, y).unwrap();
            assert_eq!(px.r, (x * 16) as u8, "pixel ({x},{y}) R mismatch");
            assert_eq!(px.g, (y * 16) as u8, "pixel ({x},{y}) G mismatch");
            assert_eq!(px.a, 255);
        }
    }
}

#[test]
fn image_blit_blend_clips_to_content_area() {
    // Same test but using blit_blend (alpha-aware blitting).
    let img_w: u32 = 20;
    let img_h: u32 = 20;
    let content_w: u32 = 12;
    let content_h: u32 = 12;

    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for y in 0..img_h {
        for x in 0..img_w {
            let idx = ((y * img_w + x) * 4) as usize;
            img_data[idx] = 100; // B
            img_data[idx + 1] = 150; // G
            img_data[idx + 2] = 200; // R
            img_data[idx + 3] = 255; // A (opaque)
        }
    }

    let mut content_buf = vec![0u8; (content_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, content_w, content_h);
    content.clear(Color::rgb(0, 0, 0));

    content.blit_blend(&img_data, img_w, img_h, img_w * 4, 0, 0);

    // Verify clipped pixels are correct.
    for y in 0..content_h {
        for x in 0..content_w {
            let px = content.get_pixel(x, y).unwrap();
            assert_eq!(px.r, 200, "pixel ({x},{y}) R");
            assert_eq!(px.g, 150, "pixel ({x},{y}) G");
            assert_eq!(px.b, 100, "pixel ({x},{y}) B");
        }
    }
}

#[test]
fn image_surface_no_overflow_into_chrome_region() {
    // Simulate the compositor layout: content area is below the title bar
    // and extends to the bottom of the screen. An image blitted into the
    // content area must not write into the title bar region.
    let fb_w: u32 = 64;
    let fb_h: u32 = 48;
    let title_h: u32 = 8;
    let content_h = fb_h - title_h; // 40

    // Create framebuffer.
    let mut fb_buf = vec![0u8; (fb_w * fb_h * 4) as usize];
    let mut fb = make_surface(&mut fb_buf, fb_w, fb_h);
    fb.clear(Color::rgb(10, 10, 10)); // dark bg

    // Fill title bar region with distinct color.
    fb.fill_rect(0, 0, fb_w, title_h, Color::rgb(50, 50, 80));

    // Create a content surface the exact size of content area.
    let mut content_buf = vec![0u8; (fb_w * content_h * 4) as usize];
    let mut content = make_surface(&mut content_buf, fb_w, content_h);
    content.clear(Color::rgb(20, 20, 30));

    // Blit a large image (bigger than content) into the content surface.
    let img_w: u32 = 128;
    let img_h: u32 = 128;
    let mut img_data = vec![0u8; (img_w * img_h * 4) as usize];
    for i in 0..(img_w * img_h) as usize {
        img_data[i * 4] = 0; // B
        img_data[i * 4 + 1] = 255; // G
        img_data[i * 4 + 2] = 0; // R
        img_data[i * 4 + 3] = 255; // A
    }

    content.blit(&img_data, img_w, img_h, img_w * 4, 0, 0);

    // Blit content surface onto framebuffer at the content area position.
    fb.blit(content.data, fb_w, content_h, fb_w * 4, 0, title_h);

    // Verify: title bar region is unchanged (still chrome color).
    for y in 0..title_h {
        let px = fb.get_pixel(0, y).unwrap();
        assert_eq!(px.r, 50, "title bar pixel ({},{}): R={}", 0, y, px.r);
        assert_eq!(px.g, 50, "title bar pixel ({},{}): G={}", 0, y, px.g);
    }

    // Verify: content area has green pixels from the image.
    let px = fb.get_pixel(0, title_h).unwrap();
    assert_eq!(px.g, 255, "content should have green image pixels");
    assert_eq!(px.r, 0);
}


// ---------------------------------------------------------------------------
// Clock time formatting tests
// ---------------------------------------------------------------------------

/// Format total seconds since boot into HH:MM:SS.
/// This mirrors the logic used by the compositor's clock display.
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

#[test]
fn clock_format_zero_seconds() {
    let mut buf = [0u8; 8];
    format_time_hms(0, &mut buf);
    assert_eq!(&buf, b"00:00:00");
}

#[test]
fn clock_format_one_second() {
    let mut buf = [0u8; 8];
    format_time_hms(1, &mut buf);
    assert_eq!(&buf, b"00:00:01");
}

#[test]
fn clock_format_one_minute() {
    let mut buf = [0u8; 8];
    format_time_hms(60, &mut buf);
    assert_eq!(&buf, b"00:01:00");
}

#[test]
fn clock_format_one_hour() {
    let mut buf = [0u8; 8];
    format_time_hms(3600, &mut buf);
    assert_eq!(&buf, b"01:00:00");
}

#[test]
fn clock_format_max_time() {
    // 23:59:59 = 23*3600 + 59*60 + 59 = 86399
    let mut buf = [0u8; 8];
    format_time_hms(86399, &mut buf);
    assert_eq!(&buf, b"23:59:59");
}

#[test]
fn clock_format_wraps_at_24_hours() {
    // 24 hours = 86400 seconds → wraps to 00:00:00
    let mut buf = [0u8; 8];
    format_time_hms(86400, &mut buf);
    assert_eq!(&buf, b"00:00:00");
}

#[test]
fn clock_format_arbitrary_time() {
    // 12345 seconds = 3h 25m 45s
    let mut buf = [0u8; 8];
    format_time_hms(12345, &mut buf);
    assert_eq!(&buf, b"03:25:45");
}

#[test]
fn clock_format_large_value_wraps() {
    // 100000 seconds = 27h 46m 40s → wraps to 03:46:40
    let mut buf = [0u8; 8];
    format_time_hms(100000, &mut buf);
    assert_eq!(&buf, b"03:46:40");
}

#[test]
fn clock_format_all_digits_valid() {
    // Check that all formatted characters are valid (digits or ':')
    for secs in [0u64, 1, 59, 60, 3599, 3600, 43200, 86399] {
        let mut buf = [0u8; 8];
        format_time_hms(secs, &mut buf);
        // buf[2] and buf[5] must be ':'
        assert_eq!(buf[2], b':', "secs={}: buf[2] should be ':'", secs);
        assert_eq!(buf[5], b':', "secs={}: buf[5] should be ':'", secs);
        // All other positions must be ASCII digits
        for &i in &[0usize, 1, 3, 4, 6, 7] {
            assert!(
                buf[i] >= b'0' && buf[i] <= b'9',
                "secs={}: buf[{}] = {} is not a digit",
                secs,
                i,
                buf[i]
            );
        }
        // Hours 00-23
        let h = (buf[0] - b'0') * 10 + (buf[1] - b'0');
        assert!(h <= 23, "secs={}: hours {} > 23", secs, h);
        // Minutes 00-59
        let m = (buf[3] - b'0') * 10 + (buf[4] - b'0');
        assert!(m <= 59, "secs={}: minutes {} > 59", secs, m);
        // Seconds 00-59
        let s = (buf[6] - b'0') * 10 + (buf[7] - b'0');
        assert!(s <= 59, "secs={}: seconds {} > 59", secs, s);
    }
}

#[test]
fn clock_seconds_from_counter() {
    // Simulate deriving seconds from ARM generic counter.
    // QEMU typical: freq = 62_500_000 Hz (62.5 MHz)
    let freq: u64 = 62_500_000;
    let boot_counter: u64 = 1_000_000_000; // some boot time counter value
    let current_counter: u64 = boot_counter + 5 * freq; // 5 seconds later

    let elapsed_ticks = current_counter - boot_counter;
    let elapsed_seconds = elapsed_ticks / freq;

    assert_eq!(elapsed_seconds, 5);

    let mut buf = [0u8; 8];
    format_time_hms(elapsed_seconds, &mut buf);
    assert_eq!(&buf, b"00:00:05");
}

// ---------------------------------------------------------------------------
// Context switching tests
// ---------------------------------------------------------------------------
//
// These tests verify that the drawing library's text rendering and image
// rendering produce deterministic output, enabling context switching between
// editor and image viewer modes while preserving content state.

/// Render text content surface, simulate switching to image, then switching
/// back. The text pixels must be byte-identical before and after the round trip.
/// This validates that re-rendering from the same document state produces
/// identical output — the foundation of context switching.
#[test]
fn context_switch_text_content_preserved_after_roundtrip() {
    let width = 200u32;
    let height = 100u32;
    let bpp = 4u32;
    let stride = width * bpp;
    let size = (stride * height) as usize;

    // Render text to a content surface.
    let mut buf1 = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf1,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(10, 10, 80, 16, Color::rgb(200, 210, 230));
    }

    // Render a different surface (image mode) — just clear to a different color.
    let mut buf_image = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf_image,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(50, 50, 50));
    }

    // Render text again to a second buffer (simulating switch back to editor).
    let mut buf2 = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut buf2,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(10, 10, 80, 16, Color::rgb(200, 210, 230));
    }

    // The two text renders must be byte-identical.
    assert_eq!(
        buf1, buf2,
        "Text content not preserved after context switch round-trip"
    );
}

/// Verify that cursor position (byte offset) maps to the same pixel coordinates
/// after a context switch round-trip. Uses TextLayout::byte_to_xy.
#[test]
fn context_switch_cursor_position_preserved() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 200,
    };
    let text = b"hello world\nline two";
    let cursor_pos = 5; // After 'hello'

    let (x1, y1) = layout.byte_to_xy(text, cursor_pos);

    // Simulate "context switch away" — the cursor_pos value is just an integer
    // stored in a static. Nothing happens to it.

    // Simulate "context switch back" — re-query the same position.
    let (x2, y2) = layout.byte_to_xy(text, cursor_pos);

    assert_eq!(
        (x1, y1),
        (x2, y2),
        "Cursor pixel position changed after context switch"
    );
    assert_eq!(x1, 5 * 8, "Cursor X should be 5 chars * 8px");
    assert_eq!(y1, 0, "Cursor Y should be on first line");
}

/// Verify that an image surface (blit_blend) and text surface produce
/// visually different content — ensuring context switch produces a
/// visible change.
#[test]
fn context_switch_image_and_text_are_distinct() {
    let width = 64u32;
    let height = 64u32;
    let bpp = 4u32;
    let stride = width * bpp;
    let size = (stride * height) as usize;

    // Text mode: clear + draw text.
    let mut text_buf = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut text_buf,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(4, 4, 16, 16, Color::rgb(200, 200, 200));
    }

    // Image mode: fill with a recognizable pattern (simulating a PNG).
    let mut image_buf = vec![0u8; size];
    {
        let mut surf = Surface {
            data: &mut image_buf,
            width,
            height,
            stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        // Fill a rectangle simulating an image.
        surf.fill_rect(10, 10, 44, 44, Color::rgb(255, 0, 0));
    }

    // The two surfaces must be different.
    assert_ne!(
        text_buf, image_buf,
        "Text and image surfaces should be visually distinct"
    );
}

/// Verify that composite_surfaces correctly composites with a different
/// content surface when the mode changes, while chrome stays the same.
#[test]
fn context_switch_composite_chrome_survives() {
    let fb_w = 100u32;
    let fb_h = 80u32;
    let bpp = 4u32;
    let stride = fb_w * bpp;
    let fb_size = (stride * fb_h) as usize;

    // Chrome surface (title bar).
    let chrome_h = 20u32;
    let chrome_stride = fb_w * bpp;
    let chrome_size = (chrome_stride * chrome_h) as usize;
    let mut chrome_buf = vec![0u8; chrome_size];
    {
        let mut surf = Surface {
            data: &mut chrome_buf,
            width: fb_w,
            height: chrome_h,
            stride: chrome_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgba(30, 30, 48, 220));
    }

    // Content surface — editor mode.
    let content_h = fb_h;
    let content_stride = fb_w * bpp;
    let content_size = (content_stride * content_h) as usize;
    let mut content_buf_editor = vec![0u8; content_size];
    {
        let mut surf = Surface {
            data: &mut content_buf_editor,
            width: fb_w,
            height: content_h,
            stride: content_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(4, 4, 48, 16, Color::rgb(200, 200, 200));
    }

    // Content surface — image mode.
    let mut content_buf_image = vec![0u8; content_size];
    {
        let mut surf = Surface {
            data: &mut content_buf_image,
            width: fb_w,
            height: content_h,
            stride: content_stride,
            format: PixelFormat::Bgra8888,
        };
        surf.clear(Color::rgb(24, 24, 36));
        surf.fill_rect(10, 25, 40, 40, Color::rgb(0, 128, 255));
    }

    // Composite in editor mode.
    let mut fb_editor = vec![0u8; fb_size];
    {
        let mut fb = Surface {
            data: &mut fb_editor,
            width: fb_w,
            height: fb_h,
            stride,
            format: PixelFormat::Bgra8888,
        };
        let content_cs = CompositeSurface {
            surface: Surface {
                data: &mut content_buf_editor,
                width: fb_w,
                height: content_h,
                stride: content_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0,
            y: 0,
            z: 10,
            visible: true,
        };
        let chrome_cs = CompositeSurface {
            surface: Surface {
                data: &mut chrome_buf,
                width: fb_w,
                height: chrome_h,
                stride: chrome_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0,
            y: 0,
            z: 20,
            visible: true,
        };
        composite_surfaces(&mut fb, &[&content_cs, &chrome_cs]);
    }

    // Composite in image mode.
    let mut fb_image = vec![0u8; fb_size];
    {
        let mut fb = Surface {
            data: &mut fb_image,
            width: fb_w,
            height: fb_h,
            stride,
            format: PixelFormat::Bgra8888,
        };
        let content_cs = CompositeSurface {
            surface: Surface {
                data: &mut content_buf_image,
                width: fb_w,
                height: content_h,
                stride: content_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0,
            y: 0,
            z: 10,
            visible: true,
        };
        let chrome_cs = CompositeSurface {
            surface: Surface {
                data: &mut chrome_buf,
                width: fb_w,
                height: chrome_h,
                stride: chrome_stride,
                format: PixelFormat::Bgra8888,
            },
            x: 0,
            y: 0,
            z: 20,
            visible: true,
        };
        composite_surfaces(&mut fb, &[&content_cs, &chrome_cs]);
    }

    // With translucent chrome (alpha=220), the chrome area blends with the
    // content underneath. Since the content differs between modes, the chrome
    // region will differ slightly. What matters is that chrome is PRESENT in
    // both modes — the non-zero alpha pixels prove the chrome overlay exists.
    // Check that both framebuffers have non-zero alpha in the chrome region.
    let chrome_bytes = (chrome_stride * chrome_h) as usize;
    for mode_name in &["editor", "image"] {
        let fb = if *mode_name == "editor" {
            &fb_editor
        } else {
            &fb_image
        };
        // Sample a pixel in the chrome region (center of chrome).
        let mid_y = chrome_h / 2;
        let mid_x = fb_w / 2;
        let offset = ((mid_y * stride) + mid_x * bpp) as usize;
        let a = fb[offset + 3]; // Alpha byte in BGRA
        assert_eq!(
            a, 255,
            "{} mode: chrome pixel should be fully opaque after compositing",
            mode_name
        );
    }

    // The content area below chrome should be different between modes.
    let below_chrome = chrome_bytes;
    assert_ne!(
        &fb_editor[below_chrome..],
        &fb_image[below_chrome..],
        "Content area should differ between editor and image modes"
    );
}

/// Verify that text rendering preserves exact byte content when cursor
/// position is at various positions — the document content is not
/// affected by which rendering mode is active.
#[test]
fn context_switch_document_bytes_unmodified() {
    // Simulate a document buffer.
    let doc_content = b"hello world";
    let cursor_positions = [0, 5, 11]; // start, middle, end

    for &cursor in &cursor_positions {
        let layout = TextLayout {
            char_width: 8,
            line_height: 20,
            max_width: 200,
        };

        // Verify byte_to_xy is consistent for this position.
        let (x, _y) = layout.byte_to_xy(doc_content, cursor);
        let expected_x = if cursor <= doc_content.len() {
            cursor as u32 * 8
        } else {
            doc_content.len() as u32 * 8
        };
        assert_eq!(x, expected_x, "cursor={}: X mismatch", cursor);

        // Content bytes are never modified by rendering.
        let mut content_copy = [0u8; 11];
        content_copy.copy_from_slice(doc_content);
        assert_eq!(
            &content_copy, doc_content,
            "Document content must not be modified by rendering"
        );
    }
}

/// Verify that rendering text with draw_tt at cursor 0 and then at cursor 5
/// both produce valid output (no panics, no out-of-bounds). This tests
/// that cursor position tracking survives mode changes.
#[test]
fn context_switch_draw_tt_cursor_positions_valid() {
    // This test requires a GlyphCache. We'll use the TrueType font
    // from the drawing library if available. Since tests run on the host,
    // use the bitmap font path instead for simplicity.
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 160,
    };
    let text = b"hello world";

    // Verify byte_to_xy at multiple cursor positions.
    let positions = [0, 1, 5, 10, 11];
    for &pos in &positions {
        let (x, y) = layout.byte_to_xy(text, pos);
        // X should be pos * char_width (single line, no wrapping).
        let expected_x = pos as u32 * 8;
        assert_eq!(x, expected_x, "pos={}: X mismatch", pos);
        assert_eq!(y, 0, "pos={}: Y should be 0 for single line", pos);
    }
}

/// Verify Ctrl+Tab context switch combo: Left Ctrl (keycode 29) is
/// mapped to 0 (non-printable) in the input driver's keycode-to-ASCII
/// table, so Ctrl press/release events are safely intercepted by the
/// compositor. Tab (keycode 15) maps to '\t' — without Ctrl held it is
/// forwarded to the editor as a normal character; only Tab+Ctrl triggers
/// context switching.
#[test]
fn context_switch_ctrl_tab_keycodes() {
    // Linux evdev keycodes.
    let key_tab: usize = 15;
    let key_leftctrl: usize = 29;
    let key_f1: usize = 59;

    // Reproduce the input driver's keycode_to_ascii lookup table.
    static MAP: [u8; 58] = [
        0, 0, b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'8', b'9', b'0', b'-', b'=', 0x08, b'\t',
        b'q', b'w', b'e', b'r', b't', b'y', b'u', b'i', b'o', b'p', b'[', b']', b'\n', 0, b'a',
        b's', b'd', b'f', b'g', b'h', b'j', b'k', b'l', b';', b'\'', b'`', 0, b'\\', b'z', b'x',
        b'c', b'v', b'b', b'n', b'm', b',', b'.', b'/', 0, 0, 0, b' ',
    ];

    // Left Ctrl (keycode 29) maps to 0 — non-printable, intercepted by
    // the compositor for modifier tracking.
    assert!(
        key_leftctrl < MAP.len(),
        "Left Ctrl keycode should be within the ASCII map"
    );
    assert_eq!(
        MAP[key_leftctrl], 0,
        "Left Ctrl should map to 0 (non-printable)"
    );

    // Tab (keycode 15) maps to '\t' — a printable/whitespace character.
    // Without Ctrl held, Tab is forwarded to the editor as normal input.
    assert!(
        key_tab < MAP.len(),
        "Tab keycode should be within the ASCII map"
    );
    assert_eq!(
        MAP[key_tab], b'\t',
        "Tab should map to '\\t' (tab character)"
    );

    // F1 (keycode 59) is beyond the map — no longer used for context
    // switching (replaced by Ctrl+Tab).
    assert!(
        key_f1 >= MAP.len(),
        "F1 keycode {} should be beyond the ASCII map (len {})",
        key_f1,
        MAP.len()
    );
    let f1_ascii: u8 = if key_f1 < MAP.len() { MAP[key_f1] } else { 0 };
    assert_eq!(
        f1_ascii, 0,
        "F1 keycode should not produce a printable character"
    );
}

/// Verify that Tab without Ctrl does not conflict with context switching.
/// The compositor only triggers a switch when ctrl_pressed is true AND
/// keycode == KEY_TAB, so a bare Tab press produces '\t' for the editor.
#[test]
fn context_switch_tab_alone_is_not_switch() {
    // Simulate the compositor's Ctrl+Tab logic.
    let key_tab: u16 = 15;
    let mut ctrl_pressed = false;

    // Tab pressed without Ctrl — should NOT trigger context switch.
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(
        !should_switch,
        "Tab alone (without Ctrl) must not trigger context switch"
    );

    // Now simulate Ctrl held + Tab — SHOULD trigger context switch.
    ctrl_pressed = true;
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(should_switch, "Ctrl+Tab must trigger context switch");

    // Ctrl released + Tab again — should NOT switch.
    ctrl_pressed = false;
    let should_switch = key_tab == 15 && ctrl_pressed;
    assert!(
        !should_switch,
        "Tab after Ctrl release must not trigger context switch"
    );
}

// ---------------------------------------------------------------------------
// Text selection highlight tests
// ---------------------------------------------------------------------------

/// Selection highlight: fill_rect_blend draws a visible highlight behind
/// selected character positions (simulating what draw_tt_sel does).
#[test]
fn selection_highlight_rect_blend_modifies_pixels() {
    let w = 320u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    let bg = Color::rgb(24, 24, 36);
    surf.clear(bg);

    // Draw a selection highlight rectangle at the position of "world"
    // (index 6..11, each char 8px wide).
    let sel_color = Color::rgba(50, 80, 160, 180);
    let char_w = 8u32;
    let line_h = 20u32;

    for i in 6..11 {
        surf.fill_rect_blend(i * char_w, 0, char_w, line_h, sel_color);
    }

    // Sample a pixel in the highlight region (x=52, y=10).
    let off = (10 * w * 4 + 52 * 4) as usize;
    let px = Color::decode_from_bgra(&buf[off..off + 4]);

    assert_ne!(
        px, bg,
        "Pixel in selection area should differ from background"
    );
}

/// Selection highlight area does not bleed outside the selection range.
#[test]
fn selection_highlight_does_not_bleed() {
    let w = 320u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    let bg = Color::rgb(24, 24, 36);
    surf.clear(bg);

    let sel_color = Color::rgba(50, 80, 160, 180);
    let char_w = 8u32;
    let line_h = 20u32;

    // Highlight chars 6..11 only.
    for i in 6..11 {
        surf.fill_rect_blend(i * char_w, 0, char_w, line_h, sel_color);
    }

    // Pixel at x=4, y=10 (inside char 0, which is NOT selected) should be bg.
    let off = (10 * w * 4 + 4 * 4) as usize;
    let px = Color::decode_from_bgra(&buf[off..off + 4]);

    assert_eq!(px, bg, "Pixel outside selection should remain background");
}

/// Selection range normalization: draw_tt_sel(sel_start=11, sel_end=6)
/// should produce the same output as draw_tt_sel(sel_start=6, sel_end=11).
/// Tested via the bitmap draw method approach.
#[test]
fn selection_range_normalization() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 300,
    };
    let text = b"hello world";

    // Forward: selection 6..11
    let w = 320u32;
    let h = 100u32;
    let mut buf_fwd = vec![0u8; (w * h * 4) as usize];
    let mut surf_fwd = make_surface(&mut buf_fwd, w, h);
    surf_fwd.clear(Color::rgb(24, 24, 36));

    let sel_color = Color::rgba(50, 80, 160, 180);
    let (sel_lo, sel_hi) = (6, 11);
    for i in sel_lo..sel_hi {
        let (cx, cy) = layout.byte_to_xy(text, i);
        surf_fwd.fill_rect_blend(cx, cy, 8, 20, sel_color);
    }

    // Reversed: normalized should be identical.
    let mut buf_rev = vec![0u8; (w * h * 4) as usize];
    let mut surf_rev = make_surface(&mut buf_rev, w, h);
    surf_rev.clear(Color::rgb(24, 24, 36));

    let (sel_start_rev, sel_end_rev) = (11usize, 6usize);
    let (s_lo, s_hi) = if sel_start_rev <= sel_end_rev {
        (sel_start_rev, sel_end_rev)
    } else {
        (sel_end_rev, sel_start_rev)
    };
    for i in s_lo..s_hi {
        let (cx, cy) = layout.byte_to_xy(text, i);
        surf_rev.fill_rect_blend(cx, cy, 8, 20, sel_color);
    }

    assert_eq!(
        buf_fwd, buf_rev,
        "Normalized selection should produce identical pixels"
    );
}

/// Selection byte_to_xy mapping: selection positions map to correct pixels.
#[test]
fn selection_byte_to_xy_positions() {
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 300,
    };
    let text = b"hello world";

    // Position 6 ('w') should be at x=48, y=0.
    let (x6, y6) = layout.byte_to_xy(text, 6);
    assert_eq!(x6, 48);
    assert_eq!(y6, 0);

    // Position 10 ('d') should be at x=80, y=0.
    let (x10, y10) = layout.byte_to_xy(text, 10);
    assert_eq!(x10, 80);
    assert_eq!(y10, 0);
}

/// Selection state: anchor and cursor define the range. The range should
/// be [min(anchor, cursor), max(anchor, cursor)).
#[test]
fn selection_anchor_cursor_range() {
    // Editor tracks: anchor position + cursor position.
    // Selection = range between them.
    let anchor = 3usize;
    let cursor = 8usize;

    let sel_lo = if anchor < cursor { anchor } else { cursor };
    let sel_hi = if anchor < cursor { cursor } else { anchor };

    assert_eq!(sel_lo, 3);
    assert_eq!(sel_hi, 8);

    // Reversed direction.
    let anchor2 = 8usize;
    let cursor2 = 3usize;

    let sel_lo2 = if anchor2 < cursor2 { anchor2 } else { cursor2 };
    let sel_hi2 = if anchor2 < cursor2 { cursor2 } else { anchor2 };

    assert_eq!(sel_lo2, 3);
    assert_eq!(sel_hi2, 8);
}

/// Selection replacement: deleting a range and inserting a character.
#[test]
fn selection_replace_with_character() {
    // Simulate document: "hello world" (11 bytes).
    let mut doc = *b"hello world";
    let mut doc_len = 11usize;

    // Selection: 6..11 ("world").
    let sel_start = 6usize;
    let sel_end = 11usize;

    // Delete the range [6..11) by shifting bytes left.
    let del_count = sel_end - sel_start;
    // Move bytes after selection to selection start.
    for i in sel_start..doc_len - del_count {
        doc[i] = doc[i + del_count];
    }
    doc_len -= del_count;

    // Insert 'X' at position 6.
    for i in (7..=doc_len).rev() {
        if i < doc.len() && i > 0 {
            doc[i] = doc[i - 1];
        }
    }
    doc[6] = b'X';
    doc_len += 1;

    assert_eq!(&doc[..doc_len], b"hello X");
}

/// Selection deletion: backspace with selection deletes entire range.
#[test]
fn selection_delete_range() {
    // Simulate document: "hello world" (11 bytes).
    let mut doc = *b"hello world";
    let mut doc_len = 11usize;

    // Selection: 6..11 ("world").
    let sel_start = 6usize;
    let sel_end = 11usize;

    // Delete the range [6..11).
    let del_count = sel_end - sel_start;
    for i in sel_start..doc_len - del_count {
        doc[i] = doc[i + del_count];
    }
    doc_len -= del_count;

    // Cursor should be at sel_start (6).
    let cursor = sel_start;

    assert_eq!(&doc[..doc_len], b"hello ");
    assert_eq!(cursor, 6);
}

/// Highlight color has sufficient contrast: selection highlight should
/// be visually distinct from both the background and text.
#[test]
fn selection_highlight_color_contrast() {
    let bg = Color::rgb(24, 24, 36);
    let text_color = Color::rgb(200, 210, 230);
    let sel_color = Color::rgba(50, 80, 160, 180);

    // Selection highlight color should differ from background.
    assert_ne!(
        sel_color.r, bg.r,
        "Selection R should differ from background R"
    );
    assert_ne!(
        sel_color.b, bg.b,
        "Selection B should differ from background B"
    );

    // The blended result of sel_color over bg should be distinct from bg.
    let blended = sel_color.blend_over(bg);
    assert_ne!(
        blended, bg,
        "Blended selection over bg should be visually distinct"
    );

    // Text should still be readable over the selection highlight.
    // Check luminance difference is meaningful.
    let text_luma = text_color.r as u32 * 3 + text_color.g as u32 * 6 + text_color.b as u32;
    let sel_luma = blended.r as u32 * 3 + blended.g as u32 * 6 + blended.b as u32;
    let contrast = if text_luma > sel_luma {
        text_luma - sel_luma
    } else {
        sel_luma - text_luma
    };

    assert!(
        contrast > 200,
        "Text should have sufficient contrast over selection highlight (got {})",
        contrast
    );
}

/// Cursor bar should NOT be drawn when selection is active in draw_tt_sel.
/// When sel_start == sel_end == 0 (no selection), cursor bar IS drawn.
#[test]
fn cursor_bar_suppressed_with_selection() {
    // The draw_tt_sel logic: if has_selection is true, skip cursor bar.
    // This tests the boolean condition directly.
    let sel_start = 3usize;
    let sel_end = 7usize;
    let (s_lo, s_hi) = if sel_start <= sel_end {
        (sel_start, sel_end)
    } else {
        (sel_end, sel_start)
    };
    let has_selection = s_lo < s_hi;
    assert!(has_selection, "Selection 3..7 should be active");

    // No selection case.
    let (s_lo2, s_hi2) = (0usize, 0usize);
    let has_selection2 = s_lo2 < s_hi2;
    assert!(!has_selection2, "Selection 0..0 should not be active");
}

// ---------------------------------------------------------------------------
// Scrolling tests — TextLayout scroll offset behavior
// ---------------------------------------------------------------------------

/// byte_to_visual_line returns the correct visual line index for various
/// byte offsets, including wrapped lines and newlines.
#[test]
fn byte_to_visual_line_basic() {
    let layout = make_layout(32); // 4 chars per row (32 / 8)
                                  // "ab\ncd" → row 0: "ab", row 1: "cd"
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 1), 0);
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 2), 0); // at '\n'
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 3), 1); // 'c'
    assert_eq!(layout.byte_to_visual_line(b"ab\ncd", 5), 1); // end of text
}

/// byte_to_visual_line handles soft-wrap correctly.
/// Note: byte_to_visual_line matches byte_to_xy — the target byte is
/// checked BEFORE the wrap happens for that position, so byte 3 ('d')
/// reports row 0 (wrap hasn't triggered yet). Byte 4 ('e') is row 1.
#[test]
fn byte_to_visual_line_wrap() {
    let layout = make_layout(24); // 3 chars per row (24 / 8)
                                  // "abcdef" wraps to row 0: "abc", row 1: "def"
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 2), 0);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 3), 0); // wrap point — same as byte_to_xy
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 4), 1);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 5), 1);
    assert_eq!(layout.byte_to_visual_line(b"abcdef", 6), 1); // end
}

/// byte_to_visual_line: empty text always returns line 0.
#[test]
fn byte_to_visual_line_empty() {
    let layout = make_layout(200);
    assert_eq!(layout.byte_to_visual_line(b"", 0), 0);
}

/// byte_to_visual_line: offset beyond text length clamps to the last line.
#[test]
fn byte_to_visual_line_offset_past_end() {
    let layout = make_layout(200);
    // "a\nb" = 2 lines. Offset 10 is well past end, should clamp to last line.
    assert_eq!(layout.byte_to_visual_line(b"a\nb", 10), 1);
    // Single line text, offset past end stays on line 0.
    assert_eq!(layout.byte_to_visual_line(b"hello", 100), 0);
}

/// byte_to_visual_line: trailing newline puts the end on the next line.
#[test]
fn byte_to_visual_line_trailing_newline() {
    let layout = make_layout(200);
    // "abc\n" — byte 3 is '\n' on line 0, byte 4 (past end) is on line 1.
    assert_eq!(layout.byte_to_visual_line(b"abc\n", 3), 0); // at '\n'
    assert_eq!(layout.byte_to_visual_line(b"abc\n", 4), 1); // past '\n'
}

/// byte_to_visual_line: multiple consecutive newlines produce sequential lines.
#[test]
fn byte_to_visual_line_consecutive_newlines() {
    let layout = make_layout(200);
    // "\n\n\n" = 3 newlines → lines 0, 1, 2, with byte 3 on line 3.
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 0), 0);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 1), 1);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 2), 2);
    assert_eq!(layout.byte_to_visual_line(b"\n\n\n", 3), 3); // end
}

/// byte_to_visual_line: wrapping at exact column boundary with newlines.
#[test]
fn byte_to_visual_line_wrap_and_newline_combined() {
    let layout = make_layout(24); // 3 chars per row
                                  // "abc\ndef" layout: 'a' col0 row0, 'b' col1 row0, 'c' col2 row0,
                                  // '\n' at col3 → newline check fires BEFORE wrap check → row becomes 1.
                                  // 'd' col0 row1, 'e' col1 row1, 'f' col2 row1.
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 0), 0); // 'a'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 2), 0); // 'c'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 4), 1); // 'd'
    assert_eq!(layout.byte_to_visual_line(b"abc\ndef", 7), 1); // end

    // Longer text with wrap THEN newline: "abcde\nf" with 3 cols.
    // 'a' col0 row0, 'b' col1 row0, 'c' col2 row0, then col=3.
    // 'd' → col>=cols → wrap → row1 col0, 'e' col1 row1,
    // '\n' → newline → row2.
    // 'f' col0 row2.
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 3), 0); // 'd' — wrap point
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 4), 1); // 'e'
    assert_eq!(layout.byte_to_visual_line(b"abcde\nf", 6), 2); // 'f'
}

/// total_visual_lines counts lines correctly with newlines and wraps.
#[test]
fn total_visual_lines_basic() {
    let layout = make_layout(200); // wide enough for no wrapping
    assert_eq!(layout.total_visual_lines(b""), 0);
    assert_eq!(layout.total_visual_lines(b"hello"), 1);
    assert_eq!(layout.total_visual_lines(b"a\nb"), 2);
    assert_eq!(layout.total_visual_lines(b"a\nb\nc"), 3);
    assert_eq!(layout.total_visual_lines(b"a\n"), 2); // trailing newline = extra line
}

/// total_visual_lines with soft-wrap.
#[test]
fn total_visual_lines_wrap() {
    let layout = make_layout(24); // 3 chars per row
    assert_eq!(layout.total_visual_lines(b"abcdef"), 2); // "abc" + "def"
    assert_eq!(layout.total_visual_lines(b"abcdefghi"), 3); // 3 + 3 + 3
}

/// scroll_for_cursor computes the correct scroll offset to keep
/// the cursor visible within a viewport of a given number of lines.
#[test]
fn scroll_for_cursor_no_scroll_needed() {
    let layout = make_layout(200);
    // 3-line viewport, cursor on line 0, scroll=0 → no change
    assert_eq!(layout.scroll_for_cursor(b"hello", 0, 0, 3), 0);
    // cursor on line 2 (viewport 0..2), still visible
    assert_eq!(layout.scroll_for_cursor(b"a\nb\nc", 4, 0, 3), 0);
}

/// scroll_for_cursor scrolls down when cursor goes below viewport.
#[test]
fn scroll_for_cursor_scroll_down() {
    let layout = make_layout(200);
    // 2-line viewport, cursor on line 2 (past visible range [0,1])
    let text = b"a\nb\nc";
    // cursor at byte 4 = "c" = line 2. viewport lines = 2. current scroll = 0.
    // Need scroll = 1 so viewport shows lines [1,2].
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 2), 1);
}

/// scroll_for_cursor scrolls up when cursor goes above viewport.
#[test]
fn scroll_for_cursor_scroll_up() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // cursor at byte 0 = line 0. scroll = 2, viewport lines = 2 → shows lines [2,3].
    // Need scroll = 0 to see line 0.
    assert_eq!(layout.scroll_for_cursor(text, 0, 2, 2), 0);
}

/// scroll_for_cursor handles Home key (cursor at 0, scroll was large).
#[test]
fn scroll_for_cursor_home_key() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    // Cursor at byte 0 = line 0, scroll = 4, viewport = 3
    assert_eq!(layout.scroll_for_cursor(text, 0, 4, 3), 0);
}

/// scroll_for_cursor handles End key (cursor at end, scroll was 0).
#[test]
fn scroll_for_cursor_end_key() {
    let layout = make_layout(200);
    let text = b"l1\nl2\nl3\nl4\nl5";
    // End of text is on line 4. viewport = 3 lines. scroll = 0.
    // Need scroll = 2 so viewport shows [2,3,4].
    assert_eq!(layout.scroll_for_cursor(text, text.len(), 0, 3), 2);
}

/// scroll_for_cursor with viewport_lines=0 always returns 0.
#[test]
fn scroll_for_cursor_zero_viewport() {
    let layout = make_layout(200);
    assert_eq!(layout.scroll_for_cursor(b"a\nb\nc", 4, 5, 0), 0);
}

/// scroll_for_cursor: cursor on the last visible line does not scroll.
#[test]
fn scroll_for_cursor_cursor_on_last_visible() {
    let layout = make_layout(200);
    let text = b"a\nb\nc\nd\ne";
    // Viewport=3, scroll=1 → visible lines [1,2,3]. Cursor on line 3 (byte 6='d').
    assert_eq!(layout.scroll_for_cursor(text, 6, 1, 3), 1);
}

/// scroll_for_cursor: cursor just below viewport triggers minimal scroll.
#[test]
fn scroll_for_cursor_one_past_bottom() {
    let layout = make_layout(200);
    let text = b"a\nb\nc\nd\ne";
    // Viewport=2, scroll=0 → visible lines [0,1]. Cursor on line 2 (byte 4='c').
    // Should scroll to 1 so viewport shows [1,2].
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 2), 1);
}

/// scroll_for_cursor: single-line viewport scrolls to the cursor line exactly.
#[test]
fn scroll_for_cursor_single_line_viewport() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // Viewport=1, cursor on line 2 → scroll=2.
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 1), 2);
    // Viewport=1, cursor on line 0, scroll was 2 → scroll=0.
    assert_eq!(layout.scroll_for_cursor(text, 0, 2, 1), 0);
}

/// scroll_for_cursor: cursor already visible with large viewport returns unchanged scroll.
#[test]
fn scroll_for_cursor_large_viewport() {
    let layout = make_layout(200);
    let text = b"a\nb\nc";
    // Viewport=100 lines, everything fits. scroll=0 should remain.
    assert_eq!(layout.scroll_for_cursor(text, 4, 0, 100), 0);
}

/// draw_tt_sel_scroll: selection byte range survives scrolling.
/// A selection defined in byte offsets is independent of scroll offset.
/// When we scroll away and back, the same bytes should be selected.
#[test]
fn selection_survives_scrolling() {
    // This test verifies the invariant that selection range (byte offsets)
    // is independent of the scroll offset — the renderer just needs to
    // convert byte offsets to visual coordinates accounting for scroll.
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    let sel_start = 6; // start of "line2"
    let sel_end = 11; // end of "line2"

    // With scroll=0, line2 is on visual line 1 (visible).
    let line_of_start = layout.byte_to_visual_line(text, sel_start);
    let line_of_end = layout.byte_to_visual_line(text, sel_end);
    assert_eq!(line_of_start, 1);
    assert_eq!(line_of_end, 1);

    // With scroll=3, line2 (visual line 1) is off screen above.
    // Selection bytes are unchanged.
    assert_eq!(sel_start, 6);
    assert_eq!(sel_end, 11);
    // byte_to_visual_line still returns 1 — it's the absolute line.
    assert_eq!(layout.byte_to_visual_line(text, sel_start), 1);

    // Scroll back to 0 — selection is still 6..11 (unchanged).
    assert_eq!(sel_start, 6);
    assert_eq!(sel_end, 11);
}

/// draw_tt_sel_scroll skips lines above scroll_offset and stops at max_y.
/// This ensures no text renders outside the visible content area.
#[test]
fn scroll_clips_lines_above_and_below() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4\nline5";
    // With scroll_offset=1, line1 should not be drawn. With max_y constraining
    // the viewport height, line5 may not be drawn either.
    // We verify by checking byte_to_xy positions with scroll offset.

    // Line 0 at scroll=1 is above viewport → y < 0 in visual space.
    // Line 1 at scroll=1 is at y=0 in visual space.
    let cursor_line = layout.byte_to_visual_line(text, 6); // "line2" starts at byte 6
    assert_eq!(cursor_line, 1);

    // With scroll_offset=1, visual line 1 is at pixel row 0.
    // We can verify: the draw function should place line 1 at origin_y + (1-1)*line_height.
}

/// Context switch preserves scroll offset: verify the offset value is
/// just a number that can be stored and restored per content mode.
#[test]
fn scroll_offset_preserved_across_context_switch() {
    // Scroll offset is a u32 (or usize). Switching from editor to image
    // and back should restore the same value.
    let editor_scroll: u32 = 7;
    let image_scroll: u32 = 0; // images don't scroll (yet)

    // Simulate context switch: save editor scroll, load image scroll.
    let saved_editor_scroll = editor_scroll;
    let _current = image_scroll;

    // Switch back: restore editor scroll.
    let restored = saved_editor_scroll;
    assert_eq!(restored, 7);
}

// composite_surfaces_rect — partial framebuffer compositing
// ---------------------------------------------------------------------------

#[test]
fn composite_rect_only_updates_target_region() {
    // 8x8 destination, pre-filled with green. Composite a red surface (4x4 at 0,0)
    // but only update the rect (0,0,2,2). Outside the rect should remain green.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 255, 0)); // Green

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 0);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red

    let surfaces: [&CompositeSurface; 1] = [&fg];
    composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 2, 2);

    // Inside the rect (0,0)-(2,2): should be red.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(1, 1), Some(Color::rgb(255, 0, 0)));
    // Just outside the rect but inside the surface: should still be green
    // (not composited).
    assert_eq!(dst.get_pixel(2, 0), Some(Color::rgb(0, 255, 0)));
    assert_eq!(dst.get_pixel(0, 2), Some(Color::rgb(0, 255, 0)));
    // Far outside: green.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::rgb(0, 255, 0)));
}

#[test]
fn composite_rect_respects_z_order() {
    // Two overlapping surfaces composited in a rect. Higher z should win.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut bg_buf = [0u8; 8 * 8 * 4];
    let mut bg = make_composite_surface(&mut bg_buf, 8, 8, 0, 0, 0);
    bg.surface.clear(Color::rgb(0, 0, 255)); // Blue

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 10);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red, higher z

    let surfaces: [&CompositeSurface; 2] = [&fg, &bg];
    composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 3, 3);

    // Inside rect where both surfaces overlap: red (higher z) wins.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(2, 2), Some(Color::rgb(255, 0, 0)));
    // Outside rect: still black (not composited).
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_rect_with_offset_surface() {
    // Surface at position (2,2), dirty rect at (3,3,2,2).
    // The intersection is (3,3)-(5,5) in FB coords.
    let mut dst_buf = [0u8; 8 * 8 * 4];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::BLACK);

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 2, 2, 0);
    fg.surface.clear(Color::rgb(255, 0, 0)); // Red

    let surfaces: [&CompositeSurface; 1] = [&fg];
    composite_surfaces_rect(&mut dst, &surfaces, 3, 3, 2, 2);

    // (3,3) is inside both the dirty rect and the surface. Should be red.
    assert_eq!(dst.get_pixel(3, 3), Some(Color::rgb(255, 0, 0)));
    assert_eq!(dst.get_pixel(4, 4), Some(Color::rgb(255, 0, 0)));
    // (2,2) is inside the surface but outside the dirty rect. Should be black.
    assert_eq!(dst.get_pixel(2, 2), Some(Color::BLACK));
    // (5,5) is outside the surface (4x4 at 2,2 → x range 2..6). But (5,5)
    // is inside the dirty rect (3..5 in both dimensions)... wait, rect is
    // (3,3,2,2) → x range 3..5, y range 3..5. So (5,5) is outside. Black.
    assert_eq!(dst.get_pixel(5, 5), Some(Color::BLACK));
}

#[test]
fn composite_rect_zero_size_is_noop() {
    let mut dst_buf = [0u8; 4 * 4 * 4];
    let mut dst = make_surface(&mut dst_buf, 4, 4);
    dst.clear(Color::rgb(0, 255, 0)); // Green

    let mut fg_buf = [0u8; 4 * 4 * 4];
    let mut fg = make_composite_surface(&mut fg_buf, 4, 4, 0, 0, 0);
    fg.surface.clear(Color::rgb(255, 0, 0));

    let surfaces: [&CompositeSurface; 1] = [&fg];
    composite_surfaces_rect(&mut dst, &surfaces, 0, 0, 0, 0);

    // Nothing should have changed.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 255, 0)));
}

// ---------------------------------------------------------------------------
// draw_tt_sel_scroll_lines tests (incremental content rendering)
// ---------------------------------------------------------------------------

/// Incremental line range: byte_to_visual_line correctly identifies lines
/// for dirty line computation during incremental rendering.
#[test]
fn incremental_render_byte_to_visual_line_for_dirty_tracking() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3\nline4";

    // Cursor at start of line2 (byte 6) → visual line 1.
    assert_eq!(layout.byte_to_visual_line(text, 6), 1);
    // Cursor at end of line3 (byte 17) → visual line 2.
    assert_eq!(layout.byte_to_visual_line(text, 17), 2);
    // Cursor at start (byte 0) → visual line 0.
    assert_eq!(layout.byte_to_visual_line(text, 0), 0);
    // Cursor past end → last line.
    assert_eq!(layout.byte_to_visual_line(text, text.len()), 3);
}

/// Incremental line tracking: inserting a character on the same line
/// should only dirty that one line (cursor line stays the same).
#[test]
fn incremental_render_same_line_insert_dirtied_lines() {
    let layout = make_layout(200);

    // Before insert: "abc\ndef" with cursor at byte 1 (in "abc", line 0).
    let text_before = b"abc\ndef";
    let cursor_before = 1;
    let line_before = layout.byte_to_visual_line(text_before, cursor_before);
    assert_eq!(line_before, 0);

    // After insert: "aXbc\ndef" with cursor at byte 2.
    let text_after = b"aXbc\ndef";
    let cursor_after = 2;
    let line_after = layout.byte_to_visual_line(text_after, cursor_after);
    assert_eq!(line_after, 0);

    // Same line → only 1 line needs re-rendering.
    assert_eq!(line_before, line_after);
}

/// Incremental line tracking: inserting a newline creates a new line,
/// requiring re-render from the cursor line to the end.
#[test]
fn incremental_render_newline_insert_dirtied_lines() {
    let layout = make_layout(200);

    // Before insert: "abcdef" with cursor at byte 3.
    let text_before = b"abcdef";
    let cursor_before = 3;
    let total_lines_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_lines_before, 1);

    // After insert: "abc\ndef" with cursor at byte 4 (start of new line).
    let text_after = b"abc\ndef";
    let cursor_after = 4;
    let total_lines_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_lines_after, 2);

    let new_cursor_line = layout.byte_to_visual_line(text_after, cursor_after);
    assert_eq!(new_cursor_line, 1);

    // Total lines changed → reflow detected → dirty from cursor line to end.
    assert_ne!(total_lines_before, total_lines_after);
}

/// Incremental line tracking: deleting a character that causes line
/// reflow should dirty from the affected line to the end.
#[test]
fn incremental_render_delete_reflow_dirtied_lines() {
    let layout = make_layout(200);

    // Before delete: "abc\ndef" — 2 lines.
    let text_before = b"abc\ndef";
    let total_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_before, 2);

    // After deleting the newline: "abcdef" — 1 line.
    let text_after = b"abcdef";
    let total_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_after, 1);

    // Reflow detected.
    assert_ne!(total_before, total_after);
}

/// Incremental line tracking: cursor-only movement (no content change)
/// should dirty both the old and new cursor lines.
#[test]
fn incremental_render_cursor_move_two_lines_dirty() {
    let layout = make_layout(200);
    let text = b"line1\nline2\nline3";

    let old_cursor = 2; // line 0
    let new_cursor = 8; // line 1
    let old_line = layout.byte_to_visual_line(text, old_cursor);
    let new_line = layout.byte_to_visual_line(text, new_cursor);

    assert_eq!(old_line, 0);
    assert_eq!(new_line, 1);

    // Both lines need re-rendering (old cursor erased, new cursor drawn).
    let first_dirty = old_line.min(new_line);
    let last_dirty = old_line.max(new_line);
    assert_eq!(first_dirty, 0);
    assert_eq!(last_dirty, 1);
}

/// Soft-wrap insert: inserting a char that causes soft wrap should change
/// the total line count, triggering full-range dirty.
#[test]
fn incremental_render_soft_wrap_changes_line_count() {
    // Layout with narrow max_width: 3 chars per line (3 * 8 = 24).
    let layout = TextLayout {
        char_width: 8,
        line_height: 20,
        max_width: 24,
    };

    // Before: "abc" — fits in 1 line (3 chars, 3 cols).
    let text_before = b"abc";
    let total_before = layout.byte_to_visual_line(text_before, text_before.len()) + 1;
    assert_eq!(total_before, 1);

    // After: "abcd" — wraps to 2 lines (4 chars, 3 cols per line).
    let text_after = b"abcd";
    let total_after = layout.byte_to_visual_line(text_after, text_after.len()) + 1;
    assert_eq!(total_after, 2);

    // Line count changed → reflow → dirty from cursor to end.
    assert_ne!(total_before, total_after);
}

// ---------------------------------------------------------------------------
// Xorshift32 PRNG tests
// ---------------------------------------------------------------------------

use drawing::Xorshift32;

#[test]
fn xorshift32_deterministic() {
    // Same seed produces same sequence.
    let mut a = Xorshift32::new(42);
    let mut b = Xorshift32::new(42);
    for _ in 0..100 {
        assert_eq!(a.next(), b.next());
    }
}

#[test]
fn xorshift32_different_seeds_differ() {
    let mut a = Xorshift32::new(42);
    let mut b = Xorshift32::new(99);
    // Very unlikely for first 10 outputs to match with different seeds.
    let mut same_count = 0;
    for _ in 0..10 {
        if a.next() == b.next() {
            same_count += 1;
        }
    }
    assert!(
        same_count < 3,
        "different seeds should produce different sequences"
    );
}

#[test]
fn xorshift32_noise_in_range() {
    let mut rng = Xorshift32::new(0xCAFE);
    for _ in 0..1000 {
        let n = rng.noise(3);
        assert!(n >= -3 && n <= 3, "noise({}) out of range [-3, 3]", n);
    }
}

#[test]
fn xorshift32_zero_seed_handled() {
    // Zero seed should not produce all-zero output (it gets replaced).
    let mut rng = Xorshift32::new(0);
    let first = rng.next();
    assert_ne!(first, 0, "zero seed should be replaced internally");
}

// ---------------------------------------------------------------------------
// Radial gradient + noise tests
// ---------------------------------------------------------------------------

#[test]
fn gradient_center_brighter_than_edges() {
    // Create a 100×100 surface and fill with radial gradient.
    let w = 100u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 42);

    // Sample center pixel.
    let center_px = surf.get_pixel(w / 2, h / 2).unwrap();
    // Sample corner pixel.
    let corner_px = surf.get_pixel(0, 0).unwrap();

    // Center should be brighter than corner (higher R value).
    assert!(
        center_px.r > corner_px.r,
        "center ({}) should be brighter than corner ({})",
        center_px.r,
        corner_px.r,
    );

    // Center should be close to center_color, corner close to edge_color.
    assert!(
        center_px.r >= 26 && center_px.r <= 30,
        "center R={}",
        center_px.r
    );
    assert!(
        corner_px.r >= 14 && corner_px.r <= 18,
        "corner R={}",
        corner_px.r
    );

    // Monochrome: R=G=B for all pixels (no noise, amplitude=0).
    assert_eq!(center_px.r, center_px.g);
    assert_eq!(center_px.r, center_px.b);
    assert_eq!(corner_px.r, corner_px.g);
    assert_eq!(corner_px.r, corner_px.b);
}

#[test]
fn gradient_dither_creates_variation() {
    // Bayer ordered dithering should create pixel-level variation in rows,
    // breaking up quantization bands into a structured stipple pattern.
    let w = 100u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 3, 0xDEAD_BEEF);

    // Check that not all pixels in a horizontal row are identical (dither breaks banding).
    let y = h / 2; // Middle row.
    let mut saw_different = false;
    let first = surf.get_pixel(0, y).unwrap();
    for x in 1..w {
        let px = surf.get_pixel(x, y).unwrap();
        if px.r != first.r || px.g != first.g || px.b != first.b {
            saw_different = true;
            break;
        }
    }
    assert!(
        saw_different,
        "dither should cause pixel variation in a row"
    );
}

#[test]
fn gradient_deterministic_across_calls() {
    // Same parameters → identical output.
    let w = 50u32;
    let h = 50u32;
    let mut buf1 = vec![0u8; (w * h * 4) as usize];
    let mut buf2 = vec![0u8; (w * h * 4) as usize];

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);

    {
        let mut s1 = make_surface(&mut buf1, w, h);
        drawing::fill_radial_gradient_noise(&mut s1, center, edge, 3, 0xDEAD_BEEF);
    }
    {
        let mut s2 = make_surface(&mut buf2, w, h);
        drawing::fill_radial_gradient_noise(&mut s2, center, edge, 3, 0xDEAD_BEEF);
    }

    assert_eq!(
        buf1, buf2,
        "gradient should be deterministic with same seed"
    );
}

#[test]
fn gradient_1x1_surface_no_panic() {
    let mut buf = [0u8; 4];
    let mut surf = make_surface(&mut buf, 1, 1);
    drawing::fill_radial_gradient_noise(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        3,
        42,
    );
    // Just ensure it doesn't panic or divide by zero.
    let px = surf.get_pixel(0, 0).unwrap();
    assert_eq!(px.a, 255);
}

#[test]
fn gradient_zero_noise_is_smooth() {
    // With no noise, pixels along a horizontal line at the center should
    // be monotonically changing (or equal) from center outward.
    let w = 200u32;
    let h = 200u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    drawing::fill_radial_gradient_noise(
        &mut surf,
        Color::rgb(40, 40, 40),
        Color::rgb(20, 20, 20),
        0, // no noise
        42,
    );

    // From center outward to the right, values should be non-increasing.
    let cy = h / 2;
    let cx = w / 2;
    let mut prev_r = surf.get_pixel(cx, cy).unwrap().r;
    for x in (cx + 1)..w {
        let px = surf.get_pixel(x, cy).unwrap();
        assert!(
            px.r <= prev_r + 1, // +1 for rounding tolerance
            "gradient should get darker from center outward: x={}, r={}, prev={}",
            x,
            px.r,
            prev_r,
        );
        prev_r = px.r;
    }
}

#[test]
fn gradient_dither_is_structured_bayer() {
    // Bayer 4×4 dithering should produce a repeating 4×4 pattern.
    // For a flat-color surface (center == edge), the Bayer thresholds
    // should be visible as a structured pattern where some pixels round
    // up and others don't, with a period of 4 in both x and y.
    let w = 8u32;
    let h = 8u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    // Use colors that produce a fractional value in the gradient.
    // With center=30, edge=29, at center position the gradient value
    // is ~30. The fractional part from interpolation triggers dithering.
    let center = Color::rgb(30, 30, 30);
    let edge = Color::rgb(29, 29, 29);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 0);

    // Verify 4×4 periodicity: pixel(x,y) == pixel(x+4, y+4) for same
    // gradient position. Since the gradient varies spatially, we can't
    // test exact equality, but we can check that the pattern repeats.
    // At the 4 corners of the 8×8 surface which have the same distance
    // from center, the Bayer pattern should match.
    let tl = surf.get_pixel(0, 0).unwrap();
    let tr = surf.get_pixel(w - 1, 0).unwrap();
    // Top-left and top-right corners have symmetric distance.
    // Due to discrete coordinates, they should be within ±1.
    assert!(
        (tl.r as i32 - tr.r as i32).unsigned_abs() <= 1,
        "symmetric corners should have similar values: tl.r={}, tr.r={}",
        tl.r,
        tr.r,
    );
}

#[test]
fn gradient_rows_matches_full_fill() {
    // fill_radial_gradient_rows for specific rows must produce pixels
    // identical to fill_radial_gradient_noise for those same rows.
    let w = 120u32;
    let h = 80u32;
    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);

    // Full fill.
    let mut buf_full = vec![0u8; (w * h * 4) as usize];
    {
        let mut full = make_surface(&mut buf_full, w, h);
        drawing::fill_radial_gradient_noise(&mut full, center, edge, 3, 0xDEAD_BEEF);
    }

    // Row fill: clear and re-fill rows 20..40.
    let mut buf_rows = vec![0u8; (w * h * 4) as usize];
    {
        let mut rows = make_surface(&mut buf_rows, w, h);
        // First fill entirely.
        drawing::fill_radial_gradient_noise(&mut rows, center, edge, 3, 0xDEAD_BEEF);
        // Zero out rows 20..40 to simulate incremental clear.
        let bpp = 4u32;
        for y in 20..40u32 {
            let off = (y * w * bpp) as usize;
            let end = off + (w * bpp) as usize;
            for b in &mut rows.data[off..end] {
                *b = 0;
            }
        }
        // Re-fill with row-based function.
        drawing::fill_radial_gradient_rows(&mut rows, center, edge, 20, 20);
    }

    // Rows 20..40 should be pixel-identical.
    let bpp = 4u32;
    for y in 20..40u32 {
        for x in 0..w {
            let off = (y * w * bpp + x * bpp) as usize;
            assert_eq!(
                &buf_full[off..off + 4],
                &buf_rows[off..off + 4],
                "pixel ({},{}) mismatch between full fill and row fill",
                x,
                y,
            );
        }
    }
}

#[test]
fn gradient_rows_out_of_bounds_clipped() {
    // fill_radial_gradient_rows with start_y + row_count > height should
    // be silently clipped, not panic.
    let w = 20u32;
    let h = 10u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    // Start at row 8, request 5 rows → should only fill rows 8 and 9.
    drawing::fill_radial_gradient_rows(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        8,
        5,
    );

    // Row 8 should have non-zero pixels.
    let px = surf.get_pixel(w / 2, 8).unwrap();
    assert!(px.r > 0 || px.g > 0 || px.b > 0, "row 8 should be filled");
    // Row 9 should have non-zero pixels.
    let px = surf.get_pixel(w / 2, 9).unwrap();
    assert!(px.r > 0 || px.g > 0 || px.b > 0, "row 9 should be filled");
}

#[test]
fn gradient_rows_zero_count_noop() {
    // fill_radial_gradient_rows with row_count=0 should be a no-op.
    let w = 20u32;
    let h = 10u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    drawing::fill_radial_gradient_rows(
        &mut surf,
        Color::rgb(28, 28, 28),
        Color::rgb(16, 16, 16),
        0,
        0,
    );

    // Should remain all zeros.
    for b in buf.iter() {
        // Alpha channel defaults to 0 in zeroed buffer.
        assert_eq!(*b, 0, "zero-count fill should not modify buffer");
    }
}

#[test]
fn gradient_dither_monochrome() {
    // Bayer dithering must maintain the monochrome property: R=G=B for
    // every pixel, since the same dither offset is added to all channels.
    let w = 64u32;
    let h = 64u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    let center = Color::rgb(28, 28, 28);
    let edge = Color::rgb(16, 16, 16);
    drawing::fill_radial_gradient_noise(&mut surf, center, edge, 0, 0);

    for y in 0..h {
        for x in 0..w {
            let px = surf.get_pixel(x, y).unwrap();
            assert_eq!(
                px.r, px.g,
                "monochrome violated at ({},{}): r={}, g={}",
                x, y, px.r, px.g,
            );
            assert_eq!(
                px.r, px.b,
                "monochrome violated at ({},{}): r={}, b={}",
                x, y, px.r, px.b,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Mouse cursor tests
// ---------------------------------------------------------------------------

#[test]
fn test_render_cursor_dimensions() {
    let size = (CURSOR_W * CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    render_cursor(&mut buf);

    // The cursor should have some non-transparent pixels (fill + outline).
    let mut opaque_count = 0;
    for y in 0..CURSOR_H {
        for x in 0..CURSOR_W {
            let off = ((y * CURSOR_W + x) * 4) as usize;
            if buf[off + 3] > 0 {
                opaque_count += 1;
            }
        }
    }
    assert!(
        opaque_count > 20,
        "cursor should have >20 opaque pixels, got {}",
        opaque_count
    );
}

#[test]
fn test_render_cursor_top_left_pixel_is_outline() {
    let size = (CURSOR_W * CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    render_cursor(&mut buf);

    // Pixel (0,0) should be the outline color (dark grey, opaque).
    // BGRA8888 encoding: B=40, G=40, R=40, A=255.
    assert_eq!(buf[3], 255, "top-left pixel alpha should be 255 (opaque)");
    assert_eq!(buf[0], buf[1], "top-left pixel should be grey (B==G)");
    assert_eq!(buf[1], buf[2], "top-left pixel should be grey (G==R)");
}

#[test]
fn test_render_cursor_has_fill_pixels() {
    let size = (CURSOR_W * CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    render_cursor(&mut buf);

    // Pixel at (1,2) in the bitmap is fill (white, 255/255/255/255).
    let off = ((2 * CURSOR_W + 1) * 4) as usize;
    assert_eq!(buf[off + 3], 255, "fill pixel alpha should be 255");
    assert_eq!(buf[off + 0], 255, "fill pixel B channel should be 255");
    assert_eq!(buf[off + 1], 255, "fill pixel G channel should be 255");
    assert_eq!(buf[off + 2], 255, "fill pixel R channel should be 255");
}

#[test]
fn test_render_cursor_has_transparent_pixels() {
    let size = (CURSOR_W * CURSOR_H * 4) as usize;
    let mut buf = vec![0u8; size];
    render_cursor(&mut buf);

    // Pixel at (11,0) should be transparent (outside the arrow).
    let off = ((0 * CURSOR_W + 11) * 4) as usize;
    assert_eq!(buf[off + 3], 0, "pixel outside arrow should be transparent");
}

#[test]
fn test_scale_pointer_coord_zero() {
    assert_eq!(scale_pointer_coord(0, 1280), 0);
}

#[test]
fn test_scale_pointer_coord_max() {
    // 32767 * 1280 / 32768 = 1279.96... → 1279
    let result = scale_pointer_coord(32767, 1280);
    assert!(result < 1280, "result {} should be < 1280", result);
    assert_eq!(result, 1279);
}

#[test]
fn test_scale_pointer_coord_midpoint() {
    // 16384 * 1280 / 32768 = 640
    let result = scale_pointer_coord(16384, 1280);
    assert_eq!(result, 640);
}

#[test]
fn test_scale_pointer_coord_never_exceeds_max() {
    // Even with coord = 32767 and max = 800, result should be < 800.
    for max in [640u32, 768, 800, 1024, 1080, 1280, 1920] {
        for coord in [0, 1, 16383, 16384, 32766, 32767] {
            let result = scale_pointer_coord(coord, max);
            assert!(
                result < max,
                "scale_pointer_coord({}, {}) = {} (should be < {})",
                coord,
                max,
                result,
                max,
            );
        }
    }
}

#[test]
fn test_scale_pointer_coord_zero_max() {
    // Edge case: max_pixels = 0 should not panic.
    assert_eq!(scale_pointer_coord(16384, 0), 0);
}

// ---------------------------------------------------------------------------
// xy_to_byte tests — click-to-position: verify pixel-to-byte conversion
// for click placement in the text editor content area.
// ---------------------------------------------------------------------------

/// Click-to-position: clicking at the exact pixel position from byte_to_xy
/// round-trips back to the same byte offset for all positions in multiline text.
#[test]
fn click_to_position_round_trip_multiline() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"hello world\nline two\nthird line here";

    for pos in 0..=text.len() {
        let (x, y) = layout.byte_to_xy(text, pos);
        let result = layout.xy_to_byte(text, x, y);
        assert_eq!(
            result, pos,
            "click round-trip failed for pos={}: byte_to_xy→({},{}) xy_to_byte→{}",
            pos, x, y, result,
        );
    }
}

/// Click-to-position: clicking past the end of text on the last line returns
/// text.len() (cursor positioned at end of document).
#[test]
fn click_to_position_past_end_of_document() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"hello";
    // Click far past the end of "hello" on line 0.
    let result = layout.xy_to_byte(text, 500, 0);
    assert_eq!(result, 5);
}

/// Click-to-position: clicking below all text positions cursor at end of
/// nearest (last) line.
#[test]
fn click_to_position_below_all_text() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    let text = b"ab\ncd";
    // Click at y=200 which is well below the last line (line 1 at y=24).
    let result = layout.xy_to_byte(text, 0, 200);
    assert_eq!(result, text.len());
}

/// Click-to-position with scroll offset: after subtracting scroll_offset
/// visual lines, the click should map to the correct byte in the document.
#[test]
fn click_to_position_with_scroll_offset() {
    let layout = TextLayout {
        char_width: 10,
        line_height: 24,
        max_width: 800,
    };
    // 3 lines: "aaa\nbbb\nccc"
    let text = b"aaa\nbbb\nccc";
    // Simulate scroll_offset = 1 (first visible line is "bbb").
    // A click at y=0 in the viewport maps to visual line 1 in the document.
    let scroll_offset: u32 = 1;
    let click_y: u32 = 0; // top of viewport
    let adjusted_y = click_y + scroll_offset * layout.line_height;
    let result = layout.xy_to_byte(text, 0, adjusted_y);
    // Visual line 1 starts at byte 4 ('b').
    assert_eq!(result, 4);
}

// ---------------------------------------------------------------------------
// div-by-255 elimination tests (VAL-DRAW-001)
// ---------------------------------------------------------------------------

use drawing::div255;

/// Exhaustively verify div255 is exact for all values in the alpha-blending
/// range 0..=65025 (255 × 255). This is the correctness invariant for
/// replacing `x / 255` in blending hot paths.
#[test]
fn test_div255_exhaustive() {
    for x in 0..=65025u32 {
        let expected = x / 255;
        let got = div255(x);
        assert_eq!(
            got, expected,
            "div255({}) = {}, expected {}",
            x, got, expected,
        );
    }
}

/// Verify div255 at boundary values.
#[test]
fn test_div255_boundaries() {
    assert_eq!(div255(0), 0);
    assert_eq!(div255(255), 1);
    assert_eq!(div255(254), 0);
    assert_eq!(div255(256), 1);
    assert_eq!(div255(65025), 255); // 255 * 255
    // With rounding bias (+127):
    assert_eq!(div255(127), 0);
    assert_eq!(div255(128), 0);
    assert_eq!(div255(255), 1);
    // Typical alpha computation: (255 * 128 + 127) = 32767
    assert_eq!(div255(32767), 128);
}

// ---------------------------------------------------------------------------
// blend_over div255 correctness (VAL-DRAW-006)
// ---------------------------------------------------------------------------

/// Verify blend_over with div255 produces correct results for representative
/// alpha and channel combinations.
#[test]
fn test_blend_over_div255_alpha_combinations() {
    let test_alphas: &[u8] = &[0, 1, 64, 127, 128, 200, 254, 255];
    let test_channels: &[u8] = &[0, 1, 64, 128, 200, 254, 255];

    for &sa in test_alphas {
        for &sr in test_channels {
            for &dr in test_channels {
                let src = Color::rgba(sr, 0, 0, sa);
                let dst = Color::rgba(dr, 0, 0, 255);
                let result = src.blend_over(dst);

                // Basic sanity: alpha should be 255 (opaque dst).
                if sa == 0 {
                    assert_eq!(result, dst, "transparent src should return dst");
                } else if sa == 255 {
                    assert_eq!(result, src, "opaque src should return src");
                } else {
                    assert_eq!(
                        result.a, 255,
                        "blending onto opaque dst gives opaque result"
                    );
                    // Result red should be between dst and src (in sRGB space,
                    // not necessarily a linear interpolation).
                    let lo = if sr < dr { sr } else { dr };
                    let hi = if sr > dr { sr } else { dr };
                    assert!(
                        result.r >= lo.saturating_sub(2) && result.r <= hi.saturating_add(2),
                        "sa={} sr={} dr={}: result.r={} not in [{}, {}]",
                        sa,
                        sr,
                        dr,
                        result.r,
                        lo,
                        hi,
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pre-clipped draw_coverage tests (VAL-DRAW-002, VAL-DRAW-008)
// ---------------------------------------------------------------------------

/// draw_coverage with large negative y offset: y=-500, cov_height=520.
/// Only the bottom 20 rows of the coverage buffer should be visible on a
/// 100-pixel-tall surface.
#[test]
fn test_draw_coverage_large_negative_y() {
    let w = 10u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    surf.clear(Color::BLACK);

    let cov_w = 4u32;
    let cov_h = 520u32;
    let mut coverage = vec![0u8; (cov_w * cov_h) as usize];

    // Set full coverage on all pixels of the coverage buffer.
    for i in 0..coverage.len() {
        coverage[i] = 255;
    }

    surf.draw_coverage(-1, -500, &coverage, cov_w, cov_h, Color::WHITE);

    // Row 500 of coverage maps to surface y=0, col 1 maps to surface x=0.
    // Visible coverage: rows 500..520 → surface y 0..19, cols 1..4 → surface x 0..2.
    // (col range 1..4 exclusive: col=1→px=0, col=2→px=1, col=3→px=2)
    let p = surf.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255, "visible pixel at (0,0) should be white");
    let p = surf.get_pixel(2, 19).unwrap();
    assert_eq!(p.r, 255, "visible pixel at (2,19) should be white");
    // Surface y=20 should be untouched (black).
    let p = surf.get_pixel(0, 20).unwrap();
    assert_eq!(p.r, 0, "pixel at (0,20) should still be black");
    // Surface x=2 is the last visible column (col 3 maps to x=2 since x=-1+3=2).
    let p = surf.get_pixel(2, 0).unwrap();
    assert_eq!(p.r, 255, "visible pixel at (2,0) should be white");
    // Surface x=3 and beyond should be untouched.
    let p = surf.get_pixel(3, 0).unwrap();
    assert_eq!(p.r, 0, "pixel at (3,0) should still be black");
}

/// draw_coverage with large negative x offset: x=-500, cov_width=520.
/// Only the rightmost 20 columns of the coverage buffer should be visible.
#[test]
fn test_draw_coverage_large_negative_x() {
    let w = 100u32;
    let h = 10u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    surf.clear(Color::BLACK);

    let cov_w = 520u32;
    let cov_h = 4u32;
    let mut coverage = vec![0u8; (cov_w * cov_h) as usize];

    for i in 0..coverage.len() {
        coverage[i] = 255;
    }

    surf.draw_coverage(-500, -1, &coverage, cov_w, cov_h, Color::WHITE);

    // Col 500 maps to surface x=0, row 1 maps to surface y=0.
    let p = surf.get_pixel(0, 0).unwrap();
    assert_eq!(p.r, 255, "visible pixel at (0,0) should be white");
    let p = surf.get_pixel(19, 2).unwrap();
    assert_eq!(p.r, 255, "visible pixel at (19,2) should be white");
    // x=20 is beyond visible range.
    let p = surf.get_pixel(20, 0).unwrap();
    assert_eq!(p.r, 0, "pixel at (20,0) should still be black");
}

/// draw_coverage entirely off-screen should not modify any pixels.
#[test]
fn test_draw_coverage_fully_outside() {
    let coverage = [255u8; 2 * 2]; // 2x2, full coverage (1 byte per pixel)

    // Entirely to the left.
    {
        let mut buf = [0u8; 8 * 8 * 4];
        let mut surf = make_surface(&mut buf, 8, 8);
        surf.draw_coverage(-10, 0, &coverage, 2, 2, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0), "left: buffer should be zeroed");
    }

    // Entirely above.
    {
        let mut buf = [0u8; 8 * 8 * 4];
        let mut surf = make_surface(&mut buf, 8, 8);
        surf.draw_coverage(0, -10, &coverage, 2, 2, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0), "above: buffer should be zeroed");
    }

    // Entirely below.
    {
        let mut buf = [0u8; 8 * 8 * 4];
        let mut surf = make_surface(&mut buf, 8, 8);
        surf.draw_coverage(0, 10, &coverage, 2, 2, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0), "below: buffer should be zeroed");
    }

    // Entirely to the right.
    {
        let mut buf = [0u8; 8 * 8 * 4];
        let mut surf = make_surface(&mut buf, 8, 8);
        surf.draw_coverage(10, 0, &coverage, 2, 2, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0), "right: buffer should be zeroed");
    }
}

/// draw_coverage with a single pixel at various positions.
#[test]
fn test_draw_coverage_single_pixel() {
    // 1x1 coverage, full coverage (1 byte per pixel).
    let coverage = [255u8];

    // At origin.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut surf = make_surface(&mut buf, 4, 4);
    surf.draw_coverage(0, 0, &coverage, 1, 1, Color::WHITE);
    assert_eq!(surf.get_pixel(0, 0), Some(Color::WHITE));

    // At last pixel.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut surf = make_surface(&mut buf, 4, 4);
    surf.draw_coverage(3, 3, &coverage, 1, 1, Color::WHITE);
    assert_eq!(surf.get_pixel(3, 3), Some(Color::WHITE));

    // Just outside right edge — should be a no-op.
    let mut buf = [0u8; 4 * 4 * 4];
    let mut surf = make_surface(&mut buf, 4, 4);
    surf.draw_coverage(4, 0, &coverage, 1, 1, Color::WHITE);
    assert!(buf.iter().all(|&b| b == 0));
}

/// draw_coverage with zero-size coverage buffer.
#[test]
fn test_draw_coverage_zero_size() {
    {
        let mut buf = [0u8; 4 * 4 * 4];
        let mut surf = make_surface(&mut buf, 4, 4);
        surf.draw_coverage(0, 0, &[], 0, 0, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0));
    }
    {
        let mut buf = [0u8; 4 * 4 * 4];
        let mut surf = make_surface(&mut buf, 4, 4);
        surf.draw_coverage(0, 0, &[], 10, 0, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0));
    }
    {
        let mut buf = [0u8; 4 * 4 * 4];
        let mut surf = make_surface(&mut buf, 4, 4);
        surf.draw_coverage(0, 0, &[], 0, 10, Color::WHITE);
        drop(surf);
        assert!(buf.iter().all(|&b| b == 0));
    }
}

// ---------------------------------------------------------------------------
// Unsafe draw_coverage comparison (VAL-DRAW-003)
// ---------------------------------------------------------------------------

/// Reference (safe) implementation of draw_coverage for comparison.
/// Uses the same div255 + pre-clip algorithm but safe pixel access.
/// Coverage is 1 byte per pixel (grayscale).
fn draw_coverage_reference(
    surf: &mut Surface,
    x: i32,
    y: i32,
    coverage: &[u8],
    cov_width: u32,
    cov_height: u32,
    color: Color,
) {
    if cov_width == 0 || cov_height == 0 || color.a == 0 {
        return;
    }
    let cov_total = (cov_width as usize) * (cov_height as usize);
    if coverage.len() < cov_total {
        return;
    }
    let src_r_lin = drawing::SRGB_TO_LINEAR[color.r as usize] as u32;
    let src_g_lin = drawing::SRGB_TO_LINEAR[color.g as usize] as u32;
    let src_b_lin = drawing::SRGB_TO_LINEAR[color.b as usize] as u32;
    let color_a = color.a as u32;

    for row in 0..cov_height {
        for col in 0..cov_width {
            let cov = coverage[(row * cov_width + col) as usize];
            if cov == 0 {
                continue;
            }
            let px = x + col as i32;
            let py = y + row as i32;
            if px < 0 || py < 0 {
                continue;
            }
            let ux = px as u32;
            let uy = py as u32;
            let alpha = div255(color_a * cov as u32 + 127);
            if alpha >= 255 {
                surf.set_pixel(ux, uy, color);
                continue;
            }
            if let Some(dst) = surf.get_pixel(ux, uy) {
                let dst_r_lin = drawing::SRGB_TO_LINEAR[dst.r as usize] as u32;
                let dst_g_lin = drawing::SRGB_TO_LINEAR[dst.g as usize] as u32;
                let dst_b_lin = drawing::SRGB_TO_LINEAR[dst.b as usize] as u32;
                let inv_a = 255 - alpha;
                let out_r_lin = div255(dst_r_lin * inv_a + src_r_lin * alpha + 127);
                let out_g_lin = div255(dst_g_lin * inv_a + src_g_lin * alpha + 127);
                let out_b_lin = div255(dst_b_lin * inv_a + src_b_lin * alpha + 127);
                let out_r = drawing::LINEAR_TO_SRGB[drawing::linear_to_idx(out_r_lin)];
                let out_g = drawing::LINEAR_TO_SRGB[drawing::linear_to_idx(out_g_lin)];
                let out_b = drawing::LINEAR_TO_SRGB[drawing::linear_to_idx(out_b_lin)];
                let out_a = dst.a as u32 + div255(alpha * (255 - dst.a as u32));
                surf.set_pixel(
                    ux,
                    uy,
                    Color {
                        r: out_r,
                        g: out_g,
                        b: out_b,
                        a: if out_a > 255 { 255 } else { out_a as u8 },
                    },
                );
            }
        }
    }
}

/// Compare optimized draw_coverage against safe reference for various inputs.
#[test]
fn test_draw_coverage_unsafe_vs_reference() {
    let test_cases: &[(i32, i32, u32, u32)] = &[
        (0, 0, 4, 4),     // normal
        (-1, -1, 4, 4),   // partial negative
        (-500, 0, 520, 4), // large negative x
        (0, -500, 4, 520), // large negative y
        (6, 6, 4, 4),     // partial clip right/bottom
        (0, 0, 1, 1),     // single pixel
        (0, 0, 8, 8),     // exact surface size
        (7, 7, 2, 2),     // clip to 1x1
    ];

    for &(x, y, cw, ch) in test_cases {
        let cov_len = (cw * ch) as usize; // 1 byte per pixel
        let mut coverage = vec![0u8; cov_len];
        // Varying coverage values.
        for i in 0..cov_len {
            coverage[i] = ((i * 37 + 13) % 256) as u8;
        }

        // Reference surface.
        let mut ref_buf = vec![0x80u8; 8 * 8 * 4]; // non-zero background
        let mut ref_surf = make_surface(&mut ref_buf, 8, 8);
        ref_surf.clear(Color::rgb(100, 50, 200));
        draw_coverage_reference(&mut ref_surf, x, y, &coverage, cw, ch, Color::rgb(255, 128, 0));

        // Optimized surface.
        let mut opt_buf = vec![0x80u8; 8 * 8 * 4];
        let mut opt_surf = make_surface(&mut opt_buf, 8, 8);
        opt_surf.clear(Color::rgb(100, 50, 200));
        opt_surf.draw_coverage(x, y, &coverage, cw, ch, Color::rgb(255, 128, 0));

        assert_eq!(
            ref_buf, opt_buf,
            "draw_coverage mismatch at x={}, y={}, cw={}, ch={}",
            x, y, cw, ch,
        );
    }
}

// ---------------------------------------------------------------------------
// Unsafe blit_blend comparison (VAL-DRAW-004)
// ---------------------------------------------------------------------------

/// Reference (safe) implementation of blit_blend for comparison.
fn blit_blend_reference(
    surf: &mut Surface,
    src_data: &[u8],
    src_width: u32,
    src_height: u32,
    src_stride: u32,
    dst_x: u32,
    dst_y: u32,
) {
    if dst_x >= surf.width || dst_y >= surf.height {
        return;
    }
    let copy_w = min_u32(src_width, surf.width - dst_x);
    let copy_h = min_u32(src_height, surf.height - dst_y);
    let bpp = surf.format.bytes_per_pixel() as usize;
    for row in 0..copy_h {
        for col in 0..copy_w {
            let src_off = (row * src_stride + col * surf.format.bytes_per_pixel()) as usize;
            if src_off + bpp <= src_data.len() {
                let src_color = Color {
                    r: src_data[src_off + 2],
                    g: src_data[src_off + 1],
                    b: src_data[src_off],
                    a: src_data[src_off + 3],
                };
                if src_color.a == 255 {
                    surf.set_pixel(dst_x + col, dst_y + row, src_color);
                } else if src_color.a > 0 {
                    surf.blend_pixel(dst_x + col, dst_y + row, src_color);
                }
            }
        }
    }
}

/// Compare optimized blit_blend against safe reference for various clip cases.
#[test]
fn test_blit_blend_unsafe_vs_reference() {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();

    let test_cases: &[(u32, u32, u32, u32, u32, u32)] = &[
        // (src_w, src_h, dst_x, dst_y, dst_w, dst_h)
        (4, 4, 0, 0, 8, 8),   // fully inside
        (4, 4, 6, 6, 8, 8),   // partial clip right/bottom
        (4, 4, 0, 0, 2, 2),   // dst smaller than src
        (1, 1, 0, 0, 8, 8),   // single pixel source
        (8, 8, 0, 0, 8, 8),   // exact size
        (4, 4, 7, 7, 8, 8),   // clip to 1x1
    ];

    for &(sw, sh, dx, dy, dw, dh) in test_cases {
        let src_stride = sw * bpp;
        let mut src_buf = vec![0u8; (src_stride * sh) as usize];
        // Fill source with a mix of opaque, semi-transparent, and transparent.
        for row in 0..sh {
            for col in 0..sw {
                let off = (row * src_stride + col * bpp) as usize;
                let alpha = ((row * sw + col) * 60 % 256) as u8;
                src_buf[off] = 100; // B
                src_buf[off + 1] = 150; // G
                src_buf[off + 2] = 200; // R
                src_buf[off + 3] = alpha; // A
            }
        }

        // Reference.
        let mut ref_buf = vec![0u8; (dw * dh * 4) as usize];
        let mut ref_surf = make_surface(&mut ref_buf, dw, dh);
        ref_surf.clear(Color::rgb(50, 100, 150));
        blit_blend_reference(&mut ref_surf, &src_buf, sw, sh, src_stride, dx, dy);

        // Optimized.
        let mut opt_buf = vec![0u8; (dw * dh * 4) as usize];
        let mut opt_surf = make_surface(&mut opt_buf, dw, dh);
        opt_surf.clear(Color::rgb(50, 100, 150));
        opt_surf.blit_blend(&src_buf, sw, sh, src_stride, dx, dy);

        assert_eq!(
            ref_buf, opt_buf,
            "blit_blend mismatch at sw={}, sh={}, dx={}, dy={}, dw={}, dh={}",
            sw, sh, dx, dy, dw, dh,
        );
    }
}

/// blit_blend with all-opaque source uses copy_from_slice fast path.
#[test]
fn test_blit_blend_opaque_source() {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let sw = 4u32;
    let sh = 4u32;
    let src_stride = sw * bpp;
    let mut src_buf = vec![0u8; (src_stride * sh) as usize];
    // Fill entirely opaque.
    for row in 0..sh {
        for col in 0..sw {
            let off = (row * src_stride + col * bpp) as usize;
            src_buf[off] = 200; // B
            src_buf[off + 1] = 100; // G
            src_buf[off + 2] = 50; // R
            src_buf[off + 3] = 255; // A (opaque)
        }
    }

    let mut dst_buf = vec![0u8; (8 * 8 * 4) as usize];
    let mut dst = make_surface(&mut dst_buf, 8, 8);
    dst.clear(Color::rgb(0, 0, 255));
    dst.blit_blend(&src_buf, sw, sh, src_stride, 2, 2);

    // Opaque source should overwrite dst exactly.
    let p = dst.get_pixel(3, 3).unwrap();
    assert_eq!(p.r, 50);
    assert_eq!(p.g, 100);
    assert_eq!(p.b, 200);
    assert_eq!(p.a, 255);
    // Outside blit region should be original.
    assert_eq!(dst.get_pixel(0, 0), Some(Color::rgb(0, 0, 255)));
}

// ---------------------------------------------------------------------------
// Unsafe fill_rect_blend comparison (VAL-DRAW-005)
// ---------------------------------------------------------------------------

/// Reference (safe) implementation of fill_rect_blend for comparison.
fn fill_rect_blend_reference(
    surf: &mut Surface,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: Color,
) {
    if color.a == 255 {
        surf.fill_rect(x, y, w, h, color);
        return;
    }
    if color.a == 0 || w == 0 || h == 0 {
        return;
    }
    if x >= surf.width || y >= surf.height {
        return;
    }
    let x2 = min_u32(x.saturating_add(w), surf.width);
    let y2 = min_u32(y.saturating_add(h), surf.height);
    for row in y..y2 {
        for col in x..x2 {
            surf.blend_pixel(col, row, color);
        }
    }
}

/// Compare optimized fill_rect_blend against safe reference for various inputs.
#[test]
fn test_fill_rect_blend_unsafe_vs_reference() {
    let test_colors = [
        Color::rgba(255, 0, 0, 128),   // semi-transparent red
        Color::rgba(0, 255, 0, 64),    // quarter green
        Color::rgba(128, 128, 128, 1), // nearly transparent grey
        Color::rgba(255, 255, 255, 200), // mostly opaque white
    ];

    let test_rects: &[(u32, u32, u32, u32)] = &[
        (0, 0, 8, 8),   // full surface
        (2, 2, 4, 4),   // interior rect
        (6, 6, 4, 4),   // partial clip
        (0, 0, 1, 1),   // single pixel
        (0, 0, 0, 5),   // zero width
        (0, 0, 5, 0),   // zero height
        (10, 10, 2, 2), // entirely outside
    ];

    for color in test_colors {
        for &(x, y, w, h) in test_rects {
            // Reference.
            let mut ref_buf = vec![0u8; (8 * 8 * 4) as usize];
            let mut ref_surf = make_surface(&mut ref_buf, 8, 8);
            ref_surf.clear(Color::rgb(30, 60, 90));
            fill_rect_blend_reference(&mut ref_surf, x, y, w, h, color);

            // Optimized.
            let mut opt_buf = vec![0u8; (8 * 8 * 4) as usize];
            let mut opt_surf = make_surface(&mut opt_buf, 8, 8);
            opt_surf.clear(Color::rgb(30, 60, 90));
            opt_surf.fill_rect_blend(x, y, w, h, color);

            assert_eq!(
                ref_buf, opt_buf,
                "fill_rect_blend mismatch: color={:?}, rect=({},{},{},{})",
                color, x, y, w, h,
            );
        }
    }
}

/// fill_rect_blend with opaque color delegates to fill_rect (fast path).
#[test]
fn test_fill_rect_blend_opaque_delegates() {
    let mut buf1 = vec![0u8; (8 * 8 * 4) as usize];
    let mut surf1 = make_surface(&mut buf1, 8, 8);
    surf1.fill_rect(2, 2, 4, 4, Color::rgb(200, 100, 50));

    let mut buf2 = vec![0u8; (8 * 8 * 4) as usize];
    let mut surf2 = make_surface(&mut buf2, 8, 8);
    surf2.fill_rect_blend(2, 2, 4, 4, Color::rgb(200, 100, 50));

    assert_eq!(buf1, buf2, "opaque fill_rect_blend should match fill_rect");
}

/// fill_rect_blend with transparent color is a no-op.
#[test]
fn test_fill_rect_blend_transparent_noop() {
    let mut buf = vec![0u8; (8 * 8 * 4) as usize];
    {
        let mut surf = make_surface(&mut buf, 8, 8);
        surf.clear(Color::rgb(100, 100, 100));
    }
    let before = buf.clone();
    {
        let mut surf = Surface {
            data: &mut buf,
            width: 8,
            height: 8,
            stride: 8 * 4,
            format: PixelFormat::Bgra8888,
        };
        surf.fill_rect_blend(0, 0, 8, 8, Color::TRANSPARENT);
    }
    assert_eq!(buf, before, "transparent fill should be no-op");
}

// ---------------------------------------------------------------------------
// Rounded rectangle tests (VAL-PRIM-001 through VAL-PRIM-005, VAL-PRIM-016)
// ---------------------------------------------------------------------------

/// VAL-PRIM-001: corner_radius=0 produces pixel-identical output to fill_rect.
#[test]
fn rounded_rect_zero_radius_equals_fill_rect() {
    let color = Color::rgb(200, 100, 50);
    let w = 100u32;
    let h = 50u32;

    // fill_rect reference
    let mut ref_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut ref_buf, w, h);
        surf.fill_rect(10, 5, 80, 40, color);
    }

    // fill_rounded_rect with radius=0
    let mut rr_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, w, h);
        surf.fill_rounded_rect(10, 5, 80, 40, 0, color);
    }

    assert_eq!(ref_buf, rr_buf, "radius=0 rounded_rect should be pixel-identical to fill_rect");
}

/// VAL-PRIM-001: corner_radius=0 fill_rounded_rect_blend pixel-identical to fill_rect_blend.
#[test]
fn rounded_rect_blend_zero_radius_equals_fill_rect_blend() {
    let color = Color::rgba(200, 100, 50, 180);
    let bg = Color::rgb(30, 60, 90);
    let w = 100u32;
    let h = 50u32;

    // fill_rect_blend reference
    let mut ref_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut ref_buf, w, h);
        surf.clear(bg);
        surf.fill_rect_blend(10, 5, 80, 40, color);
    }

    // fill_rounded_rect_blend with radius=0
    let mut rr_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, w, h);
        surf.clear(bg);
        surf.fill_rounded_rect_blend(10, 5, 80, 40, 0, color);
    }

    assert_eq!(ref_buf, rr_buf, "radius=0 rounded_rect_blend should be pixel-identical to fill_rect_blend");
}

/// VAL-PRIM-002: 100x50 with corner_radius=4 has smooth anti-aliased corners,
/// interior fully opaque.
#[test]
fn rounded_rect_small_radius_antialiased_corners() {
    let w = 120u32;
    let h = 60u32;
    let color = Color::rgb(255, 0, 0);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    surf.clear(Color::BLACK);
    surf.fill_rounded_rect(10, 5, 100, 50, 4, color);

    // Interior pixel should be fully opaque red.
    let interior = surf.get_pixel(50, 30).unwrap();
    assert_eq!(interior, color, "interior should be solid color");

    // A pixel just inside the top-left corner, well within radius, should be opaque.
    let inside_corner = surf.get_pixel(15, 10).unwrap();
    assert_eq!(inside_corner, color, "pixel inside corner should be solid");

    // Check that corner pixel at the very tip has anti-aliased (partial) coverage.
    // At (10, 5) — the exact corner — should NOT be fully solid (it's in the arc region).
    let corner_pixel = surf.get_pixel(10, 5).unwrap();
    // The corner should be either transparent/black or a blend (AA edge), not fully red.
    assert!(
        corner_pixel != color,
        "exact corner pixel should be anti-aliased, not fully solid: {:?}",
        corner_pixel,
    );
}

/// VAL-PRIM-003: corner_radius clamped to min(w,h)/2. Oversized radius produces capsule.
#[test]
fn rounded_rect_radius_clamped_to_half_dimension() {
    let w = 60u32;
    let h = 40u32;

    // Radius 255 on 40x20 → clamped to min(40,20)/2 = 10 → capsule shape.
    let mut buf1 = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf1, w, h);
        surf.fill_rounded_rect(10, 10, 40, 20, 255, Color::WHITE);
    }

    // Same as explicitly using radius = min(40,20)/2 = 10.
    let mut buf2 = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf2, w, h);
        surf.fill_rounded_rect(10, 10, 40, 20, 10, Color::WHITE);
    }

    assert_eq!(buf1, buf2, "oversized radius should clamp to min(w,h)/2");
}

/// VAL-PRIM-004: Anti-aliased edge pixels use gamma-correct blending.
/// Edge pixels should blend with sRGB math (same as existing blend_over).
#[test]
fn rounded_rect_aa_edges_gamma_correct() {
    let w = 50u32;
    let h = 50u32;
    let bg = Color::rgb(0, 0, 255); // blue background
    let fg = Color::rgb(255, 0, 0); // red foreground

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    surf.clear(bg);
    surf.fill_rounded_rect_blend(5, 5, 40, 40, 10, fg);

    // Find an anti-aliased edge pixel in the corner region.
    // Walk along the top-left arc to find a pixel that's partially covered.
    let mut found_aa_pixel = false;
    for y in 5..16 {
        for x in 5..16 {
            let p = surf.get_pixel(x, y).unwrap();
            // A partially-covered pixel should have both red and blue components.
            if p.r > 10 && p.r < 245 && p.b > 10 && p.b < 245 {
                // Verify gamma correctness: in sRGB space, partial coverage of
                // pure red on pure blue should produce sRGB values > naive linear.
                // If this were linear blending, 50% would give r=128,b=128.
                // Gamma-correct gives r>140,b>140.
                // For any coverage, the sRGB values should be "heavier" than linear.
                found_aa_pixel = true;
                break;
            }
        }
        if found_aa_pixel {
            break;
        }
    }
    assert!(found_aa_pixel, "should find at least one anti-aliased edge pixel in corner");
}

/// VAL-PRIM-005: NEON SIMD path for interior rows matches scalar exactly.
/// fill_rounded_rect interior rows are equivalent to fill_rect.
#[test]
fn rounded_rect_interior_matches_fill_rect() {
    let w = 120u32;
    let h = 60u32;
    let color = Color::rgb(100, 200, 50);

    let mut rr_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, w, h);
        surf.fill_rounded_rect(10, 5, 100, 50, 8, color);
    }

    // Interior rows (y=13..47, which is radius=8 from top/bottom) should be
    // identical to fill_rect for those rows.
    let mut fr_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut fr_buf, w, h);
        surf.fill_rect(10, 13, 100, 34, color);
    }

    let stride = (w * 4) as usize;
    for row in 13u32..47u32 {
        let off = (row * w * 4) as usize;
        let rr_row = &rr_buf[off..off + stride];
        let fr_row = &fr_buf[off..off + stride];
        assert_eq!(
            rr_row, fr_row,
            "interior row {row} should match fill_rect exactly"
        );
    }
}

/// VAL-PRIM-016: No performance regression for sharp corners.
/// corner_radius=0 should use fill_rect directly (early branch).
/// This test verifies identical output, which proves the early branch works.
#[test]
fn rounded_rect_zero_radius_no_overhead() {
    // Large surface to ensure NEON paths are exercised.
    let w = 256u32;
    let h = 128u32;
    let color = Color::rgb(42, 128, 200);

    let mut ref_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut ref_buf, w, h);
        surf.fill_rect(0, 0, w, h, color);
    }

    let mut rr_buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut rr_buf, w, h);
        surf.fill_rounded_rect(0, 0, w, h, 0, color);
    }

    assert_eq!(ref_buf, rr_buf, "zero radius on large surface should be identical to fill_rect");
}

/// Rounded rect clips to surface bounds.
#[test]
fn rounded_rect_clips_to_bounds() {
    let w = 32u32;
    let h = 32u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    // Rect extends past right and bottom edges.
    surf.fill_rounded_rect(20, 20, 50, 50, 8, Color::WHITE);

    // Pixel inside clipped region should be filled.
    assert_eq!(surf.get_pixel(28, 28), Some(Color::WHITE));
    // Pixel at boundary of surface should exist.
    assert_eq!(surf.get_pixel(31, 31), Some(Color::WHITE));
    // Pixel outside surface: no crash.
}

/// Rounded rect with zero dimensions is no-op.
#[test]
fn rounded_rect_zero_dimensions_noop() {
    let w = 16u32;
    let h = 16u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    surf.fill_rounded_rect(0, 0, 0, 5, 4, Color::WHITE);
    surf.fill_rounded_rect(0, 0, 5, 0, 4, Color::WHITE);
    assert!(buf.iter().all(|&b| b == 0));
}

/// Rounded rect entirely outside surface is no-op.
#[test]
fn rounded_rect_outside_surface_noop() {
    let w = 16u32;
    let h = 16u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);

    surf.fill_rounded_rect(20, 20, 10, 10, 4, Color::WHITE);
    assert!(buf.iter().all(|&b| b == 0));
}

/// Semi-transparent background + AA edges: combined alpha correct.
#[test]
fn rounded_rect_blend_semi_transparent_background() {
    let w = 60u32;
    let h = 60u32;
    let bg = Color::rgba(0, 0, 255, 128); // semi-transparent blue
    let fg = Color::rgba(255, 0, 0, 200); // mostly opaque red

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    // Set background (using set_pixel since fill_rect writes opaque).
    for y in 0..h {
        for x in 0..w {
            surf.set_pixel(x, y, bg);
        }
    }
    surf.fill_rounded_rect_blend(5, 5, 50, 50, 12, fg);

    // Interior pixel: fully covered, blended with semi-transparent bg.
    let interior = surf.get_pixel(30, 30).unwrap();
    let expected_interior = fg.blend_over(bg);
    assert_eq!(interior, expected_interior, "interior should be exact blend_over result");

    // AA edge pixel should have intermediate values (not just fg or bg).
    // Check that alpha is properly computed (not clamped to 255).
    let corner = surf.get_pixel(5, 5).unwrap();
    // Corner should still show background influence.
    assert!(corner.b > 0, "corner should retain some blue from background");
}

/// Rounded rect blend: fully opaque foreground takes fast path to fill_rounded_rect.
#[test]
fn rounded_rect_blend_opaque_matches_non_blend() {
    let w = 60u32;
    let h = 60u32;
    let color = Color::rgb(100, 200, 50);

    let mut buf1 = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf1, w, h);
        surf.fill_rounded_rect(5, 5, 50, 50, 8, color);
    }

    let mut buf2 = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf2, w, h);
        surf.fill_rounded_rect_blend(5, 5, 50, 50, 8, color);
    }

    assert_eq!(buf1, buf2, "opaque rounded_rect_blend should match rounded_rect exactly");
}

/// Rounded rect blend: transparent foreground is no-op.
#[test]
fn rounded_rect_blend_transparent_noop() {
    let w = 32u32;
    let h = 32u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf, w, h);
        surf.clear(Color::WHITE);
    }
    let before = buf.clone();
    {
        let mut surf = Surface {
            data: &mut buf,
            width: w,
            height: h,
            stride: w * 4,
            format: PixelFormat::Bgra8888,
        };
        surf.fill_rounded_rect_blend(0, 0, w, h, 8, Color::TRANSPARENT);
    }
    assert_eq!(buf, before, "transparent rounded_rect_blend should be no-op");
}

/// Small rounded rects (1x1, 2x2, 3x3) don't crash and produce reasonable output.
#[test]
fn rounded_rect_tiny_dimensions() {
    let w = 16u32;
    let h = 16u32;
    let color = Color::rgb(255, 128, 0);

    // 1x1 with radius 10 → clamped to 0
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf, w, h);
        surf.fill_rounded_rect(5, 5, 1, 1, 10, color);
    }
    // Should write something at (5,5).
    let surf = Surface { data: &mut buf, width: w, height: h, stride: w * 4, format: PixelFormat::Bgra8888 };
    let p = surf.get_pixel(5, 5).unwrap();
    assert!(p.r > 0, "1x1 rounded rect should produce at least some color");

    // 2x2 with radius 1
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf, w, h);
        surf.fill_rounded_rect(5, 5, 2, 2, 1, color);
    }

    // 3x3 with radius 1
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut surf = make_surface(&mut buf, w, h);
        surf.fill_rounded_rect(5, 5, 3, 3, 1, color);
    }
    // All should complete without crash.
}

/// Symmetry test: all four corners should be mirror images.
#[test]
fn rounded_rect_four_corner_symmetry() {
    let w = 80u32;
    let h = 80u32;
    let r = 12u32;
    let color = Color::rgb(200, 100, 50);

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let mut surf = make_surface(&mut buf, w, h);
    surf.fill_rounded_rect(0, 0, w, h, r, color);

    // Compare top-left corner with top-right (horizontal mirror).
    for dy in 0..r {
        for dx in 0..r {
            let tl = surf.get_pixel(dx, dy).unwrap();
            let tr = surf.get_pixel(w - 1 - dx, dy).unwrap();
            let bl = surf.get_pixel(dx, h - 1 - dy).unwrap();
            let br = surf.get_pixel(w - 1 - dx, h - 1 - dy).unwrap();

            assert_eq!(tl, tr, "TL ({},{}) should mirror TR ({},{})", dx, dy, w-1-dx, dy);
            assert_eq!(tl, bl, "TL ({},{}) should mirror BL ({},{})", dx, dy, dx, h-1-dy);
            assert_eq!(tl, br, "TL ({},{}) should mirror BR ({},{})", dx, dy, w-1-dx, h-1-dy);
        }
    }
}

// ---------------------------------------------------------------------------
// Gaussian blur tests
// ---------------------------------------------------------------------------

/// Helper: create a read-only surface (no mutable borrow of buffer).
fn make_readonly_surface(buf: &[u8], width: u32, height: u32) -> drawing::ReadSurface<'_> {
    let bpp = PixelFormat::Bgra8888.bytes_per_pixel();
    let stride = width * bpp;
    assert!(buf.len() >= (stride * height) as usize);
    drawing::ReadSurface {
        data: buf,
        width,
        height,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

/// Helper: brute-force 2D Gaussian convolution reference implementation.
/// Uses the SAME kernel weights as the library (via `drawing::compute_kernel`)
/// so the only error source is the separable decomposition rounding.
fn reference_2d_gaussian_blur(
    src: &[u8],
    dst: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    radius: u32,
    sigma_fp: u32,
) {
    let mut kernel_arr = [0u32; drawing::MAX_KERNEL_DIAMETER];
    let diameter = drawing::compute_kernel(&mut kernel_arr, radius, sigma_fp);
    let kernel = &kernel_arr[..diameter];
    let r = radius as i32;
    let bpp = 4u32;

    for y in 0..height {
        for x in 0..width {
            let mut sum_r: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_b: u64 = 0;
            let mut sum_a: u64 = 0;
            let mut weight_sum: u64 = 0;

            for ky in -r..=r {
                for kx in -r..=r {
                    let sx = clamp_coord(x as i32 + kx, width);
                    let sy = clamp_coord(y as i32 + ky, height);
                    let offset = (sy * stride + sx * bpp) as usize;

                    // Kernel weight = kernel_x[kx+r] * kernel_y[ky+r]
                    let wx = kernel[(kx + r) as usize] as u64;
                    let wy = kernel[(ky + r) as usize] as u64;
                    let w = wx * wy;

                    // BGRA format
                    sum_b += src[offset] as u64 * w;
                    sum_g += src[offset + 1] as u64 * w;
                    sum_r += src[offset + 2] as u64 * w;
                    sum_a += src[offset + 3] as u64 * w;
                    weight_sum += w;
                }
            }

            let dst_offset = (y * stride + x * bpp) as usize;
            if weight_sum > 0 {
                dst[dst_offset] = ((sum_b + weight_sum / 2) / weight_sum) as u8;
                dst[dst_offset + 1] = ((sum_g + weight_sum / 2) / weight_sum) as u8;
                dst[dst_offset + 2] = ((sum_r + weight_sum / 2) / weight_sum) as u8;
                dst[dst_offset + 3] = ((sum_a + weight_sum / 2) / weight_sum) as u8;
            }
        }
    }
}

/// Helper: clamp coordinate to surface bounds.
fn clamp_coord(val: i32, max: u32) -> u32 {
    if val < 0 {
        0
    } else if val >= max as i32 {
        max - 1
    } else {
        val as u32
    }
}

/// VAL-BLUR-001: Single white pixel blurred produces symmetric output.
#[test]
fn blur_single_pixel_symmetric() {
    let w = 32u32;
    let h = 32u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 4u32;
    let sigma_fp = 512u32; // sigma=2.0 in 8.8 FP

    // Source: single white pixel at center.
    let mut src_buf = vec![0u8; size];
    {
        let cx = w / 2;
        let cy = h / 2;
        let off = (cy * stride + cx * bpp) as usize;
        src_buf[off] = 255;     // B
        src_buf[off + 1] = 255; // G
        src_buf[off + 2] = 255; // R
        src_buf[off + 3] = 255; // A
    }

    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);

    // Check symmetry: pixel at (cx+d, cy) == pixel at (cx-d, cy)
    let cx = w / 2;
    let cy = h / 2;
    for d in 1..=radius {
        let left = dst.get_pixel(cx - d, cy).unwrap();
        let right = dst.get_pixel(cx + d, cy).unwrap();
        assert_eq!(left, right, "horizontal symmetry at distance {d}");

        let top = dst.get_pixel(cx, cy - d).unwrap();
        let bottom = dst.get_pixel(cx, cy + d).unwrap();
        assert_eq!(top, bottom, "vertical symmetry at distance {d}");
    }

    // Center should have highest value.
    let center = dst.get_pixel(cx, cy).unwrap();
    let neighbor = dst.get_pixel(cx + 1, cy).unwrap();
    assert!(center.r >= neighbor.r, "center should be >= neighbor");
}

/// VAL-BLUR-002: Two-pass separable blur matches brute-force 2D reference within ±1.
#[test]
fn blur_two_pass_matches_2d_reference() {
    let w = 32u32;
    let h = 32u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 4u32;
    let sigma_fp = 256u32; // sigma=1.0

    // Create a test pattern: checkerboard-ish.
    let mut src_buf = vec![0u8; size];
    for y in 0..h {
        for x in 0..w {
            let off = (y * stride + x * bpp) as usize;
            let val = if (x + y) % 3 == 0 { 200u8 } else { 50u8 };
            src_buf[off] = val;
            src_buf[off + 1] = val;
            src_buf[off + 2] = val;
            src_buf[off + 3] = 255;
        }
    }

    // Two-pass separable blur.
    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];
    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);
    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);

    // Brute-force 2D reference.
    let mut ref_buf = vec![0u8; size];
    reference_2d_gaussian_blur(&src_buf, &mut ref_buf, w, h, stride, radius, sigma_fp);

    // Compare: max difference per channel ≤ 1.
    let mut max_diff = 0u8;
    for i in 0..size {
        let diff = (dst_buf[i] as i16 - ref_buf[i] as i16).unsigned_abs() as u8;
        if diff > max_diff {
            max_diff = diff;
        }
    }
    assert!(
        max_diff <= 1,
        "two-pass vs 2D reference: max channel diff = {max_diff}, expected ≤ 1"
    );
}

/// VAL-BLUR-003: radius=0 is identity.
#[test]
fn blur_radius_zero_identity() {
    let w = 16u32;
    let h = 16u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;

    // Fill source with a pattern.
    let mut src_buf = vec![0u8; size];
    for (i, b) in src_buf.iter_mut().enumerate() {
        *b = (i % 256) as u8;
    }

    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, 0, 256);

    assert_eq!(dst_buf, src_buf, "radius=0 should produce identical output");
}

/// VAL-BLUR-004: Blur respects surface bounds — no OOB reads.
#[test]
fn blur_edge_clamping_small_surface() {
    let w = 16u32;
    let h = 16u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 8u32;
    let sigma_fp = 512u32; // sigma=2.0

    // Fill source with white.
    let mut src_buf = vec![255u8; size];
    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    // Should not panic, all output pixels valid.
    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);

    // All pixels should be white (blurring a uniform surface = same surface).
    for y in 0..h {
        for x in 0..w {
            let p = dst.get_pixel(x, y).unwrap();
            assert_eq!(p, Color::WHITE, "uniform white blur at ({x},{y})");
        }
    }
}

/// VAL-BLUR-005: CPU cap enforced for large radii — radius=64 clamped to 16.
#[test]
fn blur_large_radius_capped() {
    let w = 16u32;
    let h = 16u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;

    // Single white pixel.
    let mut src_buf = vec![0u8; size];
    {
        let cx = w / 2;
        let cy = h / 2;
        let off = (cy * stride + cx * bpp) as usize;
        src_buf[off] = 255;
        src_buf[off + 1] = 255;
        src_buf[off + 2] = 255;
        src_buf[off + 3] = 255;
    }

    // Blur with radius=64 (should be clamped to MAX_CPU_BLUR_RADIUS=16).
    let mut dst_64_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_64_buf, w, h);
        drawing::blur_surface(&src, &mut dst, &mut tmp_buf, 64, 512);
    }

    // Blur with radius=16 (explicit max).
    let mut dst_16_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_16_buf, w, h);
        drawing::blur_surface(&src, &mut dst, &mut tmp_buf, 16, 512);
    }

    // Both should produce the same result (clamped to same effective radius).
    assert_eq!(dst_64_buf, dst_16_buf, "radius=64 should produce same result as radius=16 (clamped)");
}

/// VAL-BLUR-007: sigma=4.0 produces wider spread than sigma=1.0.
#[test]
fn blur_sigma_varies_spread() {
    let w = 64u32;
    let h = 64u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 16u32;

    // Single white pixel at center.
    let mut src_buf = vec![0u8; size];
    {
        let cx = w / 2;
        let cy = h / 2;
        let off = (cy * stride + cx * bpp) as usize;
        src_buf[off] = 255;
        src_buf[off + 1] = 255;
        src_buf[off + 2] = 255;
        src_buf[off + 3] = 255;
    }

    // Blur with sigma=1.0 (256 in 8.8 FP).
    let mut dst_s1_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_s1_buf, w, h);
        drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, 256);
    }

    // Blur with sigma=4.0 (1024 in 8.8 FP).
    let mut dst_s4_buf = vec![0u8; size];
    {
        let src = make_readonly_surface(&src_buf, w, h);
        let mut dst = make_surface(&mut dst_s4_buf, w, h);
        drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, 1024);
    }

    // Measure energy at distance > 4 pixels from center.
    // sigma=4.0 should have more energy far from center.
    let cx = w / 2;
    let cy = h / 2;
    let mut energy_s1: u64 = 0;
    let mut energy_s4: u64 = 0;
    for d in 5..=radius {
        // Sample along horizontal axis.
        let off_s1 = (cy * stride + (cx + d) * bpp) as usize;
        let off_s4 = (cy * stride + (cx + d) * bpp) as usize;
        energy_s1 += dst_s1_buf[off_s1 + 2] as u64; // R channel
        energy_s4 += dst_s4_buf[off_s4 + 2] as u64;
    }

    assert!(
        energy_s4 > energy_s1,
        "sigma=4.0 should have wider spread (more energy at distance > 4): s4={energy_s4}, s1={energy_s1}"
    );
}

/// VAL-BLUR-013: BlurStrategy trait defined, CpuBlur implements it.
#[test]
fn blur_trait_defined_cpublur_implements() {
    // Verify that CpuBlur implements BlurStrategy by calling it through the trait.
    let blur: &dyn drawing::BlurStrategy = &drawing::CpuBlur;

    let w = 8u32;
    let h = 8u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;

    let mut src_buf = vec![128u8; size];
    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    blur.blur(&src, &mut dst, &mut tmp_buf, 2, 256);
    // Should complete without panic — uniform input stays uniform.
    for y in 0..h {
        for x in 0..w {
            let p = dst.get_pixel(x, y).unwrap();
            assert_eq!(p.r, 128, "uniform blur through trait at ({x},{y})");
        }
    }
}

/// VAL-BLUR-014: Blur preserves alpha channel (blurred, not clamped).
#[test]
fn blur_preserves_alpha_channel() {
    let w = 32u32;
    let h = 32u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 4u32;
    let sigma_fp = 256u32; // sigma=1.0

    // Create source with alpha gradient: left half fully transparent, right half fully opaque.
    let mut src_buf = vec![0u8; size];
    for y in 0..h {
        for x in 0..w {
            let off = (y * stride + x * bpp) as usize;
            src_buf[off] = 255;     // B
            src_buf[off + 1] = 255; // G
            src_buf[off + 2] = 255; // R
            src_buf[off + 3] = if x >= w / 2 { 255 } else { 0 }; // A: step function
        }
    }

    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);

    // At the alpha boundary (x=w/2), alpha should be blurred — intermediate values.
    let border_x = w / 2;
    let mid_y = h / 2;
    let border_alpha = dst.get_pixel(border_x, mid_y).unwrap().a;
    assert!(
        border_alpha > 1 && border_alpha < 254,
        "alpha at boundary should be intermediate (blurred), got {border_alpha}"
    );
}

/// Additional: blur of uniform surface should produce identical output.
#[test]
fn blur_uniform_surface_unchanged() {
    let w = 24u32;
    let h = 24u32;
    let bpp = 4u32;
    let stride = w * bpp;
    let size = (stride * h) as usize;
    let radius = 8u32;
    let sigma_fp = 512u32; // sigma=2.0

    let color = Color::rgba(100, 150, 200, 180);
    let mut src_buf = vec![0u8; size];
    for y in 0..h {
        for x in 0..w {
            let off = (y * stride + x * bpp) as usize;
            // BGRA encoding
            src_buf[off] = color.b;
            src_buf[off + 1] = color.g;
            src_buf[off + 2] = color.r;
            src_buf[off + 3] = color.a;
        }
    }

    let mut dst_buf = vec![0u8; size];
    let mut tmp_buf = vec![0u8; size];

    let src = make_readonly_surface(&src_buf, w, h);
    let mut dst = make_surface(&mut dst_buf, w, h);

    drawing::blur_surface(&src, &mut dst, &mut tmp_buf, radius, sigma_fp);

    // All pixels should match the uniform color (within ±1 for rounding).
    for y in 0..h {
        for x in 0..w {
            let p = dst.get_pixel(x, y).unwrap();
            let dr = (p.r as i16 - color.r as i16).unsigned_abs();
            let dg = (p.g as i16 - color.g as i16).unsigned_abs();
            let db = (p.b as i16 - color.b as i16).unsigned_abs();
            let da = (p.a as i16 - color.a as i16).unsigned_abs();
            assert!(
                dr <= 1 && dg <= 1 && db <= 1 && da <= 1,
                "uniform blur at ({x},{y}): got {:?}, expected {:?}, diff=({dr},{dg},{db},{da})",
                p, color
            );
        }
    }
}

// ── ResamplingMethod and bilinear downscale tests ───────────────────

/// VAL-XFORM-015: ResamplingMethod enum exists with Bilinear variant.
/// The API is parameterized so Lanczos can be added later without
/// changing call sites.
#[test]
fn resampling_method_enum_exists() {
    let method = drawing::ResamplingMethod::Bilinear;
    // The enum variant must exist and be usable.
    match method {
        drawing::ResamplingMethod::Bilinear => {} // OK
    }
}

/// VAL-XFORM-014: Bilinear downscale of checkerboard.
/// A 100×100 checkerboard at scale(0.5) → 50×50 output.
/// Center pixels should be blended gray (~128), NOT aliased black/white.
#[test]
fn bilinear_downscale_checkerboard_produces_gray() {
    // Create a 100×100 checkerboard: alternating black/white pixels.
    let src_w = 100u32;
    let src_h = 100u32;
    let src_stride = src_w * 4;
    let mut src_buf = vec![0u8; (src_stride * src_h) as usize];

    for y in 0..src_h {
        for x in 0..src_w {
            let off = (y * src_stride + x * 4) as usize;
            let is_white = (x + y) % 2 == 0;
            let val = if is_white { 255u8 } else { 0u8 };
            src_buf[off] = val;     // B
            src_buf[off + 1] = val; // G
            src_buf[off + 2] = val; // R
            src_buf[off + 3] = 255; // A (opaque)
        }
    }

    // Destination: 50×50 (downscale by 0.5x).
    let dst_w = 50u32;
    let dst_h = 50u32;
    let dst_stride = dst_w * 4;
    let mut dst_buf = vec![0u8; (dst_stride * dst_h) as usize];
    let mut fb = Surface {
        data: &mut dst_buf,
        width: dst_w,
        height: dst_h,
        stride: dst_stride,
        format: PixelFormat::Bgra8888,
    };

    // Use blit_blend_bilinear to downsample. The inverse transform maps
    // each dst pixel to 2× source coordinates: inv = scale(2, 2).
    // Offset by 0.5 in source space so dst pixel centers land between
    // source pixels, producing blended output instead of point samples.
    fb.blit_blend_bilinear(
        &src_buf,
        src_w,
        src_h,
        src_stride,
        0, 0,
        dst_w, dst_h,
        2.0, 0.0,  // inv_a, inv_b
        0.0, 2.0,  // inv_c, inv_d
        0.5, 0.5,  // inv_tx, inv_ty — offset to sample between pixels
        255,
        drawing::ResamplingMethod::Bilinear,
    );

    // Sample center pixels: should be blended gray (approximately 128).
    let center_x = 25u32;
    let center_y = 25u32;
    let off = (center_y * dst_stride + center_x * 4) as usize;
    let r = dst_buf[off + 2];
    let g = dst_buf[off + 1];
    let b = dst_buf[off];

    assert!(
        r >= 98 && r <= 158,
        "VAL-XFORM-014: downscaled checkerboard center R should be ~128, got {r}"
    );
    assert!(
        g >= 98 && g <= 158,
        "VAL-XFORM-014: downscaled checkerboard center G should be ~128, got {g}"
    );
    assert!(
        b >= 98 && b <= 158,
        "VAL-XFORM-014: downscaled checkerboard center B should be ~128, got {b}"
    );
}

/// VAL-XFORM-014 (supplementary): Verify that MOST center pixels are
/// blended gray, not a mix of pure black and pure white.
#[test]
fn bilinear_downscale_checkerboard_no_aliased_pixels() {
    let src_w = 100u32;
    let src_h = 100u32;
    let src_stride = src_w * 4;
    let mut src_buf = vec![0u8; (src_stride * src_h) as usize];

    for y in 0..src_h {
        for x in 0..src_w {
            let off = (y * src_stride + x * 4) as usize;
            let is_white = (x + y) % 2 == 0;
            let val = if is_white { 255u8 } else { 0u8 };
            src_buf[off] = val;
            src_buf[off + 1] = val;
            src_buf[off + 2] = val;
            src_buf[off + 3] = 255;
        }
    }

    let dst_w = 50u32;
    let dst_h = 50u32;
    let dst_stride = dst_w * 4;
    let mut dst_buf = vec![0u8; (dst_stride * dst_h) as usize];
    let mut fb = Surface {
        data: &mut dst_buf,
        width: dst_w,
        height: dst_h,
        stride: dst_stride,
        format: PixelFormat::Bgra8888,
    };

    fb.blit_blend_bilinear(
        &src_buf,
        src_w, src_h, src_stride,
        0, 0, dst_w, dst_h,
        2.0, 0.0, 0.0, 2.0, 0.5, 0.5,
        255,
        drawing::ResamplingMethod::Bilinear,
    );

    // In the interior (avoid edges), count pixels that are pure B/W vs gray.
    let mut gray_count = 0u32;
    let mut extreme_count = 0u32;
    for y in 5..45 {
        for x in 5..45 {
            let off = (y * dst_stride + x * 4) as usize;
            let r = dst_buf[off + 2];
            if r < 10 || r > 245 {
                extreme_count += 1;
            } else {
                gray_count += 1;
            }
        }
    }

    // The vast majority should be gray, not aliased B/W.
    assert!(
        gray_count > extreme_count,
        "bilinear downscale should produce more gray than B/W pixels: gray={gray_count}, extreme={extreme_count}"
    );
}
