//! Scene graph node type, header, and memory layout constants.

use crate::{
    primitives::{bitflags, Border, Color, Content, DataRef},
    transform::AffineTransform,
};

// ── Node ID ─────────────────────────────────────────────────────────

/// Index into the node array. `NULL` means no node.
pub type NodeId = u16;
pub const NULL: NodeId = u16::MAX;

// ── Millipoint coordinate unit ─────────────────────────────────────

/// 1/1024 of a point. The internal coordinate unit for all spatial
/// values in the scene graph, layout engine, and core service.
///
/// Precision: ~0.001 pt (sub-pixel at any density).
/// i32 range: +/-2,097,151 pt (~2,489 A4 pages).
/// Convert to/from whole points: `pt << 10` / `mpt >> 10`.
pub type Mpt = i32;

/// Unsigned millipoint for dimensions (width, height).
pub type Umpt = u32;

/// Millipoints per point.
pub const MPT_PER_PT: i32 = 1024;

/// Convert signed whole points to millipoints.
pub const fn pt(points: i32) -> Mpt {
    points * MPT_PER_PT
}

/// Convert unsigned whole points to unsigned millipoints.
pub const fn upt(points: u32) -> Umpt {
    points * MPT_PER_PT as u32
}

/// Convert millipoints to f32 points (for AffineTransform / render boundary).
pub fn mpt_to_f32(mpt: Mpt) -> f32 {
    mpt as f32 / MPT_PER_PT as f32
}

/// Convert unsigned millipoints to f32 points.
pub fn umpt_to_f32(mpt: Umpt) -> f32 {
    mpt as f32 / MPT_PER_PT as f32
}

/// Convert f32 points to millipoints (for spring output boundary).
pub fn f32_to_mpt(points: f32) -> Mpt {
    (points * MPT_PER_PT as f32) as Mpt
}

/// Round millipoints to the nearest whole-point-aligned value
/// (nearest multiple of MPT_PER_PT). Used for settle snap.
pub fn mpt_round_pt(mpt: Mpt) -> Mpt {
    if mpt >= 0 {
        (mpt + MPT_PER_PT / 2) / MPT_PER_PT * MPT_PER_PT
    } else {
        (mpt - MPT_PER_PT / 2) / MPT_PER_PT * MPT_PER_PT
    }
}

// ── Node flags ──────────────────────────────────────────────────────

bitflags! {
    /// Node flags packed into a single byte.
    pub struct NodeFlags: u8 {
        const CLIPS_CHILDREN = 0b0000_0001;
        const VISIBLE        = 0b0000_0010;
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
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
    // ── content transform ──
    /// 2D affine transform applied to children's coordinate space.
    /// Used for scrolling (pure translation) and zoom (scale).
    /// Identity by default (no offset).
    pub content_transform: AffineTransform,
    // ── visual decoration ──
    pub background: Color,
    pub border: Border,
    pub corner_radius: u8,
    pub opacity: u8,
    // ── flags ──
    pub flags: NodeFlags,
    pub backdrop_blur_radius: u8,
    // ── shadow ──
    /// Shadow color (TRANSPARENT = no shadow).
    pub shadow_color: Color,
    /// Horizontal shadow offset in points.
    pub shadow_offset_x: i16,
    /// Vertical shadow offset in points.
    pub shadow_offset_y: i16,
    /// Shadow blur radius in points (0 = hard shadow).
    pub shadow_blur_radius: u8,
    /// Shadow spread in points (positive expands, negative shrinks).
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
    // ── clip path ──
    /// Reference to serialized path commands in the data buffer that define
    /// a clip region for this node and its children. `DataRef::EMPTY` means
    /// no path clip (rectangular clip via `CLIPS_CHILDREN` flag still applies).
    pub clip_path: DataRef,
    /// Reserved for future fields. Must be zero.
    pub _reserved: [u8; 8],
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
        content_transform: AffineTransform::identity(),
        background: Color::TRANSPARENT,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0,
            _pad: [0; 3],
        },
        corner_radius: 0,
        opacity: 255,
        flags: NodeFlags::VISIBLE,
        backdrop_blur_radius: 0,
        shadow_color: Color::TRANSPARENT,
        shadow_offset_x: 0,
        shadow_offset_y: 0,
        shadow_blur_radius: 0,
        shadow_spread: 0,
        _shadow_pad: [0; 2],
        transform: AffineTransform::identity(),
        content_hash: 0,
        clip_path: DataRef::EMPTY,
        _reserved: [0; 8],
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

// Compile-time size assertion: Node must be exactly 136 bytes.
// This prevents silent shared-memory layout drift between core and compositor.
// If you add a field, update this assertion and verify both sides agree.
// Layout: 96 bytes pre-content + clip_path (8) + _reserved (8) + content (24) = 136.
const _: () = assert!(core::mem::size_of::<Node>() == 136);

// ── Shared memory layout ────────────────────────────────────────────

pub const MAX_NODES: usize = 512;
pub const DATA_BUFFER_SIZE: usize = 128 * 1024;
pub const NODES_OFFSET: usize = core::mem::size_of::<SceneHeader>();
pub const DATA_OFFSET: usize = NODES_OFFSET + MAX_NODES * core::mem::size_of::<Node>();
pub const SCENE_SIZE: usize = DATA_OFFSET + DATA_BUFFER_SIZE;

/// Number of u64 words in the dirty bitmap (512 bits / 64 = 8 words).
pub const DIRTY_BITMAP_WORDS: usize = 8;

const _: () = assert!(core::mem::size_of::<SceneHeader>() == 80);

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
    /// 512-bit dirty bitmap: one bit per node slot. Bit `i` is set if
    /// node `i` was modified since the last frame. All bits set means
    /// full repaint (e.g., after `clear()`).
    pub dirty_bits: [u64; DIRTY_BITMAP_WORDS],
}
