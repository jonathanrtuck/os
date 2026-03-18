//! Scene graph data structures for the compositor interface.
//!
//! The OS service builds a tree of `Node` values in shared memory.
//! The compositor reads this tree and renders it to pixels.
//!
//! # Memory layout
//!
//! A scene graph occupies a contiguous shared memory region:
//!
//! ```text
//! ┌──────────┬─────────────────────┬──────────────────────┐
//! │  Header  │  Node array         │  Data buffer          │
//! │  64 B    │  N × NODE_SIZE      │  variable-length      │
//! └──────────┴─────────────────────┴──────────────────────┘
//! ```
//!
//! - **Header:** generation counter, node count, data buffer usage.
//! - **Node array:** fixed-size entries, indexed by `NodeId`.
//! - **Data buffer:** text strings and path commands referenced by
//!   offset+length from nodes.
//!
//! # Design
//!
//! One node type with optional content (Core Animation model). Every node
//! can have children, visual decoration (background, border, corner radius),
//! and an optional content variant (Image, Glyphs). This avoids
//! wrapper nodes in compound documents where containers routinely need
//! backgrounds and borders.

#![no_std]

extern crate alloc;

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

// ── Primitive types ─────────────────────────────────────────────────

/// Index into the node array. `NULL` means no node.
pub type NodeId = u16;
pub const NULL: NodeId = u16::MAX;

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
/// A reference to variable-length data in the data buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct DataRef {
    pub offset: u32,
    pub length: u32,
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

bitflags! {
    /// Node flags packed into a single byte.
    pub struct NodeFlags: u8 {
        const CLIPS_CHILDREN = 0b0000_0001;
        const VISIBLE        = 0b0000_0010;
    }
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

// ── Path commands ───────────────────────────────────────────────────

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

// ── Affine transform ────────────────────────────────────────────────

/// 2D affine transform stored as a 3×3 matrix with the bottom row
/// implicitly `[0, 0, 1]`.
///
/// ```text
/// ┌         ┐
/// │ a  c  tx│
/// │ b  d  ty│
/// │ 0  0   1│
/// └         ┘
/// ```
///
/// Standard 2D affine convention: `a`, `b` are the first column,
/// `c`, `d` are the second column, `tx`, `ty` are the translation.
///
/// Transforming a point `(x, y)`:
///   `x' = a*x + c*y + tx`
///   `y' = b*x + d*y + ty`
///
/// Identity by default (a=1, d=1, all others 0).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct AffineTransform {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

// Compile-time size assertion: 6 × f32 = 24 bytes.
const _: () = assert!(core::mem::size_of::<AffineTransform>() == 24);

impl AffineTransform {
    /// Identity transform — no translation, rotation, scale, or skew.
    pub const fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Pure translation by `(x, y)`.
    pub const fn translate(x: f32, y: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: x,
            ty: y,
        }
    }

    /// Rotation by `radians` counter-clockwise.
    pub fn rotate(radians: f32) -> Self {
        let (sin, cos) = sin_cos_f32(radians);
        Self {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Non-uniform scale by `(sx, sy)`.
    pub const fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Horizontal skew (shear) by `angle` radians.
    /// The x-coordinate is shifted proportional to the y-coordinate:
    /// `x' = x + tan(angle) * y`.
    pub fn skew_x(angle: f32) -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: tan_f32(angle),
            d: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// Matrix multiplication: `self × other`.
    ///
    /// The resulting transform applies `other` first, then `self`.
    /// Use to compose parent and child transforms:
    /// `world = parent.compose(child)`.
    pub fn compose(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            tx: self.a * other.tx + self.c * other.ty + self.tx,
            ty: self.b * other.tx + self.d * other.ty + self.ty,
        }
    }

    /// Compute the inverse of this affine transform.
    /// Returns `None` if the matrix is singular (determinant ≈ 0).
    pub fn inverse(&self) -> Option<Self> {
        let det = self.a * self.d - self.b * self.c;
        if det > -1e-10 && det < 1e-10 {
            return None; // Singular matrix.
        }
        let inv_det = 1.0 / det;
        Some(Self {
            a: self.d * inv_det,
            b: -self.b * inv_det,
            c: -self.c * inv_det,
            d: self.a * inv_det,
            tx: (self.c * self.ty - self.d * self.tx) * inv_det,
            ty: (self.b * self.tx - self.a * self.ty) * inv_det,
        })
    }

    /// Returns `true` if this is the identity transform.
    pub fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx == 0.0
            && self.ty == 0.0
    }

    /// Returns `true` if this is a pure integer translation (no rotation,
    /// scale, or skew). Integer translations can be applied as an exact
    /// pixel shift with no resampling.
    pub fn is_integer_translation(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.tx == (self.tx as i32) as f32
            && self.ty == (self.ty as i32) as f32
    }

    /// Transform a point `(x, y)` by this matrix.
    pub fn transform_point(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.tx,
            self.b * x + self.d * y + self.ty,
        )
    }

    /// Compute the axis-aligned bounding box (AABB) of a rectangle
    /// `(x, y, w, h)` after applying this transform.
    ///
    /// Returns `(min_x, min_y, width, height)` of the bounding box.
    pub fn transform_aabb(&self, x: f32, y: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
        // Transform all four corners of the input rectangle.
        let (x0, y0) = self.transform_point(x, y);
        let (x1, y1) = self.transform_point(x + w, y);
        let (x2, y2) = self.transform_point(x + w, y + h);
        let (x3, y3) = self.transform_point(x, y + h);

        let min_x = min4(x0, x1, x2, x3);
        let min_y = min4(y0, y1, y2, y3);
        let max_x = max4(x0, x1, x2, x3);
        let max_y = max4(y0, y1, y2, y3);

        (min_x, min_y, max_x - min_x, max_y - min_y)
    }
}

/// Minimum of four f32 values.
fn min4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let ab = if a < b { a } else { b };
    let cd = if c < d { c } else { d };
    if ab < cd {
        ab
    } else {
        cd
    }
}

/// Maximum of four f32 values.
fn max4(a: f32, b: f32, c: f32, d: f32) -> f32 {
    let ab = if a > b { a } else { b };
    let cd = if c > d { c } else { d };
    if ab > cd {
        ab
    } else {
        cd
    }
}

/// Sine and cosine for `no_std`. Uses Taylor series with Payne-Hanek-style
/// range reduction to `[-π/2, π/2]` for accuracy across the full circle.
fn sin_cos_f32(x: f32) -> (f32, f32) {
    // Range-reduce to [-π, π].
    let pi = core::f32::consts::PI;
    let two_pi = 2.0 * pi;
    let half_pi = core::f32::consts::FRAC_PI_2;
    let mut a = x;
    if a > pi || a < -pi {
        a = a - ((a / two_pi).floor_f32() * two_pi);
        if a > pi {
            a -= two_pi;
        }
    }

    // Further reduce to [-π/2, π/2] using symmetry.
    let (sin_sign, cos_sign, reduced) = if a > half_pi {
        // sin(π - a) = sin(a), cos(π - a) = -cos(a)
        (1.0_f32, -1.0_f32, pi - a)
    } else if a < -half_pi {
        // sin(-π - a) = -sin(-a) = sin(a), cos(-π - a) = -cos(a)
        (1.0_f32, -1.0_f32, -pi - a)
    } else {
        (1.0_f32, 1.0_f32, a)
    };

    // Taylor series around 0, valid for [-π/2, π/2] with excellent accuracy.
    let a2 = reduced * reduced;

    // sin: x - x³/6 + x⁵/120 - x⁷/5040 + x⁹/362880 - x¹¹/39916800
    let sin_val = reduced
        * (1.0
            - a2 / 6.0
                * (1.0 - a2 / 20.0 * (1.0 - a2 / 42.0 * (1.0 - a2 / 72.0 * (1.0 - a2 / 110.0)))));

    // cos: 1 - x²/2 + x⁴/24 - x⁶/720 + x⁸/40320 - x¹⁰/3628800
    let cos_val = 1.0
        - a2 / 2.0 * (1.0 - a2 / 12.0 * (1.0 - a2 / 30.0 * (1.0 - a2 / 56.0 * (1.0 - a2 / 90.0))));

    (sin_sign * sin_val, cos_sign * cos_val)
}

/// Tangent for `no_std`. Computed as `sin / cos`.
fn tan_f32(x: f32) -> f32 {
    let (sin, cos) = sin_cos_f32(x);
    if cos.abs_f32() < 1e-10 {
        if sin >= 0.0 {
            f32::MAX
        } else {
            f32::MIN
        }
    } else {
        sin / cos
    }
}

/// Helper trait for f32 methods missing in `no_std`.
trait F32Ext {
    fn floor_f32(self) -> f32;
    fn abs_f32(self) -> f32;
}

impl F32Ext for f32 {
    #[inline]
    fn floor_f32(self) -> f32 {
        let i = self as i32;
        let f = i as f32;
        if self < f {
            f - 1.0
        } else {
            f
        }
    }

    #[inline]
    fn abs_f32(self) -> f32 {
        if self < 0.0 {
            -self
        } else {
            self
        }
    }
}

// ── Node ────────────────────────────────────────────────────────────

/// A single node in the scene graph.
///
/// Fixed size for flat array storage in shared memory. Tree structure is
/// encoded via `first_child` / `next_sibling` indices (left-child
/// right-sibling representation).
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct Node {
    // ── tree ──
    pub first_child: NodeId,
    pub next_sibling: NodeId,
    // ── geometry (relative to parent content area) ──
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    // ── scrolling ──
    /// Vertical scroll offset applied to children.
    pub scroll_y: i32,
    // ── visual decoration ──
    pub background: Color,
    pub border: Border,
    pub corner_radius: u8,
    pub opacity: u8,
    // ── flags ──
    pub flags: NodeFlags,
    pub _pad: u8,
    // ── shadow ──
    /// Shadow color (TRANSPARENT = no shadow).
    pub shadow_color: Color,
    /// Horizontal shadow offset in logical pixels.
    pub shadow_offset_x: i16,
    /// Vertical shadow offset in logical pixels.
    pub shadow_offset_y: i16,
    /// Shadow blur radius in logical pixels (0 = hard shadow).
    pub shadow_blur_radius: u8,
    /// Shadow spread in logical pixels (positive expands, negative shrinks).
    pub shadow_spread: i8,
    pub _shadow_pad: [u8; 2],
    // ── transform ──
    /// 2D affine transform applied to this node during rendering.
    /// Identity by default (no effect). The compositor maintains a
    /// transform stack: world = parent_world × node_local.
    pub transform: AffineTransform,
    // ── content hash (FNV-1a of variable-length data referenced by Content) ──
    /// Hash of the node's variable-length data (glyph arrays, image pixels).
    /// Computed by the scene writer when content is set. The compositor
    /// uses this for scene diffing — a changed hash means the data buffer
    /// content changed even if the DataRef is identical.
    pub content_hash: u32,
    // ── content ──
    pub content: Content,
}

impl Node {
    pub const EMPTY: Self = Self {
        first_child: NULL,
        next_sibling: NULL,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        scroll_y: 0,
        background: Color::TRANSPARENT,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0,
            _pad: [0; 3],
        },
        corner_radius: 0,
        opacity: 255,
        flags: NodeFlags::VISIBLE,
        _pad: 0,
        shadow_color: Color::TRANSPARENT,
        shadow_offset_x: 0,
        shadow_offset_y: 0,
        shadow_blur_radius: 0,
        shadow_spread: 0,
        _shadow_pad: [0; 2],
        transform: AffineTransform::identity(),
        content_hash: 0,
        content: Content::None,
    };

    /// Returns true if this node has a non-default shadow (any shadow
    /// field is non-zero/non-transparent).
    pub fn has_shadow(&self) -> bool {
        self.shadow_color.a > 0
            && (self.shadow_blur_radius > 0
                || self.shadow_offset_x != 0
                || self.shadow_offset_y != 0
                || self.shadow_spread != 0)
    }

    pub fn clips_children(&self) -> bool {
        self.flags.contains(NodeFlags::CLIPS_CHILDREN)
    }
    pub fn visible(&self) -> bool {
        self.flags.contains(NodeFlags::VISIBLE)
    }
}

// Compile-time size assertion: Node must be exactly 96 bytes.
// This prevents silent shared-memory layout drift between core and compositor.
// If you add a field, update this assertion and verify both sides agree.
const _: () = assert!(core::mem::size_of::<Node>() == 96);

// ── Shared memory layout ────────────────────────────────────────────

pub const MAX_NODES: usize = 512;
pub const DATA_BUFFER_SIZE: usize = 64 * 1024;
pub const NODES_OFFSET: usize = core::mem::size_of::<SceneHeader>();
pub const DATA_OFFSET: usize = NODES_OFFSET + MAX_NODES * core::mem::size_of::<Node>();
pub const SCENE_SIZE: usize = DATA_OFFSET + DATA_BUFFER_SIZE;

const _: () = assert!(core::mem::size_of::<SceneHeader>() == 64);

/// Maximum number of changed node IDs that fit in the scene header's
/// change list. Sized to fill the 52-byte reserved area alongside
/// `change_count` (u16) and 2 bytes padding: (52 - 2 - 2) / 2 = 24.
pub const CHANGE_LIST_CAPACITY: usize = 24;

/// Sentinel value for `SceneHeader::change_count` indicating that the
/// change list overflowed (or a full rebuild occurred) and the compositor
/// must repaint the entire screen.
pub const FULL_REPAINT: u16 = u16::MAX;

/// Header at the start of the shared memory region.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct SceneHeader {
    /// Incremented by the writer after each update.
    pub generation: u32,
    /// Number of live nodes in the node array.
    pub node_count: u16,
    /// Index of the root node (usually 0).
    pub root: NodeId,
    /// Bytes used in the data buffer.
    pub data_used: u32,
    /// Number of entries in `changed_nodes`, or `FULL_REPAINT` sentinel.
    pub change_count: u16,
    /// Node IDs that changed this frame (valid entries: `0..change_count`).
    pub changed_nodes: [NodeId; CHANGE_LIST_CAPACITY],
    pub _reserved2: [u8; 2],
}

// ── SceneWriter ─────────────────────────────────────────────────────

const NODE_SIZE: usize = core::mem::size_of::<Node>();

/// Builds and mutates a scene graph in a flat byte buffer conforming
/// to the shared memory layout (Header + Node array + Data buffer).
///
/// The writer operates on a `&mut [u8]` of at least `SCENE_SIZE` bytes.
/// In the process split, the OS service writes to shared memory via
/// this API; the compositor reads via `SceneReader`.
pub struct SceneWriter<'a> {
    buf: &'a mut [u8],
}

impl<'a> SceneWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        // SAFETY: buf is at least SCENE_SIZE bytes (asserted above).
        // SceneHeader is repr(C) at offset 0 with size <= SCENE_SIZE.
        // Exclusive &mut borrow prevents aliasing.
        let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut SceneHeader) };

        hdr.generation = 0;
        hdr.node_count = 0;
        hdr.root = NULL;
        hdr.data_used = 0;
        hdr.change_count = 0;
        hdr.changed_nodes = [NULL; CHANGE_LIST_CAPACITY];

        Self { buf }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer. The shared borrow on `self` prevents concurrent mutation.
        unsafe { &*(self.buf.as_ptr() as *const SceneHeader) }
    }
    fn header_mut(&mut self) -> &mut SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer. The exclusive borrow on `self` prevents aliasing.
        unsafe { &mut *(self.buf.as_mut_ptr() as *mut SceneHeader) }
    }

    /// Link `child` as the last child of `parent`.
    pub fn add_child(&mut self, parent: NodeId, child: NodeId) {
        debug_assert!(parent != child, "add_child: self-parenting");
        let first = self.node(parent).first_child;

        if first == NULL {
            self.node_mut(parent).first_child = child;

            return;
        }

        // Walk to the last sibling.
        let mut cur = first;

        loop {
            let next = self.node(cur).next_sibling;

            if next == NULL {
                break;
            }

            cur = next;
        }

        self.node_mut(cur).next_sibling = child;
    }
    /// Allocate a new node slot. Returns `None` if the array is full.
    /// The node is initialized to `Node::EMPTY`.
    pub fn alloc_node(&mut self) -> Option<NodeId> {
        let count = self.header().node_count;

        if (count as usize) >= MAX_NODES {
            return None;
        }

        self.header_mut().node_count = count + 1;

        let id = count;
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: offset is within bounds (checked by MAX_NODES cap).
        unsafe {
            let ptr = self.buf.as_mut_ptr().add(offset) as *mut Node;

            core::ptr::write(ptr, Node::EMPTY);
        }

        Some(id)
    }
    /// Reset node count and data usage. Preserves generation.
    /// Sets change_count to FULL_REPAINT — a full rebuild means the
    /// compositor must repaint the entire screen.
    pub fn clear(&mut self) {
        self.header_mut().node_count = 0;
        self.header_mut().data_used = 0;
        self.header_mut().root = NULL;
        self.header_mut().change_count = FULL_REPAINT;
    }
    /// Increment the generation counter (signals a complete update).
    pub fn commit(&mut self) {
        let g = self.header().generation;

        self.header_mut().generation = g.wrapping_add(1);
    }
    /// Get the used portion of the data buffer as a read-only slice.
    pub fn data_buf(&self) -> &[u8] {
        let used = (self.data_used() as usize).min(DATA_BUFFER_SIZE);

        &self.buf[DATA_OFFSET..DATA_OFFSET + used]
    }
    pub fn data_used(&self) -> u32 {
        self.header().data_used
    }
    /// Wrap a previously initialized buffer without resetting state.
    pub fn from_existing(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        Self { buf }
    }
    pub fn generation(&self) -> u32 {
        self.header().generation
    }
    /// Record a node ID in the change list. If the list is already at
    /// capacity, sets the FULL_REPAINT sentinel instead. Duplicate IDs
    /// are stored as-is (the compositor treats them as a set).
    pub fn mark_changed(&mut self, node_id: NodeId) {
        let hdr = self.header_mut();

        if hdr.change_count == FULL_REPAINT {
            return; // already overflowed
        }

        let idx = hdr.change_count as usize;

        if idx >= CHANGE_LIST_CAPACITY {
            hdr.change_count = FULL_REPAINT;

            return;
        }

        hdr.changed_nodes[idx] = node_id;
        hdr.change_count = (idx + 1) as u16;
    }
    /// Get a shared reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        debug_assert!((id as usize) < MAX_NODES, "NodeId out of bounds");
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: `id` is a NodeId returned by `alloc_node` (bounded by
        // MAX_NODES), so `offset` is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. Shared borrow prevents mutation.
        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    /// Set the node count directly.
    ///
    /// Used to truncate the node array (e.g., removing dynamic selection
    /// rect nodes by resetting count to the well-known node count).
    /// The caller must ensure `count` does not exceed the previously
    /// allocated high-water mark within the current buffer.
    pub fn set_node_count(&mut self, count: u16) {
        self.header_mut().node_count = count;
    }
    /// Get a mutable reference to a node by ID.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        debug_assert!((id as usize) < MAX_NODES, "NodeId out of bounds");
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: Same bounds reasoning as `node()`. Exclusive borrow on
        // `self` prevents aliasing.
        unsafe { &mut *(self.buf.as_mut_ptr().add(offset) as *mut Node) }
    }
    /// Get all live nodes as a read-only slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        // SAFETY: NODES_OFFSET is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. `count` <= MAX_NODES.
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        // Shared borrow on `self` prevents concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    /// Append bytes to the data buffer. Returns a `DataRef`.
    /// If the buffer is full, truncates to fit.
    pub fn push_data(&mut self, bytes: &[u8]) -> DataRef {
        let used = self.header().data_used;
        let avail = DATA_BUFFER_SIZE.saturating_sub(used as usize);
        let actual = if bytes.len() < avail {
            bytes.len()
        } else {
            avail
        };

        if actual > 0 {
            let start = DATA_OFFSET + used as usize;

            self.buf[start..start + actual].copy_from_slice(&bytes[..actual]);

            self.header_mut().data_used = used + actual as u32;
        }

        DataRef {
            offset: used,
            length: actual as u32,
        }
    }
    /// Push serialized path commands into the data buffer.
    /// Ensures 4-byte alignment (f32 alignment) before writing.
    /// Returns a `DataRef` covering the path command data.
    pub fn push_path_commands(&mut self, commands: &[u8]) -> DataRef {
        // Align data_used to 4 bytes (f32 alignment).
        let align = 4usize;
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            self.header_mut().data_used = aligned as u32;
        }

        self.push_data(commands)
    }
    /// Push an array of `ShapedGlyph` structs into the data buffer.
    /// Aligns the write offset to `align_of::<ShapedGlyph>()` first.
    /// Returns a `DataRef` covering the glyph data.
    pub fn push_shaped_glyphs(&mut self, glyphs: &[ShapedGlyph]) -> DataRef {
        // Align data_used to ShapedGlyph alignment (2 bytes for i16/u16).
        let align = core::mem::align_of::<ShapedGlyph>();
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            self.header_mut().data_used = aligned as u32;
        }

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding, so
        // transmuting to bytes is safe for serialization.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                glyphs.as_ptr() as *const u8,
                glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
            )
        };

        self.push_data(bytes)
    }

    /// Append new data (old DataRef is abandoned — bump allocator).
    pub fn replace_data(&mut self, bytes: &[u8]) -> DataRef {
        self.push_data(bytes)
    }
    /// Reset the data buffer usage counter (bump allocator rewind).
    pub fn reset_data(&mut self) {
        self.header_mut().data_used = 0;
    }
    pub fn root(&self) -> NodeId {
        self.header().root
    }
    pub fn set_root(&mut self, id: NodeId) {
        self.header_mut().root = id;
    }
    /// Overwrite an existing DataRef in place (must be same length).
    /// Returns true on success, false if lengths don't match.
    pub fn update_data(&mut self, dref: DataRef, bytes: &[u8]) -> bool {
        if bytes.len() != dref.length as usize {
            return false;
        }

        let start = DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end > DATA_OFFSET + DATA_BUFFER_SIZE {
            return false;
        }

        self.buf[start..end].copy_from_slice(bytes);

        true
    }
}

// ── SceneReader ─────────────────────────────────────────────────────

/// Read-only view of a scene graph in a flat byte buffer.
///
/// The compositor uses this to walk the tree and render to pixels.
/// Operates on the same shared memory layout as `SceneWriter`.
pub struct SceneReader<'a> {
    buf: &'a [u8],
}

impl<'a> SceneReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        assert!(buf.len() >= SCENE_SIZE);

        Self { buf }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: SceneHeader is repr(C) at offset 0 within the SCENE_SIZE
        // buffer (asserted in `new`). Shared borrow prevents mutation.
        unsafe { &*(self.buf.as_ptr() as *const SceneHeader) }
    }

    /// Resolve a `DataRef` to a byte slice.
    /// Returns an empty slice if the reference is out of bounds.
    pub fn data(&self, dref: DataRef) -> &[u8] {
        let start = DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.buf.len() && dref.offset + dref.length <= self.header().data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }
    /// Get the used portion of the data buffer as a slice.
    pub fn data_buf(&self) -> &[u8] {
        let used = (self.data_used() as usize).min(DATA_BUFFER_SIZE);

        &self.buf[DATA_OFFSET..DATA_OFFSET + used]
    }
    pub fn data_used(&self) -> u32 {
        self.header().data_used
    }
    pub fn generation(&self) -> u32 {
        self.header().generation
    }
    /// Get a reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        debug_assert!(
            (id as usize) < MAX_NODES,
            "NodeId out of bounds in reader"
        );
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        // SAFETY: `id` is a valid NodeId (bounded by node_count <= MAX_NODES),
        // so `offset` is within the SCENE_SIZE buffer. Node is repr(C).
        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    /// Get all live nodes as a slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        // SAFETY: NODES_OFFSET is within the SCENE_SIZE buffer. Node is
        // repr(C) with size NODE_SIZE. `count` <= MAX_NODES.
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        // Shared borrow on `self` prevents concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    pub fn root(&self) -> NodeId {
        self.header().root
    }
    /// Interpret a DataRef as an array of `ShapedGlyph` structs.
    ///
    /// `glyph_count` is the number of glyphs expected (from `Content::Glyphs`).
    /// Returns a slice of up to `glyph_count` glyphs, or fewer if the data
    /// buffer doesn't contain enough bytes.
    pub fn shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)], data buffer is aligned by
        // push_shaped_glyphs to ShapedGlyph alignment.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }
}

// ── Triple-buffered scene graph (mailbox semantics) ─────────────────

/// Control region layout at the end of the triple-buffer shared memory.
///
/// ```text
/// Offset 0: latest_buf   (u32) — index (0,1,2) of the most recently published buffer
/// Offset 4: reader_buf   (u32) — index of buffer reader is using (0xFF = none)
/// Offset 8: generation    (u32) — global generation, incremented on each publish()
/// Offset 12: reader_done_gen (u32) — generation reader last finished reading
/// ```
const TRIPLE_CONTROL_SIZE: usize = 16;

/// Total size for a triple-buffered scene graph: three full scene buffers
/// plus a control region for lock-free coordination.
///
/// Mailbox semantics: the writer always has a free buffer (never blocks),
/// the reader always gets the most recent published buffer (intermediate
/// frames are silently skipped). Three buffers: one for reader, one
/// "latest" (published), one free for writer.
pub const TRIPLE_SCENE_SIZE: usize = 3 * SCENE_SIZE + TRIPLE_CONTROL_SIZE;

/// Compile-time assertion: TRIPLE_SCENE_SIZE is exactly what we expect.
const _: () = assert!(TRIPLE_SCENE_SIZE == 3 * SCENE_SIZE + 16);

/// Byte offset of the control region within the triple-buffer layout.
const TRIPLE_CONTROL_OFFSET: usize = 3 * SCENE_SIZE;

/// Sentinel value for `reader_buf` meaning no reader is active.
const NO_READER: u32 = 0xFF;

/// Read a u32 field from the control region at the given byte offset
/// within the control region. Uses volatile to prevent reordering.
fn triple_read_ctrl(buf: &[u8], field_offset: usize) -> u32 {
    // SAFETY: TRIPLE_CONTROL_OFFSET + field_offset is within the
    // TRIPLE_SCENE_SIZE buffer. The field is a u32 at a 4-byte aligned
    // offset (TRIPLE_CONTROL_OFFSET = 3 * SCENE_SIZE, a multiple of 4).
    unsafe {
        core::ptr::read_volatile(
            buf.as_ptr().add(TRIPLE_CONTROL_OFFSET + field_offset) as *const u32
        )
    }
}

/// Write a u32 field to the control region at the given byte offset
/// within the control region. Uses volatile to prevent reordering.
fn triple_write_ctrl(buf: &[u8], field_offset: usize, value: u32) {
    // SAFETY: TRIPLE_CONTROL_OFFSET + field_offset is within the
    // TRIPLE_SCENE_SIZE buffer. We cast away const because the control
    // region is conceptually shared mutable state accessed via volatile
    // (same pattern as the double-buffer's reader_done_gen).
    unsafe {
        core::ptr::write_volatile(
            buf.as_ptr().add(TRIPLE_CONTROL_OFFSET + field_offset) as *mut u32,
            value,
        )
    }
}

/// Write a u32 field to the control region with a preceding release fence.
fn triple_write_ctrl_release(buf: &[u8], field_offset: usize, value: u32) {
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
    triple_write_ctrl(buf, field_offset, value);
}

// Control region field offsets.
const CTRL_LATEST_BUF: usize = 0;
const CTRL_READER_BUF: usize = 4;
const CTRL_GENERATION: usize = 8;
const CTRL_READER_DONE_GEN: usize = 12;

/// Mutable access to a triple-buffered scene graph (mailbox semantics).
///
/// The OS service uses this to write scenes and publish them. `acquire()`
/// always returns a free buffer — never blocks, never fails. `publish()`
/// atomically makes the acquired buffer the latest. Intermediate frames
/// are silently skipped (only the latest matters for interactive UI).
pub struct TripleWriter<'a> {
    buf: &'a mut [u8],
    /// Index (0, 1, or 2) of the buffer currently acquired by the writer.
    /// Set by `acquire()`, consumed by `publish()`.
    acquired: u32,
}

/// Read-only access to a triple-buffered scene graph (mailbox semantics).
///
/// The compositor uses this when reading from shared memory written by
/// the OS service. Construction atomically claims the latest published
/// buffer. All reads within a single `TripleReader` instance reference
/// the same physical buffer.
pub struct TripleReader<'a> {
    buf: &'a [u8],
    /// Byte offset of the buffer being read (0, SCENE_SIZE, or 2*SCENE_SIZE).
    read_off: usize,
    /// Generation of the buffer being read.
    read_gen: u32,
}

/// Find the byte offset of buffer `idx` (0, 1, or 2).
#[inline]
fn buf_offset(idx: u32) -> usize {
    (idx as usize) * SCENE_SIZE
}

/// Find the free buffer index: the one that is neither `a` nor `b`.
/// Precondition: a != b, both in {0, 1, 2}.
#[inline]
fn free_index(a: u32, b: u32) -> u32 {
    // 0 + 1 + 2 = 3. The free one is 3 - a - b.
    3 - a - b
}

impl<'a> TripleWriter<'a> {
    /// Initialize a new triple-buffered scene graph. All three buffers
    /// are initialized to empty scenes. Buffer 0 starts as the "latest"
    /// (published) buffer with generation 0.
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        // Initialize all three scene buffer headers.
        {
            let (b0, rest) = buf.split_at_mut(SCENE_SIZE);
            let _ = SceneWriter::new(b0);
            let (b1, rest2) = rest.split_at_mut(SCENE_SIZE);
            let _ = SceneWriter::new(b1);
            let _ = SceneWriter::new(&mut rest2[..SCENE_SIZE]);
        }

        // Initialize control region.
        // SAFETY: Control region is within the TRIPLE_SCENE_SIZE buffer.
        unsafe {
            let ctrl = buf.as_mut_ptr().add(TRIPLE_CONTROL_OFFSET);
            // latest_buf = 0 (buffer 0 is the initial "latest")
            core::ptr::write_volatile(ctrl as *mut u32, 0);
            // reader_buf = NO_READER (no reader connected)
            core::ptr::write_volatile(ctrl.add(4) as *mut u32, NO_READER);
            // generation = 0
            core::ptr::write_volatile(ctrl.add(8) as *mut u32, 0);
            // reader_done_gen = 0
            core::ptr::write_volatile(ctrl.add(12) as *mut u32, 0);
        }

        Self { buf, acquired: 1 } // Writer starts with buffer 1 as acquired (free)
    }

    /// Wrap a previously initialized triple buffer without resetting.
    pub fn from_existing(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        // Determine a free buffer to use as initial acquired slot.
        let latest = triple_read_ctrl(buf, CTRL_LATEST_BUF);
        let reader = triple_read_ctrl(buf, CTRL_READER_BUF);
        let free = if reader == NO_READER || reader > 2 || reader == latest {
            // Pick any buffer that isn't latest.
            if latest == 0 {
                1
            } else {
                0
            }
        } else {
            free_index(latest, reader)
        };

        Self {
            buf,
            acquired: free,
        }
    }

    /// Acquire a free buffer for writing. Always succeeds — the writer
    /// always has a buffer that neither the reader nor the "latest" slot
    /// is using. Returns a `SceneWriter` for the acquired buffer.
    ///
    /// The returned `SceneWriter` operates on a clean buffer from the
    /// caller's perspective — the caller should call `clear()` to reset
    /// it, or use `from_existing` semantics to continue from previous
    /// state (the buffer may contain stale data from a previous frame).
    ///
    /// For incremental updates, call `copy_latest_to_acquired()` first,
    /// then `acquire()` to get a writable view of the copied buffer.
    pub fn acquire(&mut self) -> SceneWriter<'_> {
        self.select_free_buffer();

        let off = buf_offset(self.acquired);
        SceneWriter::from_existing(&mut self.buf[off..off + SCENE_SIZE])
    }

    /// Select a free buffer for writing without returning a SceneWriter.
    /// Use this before `copy_latest_to_acquired()` when you need to
    /// copy first and then get a writer via a second `acquire()` call.
    fn select_free_buffer(&mut self) {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        let reader = triple_read_ctrl(self.buf, CTRL_READER_BUF);

        // Find a free buffer (not latest, not reader).
        // When reader == latest or reader is inactive (NO_READER),
        // there are two free buffers — pick the first one that isn't latest.
        self.acquired = if reader == NO_READER || reader > 2 || reader == latest {
            // Pick any buffer that isn't latest.
            if latest == 0 {
                1
            } else if latest == 1 {
                2
            } else {
                0
            }
        } else {
            // Reader and latest are different — exactly one buffer is free.
            free_index(latest, reader)
        };
    }

    /// Publish the acquired buffer as the new "latest". Atomically swaps
    /// the latest pointer. The old latest becomes the new free buffer.
    /// Increments the global generation counter.
    ///
    /// A release fence ensures all scene data written via the `SceneWriter`
    /// returned by `acquire()` is visible before the latest pointer update.
    pub fn publish(&mut self) {
        // Increment global generation.
        let gen = triple_read_ctrl(self.buf, CTRL_GENERATION).wrapping_add(1);

        // Write generation into the acquired buffer's header.
        write_generation(self.buf, buf_offset(self.acquired), gen);

        // Update control region: generation first, then publish latest_buf
        // with a release fence so all scene data + generation are visible
        // before the reader sees the new latest_buf pointer.
        triple_write_ctrl(self.buf, CTRL_GENERATION, gen);
        triple_write_ctrl_release(self.buf, CTRL_LATEST_BUF, self.acquired);
    }

    /// Read the current global generation counter.
    pub fn generation(&self) -> u32 {
        triple_read_ctrl(self.buf, CTRL_GENERATION)
    }

    /// Get a read-only view of the latest published buffer's nodes.
    pub fn latest_nodes(&self) -> &[Node] {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: `off` is a valid scene buffer offset. SceneHeader is repr(C)
        // at the buffer start.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let count = hdr.node_count as usize;
        // SAFETY: NODES_OFFSET is within each SCENE_SIZE buffer. Node is repr(C).
        let ptr = unsafe { self.buf.as_ptr().add(off + NODES_OFFSET) as *const Node };
        // SAFETY: `ptr` points to `count` contiguous Node-sized entries.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }

    /// Generation counter of the latest published buffer.
    pub fn latest_generation(&self) -> u32 {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        read_generation(self.buf, buf_offset(latest))
    }

    /// Data buffer slice from the latest published buffer.
    pub fn latest_data_buf(&self) -> &[u8] {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: Same as latest_nodes.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let used = (hdr.data_used as usize).min(DATA_BUFFER_SIZE);
        &self.buf[off + DATA_OFFSET..off + DATA_OFFSET + used]
    }

    /// Resolve a DataRef against the latest published buffer.
    pub fn latest_data(&self, dref: DataRef) -> &[u8] {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        let off = buf_offset(latest);
        // SAFETY: Same as latest_nodes.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;
        if end <= self.buf.len() && dref.offset + dref.length <= hdr.data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }

    /// Interpret a DataRef from the latest buffer as ShapedGlyph array.
    pub fn latest_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.latest_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();
        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }
        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);
        // SAFETY: ShapedGlyph is #[repr(C)] with no padding.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }

    /// Acquire a free buffer and copy the latest published buffer into it.
    /// Returns a `SceneWriter` for the acquired buffer, pre-populated with
    /// the latest scene state. The caller can then mutate specific nodes
    /// and call `publish()`.
    ///
    /// This is the triple-buffer equivalent of the old `copy_front_to_back()`
    /// + `back()` pattern, but it always succeeds — the acquired buffer is
    /// always free (not held by the reader).
    pub fn acquire_copy(&mut self) -> SceneWriter<'_> {
        self.select_free_buffer();
        self.copy_latest_to_acquired_inner();

        let off = buf_offset(self.acquired);
        SceneWriter::from_existing(&mut self.buf[off..off + SCENE_SIZE])
    }

    /// Copy the latest published buffer into the acquired buffer.
    /// This enables the copy-forward pattern: copy the previous frame,
    /// mutate specific nodes, then publish. Unlike the old double-buffer
    /// `copy_front_to_back()`, this always succeeds — the acquired buffer
    /// is always free (not held by the reader).
    ///
    /// The acquired buffer's change list is reset to empty. The generation
    /// is NOT copied — it will be set by the next `publish()` call.
    ///
    /// Must be called after `select_free_buffer()` / `acquire()` and
    /// before `publish()`.
    fn copy_latest_to_acquired_inner(&mut self) {
        let latest = triple_read_ctrl(self.buf, CTRL_LATEST_BUF);
        let src_off = buf_offset(latest);
        let dst_off = buf_offset(self.acquired);

        // Read source header to determine how much to copy.
        // SAFETY: src_off is a valid scene buffer offset (0, SCENE_SIZE,
        // or 2*SCENE_SIZE). SceneHeader is repr(C) at the start.
        let src_hdr =
            unsafe { core::ptr::read(self.buf.as_ptr().add(src_off) as *const SceneHeader) };

        let node_count = src_hdr.node_count;
        let data_used = src_hdr.data_used;

        // Copy node array (only live nodes).
        let node_bytes = node_count as usize * NODE_SIZE;
        if node_bytes > 0 {
            // SAFETY: src and dst are valid scene buffer offsets that don't
            // overlap (acquired != latest). NODES_OFFSET + node_bytes is
            // within SCENE_SIZE (bounded by MAX_NODES * NODE_SIZE).
            unsafe {
                let src = self.buf.as_ptr().add(src_off + NODES_OFFSET);
                let dst = self.buf.as_mut_ptr().add(dst_off + NODES_OFFSET);
                core::ptr::copy_nonoverlapping(src, dst, node_bytes);
            }
        }

        // Copy data buffer (only used portion).
        let data_bytes = data_used as usize;
        if data_bytes > 0 {
            // SAFETY: Same reasoning — DATA_OFFSET + data_bytes is within
            // SCENE_SIZE. src and dst don't overlap.
            unsafe {
                let src = self.buf.as_ptr().add(src_off + DATA_OFFSET);
                let dst = self.buf.as_mut_ptr().add(dst_off + DATA_OFFSET);
                core::ptr::copy_nonoverlapping(src, dst, data_bytes);
            }
        }

        // Write destination header: copy source metadata, reset change list.
        // SAFETY: dst_off is a valid scene buffer offset. SceneHeader is
        // repr(C) at offset 0. Exclusive &mut borrow prevents aliasing.
        let dst_hdr = unsafe { &mut *(self.buf.as_mut_ptr().add(dst_off) as *mut SceneHeader) };
        dst_hdr.node_count = node_count;
        dst_hdr.root = src_hdr.root;
        dst_hdr.data_used = data_used;
        dst_hdr.change_count = 0;
        dst_hdr.changed_nodes = [NULL; CHANGE_LIST_CAPACITY];
    }

    /// Get the index of the buffer currently acquired by the writer.
    pub fn acquired_index(&self) -> u32 {
        self.acquired
    }
}

impl<'a> TripleReader<'a> {
    /// Claim the latest published buffer for reading. The reader atomically
    /// takes ownership of the latest buffer — the writer will not write to
    /// it. All reads within this `TripleReader` reference the same buffer.
    pub fn new(buf: &'a [u8]) -> Self {
        assert!(buf.len() >= TRIPLE_SCENE_SIZE);

        // Read the latest buffer index published by the writer.
        let latest = triple_read_ctrl(buf, CTRL_LATEST_BUF);

        // Acquire fence: pairs with the writer's release fence in publish().
        // Ensures all scene data written before publish() is visible to us
        // before we access the buffer contents.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        // Claim this buffer so the writer won't recycle it.
        triple_write_ctrl(buf, CTRL_READER_BUF, latest);

        let read_off = buf_offset(latest);
        let read_gen = read_generation(buf, read_off);

        Self {
            buf,
            read_off,
            read_gen,
        }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: read_off is a valid scene buffer offset. SceneHeader is
        // repr(C) at the start of each scene buffer.
        unsafe { &*(self.buf.as_ptr().add(self.read_off) as *const SceneHeader) }
    }

    /// Resolve a `DataRef` against the claimed buffer.
    pub fn front_data(&self, dref: DataRef) -> &[u8] {
        let off = self.read_off;
        let hdr = self.header();
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;
        if end <= self.buf.len() && dref.offset + dref.length <= hdr.data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }

    /// Data buffer slice from the claimed buffer.
    pub fn front_data_buf(&self) -> &[u8] {
        let off = self.read_off;
        let hdr = self.header();
        let used = (hdr.data_used as usize).min(DATA_BUFFER_SIZE);
        &self.buf[off + DATA_OFFSET..off + DATA_OFFSET + used]
    }

    /// Generation of the buffer being read.
    pub fn front_generation(&self) -> u32 {
        self.read_gen
    }

    /// Root node ID from the claimed buffer.
    pub fn front_root(&self) -> NodeId {
        self.header().root
    }

    /// Node slice from the claimed buffer.
    pub fn front_nodes(&self) -> &[Node] {
        let off = self.read_off;
        let hdr = self.header();
        let count = hdr.node_count as usize;
        let ptr = unsafe { self.buf.as_ptr().add(off + NODES_OFFSET) as *const Node };
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }

    /// Interpret a DataRef from the claimed buffer as ShapedGlyph array.
    pub fn front_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.front_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();
        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }
        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);
        // SAFETY: ShapedGlyph is #[repr(C)] with no padding.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }

    /// Returns the change list from the buffer, or `None` if the
    /// FULL_REPAINT sentinel is set.
    pub fn change_list(&self) -> Option<&[NodeId]> {
        let hdr = self.header();
        if hdr.change_count == FULL_REPAINT {
            return None;
        }
        let count = (hdr.change_count as usize).min(CHANGE_LIST_CAPACITY);
        Some(&hdr.changed_nodes[..count])
    }

    /// Returns `true` if the buffer's change list indicates a full repaint.
    pub fn is_full_repaint(&self) -> bool {
        self.header().change_count == FULL_REPAINT
    }

    /// Signal that the reader has finished reading. Releases the buffer
    /// back to the free pool so the writer can acquire it.
    pub fn finish_read(&self, gen: u32) {
        // Write reader_done_gen with release fence so writer sees it.
        triple_write_ctrl_release(self.buf, CTRL_READER_DONE_GEN, gen);
        // Release reader_buf — buffer is now free for the writer.
        triple_write_ctrl(self.buf, CTRL_READER_BUF, NO_READER);
    }
}

// ── Legacy double-buffered scene graph (deprecated) ─────────────────

/// Byte offset of the reader state region, placed after both scene buffers.
/// Contains `reader_done_gen: u32` — the generation the reader last finished
/// reading. The writer checks this before overwriting the back buffer.
pub const READER_STATE_OFFSET: usize = 2 * SCENE_SIZE;

/// Total size for a double-buffered scene graph: two full scene buffers
/// side by side, plus an 8-byte reader state region (4-byte generation +
/// 4-byte padding for alignment).
///
/// **Deprecated:** Use `TRIPLE_SCENE_SIZE` instead.
pub const DOUBLE_SCENE_SIZE: usize = 2 * SCENE_SIZE + 8;

/// Mutable access to a double-buffered scene graph.
///
/// The OS service uses this to write scenes and publish them. It can
/// also read the current front buffer (e.g. for diffing).
pub struct DoubleWriter<'a> {
    buf: &'a mut [u8],
}
/// Read-only access to a double-buffered scene graph.
///
/// The compositor uses this when reading from shared memory written by
/// the OS service. Always reads the front buffer (higher generation).
///
/// The front buffer offset and generation are captured at construction
/// time so that all reads within a single `DoubleReader` instance are
/// consistent — they all reference the same physical buffer even if the
/// writer swaps between method calls.
pub struct DoubleReader<'a> {
    buf: &'a [u8],
    /// Cached byte offset of the front buffer (0 or SCENE_SIZE).
    front_off: usize,
    /// Cached generation of the front buffer.
    front_gen: u32,
}

/// Return the byte offset of the back (lower-gen) buffer.
fn back_offset_of(buf: &[u8]) -> usize {
    let g0 = read_generation(buf, 0);
    let g1 = read_generation(buf, SCENE_SIZE);

    if g0 <= g1 {
        0
    } else {
        SCENE_SIZE
    }
}
/// Return the byte offset and generation of the front (higher-gen) buffer.
/// When both generations are equal, buffer 0 is the front (arbitrary tiebreak).
fn front_of(buf: &[u8]) -> (usize, u32) {
    let g0 = read_generation(buf, 0);
    let g1 = read_generation(buf, SCENE_SIZE);

    if g1 > g0 {
        (SCENE_SIZE, g1)
    } else {
        (0, g0)
    }
}
/// Read the generation counter from a scene buffer at the given byte
/// offset within the parent buffer. Uses volatile to prevent reordering
/// past the read (important for cross-process shared memory).
fn read_generation(buf: &[u8], offset: usize) -> u32 {
    // SAFETY: SceneHeader starts at `offset`; generation is the first u32.
    unsafe { core::ptr::read_volatile(buf.as_ptr().add(offset) as *const u32) }
}
/// Write a generation counter to a scene buffer at the given offset.
/// Uses volatile + release fence to ensure all prior writes (node data,
/// text content) are visible before the generation update is published.
fn write_generation(buf: &mut [u8], offset: usize, value: u32) {
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

    // SAFETY: SceneHeader starts at `offset`; generation is the first u32.
    unsafe { core::ptr::write_volatile(buf.as_mut_ptr().add(offset) as *mut u32, value) }
}

/// Read the `reader_done_gen` field from the reader state region.
/// This is the generation the reader last finished reading. Uses volatile
/// to prevent the compiler from reordering or caching the read.
fn read_reader_done_gen(buf: &[u8]) -> u32 {
    // SAFETY: READER_STATE_OFFSET is within the DOUBLE_SCENE_SIZE buffer
    // (READER_STATE_OFFSET + 4 <= DOUBLE_SCENE_SIZE). The field is a u32
    // aligned to a 4-byte boundary (READER_STATE_OFFSET = 2 * SCENE_SIZE
    // which is a multiple of 4).
    unsafe { core::ptr::read_volatile(buf.as_ptr().add(READER_STATE_OFFSET) as *const u32) }
}

/// Write the `reader_done_gen` field in the reader state region.
/// Called by the reader after finishing a read cycle. Uses volatile +
/// release fence to ensure the value is visible to the writer.
fn write_reader_done_gen(buf: &[u8], value: u32) {
    core::sync::atomic::fence(core::sync::atomic::Ordering::Release);

    // SAFETY: READER_STATE_OFFSET is within the DOUBLE_SCENE_SIZE buffer.
    // The field is a u32 at a 4-byte aligned offset. We cast away const
    // because the reader state region is conceptually "owned" by the reader
    // even though the buffer is shared (reader writes this field, writer
    // reads it). In the real system, this is shared memory with volatile
    // access; the &[u8] immutability is a Rust-side convenience.
    unsafe { core::ptr::write_volatile(buf.as_ptr().add(READER_STATE_OFFSET) as *mut u32, value) }
}

impl<'a> DoubleWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= DOUBLE_SCENE_SIZE);

        // Initialize both scene buffer headers.
        {
            let (b0, rest) = buf.split_at_mut(SCENE_SIZE);
            let _ = SceneWriter::new(b0);
            let _ = SceneWriter::new(&mut rest[..SCENE_SIZE]);
        }

        // Initialize reader_done_gen to u32::MAX ("no reader connected").
        // This allows the writer to freely swap buffers before a reader
        // calls finish_read(). Once a reader starts acknowledging frames,
        // the actual generation value constrains the writer.
        // SAFETY: READER_STATE_OFFSET is within the buffer (asserted above).
        unsafe {
            core::ptr::write_volatile(
                buf.as_mut_ptr().add(READER_STATE_OFFSET) as *mut u32,
                u32::MAX,
            );
        }

        Self { buf }
    }

    /// Get a `SceneWriter` for the back buffer (lower generation).
    /// The caller writes the scene, then calls `swap()` to publish.
    pub fn back(&mut self) -> SceneWriter<'_> {
        let off = back_offset_of(self.buf);

        SceneWriter::from_existing(&mut self.buf[off..off + SCENE_SIZE])
    }
    /// Copy the front buffer's node array and data buffer to the back
    /// buffer, preserving the back buffer's generation counter. Resets
    /// the back buffer's change list to empty. After this call, `back()`
    /// returns a writer whose scene matches the current front — the
    /// caller can then mutate individual nodes and call `swap()`.
    ///
    /// Returns `true` if the copy succeeded, `false` if the reader may
    /// still be reading the back buffer (the reader has not yet called
    /// `finish_read()` for the back buffer's generation). When `false`
    /// is returned, the back buffer is not modified — the caller should
    /// skip the update or fall back to a full rebuild.
    pub fn copy_front_to_back(&mut self) -> bool {
        let front_off = front_of(self.buf).0;
        let back_off = back_offset_of(self.buf);

        // Save back buffer's generation before overwriting.
        let back_gen = read_generation(self.buf, back_off);

        // Check if the reader has finished reading the back buffer.
        // The reader writes `reader_done_gen` after completing a read.
        // If reader_done_gen >= back_gen, the reader is done with the
        // back buffer (it has since read a generation at or past the
        // one stored in the back buffer). Safe to overwrite.
        //
        // If reader_done_gen < back_gen, the reader may still be
        // reading the back buffer — return false to prevent torn reads.
        let reader_done = read_reader_done_gen(self.buf);

        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        if back_gen > 0 && reader_done < back_gen {
            return false;
        }

        // Read front header to determine how much to copy.
        // SAFETY: front_off is 0 or SCENE_SIZE, within the DOUBLE_SCENE_SIZE
        // buffer. SceneHeader is repr(C) at the start of each scene buffer.
        let front_hdr =
            unsafe { core::ptr::read(self.buf.as_ptr().add(front_off) as *const SceneHeader) };

        let node_count = front_hdr.node_count;
        let data_used = front_hdr.data_used;

        // Copy node array (only the live nodes).
        let node_bytes = node_count as usize * NODE_SIZE;

        if node_bytes > 0 {
            // SAFETY: Both front_off and back_off are valid scene buffer
            // offsets (0 or SCENE_SIZE). NODES_OFFSET + node_bytes is within
            // SCENE_SIZE (bounded by MAX_NODES * NODE_SIZE). src and dst do
            // not overlap because front_off != back_off (one is 0, the other
            // is SCENE_SIZE). Using copy_nonoverlapping for performance.
            unsafe {
                let src = self.buf.as_ptr().add(front_off + NODES_OFFSET);
                let dst = self.buf.as_mut_ptr().add(back_off + NODES_OFFSET);

                core::ptr::copy_nonoverlapping(src, dst, node_bytes);
            }
        }

        // Copy data buffer (only the used portion).
        let data_bytes = data_used as usize;

        if data_bytes > 0 {
            // SAFETY: Same reasoning — DATA_OFFSET + data_bytes is within
            // SCENE_SIZE (bounded by DATA_BUFFER_SIZE). Non-overlapping
            // because front and back buffers are SCENE_SIZE apart.
            unsafe {
                let src = self.buf.as_ptr().add(front_off + DATA_OFFSET);
                let dst = self.buf.as_mut_ptr().add(back_off + DATA_OFFSET);

                core::ptr::copy_nonoverlapping(src, dst, data_bytes);
            }
        }

        // Write back header: copy front's metadata but preserve generation
        // and reset change list.
        // SAFETY: back_off is a valid scene buffer offset. SceneHeader is
        // repr(C) at offset 0 of each scene buffer. Exclusive &mut borrow
        // on self prevents aliasing.
        let back_hdr = unsafe { &mut *(self.buf.as_mut_ptr().add(back_off) as *mut SceneHeader) };

        back_hdr.node_count = node_count;
        back_hdr.root = front_hdr.root;
        back_hdr.data_used = data_used;
        back_hdr.change_count = 0;
        back_hdr.changed_nodes = [NULL; CHANGE_LIST_CAPACITY];

        // Restore back buffer's generation (do NOT copy front's generation).
        write_generation(self.buf, back_off, back_gen);

        true
    }
    /// Wrap a previously initialized double buffer without resetting.
    pub fn from_existing(buf: &'a mut [u8]) -> Self {
        assert!(buf.len() >= DOUBLE_SCENE_SIZE);

        Self { buf }
    }
    /// Resolve a `DataRef` against the current front buffer.
    pub fn front_data(&self, dref: DataRef) -> &[u8] {
        let (off, _) = front_of(self.buf);
        // SAFETY: `off` is 0 or SCENE_SIZE (from `front_of`), both within
        // the DOUBLE_SCENE_SIZE buffer. SceneHeader is repr(C) at the start
        // of each scene buffer, so the cast is correctly aligned and in-bounds.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.buf.len() && dref.offset + dref.length <= hdr.data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }
    /// Data buffer slice from the current front buffer.
    pub fn front_data_buf(&self) -> &[u8] {
        let (off, _) = front_of(self.buf);
        // SAFETY: Same as front_data — `off` is a valid scene buffer offset,
        // SceneHeader is repr(C) at the start of each scene buffer.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let used = hdr.data_used as usize;

        &self.buf[off + DATA_OFFSET..off + DATA_OFFSET + used]
    }
    /// Generation counter of the current front buffer.
    pub fn front_generation(&self) -> u32 {
        let (_, g) = front_of(self.buf);

        g
    }
    /// Node slice from the current front buffer.
    pub fn front_nodes(&self) -> &[Node] {
        let (off, _) = front_of(self.buf);
        // SAFETY: `off` is a valid scene buffer offset. SceneHeader is repr(C)
        // at the buffer start; reading node_count is in-bounds.
        let hdr = unsafe { &*(self.buf.as_ptr().add(off) as *const SceneHeader) };
        let count = hdr.node_count as usize;
        // SAFETY: NODES_OFFSET is within each SCENE_SIZE buffer. Node is repr(C)
        // with size NODE_SIZE. `count` is bounded by MAX_NODES (checked at alloc).
        let ptr = unsafe { self.buf.as_ptr().add(off + NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries within
        // the buffer. The slice borrows `self`, preventing concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    /// Interpret a DataRef from the front buffer as ShapedGlyph array.
    pub fn front_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.front_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding. push_shaped_glyphs
        // aligns the data buffer to ShapedGlyph alignment. `count` is bounded by
        // available bytes. The slice borrows `self`, preventing concurrent mutation.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }
    /// Publish the back buffer as the new front by setting its generation
    /// above the current front's. A release fence ensures all scene data
    /// written via `back()` is visible before the generation update.
    pub fn swap(&mut self) {
        let g0 = read_generation(self.buf, 0);
        let g1 = read_generation(self.buf, SCENE_SIZE);
        // The back buffer is the one with the lower generation (same
        // tiebreak as back()). Set its generation above the front's.
        let (back_off, max_gen) = if g0 <= g1 { (0, g1) } else { (SCENE_SIZE, g0) };

        write_generation(self.buf, back_off, max_gen.wrapping_add(1));
    }
    /// Simulate the reader acknowledging a generation. Writes the
    /// `reader_done_gen` field as if the reader called `finish_read()`.
    ///
    /// This is useful in single-address-space tests where the same buffer
    /// is accessed by both writer and reader logic without requiring a
    /// separate `DoubleReader` borrow.
    pub fn ack_reader(&mut self, gen: u32) {
        write_reader_done_gen(self.buf, gen);
    }
}
impl<'a> DoubleReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        assert!(buf.len() >= DOUBLE_SCENE_SIZE);

        // Capture the front buffer offset and generation once. All subsequent
        // reads use this cached offset to ensure consistency even if the writer
        // swaps buffers between our method calls.
        let (front_off, front_gen) = front_of(buf);

        // Acquire fence: ensures all node/data writes from the writer
        // (which preceded the writer's release fence in swap/write_generation)
        // are visible to us after this point.
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        Self {
            buf,
            front_off,
            front_gen,
        }
    }

    fn header(&self) -> &SceneHeader {
        // SAFETY: front_off is 0 or SCENE_SIZE (from front_of), within the
        // DOUBLE_SCENE_SIZE buffer. SceneHeader is repr(C) at the start of
        // each scene buffer.
        unsafe { &*(self.buf.as_ptr().add(self.front_off) as *const SceneHeader) }
    }

    /// Resolve a `DataRef` against the current front buffer.
    pub fn front_data(&self, dref: DataRef) -> &[u8] {
        let off = self.front_off;
        let hdr = self.header();
        let start = off + DATA_OFFSET + dref.offset as usize;
        let end = start + dref.length as usize;

        if end <= self.buf.len() && dref.offset + dref.length <= hdr.data_used {
            &self.buf[start..end]
        } else {
            &[]
        }
    }
    /// Data buffer slice from the current front buffer.
    pub fn front_data_buf(&self) -> &[u8] {
        let off = self.front_off;
        let hdr = self.header();
        let used = hdr.data_used as usize;

        &self.buf[off + DATA_OFFSET..off + DATA_OFFSET + used]
    }
    /// Generation counter of the front buffer (captured at construction).
    pub fn front_generation(&self) -> u32 {
        self.front_gen
    }
    /// Node slice from the current front buffer.
    pub fn front_nodes(&self) -> &[Node] {
        let off = self.front_off;
        let hdr = self.header();
        let count = hdr.node_count as usize;
        // SAFETY: NODES_OFFSET is within each SCENE_SIZE buffer. Node is repr(C)
        // with size NODE_SIZE. `count` is bounded by MAX_NODES (checked at alloc).
        let ptr = unsafe { self.buf.as_ptr().add(off + NODES_OFFSET) as *const Node };

        // SAFETY: `ptr` points to `count` contiguous Node-sized entries within
        // the buffer. The slice borrows `self`, preventing concurrent mutation.
        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    /// Interpret a DataRef from the front buffer as ShapedGlyph array.
    pub fn front_shaped_glyphs(&self, dref: DataRef, glyph_count: u16) -> &[ShapedGlyph] {
        let bytes = self.front_data(dref);
        let glyph_size = core::mem::size_of::<ShapedGlyph>();

        if bytes.is_empty() || bytes.len() < glyph_size {
            return &[];
        }

        let available = bytes.len() / glyph_size;
        let count = (glyph_count as usize).min(available);

        // SAFETY: ShapedGlyph is #[repr(C)] with no padding. push_shaped_glyphs
        // aligns the data buffer to ShapedGlyph alignment. `count` is bounded by
        // available bytes. The acquire fence at construction ensures visibility.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const ShapedGlyph, count) }
    }
    /// Returns the change list from the front buffer, or `None` if the
    /// FULL_REPAINT sentinel is set (overflow or full rebuild).
    pub fn change_list(&self) -> Option<&[NodeId]> {
        let hdr = self.header();

        if hdr.change_count == FULL_REPAINT {
            return None;
        }

        let count = (hdr.change_count as usize).min(CHANGE_LIST_CAPACITY);

        Some(&hdr.changed_nodes[..count])
    }
    /// Returns `true` if the front buffer's change list indicates a full
    /// repaint is needed (FULL_REPAINT sentinel or clear() was called).
    pub fn is_full_repaint(&self) -> bool {
        let hdr = self.header();

        hdr.change_count == FULL_REPAINT
    }
    /// Signal that the reader has finished reading the frame with the
    /// given generation. The writer checks this value before overwriting
    /// the back buffer (which may be the buffer the reader was reading).
    ///
    /// Call this after reading all nodes and data for the current frame.
    /// The generation should be the value returned by `front_generation()`.
    pub fn finish_read(&self, gen: u32) {
        write_reader_done_gen(self.buf, gen);
    }
}

// ── Scene diffing ───────────────────────────────────────────────────

/// Build a parent map from the node array. `parent[i]` is the parent
/// NodeId of node `i`, or `NULL` if it has no parent (root or unused).
/// One pass over the tree structure.
pub fn build_parent_map(nodes: &[Node], count: usize) -> [NodeId; MAX_NODES] {
    let mut parent = [NULL; MAX_NODES];
    let n = count.min(nodes.len()).min(MAX_NODES);
    for i in 0..n {
        let mut child = nodes[i].first_child;
        while child != NULL && (child as usize) < n {
            parent[child as usize] = i as NodeId;
            child = nodes[child as usize].next_sibling;
        }
    }
    parent
}

/// Compute absolute bounding rect of a node by walking up the parent chain.
/// Returns `(x, y, width, height)` in absolute logical coordinates.
///
/// Each parent's `scroll_y` is subtracted from the y accumulator because
/// scroll offsets its *children* upward by `scroll_y` pixels. Without this,
/// damage tracking would compute incorrect dirty rects for nodes inside
/// scrolled containers.
///
/// When a node has a non-identity transform, the returned bounding rect is
/// the axis-aligned bounding box (AABB) of the transformed node bounds.
/// This ensures damage tracking covers the full area affected by rotated,
/// scaled, or skewed nodes.
pub fn abs_bounds(
    nodes: &[Node],
    parent_map: &[NodeId; MAX_NODES],
    id: usize,
) -> (i32, i32, u32, u32) {
    let node = &nodes[id];
    let mut ax = node.x as i32;
    let mut ay = node.y as i32;
    let mut cur = parent_map[id];
    while cur != NULL && (cur as usize) < nodes.len() {
        let p = &nodes[cur as usize];
        ax += p.x as i32;
        // Subtract scroll_y: a parent's scroll offsets its children upward.
        ay += p.y as i32 - p.scroll_y;
        cur = parent_map[cur as usize];
    }

    // Start with the node's logical size.
    let mut bw = node.width as u32;
    let mut bh = node.height as u32;
    let mut bx = ax;
    let mut by = ay;

    // If the node has a non-identity transform, compute the AABB of the
    // transformed bounds. The transform shifts the node's visual footprint
    // — damage tracking must cover the full transformed area.
    if !node.transform.is_identity() {
        let (aabb_x, aabb_y, aabb_w, aabb_h) =
            node.transform
                .transform_aabb(0.0, 0.0, node.width as f32, node.height as f32);

        // The AABB origin is relative to the node's position.
        // Round conservatively: floor for origin, ceil for size.
        let aabb_xi = floor_f32(aabb_x) as i32;
        let aabb_yi = floor_f32(aabb_y) as i32;
        let aabb_wi = ceil_f32(aabb_w) as u32;
        let aabb_hi = ceil_f32(aabb_h) as u32;

        bx = ax + aabb_xi;
        by = ay + aabb_yi;
        bw = aabb_wi;
        bh = aabb_hi;
    }

    // Expand bounds by shadow overflow if the node has a shadow.
    if node.has_shadow() {
        let blur = node.shadow_blur_radius as i32;
        let spread = node.shadow_spread as i32;
        let off_x = node.shadow_offset_x as i32;
        let off_y = node.shadow_offset_y as i32;

        // Shadow extends by spread + blur on each side, shifted by offset.
        let extent = spread + blur;
        let left = (extent - off_x).max(0);
        let top = (extent - off_y).max(0);
        let right = (extent + off_x).max(0);
        let bottom = (extent + off_y).max(0);

        let new_x = bx - left;
        let new_y = by - top;
        let new_w = (bw as i32 + left + right).max(0) as u32;
        let new_h = (bh as i32 + top + bottom).max(0) as u32;

        return (new_x, new_y, new_w, new_h);
    }

    (bx, by, bw, bh)
}

/// Floor for f32 in `no_std`.
fn floor_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x < f {
        f - 1.0
    } else {
        f
    }
}

/// Ceil for f32 in `no_std`.
fn ceil_f32(x: f32) -> f32 {
    let i = x as i32;
    let f = i as f32;
    if x > f {
        f + 1.0
    } else {
        f
    }
}

/// Compare two scene snapshots and return dirty rectangles.
///
/// `prev_nodes` / `curr_nodes` are the node arrays from the previous and
/// current frames. If node counts differ, returns `None` (full repaint).
/// Otherwise, returns a list of `(x, y, w, h)` absolute bounding rects
/// for all changed nodes. The caller unions these into DirtyRects.
pub fn diff_scenes(
    prev_nodes: &[Node],
    prev_count: usize,
    curr_nodes: &[Node],
    curr_count: usize,
) -> Option<Vec<(i32, i32, u32, u32)>> {
    if prev_count != curr_count || prev_count == 0 {
        return None;
    }
    let n = prev_count
        .min(prev_nodes.len())
        .min(curr_nodes.len())
        .min(MAX_NODES);
    let curr_parents = build_parent_map(curr_nodes, n);
    let prev_parents = build_parent_map(prev_nodes, n);
    let node_size = core::mem::size_of::<Node>();
    let mut rects = Vec::new();
    for i in 0..n {
        // SAFETY: Node is repr(C), fixed size — byte comparison is sound.
        let prev_bytes = unsafe {
            core::slice::from_raw_parts(&prev_nodes[i] as *const Node as *const u8, node_size)
        };
        let curr_bytes = unsafe {
            core::slice::from_raw_parts(&curr_nodes[i] as *const Node as *const u8, node_size)
        };
        if prev_bytes != curr_bytes {
            // Damage both old and new positions (handles node movement).
            let old_rect = abs_bounds(prev_nodes, &prev_parents, i);
            let new_rect = abs_bounds(curr_nodes, &curr_parents, i);
            rects.push(old_rect);
            if old_rect != new_rect {
                rects.push(new_rect);
            }
        }
    }
    Some(rects)
}
