//! Compositor — composites overlapping surfaces with alpha blending.
//!
//! Receives framebuffer config via IPC ring buffer from init. Draws each panel
//! into its own surface buffer, then composites them onto the framebuffer in
//! z-order with per-pixel alpha blending.

#![no_std]
#![no_main]

extern crate alloc;

/// Channel shared memory base (first channel in our address space).
const CHANNEL_SHM_BASE: usize = 0x4000_0000;
// Protocol message type (must match init's definition).
const MSG_COMPOSITOR_CONFIG: u32 = 3;
// Panel surface dimensions. Each panel gets its own pixel buffer in BSS,
// demand-paged by the kernel when first touched.
const PANEL_W: u32 = 400;
const PANEL_H: u32 = 260;
const PANEL_STRIDE: u32 = PANEL_W * 4;
const PANEL_BUF_SIZE: usize = (PANEL_STRIDE * PANEL_H) as usize;

#[repr(C)]
#[derive(Clone, Copy)]
struct CompositorConfig {
    fb_va: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_size: u32,
}
static mut PANEL_A_BUF: [u8; PANEL_BUF_SIZE] = [0u8; PANEL_BUF_SIZE];
static mut PANEL_B_BUF: [u8; PANEL_BUF_SIZE] = [0u8; PANEL_BUF_SIZE];
static mut PANEL_C_BUF: [u8; PANEL_BUF_SIZE] = [0u8; PANEL_BUF_SIZE];
// TrueType font rasterization scratch space (BSS, demand-paged).
static mut TTF_SCRATCH: drawing::RasterScratch = drawing::RasterScratch::zeroed();
static mut TTF_RASTER_BUF: [u8; 128 * 128] = [0u8; 128 * 128];

// Embedded TrueType font.
const PROGGY_TTF: &[u8] = include_bytes!("../../libraries/drawing/ProggyClean.ttf");

fn panel_surface(buf: &mut [u8]) -> drawing::Surface<'_> {
    drawing::Surface {
        data: buf,
        width: PANEL_W,
        height: PANEL_H,
        stride: PANEL_STRIDE,
        format: drawing::PixelFormat::Bgra8888,
    }
}

// ---------------------------------------------------------------------------
// Panel content — each drawn into its own surface buffer
// ---------------------------------------------------------------------------

/// Panel A: document editor (blue, semi-transparent background).
fn draw_panel_a(s: &mut drawing::Surface) {
    use drawing::{Color, FONT_8X16 as F};

    // Semi-transparent blue background.
    s.fill_rect(0, 0, s.width, s.height, Color::rgba(40, 50, 120, 200));

    // Title bar (slightly more opaque).
    s.fill_rect(0, 0, s.width, 32, Color::rgba(55, 65, 145, 235));
    s.draw_text(12, 8, "Document Editor", &F, Color::WHITE);

    // Border.
    s.draw_rect(0, 0, s.width, s.height, Color::rgba(100, 120, 200, 240));

    // Text content.
    let tc = Color::rgba(190, 195, 220, 255);
    let lines = [
        "The quick brown fox jumps",
        "over the lazy dog. This is",
        "a document being edited in",
        "the compositor demo.",
        "",
        "Alpha blending lets panels",
        "overlap with transparency.",
        "",
        "Look through this panel to",
        "see the background behind.",
    ];
    for (i, line) in lines.iter().enumerate() {
        s.draw_text(16, 44 + i as u32 * 20, line, &F, tc);
    }
}

/// Panel B: image viewer (green, semi-transparent background).
fn draw_panel_b(s: &mut drawing::Surface) {
    use drawing::{Color, FONT_8X16 as F};

    // Semi-transparent green background.
    s.fill_rect(0, 0, s.width, s.height, Color::rgba(30, 85, 50, 180));

    // Title bar.
    s.fill_rect(0, 0, s.width, 32, Color::rgba(35, 110, 55, 220));
    s.draw_text(12, 8, "Image Viewer", &F, Color::WHITE);

    // Border.
    s.draw_rect(0, 0, s.width, s.height, Color::rgba(60, 160, 80, 230));

    // Gradient "image" — opaque pixels within the panel.
    for y in 0..140u32 {
        for x in 0..220u32 {
            let r = ((x * 255) / 220) as u8;
            let g = ((y * 200) / 140) as u8;
            let b = 128u8.saturating_sub((x as u8).wrapping_mul(2) / 5);
            s.set_pixel(16 + x, 44 + y, Color::rgb(r, g, b));
        }
    }

    s.draw_text(16, 196, "photo.png", &F, Color::rgba(140, 200, 160, 255));
    s.draw_text(
        16,
        216,
        "1920x1080 | 2.4 MB",
        &F,
        Color::rgba(100, 160, 120, 255),
    );
}

/// Panel C: terminal (warm amber, semi-transparent background).
fn draw_panel_c(s: &mut drawing::Surface) {
    use drawing::{Color, FONT_8X16 as F};

    // Semi-transparent warm background.
    s.fill_rect(0, 0, s.width, s.height, Color::rgba(95, 55, 25, 200));

    // Title bar.
    s.fill_rect(0, 0, s.width, 32, Color::rgba(125, 75, 35, 235));
    s.draw_text(12, 8, "Terminal", &F, Color::WHITE);

    // Border.
    s.draw_rect(0, 0, s.width, s.height, Color::rgba(180, 120, 60, 240));

    // Terminal session content.
    let prompt = Color::rgba(100, 220, 100, 255);
    let cmd = Color::rgba(220, 220, 220, 255);
    let out = Color::rgba(160, 160, 200, 255);
    let note = Color::rgba(200, 180, 100, 255);

    let mut y = 44u32;
    s.draw_text(16, y, "$ ", &F, prompt);
    s.draw_text(32, y, "query mimetype:image/*", &F, cmd);
    y += 20;
    s.draw_text(16, y, "  photo.png", &F, out);
    y += 18;
    s.draw_text(16, y, "  sunset.jpg", &F, out);
    y += 18;
    s.draw_text(16, y, "  diagram.svg", &F, out);
    y += 26;
    s.draw_text(16, y, "$ ", &F, prompt);
    s.draw_text(32, y, "view photo.png", &F, cmd);
    y += 20;
    s.draw_text(16, y, "  [opening in Image Viewer]", &F, note);
    y += 26;
    s.draw_text(16, y, "$ ", &F, prompt);
    s.draw_text(32, y, "_", &F, cmd);
}

// ---------------------------------------------------------------------------
// Compositing — assemble the final framebuffer
// ---------------------------------------------------------------------------

/// Composite all panels onto the framebuffer in z-order (back → front).
///
/// Each panel is blitted with per-pixel alpha — semi-transparent backgrounds
/// blend with whatever is behind them, opaque text and image pixels overwrite.
fn composite(fb: &mut drawing::Surface) {
    use drawing::{Color, FONT_8X16 as F};

    // Opaque dark background.
    fb.clear(Color::rgb(20, 20, 28));

    // Subtle background grid to make transparency visible.
    let grid = Color::rgb(28, 28, 38);
    let mut gy = 0u32;
    while gy < fb.height {
        fb.draw_hline(0, gy, fb.width, grid);
        gy += 20;
    }
    let mut gx = 0u32;
    while gx < fb.width {
        fb.draw_vline(gx, 0, fb.height, grid);
        gx += 20;
    }

    // Title bar.
    fb.fill_rect(0, 0, fb.width, 36, Color::rgb(32, 32, 48));
    fb.draw_text(12, 10, "Document OS", &F, Color::rgb(200, 200, 220));
    let version = "Compositor v0.3 | TrueType";
    let vx = fb.width.saturating_sub(12 + version.len() as u32 * 8);
    fb.draw_text(vx, 10, version, &F, Color::rgb(90, 90, 110));

    // Composite panels in z-order: A (back) → B (middle) → C (front).
    let a_buf = unsafe { &PANEL_A_BUF[..] };
    fb.blit_blend(a_buf, PANEL_W, PANEL_H, PANEL_STRIDE, 60, 56);

    let b_buf = unsafe { &PANEL_B_BUF[..] };
    fb.blit_blend(b_buf, PANEL_W, PANEL_H, PANEL_STRIDE, 280, 140);

    let c_buf = unsafe { &PANEL_C_BUF[..] };
    fb.blit_blend(c_buf, PANEL_W, PANEL_H, PANEL_STRIDE, 500, 90);

    // TrueType demo text — rendered below the panels.
    draw_truetype_demo(fb);

    // Status bar.
    let bar_y = fb.height.saturating_sub(28);
    fb.fill_rect(0, bar_y, fb.width, 28, Color::rgb(32, 32, 48));
    fb.draw_text(
        12,
        bar_y + 6,
        "3 surfaces | alpha compositing | TrueType rasterizer",
        &F,
        Color::rgb(130, 130, 150),
    );
}

/// Render TrueType text onto the framebuffer for visual comparison with bitmap.
fn draw_truetype_demo(fb: &mut drawing::Surface) {
    use drawing::{Color, RasterBuffer, TrueTypeFont, FONT_8X16 as F};

    let font = match TrueTypeFont::new(PROGGY_TTF) {
        Some(f) => f,
        None => {
            sys::print(b"compositor: failed to parse TTF\n");
            return;
        }
    };

    let scratch = unsafe { &mut TTF_SCRATCH };
    let raster_buf = unsafe { &mut TTF_RASTER_BUF };

    // Draw a comparison label with bitmap font.
    let label_y = fb.height.saturating_sub(120);
    fb.fill_rect(0, label_y, fb.width, 90, Color::rgb(24, 24, 36));
    fb.draw_text(
        12,
        label_y + 4,
        "Bitmap 8x16:",
        &F,
        Color::rgb(120, 120, 140),
    );
    fb.draw_text(
        140,
        label_y + 4,
        "Hello, Document OS!",
        &F,
        Color::rgb(220, 220, 240),
    );

    // TrueType text at multiple sizes.
    let sizes: [u32; 3] = [16, 24, 32];
    let labels = ["TTF 16px:", "TTF 24px:", "TTF 32px:"];
    let text = "Hello, Document OS!";

    for (i, &size) in sizes.iter().enumerate() {
        let row_y = label_y + 22 + i as u32 * 22;
        fb.draw_text(12, row_y, labels[i], &F, Color::rgb(120, 120, 140));

        // Render each character with the TrueType rasterizer.
        let mut pen_x: i32 = 140;
        let baseline_y = row_y as i32 + size as i32 - 4;

        for ch in text.chars() {
            let mut raster = RasterBuffer {
                data: raster_buf,
                width: 128,
                height: 128,
            };

            if let Some(metrics) = font.rasterize(ch, size, &mut raster, scratch) {
                if metrics.width > 0 && metrics.height > 0 {
                    fb.draw_coverage(
                        pen_x + metrics.bearing_x,
                        baseline_y - metrics.bearing_y,
                        &raster_buf[..(metrics.width * metrics.height) as usize],
                        metrics.width,
                        metrics.height,
                        Color::rgb(220, 220, 240),
                    );
                }
                pen_x += metrics.advance as i32;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    // Smoke test: dynamic allocation via Vec (uses memory_alloc under the hood).
    {
        let mut v: alloc::vec::Vec<u8> = alloc::vec::Vec::new();

        v.push(b'h');
        v.push(b'e');
        v.push(b'a');
        v.push(b'p');

        sys::print(b"     heap ok: ");
        sys::print(&v);
        sys::print(b"\n");
    }

    // Read compositor config from ring buffer (first message, sent by init).
    let ch = unsafe { ipc::Channel::from_base(CHANNEL_SHM_BASE, ipc::PAGE_SIZE, 1) };
    let mut msg = ipc::Message::new(0);

    if !ch.try_recv(&mut msg) || msg.msg_type != MSG_COMPOSITOR_CONFIG {
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

    // Draw each panel into its own surface buffer.
    unsafe {
        draw_panel_a(&mut panel_surface(&mut PANEL_A_BUF));
        draw_panel_b(&mut panel_surface(&mut PANEL_B_BUF));
        draw_panel_c(&mut panel_surface(&mut PANEL_C_BUF));
    }

    // Composite all panels onto the shared framebuffer.
    let fb_slice = unsafe { core::slice::from_raw_parts_mut(fb_va as *mut u8, fb_size as usize) };
    let mut fb = drawing::Surface {
        data: fb_slice,
        width: fb_width,
        height: fb_height,
        stride: fb_stride,
        format: drawing::PixelFormat::Bgra8888,
    };

    composite(&mut fb);

    sys::print(b"     scene composited, signaling init\n");
    let _ = sys::channel_signal(0);
    sys::exit();
}
