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

// ── Content variant ─────────────────────────────────────────────────

/// What a node draws (beyond its container decoration).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub enum Content {
    /// Pure container -- no content, just children and decoration.
    None,
    /// A run of text. The compositor owns layout (line breaking, wrapping).
    Text {
        /// Reference to UTF-8 string in the data buffer.
        data: DataRef,
        /// Font size in pixels.
        font_size: u16,
        /// Text color.
        color: Color,
        /// Cursor position as byte offset, or `u32::MAX` for no cursor.
        cursor: u32,
        /// Selection range as (start, end) byte offsets. Equal = no selection.
        sel_start: u32,
        sel_end: u32,
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
        content: Content::None,
    };

    pub fn clips_children(&self) -> bool {
        self.flags.contains(NodeFlags::CLIPS_CHILDREN)
    }
    pub fn visible(&self) -> bool {
        self.flags.contains(NodeFlags::VISIBLE)
    }
}

// ── Shared memory layout ────────────────────────────────────────────

pub const MAX_NODES: usize = 512;
pub const DATA_BUFFER_SIZE: usize = 64 * 1024;
pub const NODES_OFFSET: usize = core::mem::size_of::<SceneHeader>();
pub const DATA_OFFSET: usize = NODES_OFFSET + MAX_NODES * core::mem::size_of::<Node>();
pub const SCENE_SIZE: usize = DATA_OFFSET + DATA_BUFFER_SIZE;

const _: () = assert!(core::mem::size_of::<SceneHeader>() == 64);

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
    pub _reserved: [u8; 52],
}

// ── Path commands ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C)]
pub struct PathCmd {
    pub kind: PathCmdKind,
    pub x: i16,
    pub y: i16,
    pub _pad: [u8; 1],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PathCmdKind {
    MoveTo = 0,
    LineTo = 1,
    Close = 2,
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

        // Zero the header to establish initial state.
        let hdr = unsafe { &mut *(buf.as_mut_ptr() as *mut SceneHeader) };

        hdr.generation = 0;
        hdr.node_count = 0;
        hdr.root = NULL;
        hdr.data_used = 0;

        Self { buf }
    }

    fn header(&self) -> &SceneHeader {
        unsafe { &*(self.buf.as_ptr() as *const SceneHeader) }
    }
    fn header_mut(&mut self) -> &mut SceneHeader {
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
    pub fn clear(&mut self) {
        self.header_mut().node_count = 0;
        self.header_mut().data_used = 0;
        self.header_mut().root = NULL;
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
    /// Get a shared reference to a node by ID.
    pub fn node(&self, id: NodeId) -> &Node {
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    /// Get a mutable reference to a node by ID.
    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        let offset = NODES_OFFSET + (id as usize) * NODE_SIZE;

        unsafe { &mut *(self.buf.as_mut_ptr().add(offset) as *mut Node) }
    }
    /// Get all live nodes as a read-only slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

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

        unsafe { &*(self.buf.as_ptr().add(offset) as *const Node) }
    }
    /// Get all live nodes as a slice.
    pub fn nodes(&self) -> &[Node] {
        let count = self.node_count() as usize;
        let ptr = unsafe { self.buf.as_ptr().add(NODES_OFFSET) as *const Node };

        unsafe { core::slice::from_raw_parts(ptr, count) }
    }
    pub fn node_count(&self) -> u16 {
        self.header().node_count
    }
    pub fn root(&self) -> NodeId {
        self.header().root
    }
}
