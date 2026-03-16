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
//! and an optional content variant (Text, Image, Path). This avoids wrapper
//! nodes in compound documents where containers routinely need backgrounds
//! and borders.

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

// ── Text runs ───────────────────────────────────────────────────────

/// A positioned run of shaped glyphs — one line (or fragment) of text.
///
/// The OS service computes layout (line breaking, shaping, positioning)
/// and emits runs. The compositor just rasterizes glyphs at the given
/// positions. Layout in the OS service, rasterization in the compositor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct TextRun {
    /// Glyph data in the data buffer. For monospace: raw UTF-8 bytes
    /// (glyph ID == byte value). For shaped text (future): array of
    /// `ShapedGlyph`.
    pub glyphs: DataRef,
    /// Number of glyphs in this run.
    pub glyph_count: u16,
    /// Starting pixel position relative to the parent node.
    pub x: i16,
    pub y: i16,
    /// Text color.
    pub color: Color,
    /// Uniform advance width per glyph (monospace). When 0, per-glyph
    /// advances are stored in the data buffer as `ShapedGlyph` entries.
    pub advance: u16,
    /// Font size in pixels (selects the glyph cache).
    pub font_size: u16,
    /// Hash of variable font axis values used for rasterization (0 = default).
    /// Flows from core service → scene graph → compositor for glyph cache key.
    pub axis_hash: u32,
}

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

// ── Content variant ─────────────────────────────────────────────────

/// What a node draws (beyond its container decoration).
///
/// Cursor and selection are not part of Text — they are regular nodes
/// with backgrounds, positioned by the OS service's layout engine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum Content {
    /// Pure container — no content, just children and decoration.
    None,
    /// Positioned text runs laid out by the OS service.
    /// The compositor rasterizes glyphs at given positions.
    Text {
        /// Reference to array of `TextRun` in the data buffer.
        runs: DataRef,
        /// Number of runs.
        run_count: u16,
        _pad: [u8; 2],
    },
    /// A pixel buffer reference.
    Image {
        /// Reference to pixel data in the data buffer.
        data: DataRef,
        /// Source image dimensions.
        src_width: u16,
        src_height: u16,
    },
    /// Arbitrary vector shape (cursor bars, decorations, highlights).
    Path {
        /// Reference to `PathCmd` array in the data buffer.
        commands: DataRef,
        /// Fill color (transparent = no fill).
        fill: Color,
        /// Stroke color (transparent = no stroke).
        stroke: Color,
        /// Stroke width in pixels (0 = no stroke).
        stroke_width: u8,
        _pad: [u8; 3],
    },
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
    // ── content hash (FNV-1a of variable-length data referenced by Content) ──
    /// Hash of the node's variable-length data (text bytes, path commands,
    /// image pixels). Computed by the scene writer when content is set.
    /// The compositor uses this for scene diffing — a changed hash means
    /// the data buffer content changed even if the DataRef is identical.
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

// Compile-time size assertion: Node must be exactly 72 bytes.
// This prevents silent shared-memory layout drift between core and compositor.
// If you add a field, update this assertion and verify both sides agree.
const _: () = assert!(core::mem::size_of::<Node>() == 72);

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

// ── Path commands ───────────────────────────────────────────────────

/// A single path command in the scene graph data buffer.
///
/// For `MoveTo`, `LineTo`, and `Close`: only `x` and `y` are used
/// (control point fields are ignored).
///
/// For `CurveTo` (cubic Bézier): `x1`,`y1` and `x2`,`y2` are the two
/// control points; `x`,`y` is the endpoint.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct PathCmd {
    pub kind: PathCmdKind,
    pub _pad: u8,
    /// Endpoint x coordinate (or unused for Close).
    pub x: i16,
    /// Endpoint y coordinate (or unused for Close).
    pub y: i16,
    /// First control point x (CurveTo only).
    pub x1: i16,
    /// First control point y (CurveTo only).
    pub y1: i16,
    /// Second control point x (CurveTo only).
    pub x2: i16,
    /// Second control point y (CurveTo only).
    pub y2: i16,
}

// Compile-time size assertion: PathCmd must be exactly 14 bytes
// (kind:1 + pad:1 + x:2 + y:2 + x1:2 + y1:2 + x2:2 + y2:2 = 14).
const _: () = assert!(core::mem::size_of::<PathCmd>() == 14);

impl PathCmd {
    /// Create a MoveTo command.
    pub const fn move_to(x: i16, y: i16) -> Self {
        Self { kind: PathCmdKind::MoveTo, _pad: 0, x, y, x1: 0, y1: 0, x2: 0, y2: 0 }
    }
    /// Create a LineTo command.
    pub const fn line_to(x: i16, y: i16) -> Self {
        Self { kind: PathCmdKind::LineTo, _pad: 0, x, y, x1: 0, y1: 0, x2: 0, y2: 0 }
    }
    /// Create a CurveTo command (cubic Bézier).
    /// `(x1,y1)` and `(x2,y2)` are control points, `(x,y)` is the endpoint.
    pub const fn curve_to(x1: i16, y1: i16, x2: i16, y2: i16, x: i16, y: i16) -> Self {
        Self { kind: PathCmdKind::CurveTo, _pad: 0, x, y, x1, y1, x2, y2 }
    }
    /// Create a Close command.
    pub const fn close() -> Self {
        Self { kind: PathCmdKind::Close, _pad: 0, x: 0, y: 0, x1: 0, y1: 0, x2: 0, y2: 0 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PathCmdKind {
    MoveTo = 0,
    LineTo = 1,
    Close = 2,
    CurveTo = 3,
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
        let used = self.data_used() as usize;

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
    /// Push an array of `TextRun` structs into the data buffer.
    /// Aligns the write offset to `align_of::<TextRun>()` first.
    /// Returns a `DataRef` and the count.
    pub fn push_text_runs(&mut self, runs: &[TextRun]) -> (DataRef, u16) {
        // Align data_used to TextRun alignment (typically 4 bytes).
        let align = core::mem::align_of::<TextRun>();
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            self.header_mut().data_used = aligned as u32;
        }

        // SAFETY: TextRun is #[repr(C)] with no padding, so
        // transmuting to bytes is safe for serialization.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                runs.as_ptr() as *const u8,
                runs.len() * core::mem::size_of::<TextRun>(),
            )
        };

        (self.push_data(bytes), runs.len() as u16)
    }
    /// Push an array of `PathCmd` structs into the data buffer.
    /// Aligns the write offset to `align_of::<PathCmd>()` first.
    /// Returns a `DataRef` covering the path command data.
    pub fn push_path_cmds(&mut self, cmds: &[PathCmd]) -> DataRef {
        let align = core::mem::align_of::<PathCmd>();
        let used = self.header().data_used as usize;
        let aligned = (used + align - 1) & !(align - 1);

        if aligned > used && aligned <= DATA_BUFFER_SIZE {
            self.header_mut().data_used = aligned as u32;
        }

        // SAFETY: PathCmd is #[repr(C)] — transmuting to bytes is safe.
        let bytes = unsafe {
            core::slice::from_raw_parts(
                cmds.as_ptr() as *const u8,
                cmds.len() * core::mem::size_of::<PathCmd>(),
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
        let used = self.data_used() as usize;

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
    /// `glyph_count` is the number of glyphs expected (from `TextRun::glyph_count`).
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
    /// Interpret a DataRef as an array of `TextRun` structs.
    pub fn text_runs(&self, dref: DataRef) -> &[TextRun] {
        let bytes = self.data(dref);
        let run_size = core::mem::size_of::<TextRun>();

        if bytes.is_empty() || bytes.len() < run_size {
            return &[];
        }

        let count = bytes.len() / run_size;

        // SAFETY: TextRun is repr(C), data buffer is aligned to node size
        // which is >= alignment of TextRun.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const TextRun, count) }
    }
    /// Interpret a DataRef as an array of `PathCmd` structs.
    pub fn path_cmds(&self, dref: DataRef) -> &[PathCmd] {
        let bytes = self.data(dref);
        let cmd_size = core::mem::size_of::<PathCmd>();

        if bytes.is_empty() || bytes.len() < cmd_size {
            return &[];
        }

        let count = bytes.len() / cmd_size;

        // SAFETY: PathCmd is #[repr(C)], data buffer is aligned by
        // push_path_cmds to PathCmd alignment.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const PathCmd, count) }
    }
}

// ── Double-buffered scene graph ─────────────────────────────────────

/// Byte offset of the reader state region, placed after both scene buffers.
/// Contains `reader_done_gen: u32` — the generation the reader last finished
/// reading. The writer checks this before overwriting the back buffer.
pub const READER_STATE_OFFSET: usize = 2 * SCENE_SIZE;

/// Total size for a double-buffered scene graph: two full scene buffers
/// side by side, plus an 8-byte reader state region (4-byte generation +
/// 4-byte padding for alignment).
///
/// The writer writes to the back buffer (lower generation), then `swap()`
/// publishes it as the new front. The reader always reads the front buffer
/// (higher generation) and calls `finish_read()` to signal completion.
/// The writer checks `reader_done_gen` before overwriting the back buffer.
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
    unsafe {
        core::ptr::write_volatile(
            buf.as_ptr().add(READER_STATE_OFFSET) as *mut u32,
            value,
        )
    }
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
        let back_hdr =
            unsafe { &mut *(self.buf.as_mut_ptr().add(back_off) as *mut SceneHeader) };

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
    /// Interpret a DataRef from the front buffer as TextRun array.
    pub fn front_text_runs(&self, dref: DataRef) -> &[TextRun] {
        let bytes = self.front_data(dref);
        let run_size = core::mem::size_of::<TextRun>();

        if bytes.is_empty() || bytes.len() < run_size {
            return &[];
        }

        let count = bytes.len() / run_size;

        // SAFETY: TextRun is #[repr(C)]. Data buffer is aligned to NODE_SIZE
        // (>= TextRun alignment). `count` is bounded by available bytes.
        // The slice borrows `self`, preventing concurrent mutation.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const TextRun, count) }
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
    /// Interpret a DataRef from the front buffer as TextRun array.
    pub fn front_text_runs(&self, dref: DataRef) -> &[TextRun] {
        let bytes = self.front_data(dref);
        let run_size = core::mem::size_of::<TextRun>();

        if bytes.is_empty() || bytes.len() < run_size {
            return &[];
        }

        let count = bytes.len() / run_size;

        // SAFETY: TextRun is #[repr(C)]. Data buffer is aligned to NODE_SIZE
        // (>= TextRun alignment). `count` is bounded by available bytes.
        // The acquire fence at construction ensures visibility.
        unsafe { core::slice::from_raw_parts(bytes.as_ptr() as *const TextRun, count) }
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
pub fn abs_bounds(nodes: &[Node], parent_map: &[NodeId; MAX_NODES], id: usize) -> (i32, i32, u32, u32) {
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

        let new_x = ax - left;
        let new_y = ay - top;
        let new_w = (node.width as i32 + left + right).max(0) as u32;
        let new_h = (node.height as i32 + top + bottom).max(0) as u32;

        return (new_x, new_y, new_w, new_h);
    }

    (ax, ay, node.width as u32, node.height as u32)
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
    let n = prev_count.min(prev_nodes.len()).min(curr_nodes.len()).min(MAX_NODES);
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
