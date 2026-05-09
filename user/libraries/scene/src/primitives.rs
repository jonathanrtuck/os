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
    pub const EMPTY: Self = Self {
        offset: 0,
        length: 0,
    };

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
///
/// `x_advance`, `x_offset`, and `y_offset` are in **16.16 fixed-point
/// points** (top 16 bits = integer points, bottom 16 bits = fractional).
/// This preserves sub-point precision from the shaping engine through to
/// the renderer, enabling subpixel glyph positioning for even letter
/// spacing. Range: ±32767 points. Precision: 1/65536 point.
///
/// To convert to floating-point points: `value as f32 / 65536.0`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct ShapedGlyph {
    /// Glyph ID in the font (0 = .notdef).
    pub glyph_id: u16,
    /// Padding for alignment (glyph_id is u16, next field is i32).
    pub _pad: u16,
    /// Horizontal advance width in 16.16 fixed-point points.
    pub x_advance: i32,
    /// Horizontal offset from default position in 16.16 fixed-point points.
    pub x_offset: i32,
    /// Vertical offset from default position in 16.16 fixed-point points.
    pub y_offset: i32,
}

// Compile-time size assertion: ShapedGlyph is 16 bytes
// (u16 + u16 pad + 3 × i32, #[repr(C)]).
const _: () = assert!(core::mem::size_of::<ShapedGlyph>() == 16);

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

/// Gradient interpolation layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GradientKind {
    /// Color varies along a direction vector defined by `angle` (radians).
    /// `angle = 0` → left-to-right, `π/2` → top-to-bottom.
    Linear = 0,
    /// Color varies from node center outward to the edges.
    Radial = 1,
    /// Color sweeps around the node center. `angle` sets the start
    /// direction (0 = right, increasing counter-clockwise).
    Conical = 2,
}

/// Convert an angle in radians to the `angle_fp` fixed-point format
/// used by `Content::Gradient`. Maps `[0, 2π)` to `0..65535`.
pub fn angle_to_fp(radians: f32) -> u16 {
    const TAU: f32 = core::f32::consts::TAU;
    let normalized = ((radians % TAU) + TAU) % TAU;

    (normalized / TAU * 65536.0) as u16
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

// ── Point-in-path test (winding number) ────────────────────────────

/// Test whether a point is inside a path using the winding number rule.
///
/// Parses the serialized path commands (MoveTo, LineTo, CubicTo, Close)
/// and counts signed crossings of a horizontal ray cast rightward from
/// the test point. Cubic Béziers are flattened to line segments.
///
/// Coordinates are in the path's local space (typically points or viewbox
/// units). The caller must transform the test point to match.
///
/// Returns the winding number. Non-zero means the point is inside
/// (for the winding fill rule). For even-odd, test `winding & 1 != 0`.
pub fn path_winding_number(path_data: &[u8], px: f32, py: f32) -> i32 {
    let mut winding: i32 = 0;
    // Current position and sub-path start (for Close).
    let mut cx: f32 = 0.0;
    let mut cy: f32 = 0.0;
    let mut sx: f32 = 0.0;
    let mut sy: f32 = 0.0;
    let mut pos = 0;

    while pos + 4 <= path_data.len() {
        let tag = u32::from_le_bytes([
            path_data[pos],
            path_data[pos + 1],
            path_data[pos + 2],
            path_data[pos + 3],
        ]);

        match tag {
            PATH_MOVE_TO => {
                if pos + PATH_MOVE_TO_SIZE > path_data.len() {
                    break;
                }

                cx = f32::from_le_bytes(path_data[pos + 4..pos + 8].try_into().unwrap());
                cy = f32::from_le_bytes(path_data[pos + 8..pos + 12].try_into().unwrap());
                sx = cx;
                sy = cy;
                pos += PATH_MOVE_TO_SIZE;
            }
            PATH_LINE_TO => {
                if pos + PATH_LINE_TO_SIZE > path_data.len() {
                    break;
                }

                let x = f32::from_le_bytes(path_data[pos + 4..pos + 8].try_into().unwrap());
                let y = f32::from_le_bytes(path_data[pos + 8..pos + 12].try_into().unwrap());

                winding += line_winding(px, py, cx, cy, x, y);
                cx = x;
                cy = y;
                pos += PATH_LINE_TO_SIZE;
            }
            PATH_CUBIC_TO => {
                if pos + PATH_CUBIC_TO_SIZE > path_data.len() {
                    break;
                }

                let c1x = f32::from_le_bytes(path_data[pos + 4..pos + 8].try_into().unwrap());
                let c1y = f32::from_le_bytes(path_data[pos + 8..pos + 12].try_into().unwrap());
                let c2x = f32::from_le_bytes(path_data[pos + 12..pos + 16].try_into().unwrap());
                let c2y = f32::from_le_bytes(path_data[pos + 16..pos + 20].try_into().unwrap());
                let x = f32::from_le_bytes(path_data[pos + 20..pos + 24].try_into().unwrap());
                let y = f32::from_le_bytes(path_data[pos + 24..pos + 28].try_into().unwrap());

                winding += cubic_winding(px, py, cx, cy, c1x, c1y, c2x, c2y, x, y);
                cx = x;
                cy = y;
                pos += PATH_CUBIC_TO_SIZE;
            }
            PATH_CLOSE => {
                winding += line_winding(px, py, cx, cy, sx, sy);
                cx = sx;
                cy = sy;
                pos += PATH_CLOSE_SIZE;
            }
            _ => break,
        }
    }

    winding
}

/// Winding contribution of a line segment for a rightward horizontal ray.
/// Returns +1 for upward crossing, -1 for downward crossing, 0 for no crossing.
fn line_winding(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> i32 {
    if (y0 <= py && y1 > py) || (y1 <= py && y0 > py) {
        // Compute x-intercept of the line at y=py.
        let t = (py - y0) / (y1 - y0);
        let ix = x0 + t * (x1 - x0);

        if px < ix {
            // Ray crosses the segment.
            return if y1 > y0 { 1 } else { -1 };
        }
    }
    0
}

/// Winding contribution of a cubic Bézier, computed by recursive
/// subdivision to line segments (de Casteljau, max depth 6 = 64 segments).
fn cubic_winding(
    px: f32,
    py: f32,
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
) -> i32 {
    cubic_winding_recursive(px, py, x0, y0, c1x, c1y, c2x, c2y, x3, y3, 0)
}

fn cubic_winding_recursive(
    px: f32,
    py: f32,
    x0: f32,
    y0: f32,
    c1x: f32,
    c1y: f32,
    c2x: f32,
    c2y: f32,
    x3: f32,
    y3: f32,
    depth: u8,
) -> i32 {
    // Flatness test: if the control points are close to the line x0→x3,
    // treat as a line segment.
    if depth >= 6 {
        return line_winding(px, py, x0, y0, x3, y3);
    }

    // Quick reject: if py is outside the vertical extent of all control
    // points, no crossing is possible.
    let min_y = f32_min(f32_min(y0, c1y), f32_min(c2y, y3));
    let max_y = f32_max(f32_max(y0, c1y), f32_max(c2y, y3));

    if py < min_y || py > max_y {
        return 0;
    }

    // Flatness: max distance of control points from the chord.
    let dx = x3 - x0;
    let dy = y3 - y0;
    let d2 = (dx * (c1y - y0) - dy * (c1x - x0)).abs();
    let d3 = (dx * (c2y - y0) - dy * (c2x - x0)).abs();
    let chord_len_sq = dx * dx + dy * dy;

    if (d2 + d3) * (d2 + d3) <= 0.25 * chord_len_sq {
        return line_winding(px, py, x0, y0, x3, y3);
    }

    // De Casteljau split at t=0.5.
    let m01x = (x0 + c1x) * 0.5;
    let m01y = (y0 + c1y) * 0.5;
    let m12x = (c1x + c2x) * 0.5;
    let m12y = (c1y + c2y) * 0.5;
    let m23x = (c2x + x3) * 0.5;
    let m23y = (c2y + y3) * 0.5;
    let m012x = (m01x + m12x) * 0.5;
    let m012y = (m01y + m12y) * 0.5;
    let m123x = (m12x + m23x) * 0.5;
    let m123y = (m12y + m23y) * 0.5;
    let mx = (m012x + m123x) * 0.5;
    let my = (m012y + m123y) * 0.5;

    cubic_winding_recursive(px, py, x0, y0, m01x, m01y, m012x, m012y, mx, my, depth + 1)
        + cubic_winding_recursive(px, py, mx, my, m123x, m123y, m23x, m23y, x3, y3, depth + 1)
}

#[inline]
fn f32_min(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

#[inline]
fn f32_max(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

// ── Font identity constants ─────────────────────────────────────────

// Font identity constants FONT_MONO/FONT_SANS/FONT_SERIF removed.
// Style IDs are now assigned at runtime by core's StyleTable.

// ── Animation ───────────────────────────────────────────────────────

/// Target property for a node animation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AnimationTarget {
    None = 0,
    Opacity = 1,
}

/// Easing curve index (matches `animation::Easing` variants).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum AnimationEasing {
    Linear = 0,
    EaseInOut = 1,
}

/// How the animation behaves after completing one cycle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum RepeatMode {
    Once = 0,
    Loop = 1,
}

/// One phase of a keyframe animation. The compositor transitions from
/// the previous phase's value to this phase's value over `duration_ms`
/// using the specified easing, then holds at this value for any remaining
/// time before the next phase begins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct AnimationPhase {
    pub value: u8,
    pub easing: AnimationEasing,
    pub duration_ms: u16,
}

const _: () = assert!(core::mem::size_of::<AnimationPhase>() == 4);

/// Per-node animation descriptor. The compositor evaluates this each frame
/// using `clock_read`. Supports up to 4 keyframe phases per animation.
///
/// For cursor blink: target=Opacity, repeat=Loop, 4 phases:
///   (255, Linear, 500)  — visible hold
///   (0,   EaseInOut, 150) — fade out
///   (0,   Linear, 300)  — hidden hold
///   (255, EaseInOut, 150) — fade in
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Animation {
    /// Absolute counter tick when the animation started. 0 = inactive.
    pub start_tick: u64,
    /// Target property.
    pub target: AnimationTarget,
    /// Repeat mode.
    pub repeat: RepeatMode,
    /// Number of phases (1-4). Unused phases are ignored.
    pub phase_count: u8,
    pub _pad: u8,
    /// Keyframe phases. Evaluated sequentially; total cycle duration is
    /// the sum of all phase durations.
    pub phases: [AnimationPhase; 4],
}

const _: () = assert!(core::mem::size_of::<Animation>() == 32);

impl Animation {
    pub const NONE: Self = Self {
        start_tick: 0,
        target: AnimationTarget::None,
        repeat: RepeatMode::Once,
        phase_count: 0,
        _pad: 0,
        phases: [AnimationPhase {
            value: 0,
            easing: AnimationEasing::Linear,
            duration_ms: 0,
        }; 4],
    };

    pub fn is_active(&self) -> bool {
        self.start_tick > 0 && self.target != AnimationTarget::None && self.phase_count > 0
    }

    pub fn cursor_blink(start_tick: u64) -> Self {
        Self {
            start_tick,
            target: AnimationTarget::Opacity,
            repeat: RepeatMode::Loop,
            phase_count: 4,
            _pad: 0,
            phases: [
                AnimationPhase {
                    value: 255,
                    easing: AnimationEasing::Linear,
                    duration_ms: 500,
                },
                AnimationPhase {
                    value: 0,
                    easing: AnimationEasing::EaseInOut,
                    duration_ms: 150,
                },
                AnimationPhase {
                    value: 0,
                    easing: AnimationEasing::Linear,
                    duration_ms: 300,
                },
                AnimationPhase {
                    value: 255,
                    easing: AnimationEasing::EaseInOut,
                    duration_ms: 150,
                },
            ],
        }
    }
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
    /// A pixel buffer stored in the scene graph's inline data buffer.
    /// Used for small, per-frame content (icons, cursor) that is
    /// regenerated each frame or on content change.
    InlineImage {
        /// Reference to pixel data in the data buffer.
        data: DataRef,
        /// Source image dimensions.
        src_width: u16,
        src_height: u16,
    },
    /// Decoded pixel data stored in the Content Region (persistent shared
    /// memory). The compositor resolves `content_id` via the Content Region
    /// registry to find the pixel data offset and length.
    Image {
        /// Content Region entry ID. The compositor looks this up in the
        /// ContentRegionHeader to find the pixel data.
        content_id: u32,
        /// Source image width in pixels.
        src_width: u16,
        /// Source image height in pixels.
        src_height: u16,
    },
    /// Cubic Bezier contours, filled and/or stroked. The render backend
    /// rasterizes them with scanline coverage (same engine as glyph
    /// outlines). Vector content scales cleanly at any display density.
    ///
    /// Fill and stroke are independent: set `color.a > 0` for fill,
    /// `stroke_width > 0 && stroke_color.a > 0` for stroke. Both can
    /// be active simultaneously (fill first, stroke on top — like SVG).
    Path {
        /// Fill color. Transparent = no fill.
        color: Color,
        /// Stroke color. Transparent or stroke_width==0 = no stroke.
        stroke_color: Color,
        /// Winding or even-odd fill rule (applies to fill; ignored for stroke).
        fill_rule: FillRule,
        /// Stroke width in 8.8 fixed-point points (0 = no stroke).
        /// Example: 2.0 pt = `0x0200`, 1.5 pt = `0x0180`.
        /// When non-zero and stroke_color is non-transparent, the render
        /// backend expands strokes to filled geometry before rasterization
        /// (round joins and caps).
        stroke_width: u16,
        /// Reference to serialized path commands in the data buffer
        /// (MoveTo, LineTo, CubicTo, Close). 4-byte aligned.
        contours: DataRef,
    },
    /// GPU-evaluated gradient fill covering the node bounds. The render
    /// backend evaluates the gradient per-fragment — no CPU rasterization.
    Gradient {
        /// Color at the start of the gradient (center for radial,
        /// start-angle edge for conical, start-direction edge for linear).
        color_start: Color,
        /// Color at the end of the gradient.
        color_end: Color,
        /// Gradient layout: linear, radial, or conical.
        kind: GradientKind,
        /// Padding for alignment.
        _pad: u8,
        /// Direction angle as fixed-point: `0..65535` maps to `[0, 2π)`.
        /// Linear: gradient direction (0 = left→right).
        /// Conical: start angle. Radial: ignored.
        /// Convert from radians: `(angle / 2π * 65536.0) as u16`.
        angle_fp: u16,
    },
    /// Path filled with a GPU gradient instead of a solid color.
    /// The render backend rasterizes path coverage into a mask, then
    /// evaluates the gradient per-fragment, multiplying by coverage.
    GradientPath {
        /// Gradient start color.
        color_start: Color,
        /// Gradient end color.
        color_end: Color,
        /// Gradient layout.
        kind: GradientKind,
        /// Padding.
        _pad: u8,
        /// Angle (same encoding as `Content::Gradient`).
        angle_fp: u16,
        /// Path contour data.
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
        /// Font size in points (e.g., 18). Render backends scale to device
        /// pixels by multiplying by scale_factor. Used as glyph cache key.
        font_size: u16,
        /// Style identifier assigned by core's StyleTable. Used as
        /// glyph cache key to distinguish fonts/weights/styles.
        style_id: u32,
    },
}

// Compile-time size assertion: Content must remain exactly 24 bytes.
// Largest payloads (tied at 20 bytes):
//   Glyphs = Color(4) + DataRef(8) + u16 + u16 + u32 = 20 bytes.
//   Path   = Color(4) + Color(4) + FillRule(1) + pad(1) + u16(2) + DataRef(8) = 20 bytes.
const _: () = assert!(core::mem::size_of::<Content>() == 24);
