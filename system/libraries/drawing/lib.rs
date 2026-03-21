//! Drawing primitives for pixel buffers.
//!
//! Pure library -- no syscalls, no hardware access. Operates on borrowed pixel
//! buffers. All drawing operations clip to surface bounds; out-of-range
//! coordinates are silently ignored (no panics).
//!
//! # Usage
//!
//! ```text
//! let mut buf = [0u8; 320 * 240 * 4];
//! let mut surface = Surface {
//!     data: &mut buf,
//!     width: 320,
//!     height: 240,
//!     stride: 320 * 4,
//!     format: PixelFormat::Bgra8888,
//! };
//! surface.clear(Color::rgb(30, 30, 30));
//! surface.fill_rect(10, 10, 100, 50, Color::rgb(220, 80, 80));
//! ```

#![no_std]

// --- Lookup tables (textual include, no internal `use` statements) ----------
include!("gamma_tables.rs");

// --- Palette constants (textual include, references `Color`) ----------------
include!("palette.rs");

// --- NEON SIMD acceleration (aarch64 only, textual include) -----------------
#[cfg(target_arch = "aarch64")]
include!("neon.rs");

// --- Submodules -------------------------------------------------------------
mod blend;
mod blit;
mod blur;
pub mod box_blur;
mod coverage;
mod fill;
mod gradient;
mod line;
mod transform;

// --- Re-exports from submodules ---------------------------------------------
pub use blur::{
    blur_surface, blur_surface_scalar, compute_kernel, BlurStrategy, CpuBlur, ReadSurface,
    MAX_CPU_BLUR_RADIUS, MAX_KERNEL_DIAMETER,
};
pub use box_blur::{box_blur_3pass, box_blur_pad, box_blur_widths};
pub use gradient::{fill_radial_gradient_noise, fill_radial_gradient_rows, Xorshift32};

// === Core types =============================================================

/// A color in canonical RGBA order. Converted to the target pixel format
/// at the point of writing -- callers always work in RGBA regardless of the
/// underlying buffer format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Resampling method for scaled or transformed blits.
///
/// The API is parameterized so new methods (e.g., Lanczos) can be added
/// without changing call sites -- callers pass the enum variant and the
/// implementation dispatches internally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResamplingMethod {
    /// Bilinear interpolation: samples 4 surrounding pixels and blends
    /// based on fractional position. Good balance of quality and speed
    /// for most scaling/rotation operations.
    Bilinear,
}

/// A mutable view into a pixel buffer.
///
/// Does not own the backing memory -- the caller provides a mutable byte slice
/// from whatever source (DMA buffer, stack allocation, heap). The surface
/// borrows the slice for its lifetime.
///
/// Stride may exceed `width * bytes_per_pixel` if rows are padded.
///
/// # Invariant
///
/// Callers must ensure `stride * height <= data.len()` and
/// `stride >= width * bytes_per_pixel`. Unsafe drawing code relies on this.
/// Use [`Surface::is_valid`] to verify.
pub struct Surface<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: PixelFormat,
}

/// Pixel byte ordering within each pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// Blue, Green, Red, Alpha -- 8 bits each. Used by virtio-gpu 2D.
    Bgra8888,
}

// === Color impl (constructors + encoding) ===================================

impl Color {
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);

    /// Decode from pixel bytes in the given format.
    pub(crate) fn decode(bytes: &[u8], format: PixelFormat) -> Self {
        match format {
            PixelFormat::Bgra8888 => Color {
                r: bytes[2],
                g: bytes[1],
                b: bytes[0],
                a: bytes[3],
            },
        }
    }

    /// Encode to pixel bytes in the given format.
    pub(crate) fn encode(self, format: PixelFormat) -> [u8; 4] {
        match format {
            PixelFormat::Bgra8888 => [self.b, self.g, self.r, self.a],
        }
    }

    /// Decode a Color from a BGRA8888 byte slice (at least 4 bytes).
    pub fn decode_from_bgra(bytes: &[u8]) -> Self {
        Color {
            b: bytes[0],
            g: bytes[1],
            r: bytes[2],
            a: bytes[3],
        }
    }

    /// Opaque color from RGB components.
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }

    /// Color with explicit alpha.
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color { r, g, b, a }
    }
}

// === PixelFormat impl =======================================================

impl PixelFormat {
    /// Number of bytes per pixel.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgra8888 => 4,
        }
    }
}

// === Surface impl (core accessors) ==========================================

impl<'a> Surface<'a> {
    /// Returns `true` if the surface's data buffer is large enough for
    /// its declared dimensions: `stride * height <= data.len()` and
    /// `stride >= width * bytes_per_pixel`.
    pub fn is_valid(&self) -> bool {
        let bpp = self.format.bytes_per_pixel();
        self.stride >= self.width * bpp
            && (self.stride as usize) * (self.height as usize) <= self.data.len()
    }

    /// Byte offset for pixel (x, y), or `None` if out of bounds.
    pub(crate) fn pixel_offset(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }

        let offset = (y * self.stride + x * self.format.bytes_per_pixel()) as usize;
        let bpp = self.format.bytes_per_pixel() as usize;

        if offset + bpp <= self.data.len() {
            Some(offset)
        } else {
            None
        }
    }

    /// Read a single pixel. Returns `None` if out of bounds.
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if let Some(offset) = self.pixel_offset(x, y) {
            let bpp = self.format.bytes_per_pixel() as usize;

            Some(Color::decode(&self.data[offset..offset + bpp], self.format))
        } else {
            None
        }
    }

    /// Write a single pixel. No-op if out of bounds.
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        if let Some(offset) = self.pixel_offset(x, y) {
            let encoded = color.encode(self.format);
            let bpp = self.format.bytes_per_pixel() as usize;

            self.data[offset..offset + bpp].copy_from_slice(&encoded[..bpp]);
        }
    }

    /// Fill the entire surface with a solid color.
    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }
}

// === Utility functions ======================================================

/// Convert a linear light value (0-65535 u32) to a LINEAR_TO_SRGB table index.
/// The table has 4096 entries; index is `value >> 4`, clamped to 4095.
pub fn linear_to_idx(v: u32) -> usize {
    let idx = v >> 4;

    if idx > 4095 {
        4095
    } else {
        idx as usize
    }
}

/// Fast integer divide-by-255: exact for 0..=65025, +/-1 for larger values.
///
/// Replaces the expensive `x / 255` in alpha-blending hot paths. The identity
/// `(x + 1 + (x >> 8)) >> 8 == x / 255` holds for all u32 values in the
/// 0..=65025 range used by alpha blending (255 * 255 = 65025).
#[inline(always)]
pub fn div255(x: u32) -> u32 {
    (x + 1 + (x >> 8)) >> 8
}

pub(crate) fn min(a: u32, b: u32) -> u32 {
    if a < b {
        a
    } else {
        b
    }
}

pub(crate) fn abs(x: i32) -> i32 {
    if x < 0 {
        -x
    } else {
        x
    }
}

/// Integer square root of a 64-bit value in 8.8 fixed-point.
///
/// Given `x` in 16.16 fixed-point (i.e., the value `n * 256 * n * 256` where
/// `n` is in 8.8 fixed-point), returns `sqrt(x)` in 8.8 fixed-point.
/// Uses binary search with bit-at-a-time refinement. Never panics.
pub fn isqrt_fp(x: u64) -> u64 {
    if x == 0 {
        return 0;
    }
    let mut result: u64 = 0;
    let mut bit: u64 = 1u64 << 30; // Start from highest reasonable bit.

    // Find the highest bit position for square root.
    while bit > x {
        bit >>= 2;
    }

    while bit != 0 {
        let candidate = result + bit;
        if x >= candidate * candidate {
            result = candidate;
        }
        bit >>= 1;
    }

    result
}
