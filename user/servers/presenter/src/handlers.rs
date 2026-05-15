//! Content handlers — per-mimetype components that produce view subtrees.

use scene::{Color, Content, SceneWriter, pt, upt};
use view_tree::{
    Constraints, ContentHandler, EventResponse, InputEvent, IntrinsicSize, ViewContent, ViewNode,
    ViewSubtree, ViewTree,
};

// ── Image handler ───────────────────────────────────────────────────

pub struct ImageHandler {
    pub content_id: u32,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl ContentHandler for ImageHandler {
    fn load(&mut self, _content: &[u8], _constraints: &Constraints) -> ViewSubtree {
        let mut tree = ViewTree::new();
        let root = tree.add(ViewNode {
            intrinsic: IntrinsicSize::Fixed {
                width: upt(self.pixel_width as u32),
                height: upt(self.pixel_height as u32),
            },
            role: scene::ROLE_IMAGE,
            content: ViewContent::Image {
                content_id: self.content_id,
                src_width: self.pixel_width,
                src_height: self.pixel_height,
            },
            shadow_color: Color::rgba(0, 0, 0, 255),
            shadow_blur_radius: upt(presenter_service::SHADOW_BLUR_RADIUS as u32),
            shadow_spread: pt(presenter_service::SHADOW_SPREAD as i32),
            ..Default::default()
        });

        ViewSubtree { tree, root }
    }

    fn resize(&mut self, constraints: &Constraints) -> ViewSubtree {
        self.load(&[], constraints)
    }

    fn update(&mut self, content: &[u8]) -> ViewSubtree {
        self.load(
            content,
            &Constraints {
                available_width: 0,
                available_height: 0,
            },
        )
    }

    fn event(&mut self, _event: &InputEvent) -> EventResponse {
        EventResponse::Unhandled
    }

    fn teardown(&mut self) {}
}

// ── View node → scene node conversion ───────────────────────────────

pub fn write_view_node(
    node: &ViewNode,
    scene: &mut SceneWriter,
    parent: scene::NodeId,
    x: scene::Mpt,
    y: scene::Mpt,
    width: scene::Umpt,
    height: scene::Umpt,
) {
    let scene_id = match scene.alloc_node() {
        Some(id) => id,
        None => return,
    };
    let n = scene.node_mut(scene_id);

    n.x = x;
    n.y = y;
    n.width = width;
    n.height = height;
    n.background = node.background;
    n.opacity = node.opacity;
    n.corner_radius = (node.corner_radius / scene::MPT_PER_PT as u32).min(255) as u8;
    n.shadow_color = node.shadow_color;
    n.shadow_blur_radius = (node.shadow_blur_radius / scene::MPT_PER_PT as u32).min(255) as u8;
    n.shadow_spread = (node.shadow_spread / scene::MPT_PER_PT).clamp(-128, 127) as i8;
    n.shadow_offset_x = (node.shadow_offset_x / scene::MPT_PER_PT).clamp(-32768, 32767) as i16;
    n.shadow_offset_y = (node.shadow_offset_y / scene::MPT_PER_PT).clamp(-32768, 32767) as i16;
    n.role = node.role;
    n.level = node.level;
    n.cursor_shape = node.cursor_shape;

    if node.clips_children {
        n.flags = scene::NodeFlags::VISIBLE.union(scene::NodeFlags::CLIPS_CHILDREN);
    }
    if !node.transform.is_identity() {
        n.transform = node.transform;
    }

    match &node.content {
        ViewContent::Image {
            content_id,
            src_width,
            src_height,
        } => {
            n.content = Content::Image {
                content_id: *content_id,
                src_width: *src_width,
                src_height: *src_height,
            };
        }
        ViewContent::Glyphs {
            glyphs,
            color,
            font_size,
            style_id,
        } => {
            let glyph_ref = scene.push_shaped_glyphs(glyphs);
            let n = scene.node_mut(scene_id);

            n.content = Content::Glyphs {
                color: *color,
                glyphs: glyph_ref,
                glyph_count: glyphs.len() as u16,
                font_size: *font_size,
                style_id: *style_id,
            };
        }
        ViewContent::Path {
            commands,
            color,
            stroke_color,
            fill_rule,
            stroke_width,
            content_hash,
        } => {
            let path_ref = scene.push_path_commands(commands);
            let n = scene.node_mut(scene_id);

            n.content = Content::Path {
                color: *color,
                stroke_color: *stroke_color,
                fill_rule: *fill_rule,
                stroke_width: *stroke_width,
                contours: path_ref,
            };
            n.content_hash = *content_hash;
        }
        ViewContent::Gradient {
            color_start,
            color_end,
            kind,
            angle_fp,
        } => {
            n.content = Content::Gradient {
                color_start: *color_start,
                color_end: *color_end,
                kind: *kind,
                _pad: 0,
                angle_fp: *angle_fp,
            };
        }
        ViewContent::GradientPath {
            color_start,
            color_end,
            kind,
            angle_fp,
            commands,
        } => {
            let path_ref = scene.push_path_commands(commands);
            let n = scene.node_mut(scene_id);

            n.content = Content::GradientPath {
                color_start: *color_start,
                color_end: *color_end,
                kind: *kind,
                _pad: 0,
                angle_fp: *angle_fp,
                contours: path_ref,
            };
        }
        ViewContent::None => {}
    }

    scene.add_child(parent, scene_id);
}
