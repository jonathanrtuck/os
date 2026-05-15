//! View tree — the single source of truth for the document's live state.
//!
//! Every content type produces view tree nodes. All rendering pipelines
//! (GPU compositor, CLI, screen reader, braille) consume them. The view
//! tree is the fan-out point where modality-specific pipelines diverge.
//!
//! Each node carries all properties any consumer might need:
//! - **Layout:** display mode, position, margins, padding, sizing
//! - **Semantic:** role, level, state, accessible name
//! - **Visual:** background, border, shadow, opacity, transform
//! - **Content:** text glyphs, image reference, vector path, gradient
//! - **Tree structure:** first_child / next_sibling indices
//!
//! Consumers pick the properties relevant to them. The layout engine
//! reads layout fields. The screen reader reads semantic fields. The
//! GPU renderer reads everything. The view tree makes no assumptions
//! about how it will be used.

#![no_std]

extern crate alloc;

use alloc::{string::String, vec, vec::Vec};

use scene::{
    AffineTransform, Animation, Color, FillRule, GradientKind, Mpt, NULL, NodeId, ROLE_NONE,
    ShapedGlyph, Umpt,
};

// ── Display mode ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// Block-level box. Children stack vertically.
    #[default]
    Block,
    /// Inline-level box. Content flows horizontally, wraps at width.
    Inline,
    /// Children at explicit coordinates within a fixed-size canvas.
    FixedCanvas,
    /// Children at explicit coordinates, no bounds.
    Freeform,
    /// Not displayed. Skipped by layout and rendering.
    None,
}

// ── Position type ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    /// Normal flow position.
    #[default]
    Static,
    /// Positioned at explicit coordinates, out of flow.
    Absolute,
}

// ── Box model edges ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Default for Edges {
    fn default() -> Self {
        Self::ZERO
    }
}

// ── Dimension ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dimension {
    /// Size determined by content or parent constraints.
    #[default]
    Auto,
    /// Fixed size in millipoints.
    Points(Umpt),
}

// ── Intrinsic sizing ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntrinsicSize {
    /// Not a leaf — size comes from children.
    #[default]
    Container,
    /// Known intrinsic dimensions (images, video frames).
    Fixed { width: Umpt, height: Umpt },
    /// Needs the ContentMeasurer to determine size at a given width.
    Measure,
}

// ── Content measurer trait ───────────────────────────────────────────

pub trait ContentMeasurer {
    /// Given a leaf node and available width, return (used_width, height).
    fn measure(&self, node_id: NodeId, available_width: Umpt) -> (Umpt, Umpt);
}

// ── View content ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ViewContent {
    /// Container — no content beyond decoration.
    None,
    /// Shaped glyph run (one font, one color).
    Glyphs {
        glyphs: Vec<ShapedGlyph>,
        color: Color,
        font_size: u16,
        style_id: u32,
    },
    /// Reference to a texture uploaded to the compositor.
    Image {
        content_id: u32,
        src_width: u16,
        src_height: u16,
    },
    /// Vector path contours.
    Path {
        commands: Vec<u8>,
        color: Color,
        stroke_color: Color,
        fill_rule: FillRule,
        stroke_width: u16,
        content_hash: u32,
    },
    /// GPU-evaluated gradient fill.
    Gradient {
        color_start: Color,
        color_end: Color,
        kind: GradientKind,
        angle_fp: u16,
    },
    /// Path filled with a gradient.
    GradientPath {
        color_start: Color,
        color_end: Color,
        kind: GradientKind,
        angle_fp: u16,
        commands: Vec<u8>,
    },
    /// Portal into a child viewer's subtree (compound document composition).
    Portal { child_idx: u16 },
}

impl Default for ViewContent {
    fn default() -> Self {
        Self::None
    }
}

// ── ViewNode ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ViewNode {
    // ── Layout ──────────────────────────────────────────────────
    pub display: Display,
    pub position: Position,
    pub margin: Edges,
    pub padding: Edges,
    pub border: Edges,
    pub width: Dimension,
    pub height: Dimension,
    pub min_width: Dimension,
    pub min_height: Dimension,
    pub max_width: Dimension,
    pub max_height: Dimension,
    pub offset_x: Mpt,
    pub offset_y: Mpt,
    pub intrinsic: IntrinsicSize,

    // ── Semantic ────────────────────────────────────────────────
    pub role: u8,
    pub level: u8,
    pub state: u32,
    pub name: Option<String>,
    pub order: u16,

    // ── Visual ─────────────────────────────────────────────────
    pub background: Color,
    pub color: Color,
    pub border_color: Color,
    pub corner_radius: Umpt,
    pub opacity: u8,
    pub shadow_color: Color,
    pub shadow_blur_radius: Umpt,
    pub shadow_spread: Mpt,
    pub shadow_offset_x: Mpt,
    pub shadow_offset_y: Mpt,
    pub backdrop_blur_radius: Umpt,
    pub clips_children: bool,
    pub child_offset_x: Mpt,
    pub child_offset_y: Mpt,
    pub cursor_shape: u8,
    pub transform: AffineTransform,
    pub animation: Animation,

    // ── Content ─────────────────────────────────────────────────
    pub content: ViewContent,

    // ── Tree structure ──────────────────────────────────────────
    pub first_child: NodeId,
    pub next_sibling: NodeId,
}

impl Default for ViewNode {
    fn default() -> Self {
        Self {
            display: Display::Block,
            position: Position::Static,
            margin: Edges::ZERO,
            padding: Edges::ZERO,
            border: Edges::ZERO,
            width: Dimension::Auto,
            height: Dimension::Auto,
            min_width: Dimension::Auto,
            min_height: Dimension::Auto,
            max_width: Dimension::Auto,
            max_height: Dimension::Auto,
            offset_x: 0,
            offset_y: 0,
            intrinsic: IntrinsicSize::Container,
            role: ROLE_NONE,
            level: 0,
            state: 0,
            name: None,
            order: 0,
            background: Color::TRANSPARENT,
            color: Color::TRANSPARENT,
            border_color: Color::TRANSPARENT,
            corner_radius: 0,
            opacity: 255,
            shadow_color: Color::TRANSPARENT,
            shadow_blur_radius: 0,
            shadow_spread: 0,
            shadow_offset_x: 0,
            shadow_offset_y: 0,
            backdrop_blur_radius: 0,
            clips_children: false,
            child_offset_x: 0,
            child_offset_y: 0,
            cursor_shape: 0,
            transform: AffineTransform::identity(),
            animation: Animation::NONE,
            content: ViewContent::None,
            first_child: NULL,
            next_sibling: NULL,
        }
    }
}

// ── LayoutBox (output) ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutBox {
    /// Position relative to parent's content box.
    pub x: Mpt,
    pub y: Mpt,
    /// Computed border-box dimensions.
    pub width: Umpt,
    pub height: Umpt,
    /// Carried forward for the scene graph builder.
    pub padding: Edges,
    pub border: Edges,
}

impl LayoutBox {
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        padding: Edges::ZERO,
        border: Edges::ZERO,
    };
}

impl Default for LayoutBox {
    fn default() -> Self {
        Self::EMPTY
    }
}

// ── ViewTree container ───────────────────────────────────────────────

pub struct ViewTree {
    nodes: Vec<ViewNode>,
}

impl Default for ViewTree {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewTree {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn add(&mut self, node: ViewNode) -> NodeId {
        let id = self.nodes.len();

        assert!(id < NULL as usize, "view tree node limit exceeded");

        self.nodes.push(node);

        id as NodeId
    }

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        let first = self.nodes[parent as usize].first_child;

        if first == NULL {
            self.nodes[parent as usize].first_child = child;

            return;
        }

        let mut cur = first;

        loop {
            let next = self.nodes[cur as usize].next_sibling;

            if next == NULL {
                self.nodes[cur as usize].next_sibling = child;

                return;
            }

            cur = next;
        }
    }

    pub fn get(&self, id: NodeId) -> &ViewNode {
        &self.nodes[id as usize]
    }

    pub fn get_mut(&mut self, id: NodeId) -> &mut ViewNode {
        &mut self.nodes[id as usize]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn nodes(&self) -> &[ViewNode] {
        &self.nodes
    }
}

// ── Child iterator ───────────────────────────────────────────────────

pub struct ChildIter<'a> {
    nodes: &'a [ViewNode],
    cur: NodeId,
}

impl<'a> Iterator for ChildIter<'a> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.cur == NULL {
            return Option::None;
        }

        let id = self.cur;

        self.cur = self.nodes[id as usize].next_sibling;

        Some(id)
    }
}

pub fn children(nodes: &[ViewNode], parent: NodeId) -> ChildIter<'_> {
    ChildIter {
        nodes,
        cur: nodes[parent as usize].first_child,
    }
}

// ── Viewer trait ─────────────────────────────────────────────────────

pub struct ViewSubtree {
    pub tree: ViewTree,
    pub root: NodeId,
    pub layout: Vec<LayoutBox>,
}

pub struct Constraints {
    pub available_width: Umpt,
    pub available_height: Umpt,
    #[allow(dead_code)]
    pub now_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResponse {
    Handled,
    Unhandled,
}

pub struct InputEvent {
    pub kind: u8,
    pub key_code: u8,
    pub modifiers: u8,
    pub character: u8,
    pub pointer_x: i32,
    pub pointer_y: i32,
    pub button: u8,
}

pub trait Viewer {
    fn subtree(&self) -> &ViewSubtree;
    fn event(&mut self, event: &InputEvent) -> EventResponse;
    fn teardown(&mut self);
}

#[deprecated(note = "renamed to Viewer")]
pub use Viewer as ContentHandler;

// ── Dimension resolution (private) ───────────────────────────────────

fn clamp_dimension(value: Umpt, min: Dimension, max: Dimension) -> Umpt {
    let v = match max {
        Dimension::Points(m) if value > m => m,
        _ => value,
    };

    match min {
        Dimension::Points(m) if v < m => m,
        _ => v,
    }
}

fn resolve_width(node: &ViewNode, available: Umpt) -> Umpt {
    let raw = match node.width {
        Dimension::Points(w) => w,
        Dimension::Auto => available,
    };

    clamp_dimension(raw, node.min_width, node.max_width)
}

fn resolve_height(node: &ViewNode, content_height: Umpt) -> Umpt {
    let raw = match node.height {
        Dimension::Points(h) => h,
        Dimension::Auto => content_height,
    };

    clamp_dimension(raw, node.min_height, node.max_height)
}

fn content_box_width(node: &ViewNode, border_box_width: Umpt) -> Umpt {
    let inset = (node.padding.horizontal() + node.border.horizontal()) as Umpt;

    border_box_width.saturating_sub(inset)
}

// ── Layout engine ────────────────────────────────────────────────────

/// Compute layout for a view tree rooted at `root`.
///
/// Returns a `Vec<LayoutBox>` with one entry per node (indexed by NodeId).
/// Nodes not visited (e.g. Display::None) have `LayoutBox::EMPTY`.
pub fn layout(
    nodes: &[ViewNode],
    root: NodeId,
    available_width: Umpt,
    available_height: Umpt,
    measurer: &dyn ContentMeasurer,
) -> Vec<LayoutBox> {
    let mut boxes = vec![LayoutBox::EMPTY; nodes.len()];

    layout_node(
        nodes,
        root,
        available_width,
        available_height,
        measurer,
        &mut boxes,
    );

    boxes
}

fn layout_node(
    nodes: &[ViewNode],
    id: NodeId,
    available_width: Umpt,
    available_height: Umpt,
    measurer: &dyn ContentMeasurer,
    boxes: &mut [LayoutBox],
) {
    let node = &nodes[id as usize];

    if node.display == Display::None {
        return;
    }

    match node.intrinsic {
        IntrinsicSize::Fixed { width, height } => {
            let w = match node.width {
                Dimension::Points(pw) => pw,
                Dimension::Auto => width,
            };
            let h = match node.height {
                Dimension::Points(ph) => ph,
                Dimension::Auto => height,
            };

            boxes[id as usize] = LayoutBox {
                x: 0,
                y: 0,
                width: clamp_dimension(w, node.min_width, node.max_width),
                height: clamp_dimension(h, node.min_height, node.max_height),
                padding: node.padding,
                border: node.border,
            };

            return;
        }
        IntrinsicSize::Measure => {
            let mw = resolve_width(node, available_width);
            let (used_w, used_h) = measurer.measure(id, mw);

            boxes[id as usize] = LayoutBox {
                x: 0,
                y: 0,
                width: clamp_dimension(used_w, node.min_width, node.max_width),
                height: clamp_dimension(used_h, node.min_height, node.max_height),
                padding: node.padding,
                border: node.border,
            };

            return;
        }
        IntrinsicSize::Container => {}
    }

    match node.display {
        Display::Block | Display::Inline => {
            layout_block(
                nodes,
                id,
                available_width,
                available_height,
                measurer,
                boxes,
            );
        }
        Display::FixedCanvas | Display::Freeform => {
            layout_fixed(
                nodes,
                id,
                available_width,
                available_height,
                measurer,
                boxes,
            );
        }
        Display::None => {}
    }
}

fn layout_block(
    nodes: &[ViewNode],
    id: NodeId,
    available_width: Umpt,
    _available_height: Umpt,
    measurer: &dyn ContentMeasurer,
    boxes: &mut [LayoutBox],
) {
    let node = &nodes[id as usize];
    let border_box_w = resolve_width(node, available_width);
    let content_w = content_box_width(node, border_box_w);
    let mut y: Mpt = 0;
    let mut prev_margin_bottom: Mpt = 0;
    let mut first_child = true;
    let mut child_id = node.first_child;

    while child_id != NULL {
        let child = &nodes[child_id as usize];
        let next = child.next_sibling;

        if child.display == Display::None {
            child_id = next;

            continue;
        }

        if child.position == Position::Absolute {
            let child_avail_w = match child.width {
                Dimension::Points(w) => w,
                Dimension::Auto => content_w,
            };
            let child_avail_h = match child.height {
                Dimension::Points(h) => h,
                Dimension::Auto => 0,
            };

            layout_node(
                nodes,
                child_id,
                child_avail_w,
                child_avail_h,
                measurer,
                boxes,
            );

            boxes[child_id as usize].x = child.offset_x;
            boxes[child_id as usize].y = child.offset_y;
            child_id = next;

            continue;
        }

        let child_margin_h = child.margin.horizontal() as Umpt;
        let child_avail_w = content_w.saturating_sub(child_margin_h);

        layout_node(nodes, child_id, child_avail_w, 0, measurer, boxes);

        let child_margin_top = child.margin.top;

        if first_child {
            y += child_margin_top;
        } else {
            y += core::cmp::max(prev_margin_bottom, child_margin_top);
        }

        boxes[child_id as usize].x = child.margin.left;
        boxes[child_id as usize].y = y;

        let child_box_h = boxes[child_id as usize].height as Mpt;

        y += child_box_h;
        prev_margin_bottom = child.margin.bottom;
        first_child = false;
        child_id = next;
    }

    if !first_child {
        y += prev_margin_bottom;
    }

    let pad_v = (node.padding.vertical() + node.border.vertical()) as Umpt;
    let content_h = y as Umpt;
    let border_box_h = resolve_height(node, content_h.saturating_add(pad_v));

    boxes[id as usize] = LayoutBox {
        x: 0,
        y: 0,
        width: border_box_w,
        height: border_box_h,
        padding: node.padding,
        border: node.border,
    };
}

fn layout_fixed(
    nodes: &[ViewNode],
    id: NodeId,
    available_width: Umpt,
    available_height: Umpt,
    measurer: &dyn ContentMeasurer,
    boxes: &mut [LayoutBox],
) {
    let node = &nodes[id as usize];
    let border_box_w = resolve_width(node, available_width);
    let border_box_h = resolve_height(node, available_height);
    let content_w = content_box_width(node, border_box_w);
    let mut child_id = node.first_child;

    while child_id != NULL {
        let child = &nodes[child_id as usize];
        let next = child.next_sibling;

        if child.display == Display::None {
            child_id = next;

            continue;
        }

        let child_avail_w = match child.width {
            Dimension::Points(w) => w,
            Dimension::Auto => content_w,
        };
        let child_avail_h = match child.height {
            Dimension::Points(h) => h,
            Dimension::Auto => border_box_h,
        };

        layout_node(
            nodes,
            child_id,
            child_avail_w,
            child_avail_h,
            measurer,
            boxes,
        );

        boxes[child_id as usize].x = child.offset_x;
        boxes[child_id as usize].y = child.offset_y;
        child_id = next;
    }

    boxes[id as usize] = LayoutBox {
        x: 0,
        y: 0,
        width: border_box_w,
        height: border_box_h,
        padding: node.padding,
        border: node.border,
    };
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use scene::{pt, upt};

    use super::*;

    // ── Test measurers ───────────────────────────────────────────────

    struct NoMeasurer;
    impl ContentMeasurer for NoMeasurer {
        fn measure(&self, _: NodeId, _: Umpt) -> (Umpt, Umpt) {
            panic!("unexpected measure call");
        }
    }

    struct FixedMeasurer(Umpt, Umpt);
    impl ContentMeasurer for FixedMeasurer {
        fn measure(&self, _: NodeId, _: Umpt) -> (Umpt, Umpt) {
            (self.0, self.1)
        }
    }

    struct WidthDependentMeasurer;
    impl ContentMeasurer for WidthDependentMeasurer {
        fn measure(&self, _: NodeId, available: Umpt) -> (Umpt, Umpt) {
            (available, upt(20))
        }
    }

    // ── Edges ────────────────────────────────────────────────────────

    #[test]
    fn edges_zero() {
        let e = Edges::ZERO;

        assert_eq!(e.horizontal(), 0);
        assert_eq!(e.vertical(), 0);
    }

    #[test]
    fn edges_uniform() {
        let e = Edges::uniform(pt(10));

        assert_eq!(e.top, pt(10));
        assert_eq!(e.right, pt(10));
        assert_eq!(e.horizontal(), pt(20));
        assert_eq!(e.vertical(), pt(20));
    }

    #[test]
    fn edges_asymmetric() {
        let e = Edges {
            top: pt(1),
            right: pt(2),
            bottom: pt(3),
            left: pt(4),
        };

        assert_eq!(e.horizontal(), pt(6));
        assert_eq!(e.vertical(), pt(4));
    }

    // ── Dimension ────────────────────────────────────────────────────

    #[test]
    fn dimension_default_is_auto() {
        assert_eq!(Dimension::default(), Dimension::Auto);
    }

    // ── ViewNode default ─────────────────────────────────────────────

    #[test]
    fn view_node_default() {
        let n = ViewNode::default();

        assert_eq!(n.display, Display::Block);
        assert_eq!(n.position, Position::Static);
        assert_eq!(n.first_child, NULL);
        assert_eq!(n.next_sibling, NULL);
        assert_eq!(n.margin, Edges::ZERO);
        assert_eq!(n.width, Dimension::Auto);
        assert_eq!(n.role, ROLE_NONE);
        assert_eq!(n.intrinsic, IntrinsicSize::Container);
    }

    // ── LayoutBox ────────────────────────────────────────────────────

    #[test]
    fn layout_box_empty() {
        let b = LayoutBox::EMPTY;

        assert_eq!(b.x, 0);
        assert_eq!(b.y, 0);
        assert_eq!(b.width, 0);
        assert_eq!(b.height, 0);
    }

    // ── ViewTree ─────────────────────────────────────────────────────

    #[test]
    fn tree_add_node() {
        let mut tree = ViewTree::new();
        let id = tree.add(ViewNode::default());

        assert_eq!(id, 0);
        assert_eq!(tree.len(), 1);
    }

    #[test]
    fn tree_parent_child() {
        let mut tree = ViewTree::new();
        let parent = tree.add(ViewNode::default());
        let child1 = tree.add(ViewNode::default());
        let child2 = tree.add(ViewNode::default());

        tree.append_child(parent, child1);
        tree.append_child(parent, child2);

        assert_eq!(tree.get(parent).first_child, child1);
        assert_eq!(tree.get(child1).next_sibling, child2);
        assert_eq!(tree.get(child2).next_sibling, NULL);
    }

    #[test]
    fn children_iterator() {
        let mut tree = ViewTree::new();
        let parent = tree.add(ViewNode::default());
        let c1 = tree.add(ViewNode::default());
        let c2 = tree.add(ViewNode::default());
        let c3 = tree.add(ViewNode::default());

        tree.append_child(parent, c1);
        tree.append_child(parent, c2);
        tree.append_child(parent, c3);

        let ids: Vec<NodeId> = children(tree.nodes(), parent).collect();

        assert_eq!(ids, vec![c1, c2, c3]);
    }

    #[test]
    fn children_of_leaf_is_empty() {
        let mut tree = ViewTree::new();
        let leaf = tree.add(ViewNode::default());
        let ids: Vec<NodeId> = children(tree.nodes(), leaf).collect();

        assert!(ids.is_empty());
    }

    // ── Dimension resolution ─────────────────────────────────────────

    #[test]
    fn resolve_width_auto_uses_available() {
        let node = ViewNode::default();

        assert_eq!(resolve_width(&node, upt(500)), upt(500));
    }

    #[test]
    fn resolve_width_fixed() {
        let node = ViewNode {
            width: Dimension::Points(upt(200)),
            ..Default::default()
        };

        assert_eq!(resolve_width(&node, upt(500)), upt(200));
    }

    #[test]
    fn resolve_width_clamped_by_max() {
        let node = ViewNode {
            max_width: Dimension::Points(upt(300)),
            ..Default::default()
        };

        assert_eq!(resolve_width(&node, upt(500)), upt(300));
    }

    #[test]
    fn resolve_width_clamped_by_min() {
        let node = ViewNode {
            width: Dimension::Points(upt(100)),
            min_width: Dimension::Points(upt(200)),
            ..Default::default()
        };

        assert_eq!(resolve_width(&node, upt(500)), upt(200));
    }

    #[test]
    fn resolve_height_auto_uses_content() {
        let node = ViewNode::default();

        assert_eq!(resolve_height(&node, upt(300)), upt(300));
    }

    #[test]
    fn resolve_height_fixed() {
        let node = ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        };

        assert_eq!(resolve_height(&node, upt(300)), upt(100));
    }

    #[test]
    fn content_box_subtracts_padding_and_border() {
        let node = ViewNode {
            padding: Edges::uniform(pt(10)),
            border: Edges::uniform(pt(2)),
            ..Default::default()
        };

        assert_eq!(content_box_width(&node, upt(500)), upt(500) - upt(24));
    }

    #[test]
    fn content_box_saturates_at_zero() {
        let node = ViewNode {
            padding: Edges::uniform(pt(300)),
            ..Default::default()
        };

        assert_eq!(content_box_width(&node, upt(100)), 0);
    }

    // ── Fixed canvas layout ──────────────────────────────────────────

    #[test]
    fn fixed_single_child_at_offset() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(800)),
            height: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            position: Position::Absolute,
            offset_x: pt(100),
            offset_y: pt(50),
            width: Dimension::Points(upt(200)),
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(800), upt(600), &NoMeasurer);

        assert_eq!(boxes[child as usize].x, pt(100));
        assert_eq!(boxes[child as usize].y, pt(50));
        assert_eq!(boxes[child as usize].width, upt(200));
        assert_eq!(boxes[child as usize].height, upt(100));
    }

    #[test]
    fn fixed_child_with_intrinsic() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(800)),
            height: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            offset_x: pt(10),
            offset_y: pt(20),
            intrinsic: IntrinsicSize::Fixed {
                width: upt(300),
                height: upt(200),
            },
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(800), upt(600), &NoMeasurer);

        assert_eq!(boxes[child as usize].width, upt(300));
        assert_eq!(boxes[child as usize].height, upt(200));
        assert_eq!(boxes[child as usize].x, pt(10));
        assert_eq!(boxes[child as usize].y, pt(20));
    }

    #[test]
    fn fixed_child_with_measure() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(800)),
            height: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            offset_x: pt(0),
            offset_y: pt(0),
            intrinsic: IntrinsicSize::Measure,
            ..Default::default()
        });

        tree.append_child(root, child);

        let m = FixedMeasurer(upt(100), upt(50));
        let boxes = layout(tree.nodes(), root, upt(800), upt(600), &m);

        assert_eq!(boxes[child as usize].width, upt(100));
        assert_eq!(boxes[child as usize].height, upt(50));
    }

    #[test]
    fn fixed_multiple_children() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(800)),
            height: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let c1 = tree.add(ViewNode {
            offset_x: pt(0),
            offset_y: pt(0),
            width: Dimension::Points(upt(100)),
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });
        let c2 = tree.add(ViewNode {
            offset_x: pt(200),
            offset_y: pt(300),
            width: Dimension::Points(upt(50)),
            height: Dimension::Points(upt(50)),
            ..Default::default()
        });

        tree.append_child(root, c1);
        tree.append_child(root, c2);

        let boxes = layout(tree.nodes(), root, upt(800), upt(600), &NoMeasurer);

        assert_eq!(boxes[c1 as usize].x, pt(0));
        assert_eq!(boxes[c2 as usize].x, pt(200));
        assert_eq!(boxes[c2 as usize].y, pt(300));
    }

    #[test]
    fn fixed_display_none_skipped() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(800)),
            height: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let hidden = tree.add(ViewNode {
            display: Display::None,
            ..Default::default()
        });

        tree.append_child(root, hidden);

        let boxes = layout(tree.nodes(), root, upt(800), upt(600), &NoMeasurer);

        assert_eq!(boxes[hidden as usize], LayoutBox::EMPTY);
    }

    // ── Block flow layout ────────────────────────────────────────────

    #[test]
    fn block_single_child() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[child as usize].width, upt(400));
        assert_eq!(boxes[child as usize].height, upt(100));
        assert_eq!(boxes[child as usize].x, 0);
        assert_eq!(boxes[child as usize].y, 0);
        assert_eq!(boxes[root as usize].height, upt(100));
    }

    #[test]
    fn block_children_stack_vertically() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let c1 = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });
        let c2 = tree.add(ViewNode {
            height: Dimension::Points(upt(200)),
            ..Default::default()
        });

        tree.append_child(root, c1);
        tree.append_child(root, c2);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[c1 as usize].y, 0);
        assert_eq!(boxes[c2 as usize].y, upt(100) as Mpt);
        assert_eq!(boxes[root as usize].height, upt(300));
    }

    #[test]
    fn block_child_margin() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            margin: Edges {
                top: pt(10),
                right: pt(20),
                bottom: pt(10),
                left: pt(20),
            },
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[child as usize].x, pt(20));
        assert_eq!(boxes[child as usize].y, pt(10));
        assert_eq!(boxes[child as usize].width, upt(400) - upt(40));
        assert_eq!(boxes[root as usize].height, upt(120));
    }

    #[test]
    fn block_padding_creates_inset() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            padding: Edges::uniform(pt(20)),
            ..Default::default()
        });
        let child = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[child as usize].width, upt(360));
        assert_eq!(boxes[root as usize].height, upt(140));
        assert_eq!(boxes[root as usize].padding, Edges::uniform(pt(20)));
    }

    #[test]
    fn block_auto_width_fills_available() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode::default());
        let child = tree.add(ViewNode {
            height: Dimension::Points(upt(50)),
            ..Default::default()
        });

        tree.append_child(root, child);

        let boxes = layout(tree.nodes(), root, upt(600), upt(1000), &NoMeasurer);

        assert_eq!(boxes[root as usize].width, upt(600));
        assert_eq!(boxes[child as usize].width, upt(600));
    }

    #[test]
    fn block_leaf_with_measure() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let leaf = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Measure,
            ..Default::default()
        });

        tree.append_child(root, leaf);

        let m = FixedMeasurer(upt(100), upt(50));
        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &m);

        assert_eq!(boxes[leaf as usize].width, upt(100));
        assert_eq!(boxes[leaf as usize].height, upt(50));
    }

    #[test]
    fn block_leaf_with_intrinsic_fixed() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let leaf = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Fixed {
                width: upt(200),
                height: upt(150),
            },
            ..Default::default()
        });

        tree.append_child(root, leaf);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[leaf as usize].width, upt(200));
        assert_eq!(boxes[leaf as usize].height, upt(150));
    }

    // ── Margin collapsing ────────────────────────────────────────────

    #[test]
    fn margin_collapse_adjacent_siblings() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let c1 = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            margin: Edges {
                top: pt(0),
                right: pt(0),
                bottom: pt(20),
                left: pt(0),
            },
            ..Default::default()
        });
        let c2 = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            margin: Edges {
                top: pt(30),
                right: pt(0),
                bottom: pt(0),
                left: pt(0),
            },
            ..Default::default()
        });

        tree.append_child(root, c1);
        tree.append_child(root, c2);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[c2 as usize].y, pt(130));
        assert_eq!(boxes[root as usize].height, upt(230));
    }

    #[test]
    fn margin_collapse_equal() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let c1 = tree.add(ViewNode {
            height: Dimension::Points(upt(50)),
            margin: Edges {
                top: pt(0),
                right: pt(0),
                bottom: pt(15),
                left: pt(0),
            },
            ..Default::default()
        });
        let c2 = tree.add(ViewNode {
            height: Dimension::Points(upt(50)),
            margin: Edges {
                top: pt(15),
                right: pt(0),
                bottom: pt(0),
                left: pt(0),
            },
            ..Default::default()
        });

        tree.append_child(root, c1);
        tree.append_child(root, c2);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[c2 as usize].y, pt(65));
    }

    // ── Mixed layout + nesting ───────────────────────────────────────

    #[test]
    fn block_containing_fixed_canvas() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(800)),
            ..Default::default()
        });
        let canvas = tree.add(ViewNode {
            display: Display::FixedCanvas,
            width: Dimension::Points(upt(400)),
            height: Dimension::Points(upt(300)),
            ..Default::default()
        });
        let canvas_child = tree.add(ViewNode {
            position: Position::Absolute,
            offset_x: pt(10),
            offset_y: pt(20),
            width: Dimension::Points(upt(50)),
            height: Dimension::Points(upt(50)),
            ..Default::default()
        });

        tree.append_child(root, canvas);
        tree.append_child(canvas, canvas_child);

        let boxes = layout(tree.nodes(), root, upt(800), upt(1000), &NoMeasurer);

        assert_eq!(boxes[canvas as usize].y, 0);
        assert_eq!(boxes[canvas as usize].width, upt(400));
        assert_eq!(boxes[canvas as usize].height, upt(300));
        assert_eq!(boxes[canvas_child as usize].x, pt(10));
        assert_eq!(boxes[canvas_child as usize].y, pt(20));
    }

    #[test]
    fn block_absolute_child_out_of_flow() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(400)),
            ..Default::default()
        });
        let flow1 = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });
        let abs = tree.add(ViewNode {
            position: Position::Absolute,
            offset_x: pt(50),
            offset_y: pt(50),
            width: Dimension::Points(upt(80)),
            height: Dimension::Points(upt(80)),
            ..Default::default()
        });
        let flow2 = tree.add(ViewNode {
            height: Dimension::Points(upt(100)),
            ..Default::default()
        });

        tree.append_child(root, flow1);
        tree.append_child(root, abs);
        tree.append_child(root, flow2);

        let boxes = layout(tree.nodes(), root, upt(400), upt(1000), &NoMeasurer);

        assert_eq!(boxes[abs as usize].x, pt(50));
        assert_eq!(boxes[abs as usize].y, pt(50));
        assert_eq!(boxes[flow2 as usize].y, upt(100) as Mpt);
        assert_eq!(boxes[root as usize].height, upt(200));
    }

    #[test]
    fn three_level_nesting() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(600)),
            ..Default::default()
        });
        let middle = tree.add(ViewNode {
            padding: Edges::uniform(pt(20)),
            ..Default::default()
        });
        let leaf = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Fixed {
                width: upt(100),
                height: upt(80),
            },
            ..Default::default()
        });

        tree.append_child(root, middle);
        tree.append_child(middle, leaf);

        let boxes = layout(tree.nodes(), root, upt(600), upt(1000), &NoMeasurer);

        assert_eq!(boxes[leaf as usize].width, upt(100));
        assert_eq!(boxes[leaf as usize].height, upt(80));
        assert_eq!(boxes[middle as usize].width, upt(600));
        assert_eq!(boxes[middle as usize].height, upt(120));
        assert_eq!(boxes[root as usize].height, upt(120));
    }

    #[test]
    fn measure_receives_available_width() {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            width: Dimension::Points(upt(500)),
            ..Default::default()
        });
        let leaf = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Measure,
            ..Default::default()
        });

        tree.append_child(root, leaf);

        let boxes = layout(
            tree.nodes(),
            root,
            upt(500),
            upt(1000),
            &WidthDependentMeasurer,
        );

        assert_eq!(boxes[leaf as usize].width, upt(500));
        assert_eq!(boxes[leaf as usize].height, upt(20));
    }
}
