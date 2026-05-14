//! View tree sketch — the shared input to the layout engine.
//!
//! This is a design sketch, not compilable code. It shows what a minimal
//! view tree looks like for the OS's current needs (block flow + fixed
//! canvas), with extension points for flex and grid.
//!
//! The view tree is produced by front-ends (manifest parser, future CSS
//! cascade) and consumed by the layout engine, which outputs positioned
//! boxes. Content handlers then build scene graph nodes from the positioned
//! output.
//!
//! Uses the existing coordinate system: Mpt (signed millipoints) for
//! positions/offsets, Umpt (unsigned millipoints) for dimensions.

use crate::node::{Mpt, NULL, NodeId, Umpt};

// ── Display mode ─────────────────────────────────────────────────────
//
// Determines which layout algorithm applies to this node's children.
// Matches manifest spatial modes and CSS display values.

/// How a node participates in layout and how it lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    /// Block-level box in a flow context. Children stack vertically.
    /// Manifest: `spatial: Flow`. CSS: `display: block`.
    Block,

    /// Inline-level box in a flow context. Content flows horizontally,
    /// wraps at container width.
    /// CSS: `display: inline`.
    Inline,

    /// Children positioned at explicit coordinates within a fixed-size
    /// canvas. No flow, no constraint negotiation.
    /// Manifest: `spatial: FixedCanvas`. CSS: `position: absolute`.
    FixedCanvas,

    /// Children positioned at explicit coordinates, no bounds.
    /// Manifest: `spatial: Freeform`.
    Freeform,

    // ── Future ──────────────────────────────────────────────────────
    // Flex,       // CSS: `display: flex`
    // Grid,       // CSS: `display: grid`. Manifest: `spatial: Grid`
    // InlineBlock, // CSS: `display: inline-block`
    /// Not displayed. Skipped entirely by layout and rendering.
    /// CSS: `display: none`.
    None,
}

// ── Position type ────────────────────────────────────────────────────
//
// Separate from Display because CSS separates them: a node can be
// `display: block; position: absolute`. In the manifest model, this
// distinction doesn't exist (FixedCanvas implies absolute positioning
// for children), but keeping them separate supports the CSS front-end.

/// How a node is positioned relative to its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Normal flow position (default).
    /// CSS: `position: static` / `position: relative`.
    Static,

    /// Positioned at explicit coordinates relative to the nearest
    /// FixedCanvas ancestor (or the viewport).
    /// CSS: `position: absolute` / `position: fixed`.
    Absolute,
}

// ── Box model ────────────────────────────────────────────────────────

/// Four-edge values for margin, padding, border width.
/// All values in Mpt (signed, because margins can be negative).
#[derive(Debug, Clone, Copy)]
pub struct Edges {
    pub top: Mpt,
    pub right: Mpt,
    pub bottom: Mpt,
    pub left: Mpt,
}

impl Edges {
    pub const ZERO: Self = Self {
        top: 0,
        right: 0,
        bottom: 0,
        left: 0,
    };

    pub const fn uniform(v: Mpt) -> Self {
        Self {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }

    pub const fn horizontal(&self) -> Mpt {
        self.left + self.right
    }
    pub const fn vertical(&self) -> Mpt {
        self.top + self.bottom
    }
}

// ── Dimension ────────────────────────────────────────────────────────

/// A length that can be auto-sized, fixed, or (future) percentage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    /// Size determined by content or parent constraints.
    Auto,

    /// Fixed size in millipoints.
    Points(Umpt),
    // ── Future ──────────────────────────────────────────────────────
    // Percent(u16),        // 0..10000 = 0%..100.00%
    // MinContent,
    // MaxContent,
    // FitContent(Umpt),
}

impl Default for Dimension {
    fn default() -> Self {
        Dimension::Auto
    }
}

// ── Content sizing ───────────────────────────────────────────────────
//
// Leaf nodes have intrinsic content that affects layout. The layout
// engine needs to know their size, which may depend on available width
// (text reflows; images don't).

/// How the layout engine determines a leaf node's intrinsic size.
pub enum IntrinsicSize {
    /// Not a leaf — size comes from children.
    Container,

    /// Fixed intrinsic dimensions (images, video frames).
    /// The layout engine uses these directly. Aspect ratio is
    /// preserved when only one dimension is constrained.
    Fixed { width: Umpt, height: Umpt },

    /// Content that reflows at different widths (text, markdown).
    /// The layout engine calls the measure function with the
    /// available width; the function returns the height needed.
    ///
    /// This is the constraint negotiation: parent proposes width,
    /// child reports height. The function encapsulates all content-
    /// specific logic (font metrics, line breaking, paragraph layout).
    Measure(MeasureFn),
}

/// Signature: given available width in Mpt, return (used_width, height).
/// The layout engine calls this during the layout pass.
///
/// For text: runs the existing layout_paragraph algorithm internally
/// and returns the resulting dimensions.
///
/// For future content types: wraps whatever content-specific sizing
/// logic the handler needs.
pub type MeasureFn = fn(available_width: Umpt) -> (Umpt, Umpt);

// -- Alternative: trait object instead of function pointer, for
// -- handlers that need state (e.g., font metrics, cached paragraph):
//
// pub trait Measurable {
//     fn measure(&self, available_width: Umpt) -> (Umpt, Umpt);
// }

// ── The view node ──────────────────────────────────────────────────
//
// This is the central type. One per logical element in the document.
// Produced by front-ends, consumed by the layout engine.

pub struct ViewNode {
    // ── Layout ──────────────────────────────────────────────────────
    /// Which layout algorithm applies to this node's children.
    pub display: Display,

    /// How this node is positioned (normal flow or absolute).
    pub position: Position,

    /// Margin outside the border box. Collapses in block flow.
    pub margin: Edges,

    /// Padding inside the border box.
    pub padding: Edges,

    /// Border width (affects box sizing; visual border is a style
    /// concern that passes through to the scene graph, not layout).
    pub border: Edges,

    /// Explicit width. Auto = determined by content/parent.
    pub width: Dimension,
    /// Explicit height. Auto = determined by content/parent.
    pub height: Dimension,

    /// Minimum width constraint.
    pub min_width: Dimension,
    /// Minimum height constraint.
    pub min_height: Dimension,
    /// Maximum width constraint.
    pub max_width: Dimension,
    /// Maximum height constraint.
    pub max_height: Dimension,

    /// For Position::Absolute: offset from the containing block.
    /// For Position::Static: ignored.
    pub offset_x: Mpt,
    pub offset_y: Mpt,

    // ── Content ─────────────────────────────────────────────────────
    /// How the layout engine determines this node's intrinsic size.
    pub intrinsic: IntrinsicSize,

    // ── Tree structure ──────────────────────────────────────────────
    // Flat array with linked indices, consistent with scene graph
    // and manifest model.
    pub first_child: NodeId,
    pub next_sibling: NodeId,
}

// ── Layout output ────────────────────────────────────────────────────
//
// The layout engine produces one of these per ViewNode.
// position + size are in the coordinate space of the parent.

pub struct LayoutBox {
    /// Position relative to parent's content box.
    pub x: Mpt,
    pub y: Mpt,

    /// Computed dimensions of the border box.
    pub width: Umpt,
    pub height: Umpt,

    // The padding and border from the input are carried forward so
    // the scene graph builder can compute the content box for
    // placing children.
    pub padding: Edges,
    pub border: Edges,
}

// ── How the manifest maps to view nodes ────────────────────────────
//
// Manifest composition tree          →  View tree
// ─────────────────────────────────────────────────────────────────────
// root [spatial: Flow]               →  ViewNode { display: Block }
// root [spatial: FixedCanvas(W,H)]   →  ViewNode { display: FixedCanvas,
//                                                     width: Points(W),
//                                                     height: Points(H) }
// child [spatial: rect(x,y,w,h)]     →  ViewNode { position: Absolute,
//                                                     offset_x: x, offset_y: y,
//                                                     width: Points(w),
//                                                     height: Points(h) }
// child [spatial: flow(Float Right)]  →  ViewNode { display: Block }
//                                        (float is a future extension)
// leaf [slot: 0, image]              →  ViewNode { intrinsic: Fixed { w, h } }
// leaf [slot: 1, text]               →  ViewNode { intrinsic: Measure(text_fn) }
//
// ── How CSS would map to view nodes (future) ──────────────────────
//
// div { display: block; margin: 12pt; padding: 8pt }
//   →  ViewNode { display: Block,
//                    margin: Edges::uniform(pt(12)),
//                    padding: Edges::uniform(pt(8)) }
//
// img { width: 200pt; height: auto }
//   →  ViewNode { width: Points(upt(200)),
//                    height: Auto,
//                    intrinsic: Fixed { w, h } }
//
// .absolute { position: absolute; left: 100pt; top: 50pt }
//   →  ViewNode { position: Absolute,
//                    offset_x: pt(100), offset_y: pt(50) }

// ── Layout algorithm (block flow, simplified) ────────────────────────
//
// fn layout_block(node: &ViewNode, available_width: Umpt) -> LayoutBox {
//     let content_width = resolve_width(node, available_width);
//     let mut y: Mpt = node.padding.top + node.border.top;
//
//     for child in node.children() {
//         match child.position {
//             Position::Static => {
//                 let child_available = content_width
//                     - child.margin.horizontal()
//                     - child.padding.horizontal()
//                     - child.border.horizontal();
//
//                 let child_box = match child.display {
//                     Display::Block => layout_block(child, child_available),
//                     Display::FixedCanvas => layout_fixed(child),
//                     _ => todo!(),
//                 };
//
//                 // Margin collapsing (simplified — top margin of first
//                 // child collapses with parent's top padding).
//                 y += child.margin.top;
//                 // Position child at (margin_left + padding + border, y).
//                 y += child_box.height as Mpt;
//                 y += child.margin.bottom;
//             }
//             Position::Absolute => {
//                 // Positioned at explicit offset, doesn't affect flow.
//                 layout_absolute(child, content_width);
//             }
//         }
//     }
//
//     let content_height = resolve_height(node, y);
//     LayoutBox {
//         x: node.margin.left,
//         y: node.margin.top,
//         width: content_width + node.padding.horizontal() + node.border.horizontal(),
//         height: content_height + node.padding.vertical() + node.border.vertical(),
//         padding: node.padding,
//         border: node.border,
//     }
// }
//
// fn layout_fixed(node: &ViewNode) -> LayoutBox {
//     // Children positioned at their explicit offsets.
//     // Each child's size is either explicit or intrinsic.
//     for child in node.children() {
//         let child_box = match child.intrinsic {
//             IntrinsicSize::Fixed { w, h } => LayoutBox { ..., width: w, height: h },
//             IntrinsicSize::Measure(f) => {
//                 let child_w = resolve_width(child, node.width);
//                 let (_, h) = f(child_w);
//                 LayoutBox { ..., width: child_w, height: h }
//             }
//             IntrinsicSize::Container => layout_block(child, resolve_width(child, node.width)),
//         };
//         // Position at child.offset_x, child.offset_y.
//     }
// }
