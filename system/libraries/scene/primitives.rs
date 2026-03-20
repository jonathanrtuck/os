//! Primitive types, constants, and helper functions for the scene graph.
//!
//! These are the leaf-level building blocks: colors, borders, data references,
//! path commands, content variants, hashing, and the bitflags macro used by
//! other modules.

use alloc::vec::Vec;

// ── Bitflags macro (must precede usage) ─────────────────────────────

macro_rules! bitflags {
    (
        $(#[$outer:meta])*
        pub struct $name:ident : $ty:ty {
            $(const $flag:ident = $val:expr;)*
        }
    ) => {
        $(#[$outer])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(transparent)]
        pub struct $name($ty);

        impl $name {
            $(pub const $flag: Self = Self($val);)*

            pub const fn bits(self) -> $ty { self.0 }
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }
            pub const fn empty() -> Self { Self(0) }
            pub const fn union(self, other: Self) -> Self { Self(self.0 | other.0) }
        }

        impl core::ops::BitAnd for $name {
            type Output = Self;

            fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
        }
        impl core::ops::BitOr for $name {
            type Output = Self;

            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }
    };
}

// Re-export the macro for use by sibling modules (node.rs).
pub(crate) use bitflags;

// ── Primitive types ─────────────────────────────────────────────────

/// Border specification: uniform width and color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Border {
    pub color: Color,
    pub width: u8,
    pub _pad: [u8; 3],
}
/// RGBA color, packed for shared memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

/// A reference to variable-length data in the data buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct DataRef {
    pub offset: u32,
    pub length: u32,
}

impl DataRef {
    pub const EMPTY: Self = Self { offset: 0, length: 0 };

    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }
}

// ── Content hashing ─────────────────────────────────────────────────

const FNV1A_OFFSET: u32 = 0x811c_9dc5;
const FNV1A_PRIME: u32 = 0x0100_0193;

/// FNV-1a hash of a byte slice (32-bit).
pub fn fnv1a(data: &[u8]) -> u32 {
    let mut h = FNV1A_OFFSET;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(FNV1A_PRIME);
    }
    h
}

// ── Shaped glyphs ───────────────────────────────────────────────────

/// A shaped glyph with individual positioning (proportional/shaped text).
///
/// Written by the OS service (via fonts library), stored in the scene
/// graph data buffer, and read by the compositor for rasterization.
/// All advance/offset values are in scaled pixel units (not font units).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct ShapedGlyph {
    /// Glyph ID in the font (0 = .notdef).
    pub glyph_id: u16,
    /// Horizontal advance width in scaled units.
    pub x_advance: i16,
    /// Horizontal offset from default position.
    pub x_offset: i16,
    /// Vertical offset from default position.
    pub y_offset: i16,
}

// Compile-time size assertion: ShapedGlyph must be exactly 8 bytes
// (4 × u16/i16 fields, #[repr(C)], no padding needed).
const _: () = assert!(core::mem::size_of::<ShapedGlyph>() == 8);

// ── Fill rule ───────────────────────────────────────────────────────

/// Fill rule for path rendering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum FillRule {
    /// Non-zero winding rule: fills all enclosed regions regardless of
    /// winding direction.
    Winding = 0,
    /// Even-odd rule: fills only regions enclosed an odd number of times.
    EvenOdd = 1,
}

// ── Path commands ───────────────────────────────────────────────────

/// Path command tags. Commands are variable-size, stored sequentially
/// in the data buffer.
pub const PATH_MOVE_TO: u32 = 0;
pub const PATH_LINE_TO: u32 = 1;
pub const PATH_CUBIC_TO: u32 = 2;
pub const PATH_CLOSE: u32 = 3;

/// Size in bytes of each path command.
pub const PATH_MOVE_TO_SIZE: usize = 12; // tag(4) + x(4) + y(4)
pub const PATH_LINE_TO_SIZE: usize = 12; // tag(4) + x(4) + y(4)
pub const PATH_CUBIC_TO_SIZE: usize = 28; // tag(4) + c1x(4) + c1y(4) + c2x(4) + c2y(4) + x(4) + y(4)
pub const PATH_CLOSE_SIZE: usize = 4; // tag(4)

/// Append a MoveTo command to a byte buffer.
pub fn path_move_to(buf: &mut Vec<u8>, x: f32, y: f32) {
    buf.extend_from_slice(&PATH_MOVE_TO.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
}

/// Append a LineTo command to a byte buffer.
pub fn path_line_to(buf: &mut Vec<u8>, x: f32, y: f32) {
    buf.extend_from_slice(&PATH_LINE_TO.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
}

/// Append a CubicTo command to a byte buffer.
pub fn path_cubic_to(buf: &mut Vec<u8>, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
    buf.extend_from_slice(&PATH_CUBIC_TO.to_le_bytes());
    buf.extend_from_slice(&c1x.to_le_bytes());
    buf.extend_from_slice(&c1y.to_le_bytes());
    buf.extend_from_slice(&c2x.to_le_bytes());
    buf.extend_from_slice(&c2y.to_le_bytes());
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
}

/// Append a Close command to a byte buffer.
pub fn path_close(buf: &mut Vec<u8>) {
    buf.extend_from_slice(&PATH_CLOSE.to_le_bytes());
}

// ── Content variant ─────────────────────────────────────────────────

/// What a node draws (beyond its container decoration).
///
/// Cursor and selection highlights use `Content::None` containers with
/// `node.background` set to the desired color. Text lines use `Glyphs`.
/// Each variant is geometric — the render backend needs no content-type
/// knowledge beyond these primitives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum Content {
    /// Pure container — no content, just children and decoration.
    /// Solid rectangle fills (cursor, selection) use this with
    /// `node.background` set to the desired color.
    None,
    /// A pixel buffer reference.
    Image {
        /// Reference to pixel data in the data buffer.
        data: DataRef,
        /// Source image dimensions.
        src_width: u16,
        src_height: u16,
    },
    /// Filled cubic Bezier contours. The render backend rasterizes them
    /// with scanline coverage (same engine as glyph outlines). Vector
    /// content scales cleanly at any display density.
    Path {
        /// Fill color.
        color: Color,
        /// Winding or even-odd fill rule.
        fill_rule: FillRule,
        /// Reference to serialized path commands in the data buffer
        /// (MoveTo, LineTo, CubicTo, Close). 4-byte aligned.
        contours: DataRef,
    },
    /// A single run of shaped glyphs — one font, one color, one glyph
    /// array. Multiple `Glyphs` nodes replace what was one multi-run
    /// `Text` node. The render backend looks up each glyph_id in the
    /// glyph cache and draws coverage at the correct position.
    Glyphs {
        /// Text color.
        color: Color,
        /// Reference to `ShapedGlyph` array in the data buffer.
        glyphs: DataRef,
        /// Number of glyphs in this run.
        glyph_count: u16,
        /// Font size in pixels (selects the glyph cache).
        font_size: u16,
        /// Hash of variable font axis values used for rasterization
        /// (0 = default). Used as glyph cache key.
        axis_hash: u32,
    },
}
