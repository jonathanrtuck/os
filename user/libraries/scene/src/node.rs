//! Scene graph node type, header, and memory layout constants.

use crate::{
    primitives::{Animation, Border, Color, Content, DataRef, bitflags},
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

// ── Cursor shape constants ──────────────────────────────────────────

/// Inherit cursor shape from the nearest ancestor with a declaration.
pub const CURSOR_INHERIT: u8 = 0;
/// Default arrow cursor (non-interactive regions).
pub const CURSOR_DEFAULT: u8 = 1;
/// Text/I-beam cursor (text content regions).
pub const CURSOR_TEXT: u8 = 2;
/// Pressable cursor (clickable interactive elements).
pub const CURSOR_PRESSABLE: u8 = 3;
/// Disabled cursor (non-interactive/unavailable elements).
pub const CURSOR_DISABLED: u8 = 4;

// ── Semantic roles ──────────────────────────────────────────────────
//
// Flat u8 enum for `Node::role`. A node has exactly one role.
// ROLE_NONE means decorative — assistive technology skips it.
// Values are grouped with gaps for future insertion without renumbering.

/// No semantic meaning. Purely decorative (shadows, backgrounds).
pub const ROLE_NONE: u8 = 0;

// Document structure (1-19)
/// The document itself — the root content surface.
pub const ROLE_DOCUMENT: u8 = 1;
/// Heading. Use `level` field for depth (1-6).
pub const ROLE_HEADING: u8 = 2;
/// Body text paragraph.
pub const ROLE_PARAGRAPH: u8 = 3;
/// Raster or vector image.
pub const ROLE_IMAGE: u8 = 4;
/// Preformatted / code block.
pub const ROLE_CODE_BLOCK: u8 = 5;

// System UI / chrome (20-39)
/// Toolbar or title bar.
pub const ROLE_TOOLBAR: u8 = 20;
/// Static text label (document title, clock).
pub const ROLE_LABEL: u8 = 21;
/// Scrollable content viewport.
pub const ROLE_SCROLL_VIEWPORT: u8 = 22;
/// Decorative or informational icon.
pub const ROLE_ICON: u8 = 23;

// Editor (40-49)
/// Text insertion cursor (caret).
pub const ROLE_CARET: u8 = 40;
/// Selected region highlight.
pub const ROLE_SELECTION: u8 = 41;

// Inline text semantics (50-69)
/// Bold / strong emphasis.
pub const ROLE_STRONG: u8 = 50;
/// Italic / emphasis.
pub const ROLE_EMPHASIS: u8 = 51;
/// Inline code span.
pub const ROLE_CODE: u8 = 52;

// Future roles (values not yet assigned):
//
// Document structure: blockquote, list, list-item, table, table-row,
//   table-cell, table-header, figure, caption, footnote, section,
//   separator, annotation, link
//
// Inline text: subscript, superscript, insertion, deletion, highlight,
//   abbreviation
//
// System UI: status-bar, menu-bar, menu, menu-item, tooltip,
//   notification, landmark
//
// Widgets (80-119)
/// Clickable button.
pub const ROLE_BUTTON: u8 = 80;

// Future widget roles: toggle-button, text-field, search-field,
//   checkbox, radio-button, switch, slider, progress, spinner,
//   combobox, dropdown, tab-list, tab, tab-panel, dialog, alert
//
// Media (120-139): video, audio, canvas, embedded-object
//
// Compound document (140-159): embed-frame, split-pane

// ── Accessibility state flags ──────────────────────────────────────
//
// Bitfield for `Node::state`. Test with `node.state & STATE_FOCUSED != 0`.
// Multi-valued states use paired flags (e.g., ORIENTED + HORIZONTAL).

/// Node currently has keyboard focus.
pub const STATE_FOCUSED: u32 = 1 << 0;
/// Node can receive keyboard focus.
pub const STATE_FOCUSABLE: u32 = 1 << 1;
/// Node is currently selected.
pub const STATE_SELECTED: u32 = 1 << 2;
/// Node is in edit mode (OS view/edit distinction).
pub const STATE_EDITABLE: u32 = 1 << 3;
/// Node is visible but not modifiable.
pub const STATE_READ_ONLY: u32 = 1 << 4;
/// Node is loading, decoding, or processing.
pub const STATE_BUSY: u32 = 1 << 5;
/// Pointer is currently over this node.
pub const STATE_HOVERED: u32 = 1 << 6;

// Future state flags (bits not yet assigned):
//
// Core: DISABLED, EXPANDED, CHECKED, PRESSED, REQUIRED, INVALID,
//   MODAL, MULTILINE, HAS_POPUP, DEFAULT, INDETERMINATE
//
// Multi-valued pairs: ORIENTED + HORIZONTAL (orientation specified +
//   direction), CHECKABLE + CHECKED + INDETERMINATE, SORTABLE +
//   SORT_ASCENDING

// ── Accessibility relation types ───────────────────────────────────
//
// Encoded in the data buffer as 4-byte entries referenced by
// `Node::relations`. Each entry: [type: u8, target: NodeId (u16), pad: u8].

/// This node is named by the target node.
pub const REL_LABELLED_BY: u8 = 1;
/// This node is described by the target node.
pub const REL_DESCRIBED_BY: u8 = 2;
/// This node controls the target node's state or content.
pub const REL_CONTROLS: u8 = 3;
/// Reading order continues at the target node (overrides tree order).
pub const REL_FLOWS_TO: u8 = 4;
/// The active (focused) child within this composite widget.
pub const REL_ACTIVE_DESCENDANT: u8 = 5;
/// The target node explains this node's error state.
pub const REL_ERROR_MESSAGE: u8 = 6;
/// The target node provides extended details for this node.
pub const REL_DETAILS: u8 = 7;

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
    // ── geometry (relative to parent content area, in millipoints) ──
    pub x: i32,
    pub y: i32,
    pub width: Umpt,
    pub height: Umpt,
    // ── child offset ──
    /// Translation applied to children's coordinate space (points).
    /// Used for scrolling and document slide. (0, 0) by default.
    pub child_offset_x: f32,
    pub child_offset_y: f32,
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
    /// Cursor shape to show when the pointer is over this node.
    /// 0 = inherit from parent, 1 = pointer/arrow, 2 = text/I-beam.
    /// Core reads this during hit-testing; the render driver ignores it.
    pub cursor_shape: u8,
    /// Reserved for future cursor/hit-test fields. Must be zero.
    pub _reserved: [u8; 3],
    // ── accessibility ──
    /// Semantic role (see `ROLE_*` constants). Determines what assistive
    /// technology announces and how it navigates. `ROLE_NONE` (0) means
    /// purely decorative — AT skips this node.
    pub role: u8,
    /// Hierarchical depth: heading level (1-6), list nesting, tree depth.
    /// 0 means not applicable.
    pub level: u8,
    /// Alignment padding. Must be zero.
    pub _pad: [u8; 2],
    /// Accessibility state flags (see `STATE_*` constants).
    pub state: u32,
    /// Accessible name: human-readable label in the data buffer (UTF-8).
    /// `DataRef::EMPTY` means derive from content (text nodes are
    /// self-naming via their glyph data) or no name (decorative nodes).
    pub name: DataRef,
    /// Typed relationships to other nodes. Encoded as packed 4-byte
    /// entries in the data buffer: `[relation_type: u8, target: NodeId,
    /// pad: u8]`. `DataRef::EMPTY` means no relationships.
    pub relations: DataRef,
    // ── animation ──
    pub animation: Animation,
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
        child_offset_x: 0.0,
        child_offset_y: 0.0,
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
        cursor_shape: 0, // inherit
        _reserved: [0; 3],
        role: ROLE_NONE,
        level: 0,
        _pad: [0; 2],
        state: 0,
        name: DataRef::EMPTY,
        relations: DataRef::EMPTY,
        animation: Animation::NONE,
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

// Compile-time size assertion: Node must be exactly 144 bytes.
// This prevents silent shared-memory layout drift between core and compositor.
// If you add a field, update this assertion and verify both sides agree.
// Layout: 84 (tree+geometry+decoration+transform+hash) + 8 (clip_path)
//       + 4 (cursor_shape+_reserved) + 24 (accessibility) + 24 (content) = 144.
const _: () = assert!(core::mem::size_of::<Node>() == 176);

// ── Shared memory layout ────────────────────────────────────────────

pub const MAX_NODES: usize = 512;
pub const DATA_BUFFER_SIZE: usize = 128 * 1024;
pub const NODES_OFFSET: usize = core::mem::size_of::<SceneHeader>();
pub const DATA_OFFSET: usize = NODES_OFFSET + MAX_NODES * core::mem::size_of::<Node>();
pub const SCENE_SIZE: usize = DATA_OFFSET + DATA_BUFFER_SIZE;

/// Byte offset of the `generation` field within `SceneHeader`.
pub const GENERATION_OFFSET: usize = 0;

/// Number of u64 words in the dirty bitmap (512 bits / 64 = 8 words).
pub const DIRTY_BITMAP_WORDS: usize = 8;

pub const MAX_DAMAGE_RECTS: usize = 4;

const _: () = assert!(core::mem::size_of::<SceneHeader>() == 152);

/// Axis-aligned damage rectangle in millipoints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct DamageRect {
    pub x: Mpt,
    pub y: Mpt,
    pub w: Umpt,
    pub h: Umpt,
}

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
    /// Number of damage rects (0 = full repaint, 1-4 = partial).
    pub damage_count: u8,
    pub _pad: [u8; 3],
    /// Up to 4 damage rects in millipoints.
    pub damage_rects: [DamageRect; MAX_DAMAGE_RECTS],
    /// Last generation the compositor successfully read. The presenter
    /// uses this to accumulate damage across skipped generations.
    pub reader_gen: u32,
}
