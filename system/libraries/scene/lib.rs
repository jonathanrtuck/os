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

            pub const fn empty() -> Self { Self(0) }
            pub const fn bits(self) -> $ty { self.0 }
            pub const fn contains(self, other: Self) -> bool { self.0 & other.0 == other.0 }
            pub const fn union(self, other: Self) -> Self { Self(self.0 | other.0) }
        }

        impl core::ops::BitOr for $name {
            type Output = Self;

            fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
        }
        impl core::ops::BitAnd for $name {
            type Output = Self;

            fn bitand(self, rhs: Self) -> Self { Self(self.0 & rhs.0) }
        }
    };
}

// ── Primitive types ─────────────────────────────────────────────────

/// Index into the node array. `NULL` means no node.
pub type NodeId = u16;
pub const NULL: NodeId = u16::MAX;

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
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
}

/// A reference to variable-length data in the data buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct DataRef {
    pub offset: u32,
    pub length: u32,
}
/// Border specification: uniform width and color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub struct Border {
    pub color: Color,
    pub width: u8,
    pub _pad: [u8; 3],
}

bitflags! {
    /// Node flags packed into a single byte.
    pub struct NodeFlags: u8 {
        const CLIPS_CHILDREN = 0b0000_0001;
        const VISIBLE        = 0b0000_0010;
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

const _: () = assert!(core::mem::size_of::<SceneHeader>() == 64);

pub const NODES_OFFSET: usize = core::mem::size_of::<SceneHeader>();
pub const DATA_OFFSET: usize = NODES_OFFSET + MAX_NODES * core::mem::size_of::<Node>();
pub const SCENE_SIZE: usize = DATA_OFFSET + DATA_BUFFER_SIZE;

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
