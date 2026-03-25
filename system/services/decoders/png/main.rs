//! PNG decoder service.
//!
//! Sandboxed service that decodes PNG images via the generic decoder
//! harness. The only format-specific code is in `png.rs` — two functions
//! that parse headers and decode pixels.

#![no_std]
#![no_main]

#[path = "../harness.rs"]
mod harness;
mod png;

/// Adapter: png_header → harness header signature.
fn header(data: &[u8]) -> Option<(u32, u32, u8)> {
    png::png_header(data).ok().map(|h| {
        let bpp = png::bits_per_pixel(h.color_type, h.bit_depth) as u8;
        (h.width, h.height, bpp)
    })
}

/// Adapter: png_decode → harness decode signature.
fn decode(data: &[u8], output: &mut [u8]) -> bool {
    png::png_decode(data, output).is_ok()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    harness::run(header, decode, b"png-decode");
}
