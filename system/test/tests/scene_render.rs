//! Tests for scene_render: subtree clip skipping optimization.
//!
//! Verifies that render_node's child clip-skip check produces
//! pixel-identical output to rendering without the optimisation.

#[path = "../../services/compositor/scene_render.rs"]
mod scene_render;

use drawing::{Color, PixelFormat, Surface};
use scene::{Node, NULL};

// NodeFlags is already imported by the scene_render module and its
// #[path] inclusion makes it available. Use a direct re-import from scene.
#[allow(unused_imports)]
use scene::NodeFlags;

// ── Helpers ─────────────────────────────────────────────────────────

/// Zeroed GlyphCache for tests that don't use text rendering.
/// GlyphCache is large (~1.3 MiB), so we Box + zero it.
fn zeroed_glyph_cache() -> Box<fonts::cache::GlyphCache> {
    // SAFETY: GlyphCache is repr(C)-like with all-integer fields.
    // A zeroed instance is valid — all metrics are 0 and coverage
    // buffers are empty. No text lookups will succeed, which is fine
    // because our test nodes don't contain text content.
    unsafe {
        let layout = std::alloc::Layout::new::<fonts::cache::GlyphCache>();
        let ptr = std::alloc::alloc_zeroed(layout) as *mut fonts::cache::GlyphCache;
        assert!(!ptr.is_null());
        Box::from_raw(ptr)
    }
}

/// Build a RenderCtx with zeroed glyph caches and no icon.
fn test_ctx<'a>(
    mono: &'a fonts::cache::GlyphCache,
    prop: &'a fonts::cache::GlyphCache,
) -> scene_render::RenderCtx<'a> {
    scene_render::RenderCtx {
        mono_cache: mono,
        prop_cache: prop,
        icon_coverage: &[],
        icon_w: 0,
        icon_h: 0,
        icon_color: Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
        icon_node: NULL,
        scale: 1,
    }
}

/// Create a 100×100 BGRA surface filled with opaque black.
fn black_surface(buf: &mut [u8]) -> Surface {
    // Fill with opaque black (BGRA: B=0, G=0, R=0, A=255)
    for pixel in buf.chunks_exact_mut(4) {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 255;
    }
    Surface {
        data: buf,
        width: 100,
        height: 100,
        stride: 100 * 4,
        format: PixelFormat::Bgra8888,
    }
}

/// Read a pixel at (x, y) from the data buffer as (R, G, B, A).
fn read_pixel(data: &[u8], stride: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let off = (y * stride + x * 4) as usize;
    // BGRA format: [B, G, R, A]
    (data[off + 2], data[off + 1], data[off], data[off + 3])
}

/// Build a minimal scene graph with a root and colored child nodes at
/// known positions. Returns (nodes, data_buf).
///
/// Layout (100×100 logical, scale=1):
///   root: (0,0) 100×100, clips children
///     child_a: (0,0)   30×30, background=RED   (top-left)
///     child_b: (70,0)  30×30, background=GREEN (top-right)
///     child_c: (0,70)  30×30, background=BLUE  (bottom-left)
///     child_d: (70,70) 30×30, background=WHITE (bottom-right)
fn build_four_corner_scene() -> (Vec<Node>, Vec<u8>) {
    let red = scene::Color::rgba(255, 0, 0, 255);
    let green = scene::Color::rgba(0, 255, 0, 255);
    let blue = scene::Color::rgba(0, 0, 255, 255);
    let white = scene::Color::rgba(255, 255, 255, 255);

    let mut nodes = vec![Node::EMPTY; 5];

    // Node 0: root — full surface, clips children
    nodes[0].x = 0;
    nodes[0].y = 0;
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Node 1: child_a — top-left red square
    nodes[1].x = 0;
    nodes[1].y = 0;
    nodes[1].width = 30;
    nodes[1].height = 30;
    nodes[1].background = red;
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].next_sibling = 2;

    // Node 2: child_b — top-right green square
    nodes[2].x = 70;
    nodes[2].y = 0;
    nodes[2].width = 30;
    nodes[2].height = 30;
    nodes[2].background = green;
    nodes[2].flags = NodeFlags::VISIBLE;
    nodes[2].next_sibling = 3;

    // Node 3: child_c — bottom-left blue square
    nodes[3].x = 0;
    nodes[3].y = 70;
    nodes[3].width = 30;
    nodes[3].height = 30;
    nodes[3].background = blue;
    nodes[3].flags = NodeFlags::VISIBLE;
    nodes[3].next_sibling = 4;

    // Node 4: child_d — bottom-right white square
    nodes[4].x = 70;
    nodes[4].y = 70;
    nodes[4].width = 30;
    nodes[4].height = 30;
    nodes[4].background = white;
    nodes[4].flags = NodeFlags::VISIBLE;

    (nodes, vec![])
}

// ── Tests ───────────────────────────────────────────────────────────

/// Full-screen render visits all children — verify all four corners drawn.
#[test]
fn full_screen_clip_renders_all_children() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    let stride = 100 * 4;

    // Check centres of each child
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 15);
    assert_eq!((r, g, b), (255, 0, 0), "top-left should be red");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 15);
    assert_eq!((r, g, b), (0, 255, 0), "top-right should be green");

    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 85);
    assert_eq!((r, g, b), (0, 0, 255), "bottom-left should be blue");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 85);
    assert_eq!((r, g, b), (255, 255, 255), "bottom-right should be white");
}

/// Partial clip covering only the top-left child.
/// Children outside the clip must not be rendered (their regions stay black).
#[test]
fn clip_to_top_left_only_renders_red_child() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);

    let dirty = protocol::DirtyRect::new(0, 0, 35, 35);
    scene_render::render_scene_clipped(&mut fb, &graph, &ctx, &dirty);

    let stride = 100 * 4;

    // Red child (top-left) should be drawn
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 15);
    assert_eq!((r, g, b), (255, 0, 0), "top-left should be red");

    // All other corners should remain black (not rendered)
    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 15);
    assert_eq!((r, g, b), (0, 0, 0), "top-right should stay black");

    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 85);
    assert_eq!((r, g, b), (0, 0, 0), "bottom-left should stay black");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 85);
    assert_eq!((r, g, b), (0, 0, 0), "bottom-right should stay black");
}

/// Partial clip covering only the bottom-right child.
#[test]
fn clip_to_bottom_right_only_renders_white_child() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);

    let dirty = protocol::DirtyRect::new(65, 65, 35, 35);
    scene_render::render_scene_clipped(&mut fb, &graph, &ctx, &dirty);

    let stride = 100 * 4;

    // White child (bottom-right) should be drawn
    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 85);
    assert_eq!((r, g, b), (255, 255, 255), "bottom-right should be white");

    // All other corners should remain black
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 15);
    assert_eq!((r, g, b), (0, 0, 0), "top-left should stay black");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 15);
    assert_eq!((r, g, b), (0, 0, 0), "top-right should stay black");

    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 85);
    assert_eq!((r, g, b), (0, 0, 0), "bottom-left should stay black");
}

/// Clip that spans two children (top row) should render both, leave bottom black.
#[test]
fn clip_spanning_top_row_renders_red_and_green() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);

    // Clip the full width, but only the top 35 pixels
    let dirty = protocol::DirtyRect::new(0, 0, 100, 35);
    scene_render::render_scene_clipped(&mut fb, &graph, &ctx, &dirty);

    let stride = 100 * 4;

    // Top-left red and top-right green should be drawn
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 15);
    assert_eq!((r, g, b), (255, 0, 0), "top-left should be red");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 15);
    assert_eq!((r, g, b), (0, 255, 0), "top-right should be green");

    // Bottom children should remain black
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 85);
    assert_eq!((r, g, b), (0, 0, 0), "bottom-left should stay black");

    let (r, g, b, _a) = read_pixel(&buf, stride, 85, 85);
    assert_eq!((r, g, b), (0, 0, 0), "bottom-right should stay black");
}

/// Pixel-identical: full-screen render vs composited partial clips.
/// Four separate clipped renders (one per corner) should produce the
/// same framebuffer as a single full-screen render.
#[test]
fn composited_partial_clips_match_full_render() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Full render (reference)
    let mut ref_buf = vec![0u8; 100 * 100 * 4];
    {
        let mut ref_fb = black_surface(&mut ref_buf);
        scene_render::render_scene(&mut ref_fb, &graph, &ctx);
    }

    // Partial renders composited into one surface
    let mut comp_buf = vec![0u8; 100 * 100 * 4];
    {
        let mut comp_fb = black_surface(&mut comp_buf);

        // Four quadrant clips covering the full 100×100
        let quads = [
            protocol::DirtyRect::new(0, 0, 50, 50),
            protocol::DirtyRect::new(50, 0, 50, 50),
            protocol::DirtyRect::new(0, 50, 50, 50),
            protocol::DirtyRect::new(50, 50, 50, 50),
        ];
        for dirty in &quads {
            scene_render::render_scene_clipped(&mut comp_fb, &graph, &ctx, dirty);
        }
    }

    // Compare all pixels
    assert_eq!(
        ref_buf, comp_buf,
        "composited partial clips must produce pixel-identical output to full render"
    );
}

/// Nested subtree: clipping skips the entire subtree, not just the direct child.
/// Node 1 is a container at (50,0) with a grandchild (node 2) with a yellow bg.
/// When clip is (0,0)-(40,100), neither node 1 nor node 2 should be visited.
#[test]
fn clip_skips_entire_subtree_not_just_direct_child() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 3];

    // Node 0: root
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Node 1: container at right half
    nodes[1].x = 50;
    nodes[1].y = 0;
    nodes[1].width = 50;
    nodes[1].height = 100;
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].first_child = 2;

    // Node 2: grandchild with yellow background, fills parent
    nodes[2].x = 0;
    nodes[2].y = 0;
    nodes[2].width = 50;
    nodes[2].height = 100;
    nodes[2].background = scene::Color::rgba(255, 255, 0, 255);
    nodes[2].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);

    // Clip to left half only — node 1 at x=50 is entirely outside
    let dirty = protocol::DirtyRect::new(0, 0, 40, 100);
    scene_render::render_scene_clipped(&mut fb, &graph, &ctx, &dirty);

    let stride = 100 * 4;

    // Centre of right half (75, 50) should remain black — subtree was skipped
    let (r, g, b, _a) = read_pixel(&buf, stride, 75, 50);
    assert_eq!(
        (r, g, b),
        (0, 0, 0),
        "right-half grandchild should not be drawn when subtree is outside clip"
    );

    // Left half should also be black (root has no background)
    let (r, g, b, _a) = read_pixel(&buf, stride, 20, 50);
    assert_eq!((r, g, b), (0, 0, 0), "left-half should be black (no child there)");
}

/// Zero-dimension child should be skipped by clip check (no intersection possible).
#[test]
fn zero_size_child_is_skipped() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Zero-size child at origin
    nodes[1].x = 0;
    nodes[1].y = 0;
    nodes[1].width = 0;
    nodes[1].height = 0;
    nodes[1].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[1].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // Pixel at (0,0) should be black — zero-size child produces no pixels
    let (r, g, b, _a) = read_pixel(&buf, 100 * 4, 0, 0);
    assert_eq!((r, g, b), (0, 0, 0), "zero-size child should not draw anything");
}

/// VAL-PIPE-012: When render_scene_clipped re-renders a dirty region where
/// a node moved away, the old position must show the background color, not
/// stale pixels from the previous frame.
///
/// Scenario: A colored child node is at (10,10) in frame 1. In frame 2 it
/// moves to (60,60). The dirty rect covering (10,10)-(40,40) should show
/// the root's background, not the child's old color.
#[test]
fn partial_render_clears_vacated_region() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let bg = scene::Color::rgba(30, 30, 30, 255);
    let red = scene::Color::rgba(255, 0, 0, 255);

    // Frame 1: root with bg, child at (10,10).
    let mut nodes = vec![Node::EMPTY; 2];
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].background = bg;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    nodes[1].x = 10;
    nodes[1].y = 10;
    nodes[1].width = 20;
    nodes[1].height = 20;
    nodes[1].background = red;
    nodes[1].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph1 = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);

    // Render frame 1 fully.
    scene_render::render_scene(&mut fb, &graph1, &ctx);

    let stride = 100 * 4;
    // Pixel (20, 20) should be red.
    let (r, g, b, _a) = read_pixel(&buf, stride, 20, 20);
    assert_eq!((r, g, b), (255, 0, 0), "frame 1: (20,20) should be red");

    // Frame 2: child moves to (60, 60).
    let mut nodes2 = nodes.clone();
    nodes2[1].x = 60;
    nodes2[1].y = 60;

    let graph2 = scene_render::SceneGraph {
        nodes: &nodes2,
        data: &data,
    };

    // Re-render the OLD position's dirty rect — this simulates what the
    // compositor does when it damages the old position of a moved node.
    let dirty_old = protocol::DirtyRect::new(10, 10, 20, 20);
    {
        let mut fb2 = Surface {
            data: &mut buf,
            width: 100,
            height: 100,
            stride,
            format: PixelFormat::Bgra8888,
        };
        scene_render::render_scene_clipped(&mut fb2, &graph2, &ctx, &dirty_old);
    }

    // The old position (20, 20) should now show the background color, NOT red.
    // Because the root's background is painted over the dirty rect first,
    // and the child is no longer at that position.
    let (r, g, b, _a) = read_pixel(&buf, stride, 20, 20);
    assert_eq!(
        (r, g, b),
        (30, 30, 30),
        "After child moved away, old position should show background, not stale red"
    );
}

/// Scale factor > 1: child bounding boxes should be scaled for clip check.
#[test]
fn scaled_clip_skips_correctly() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let mut ctx = test_ctx(&mono, &prop);
    ctx.scale = 2;

    // With scale=2, a 100×100 logical scene needs a 200×200 pixel surface
    let w = 200u32;
    let h = 200u32;
    let stride = w * 4;
    let mut data_buf = vec![0u8; (w * h * 4) as usize];
    for pixel in data_buf.chunks_exact_mut(4) {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 255; // opaque black
    }

    let (nodes, scene_data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &scene_data,
    };

    {
        let mut fb = Surface {
            data: &mut data_buf,
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgra8888,
        };

        // Clip to only the top-left quadrant in physical pixels (0,0)-(70,70)
        // At scale=2, child_a (logical 0,0 30×30) occupies physical (0,0)-(60,60)
        // child_b (logical 70,0 30×30) occupies physical (140,0)-(200,60) — outside clip
        let dirty = protocol::DirtyRect::new(0, 0, 70, 70);
        scene_render::render_scene_clipped(&mut fb, &graph, &ctx, &dirty);
    }

    // Red child should be drawn (physical center at ~30,30)
    let (r, g, b, _a) = read_pixel(&data_buf, stride, 30, 30);
    assert_eq!((r, g, b), (255, 0, 0), "top-left should be red at 2x");

    // Green child at physical (170,30) should be black (outside clip)
    let (r, g, b, _a) = read_pixel(&data_buf, stride, 170, 30);
    assert_eq!((r, g, b), (0, 0, 0), "top-right should stay black at 2x");
}
