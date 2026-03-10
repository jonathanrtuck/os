//! Toy compositor — draws a demo scene into a shared framebuffer.
//!
//! Receives framebuffer info (VA, dimensions, stride) from init via channel
//! shared memory. Draws a scene using the drawing library to prove the
//! compositor → GPU driver rendering pipeline works.
//!
//! # Channel shared page layout (written by init before start)
//!
//! ```text
//! offset 0:  fb_va      (u64) — framebuffer VA in this process's address space
//! offset 8:  fb_width   (u32) — display width in pixels
//! offset 12: fb_height  (u32) — display height in pixels
//! offset 16: fb_stride  (u32) — bytes per row
//! offset 20: fb_size    (u32) — total framebuffer size in bytes
//! ```

#![no_std]
#![no_main]

/// Channel shared memory base (first channel page in our address space).
const SHM: *const u8 = 0x4000_0000 as *const u8;

/// Draw a demo scene showing the compositor working.
fn draw_demo_scene(surface: &mut drawing::Surface) {
    let font = &drawing::FONT_8X16;

    // Dark background.
    surface.clear(drawing::Color::rgb(24, 24, 32));
    // Title bar area.
    surface.fill_rect(0, 0, surface.width, 32, drawing::Color::rgb(40, 40, 60));
    surface.draw_text(
        12,
        8,
        "Document OS - Compositor",
        font,
        drawing::Color::rgb(200, 200, 220),
    );

    // Draw colored panels to demonstrate compositing.
    let panel_w = 280;
    let panel_h = 180;
    let margin = 24;
    let top_y = 56;
    // Panel 1: Blue
    let x1 = margin;

    surface.fill_rect(
        x1,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(50, 70, 140),
    );
    surface.draw_rect(
        x1,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(80, 100, 180),
    );
    surface.draw_text(
        x1 + 12,
        top_y + 12,
        "Surface A",
        font,
        drawing::Color::WHITE,
    );
    surface.draw_text(
        x1 + 12,
        top_y + 36,
        "Text document",
        font,
        drawing::Color::rgb(160, 170, 200),
    );

    // Draw some fake text lines.
    let text_color = drawing::Color::rgb(140, 150, 180);

    for i in 0..5 {
        let y = top_y + 64 + i * 18;
        let w = panel_w - 24 - (i * 30) % 80;
        surface.fill_rect(x1 + 12, y, w, 10, text_color);
    }

    // Panel 2: Green
    let x2 = margin + panel_w + margin;

    surface.fill_rect(
        x2,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(40, 100, 60),
    );
    surface.draw_rect(
        x2,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(60, 140, 80),
    );
    surface.draw_text(
        x2 + 12,
        top_y + 12,
        "Surface B",
        font,
        drawing::Color::WHITE,
    );
    surface.draw_text(
        x2 + 12,
        top_y + 36,
        "Image viewer",
        font,
        drawing::Color::rgb(140, 200, 160),
    );

    // Draw a fake image (gradient rectangle).
    for y in 0..80 {
        for x in 0..120 {
            let r = (x * 2) as u8;
            let g = (y * 3) as u8;
            let b = 100u8;

            surface.set_pixel(x2 + 12 + x, top_y + 64 + y, drawing::Color::rgb(r, g, b));
        }
    }

    // Panel 3: Red/orange
    let x3 = margin + 2 * (panel_w + margin);

    surface.fill_rect(
        x3,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(140, 50, 40),
    );
    surface.draw_rect(
        x3,
        top_y,
        panel_w,
        panel_h,
        drawing::Color::rgb(180, 70, 60),
    );
    surface.draw_text(
        x3 + 12,
        top_y + 12,
        "Surface C",
        font,
        drawing::Color::WHITE,
    );
    surface.draw_text(
        x3 + 12,
        top_y + 36,
        "Terminal",
        font,
        drawing::Color::rgb(200, 150, 140),
    );

    // Draw fake terminal lines.
    let prompt_color = drawing::Color::rgb(100, 200, 100);
    let cmd_color = drawing::Color::rgb(200, 200, 200);

    surface.draw_text(x3 + 12, top_y + 64, "$ ", font, prompt_color);
    surface.draw_text(x3 + 28, top_y + 64, "ls documents/", font, cmd_color);
    surface.draw_text(
        x3 + 12,
        top_y + 82,
        "  notes.md",
        font,
        drawing::Color::rgb(160, 160, 200),
    );
    surface.draw_text(
        x3 + 12,
        top_y + 100,
        "  photo.png",
        font,
        drawing::Color::rgb(160, 160, 200),
    );
    surface.draw_text(x3 + 12, top_y + 118, "$ ", font, prompt_color);
    surface.draw_text(x3 + 28, top_y + 118, "_", font, cmd_color);

    // Status bar at bottom.
    let bar_y = surface.height.saturating_sub(28);

    surface.fill_rect(0, bar_y, surface.width, 28, drawing::Color::rgb(40, 40, 60));
    surface.draw_text(
        12,
        bar_y + 6,
        "3 surfaces | compositor active | memory shared",
        font,
        drawing::Color::rgb(160, 160, 180),
    );

    // Version info.
    let version = "v0.1";
    let vx = surface.width.saturating_sub(12 + version.len() as u32 * 8);

    surface.draw_text(
        vx,
        bar_y + 6,
        version,
        font,
        drawing::Color::rgb(100, 100, 120),
    );
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::write(b"  \xF0\x9F\x8E\xA8 compositor - starting\n");

    // Read framebuffer info from channel shared page.
    let fb_va = unsafe { core::ptr::read_volatile(SHM as *const u64) } as usize;
    let fb_width = unsafe { core::ptr::read_volatile(SHM.add(8) as *const u32) };
    let fb_height = unsafe { core::ptr::read_volatile(SHM.add(12) as *const u32) };
    let fb_stride = unsafe { core::ptr::read_volatile(SHM.add(16) as *const u32) };
    let fb_size = unsafe { core::ptr::read_volatile(SHM.add(20) as *const u32) };

    if fb_va == 0 || fb_width == 0 || fb_height == 0 {
        sys::write(b"compositor: bad framebuffer info\n");
        sys::exit();
    }

    // Create a drawing surface backed by the shared framebuffer.
    let fb_slice = unsafe { core::slice::from_raw_parts_mut(fb_va as *mut u8, fb_size as usize) };
    let mut surface = drawing::Surface {
        data: fb_slice,
        width: fb_width,
        height: fb_height,
        stride: fb_stride,
        format: drawing::PixelFormat::Bgra8888,
    };

    draw_demo_scene(&mut surface);

    sys::write(b"     scene drawn, signaling init\n");
    // Signal init that drawing is complete.
    sys::channel_signal(0);
    sys::exit();
}
