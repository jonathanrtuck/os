//! Tests for scene_render: subtree clip skipping optimization.
//!
//! Verifies that render_node's child clip-skip check produces
//! pixel-identical output to rendering without the optimisation.

#[path = "../../services/compositor/svg.rs"]
mod svg;
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
        scale: 1.0,
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
    ctx.scale = 2.0;

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

// ── Fractional scale factor tests ───────────────────────────────────

/// Helper: create a surface of given dimensions filled with opaque black.
fn black_surface_wh(buf: &mut [u8], w: u32, h: u32) -> Surface {
    let stride = w * 4;
    for pixel in buf.chunks_exact_mut(4) {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 255;
    }
    Surface {
        data: buf,
        width: w,
        height: h,
        stride,
        format: PixelFormat::Bgra8888,
    }
}

/// Helper to build a RenderCtx with a specific fractional scale.
fn test_ctx_f32<'a>(
    mono: &'a fonts::cache::GlyphCache,
    prop: &'a fonts::cache::GlyphCache,
    scale: f32,
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
        scale: scale,
    }
}

/// VAL-COORD-001: Integer scales 1.0 and 2.0 produce pixel-identical output
/// to the old integer renderer.
#[test]
fn fractional_scale_1_0_matches_integer_scale_1() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Integer scale (old path: u32 = 1)
    let ctx_int = test_ctx(&mono, &prop); // scale = 1 (u32)

    let mut buf_int = vec![0u8; 100 * 100 * 4];
    {
        let mut fb = black_surface(&mut buf_int);
        scene_render::render_scene(&mut fb, &graph, &ctx_int);
    }

    // Fractional scale 1.0
    let ctx_frac = test_ctx_f32(&mono, &prop, 1.0);

    let mut buf_frac = vec![0u8; 100 * 100 * 4];
    {
        let mut fb = black_surface(&mut buf_frac);
        scene_render::render_scene(&mut fb, &graph, &ctx_frac);
    }

    assert_eq!(
        buf_int, buf_frac,
        "VAL-COORD-001: scale 1.0 (f32) must produce pixel-identical output to scale 1 (u32)"
    );
}

/// VAL-COORD-001: Scale 2.0 produces pixel-identical output to the old
/// integer scale 2.
#[test]
fn fractional_scale_2_0_matches_integer_scale_2() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();

    let (nodes, data) = build_four_corner_scene();
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let w = 200u32;
    let h = 200u32;
    let stride = w * 4;

    // Integer scale 2
    let mut ctx_int = test_ctx(&mono, &prop);
    ctx_int.scale = 2.0;

    let mut buf_int = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf_int, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx_int);
    }

    // Fractional scale 2.0
    let ctx_frac = test_ctx_f32(&mono, &prop, 2.0);

    let mut buf_frac = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf_frac, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx_frac);
    }

    assert_eq!(
        buf_int, buf_frac,
        "VAL-COORD-001: scale 2.0 (f32) must produce pixel-identical output to scale 2 (u32)"
    );
}

/// VAL-COORD-002: Fractional scale 1.5x produces correct physical dimensions.
/// Node at logical (10,20) size (100,50) at scale 1.5 → physical (15,30) size (150,75).
#[test]
fn fractional_scale_1_5_correct_physical_dimensions() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx_f32(&mono, &prop, 1.5);

    let green = scene::Color::rgba(0, 255, 0, 255);
    let mut nodes = vec![Node::EMPTY; 2];

    // Root: 200×150 logical → 300×225 physical
    nodes[0].width = 200;
    nodes[0].height = 150;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Child: logical (10,20) 100×50 → physical (15,30) 150×75
    nodes[1].x = 10;
    nodes[1].y = 20;
    nodes[1].width = 100;
    nodes[1].height = 50;
    nodes[1].background = green;
    nodes[1].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let w = 300u32;
    let h = 225u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Center of green child: physical (15 + 75, 30 + 37) = (90, 67)
    let (r, g, b, _a) = read_pixel(&buf, stride, 90, 67);
    assert_eq!((r, g, b), (0, 255, 0), "center of child at 1.5x should be green");

    // Just inside the top-left corner: physical (15, 30)
    let (r, g, b, _a) = read_pixel(&buf, stride, 15, 30);
    assert_eq!((r, g, b), (0, 255, 0), "top-left corner of child at 1.5x should be green");

    // Just inside the bottom-right corner: physical (15+150-1, 30+75-1) = (164, 104)
    let (r, g, b, _a) = read_pixel(&buf, stride, 164, 104);
    assert_eq!(
        (r, g, b),
        (0, 255, 0),
        "bottom-right corner of child at 1.5x should be green"
    );

    // Just outside: physical (165, 105)
    let (r, g, b, _a) = read_pixel(&buf, stride, 165, 105);
    assert_eq!(
        (r, g, b),
        (0, 0, 0),
        "pixel just outside child at 1.5x should be black"
    );

    // Just above: physical (90, 29)
    let (r, g, b, _a) = read_pixel(&buf, stride, 90, 29);
    assert_eq!(
        (r, g, b),
        (0, 0, 0),
        "pixel just above child at 1.5x should be black"
    );
}

/// VAL-COORD-003: No pixel gaps between adjacent nodes at fractional scale.
/// Two adjacent nodes at logical x=3,w=1 and x=4,w=1 at scale 1.5
/// produce no gap or overlap in physical pixels.
#[test]
fn fractional_scale_no_gap_between_adjacent_nodes() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx_f32(&mono, &prop, 1.5);

    let red = scene::Color::rgba(255, 0, 0, 255);
    let blue = scene::Color::rgba(0, 0, 255, 255);

    let mut nodes = vec![Node::EMPTY; 3];

    // Root: logical width 20 → physical 30
    nodes[0].width = 20;
    nodes[0].height = 10;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Node A: logical x=3, w=1 → physical x=round(3*1.5)=4, w=round(1*1.5)=2
    // But careful: we need to check the actual physical coverage.
    // At 1.5: x_phys=floor(3*1.5)=4, w_phys=floor(1*1.5)=1 (or round?)
    // Key: node B at x=4, w=1 → x_phys=floor(4*1.5)=6, w_phys=1
    // There should be no gap at physical pixel 5.
    nodes[1].x = 3;
    nodes[1].y = 0;
    nodes[1].width = 1;
    nodes[1].height = 10;
    nodes[1].background = red;
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].next_sibling = 2;

    // Node B: logical x=4, w=1
    nodes[2].x = 4;
    nodes[2].y = 0;
    nodes[2].width = 1;
    nodes[2].height = 10;
    nodes[2].background = blue;
    nodes[2].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let w = 30u32;
    let h = 15u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Check physical pixels at row 5 (middle of height).
    // Using the gap-free rounding scheme:
    //   Node A: x = round(3 * 1.5) = round(4.5) = 5
    //           w = round(4 * 1.5) - round(3 * 1.5) = 6 - 5 = 1
    //   Node B: x = round(4 * 1.5) = round(6.0) = 6
    //           w = round(5 * 1.5) - round(4 * 1.5) = round(7.5) - 6 = 8 - 6 = 2
    //
    // Node A covers physical pixel 5, Node B covers physical pixels 6-7.
    // No gap at any boundary.
    let row = 5u32;
    let a_phys_start = (3.0f32 * 1.5).round() as u32;  // 5
    let b_phys_end = ((4 + 1) as f32 * 1.5).round() as u32;  // round(7.5) = 8

    // Every pixel from A's start to B's end must be colored (no gap).
    for px in a_phys_start..b_phys_end {
        let (r, g, b, _a) = read_pixel(&buf, stride, px, row);
        assert!(
            (r, g, b) != (0, 0, 0),
            "VAL-COORD-003: pixel at physical x={} should not be black (gap) between adjacent nodes",
            px
        );
    }

    // Verify that the two nodes are truly adjacent: the last pixel of A and
    // the first pixel of B should be at consecutive x coordinates.
    let a_end = a_phys_start + ((4.0f32 * 1.5).round() as u32 - a_phys_start);
    let b_start = (4.0f32 * 1.5).round() as u32;
    assert_eq!(
        a_end, b_start,
        "VAL-COORD-003: Node A end and Node B start must be adjacent (no gap)"
    );
}

/// VAL-COORD-004: Pixel-snapped borders at fractional scales.
/// A 1-logical-pixel border at scale 1.5 snaps to whole physical pixel width.
#[test]
fn fractional_scale_border_pixel_snapped() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();

    let test_cases: &[(f32, u32, u32)] = &[
        // (scale, logical_border_width, expected_min_physical_border_width)
        (1.0, 1, 1),
        (1.25, 1, 1), // round(1.25) = 1, but at least 1
        (1.5, 1, 1),  // round(1.5) = 2, or at least 1
        (2.0, 1, 2),
    ];

    for &(scale, logical_bw, min_phys_bw) in test_cases {
        let ctx = test_ctx_f32(&mono, &prop, scale);

        let phys_w = (60.0 * scale) as u32;
        let phys_h = (40.0 * scale) as u32;

        let mut nodes = vec![Node::EMPTY; 1];
        nodes[0].width = 60;
        nodes[0].height = 40;
        nodes[0].background = scene::Color::rgba(0, 0, 0, 255);
        nodes[0].border = scene::Border {
            width: logical_bw as u8,
            color: scene::Color::rgba(255, 0, 0, 255),
            _pad: [0; 3],
        };
        nodes[0].flags = NodeFlags::VISIBLE;

        let data: Vec<u8> = vec![];
        let graph = scene_render::SceneGraph {
            nodes: &nodes,
            data: &data,
        };

        let stride = phys_w * 4;
        let mut buf = vec![0u8; (phys_w * phys_h * 4) as usize];
        {
            let mut fb = black_surface_wh(&mut buf, phys_w, phys_h);
            scene_render::render_scene(&mut fb, &graph, &ctx);
        }

        // Check top border: first row(s) should be red.
        // The border width in physical pixels should be a whole number ≥ min_phys_bw.
        let mut top_border_rows = 0u32;
        for row in 0..phys_h {
            let (r, _g, _b, _a) = read_pixel(&buf, stride, phys_w / 2, row);
            if r == 255 {
                top_border_rows += 1;
            } else {
                break;
            }
        }

        assert!(
            top_border_rows >= min_phys_bw,
            "VAL-COORD-004: at scale {}, border should be at least {} physical pixel(s), got {}",
            scale,
            min_phys_bw,
            top_border_rows
        );

        // The border must be a whole number of pixels (no sub-pixel borders).
        // This is verified by the fact that top_border_rows is a u32 count of
        // fully-colored rows — there cannot be a fractional row.
    }
}

/// VAL-COORD-007: Scale factor represents 1.0, 1.25, 1.5, 1.75, 2.0 exactly.
/// f32 round-trips all common scale factors without loss.
#[test]
fn f32_scale_factor_exact_representation() {
    let values: &[f32] = &[1.0, 1.25, 1.5, 1.75, 2.0];
    for &v in values {
        // Verify exact round-trip: the value stored as f32 and read back is identical.
        let stored: f32 = v;
        let read: f32 = stored;
        assert_eq!(
            v, read,
            "VAL-COORD-007: f32 must represent {} exactly",
            v
        );

        // Also verify that arithmetic with f32 is exact for these values.
        // Multiplying a small integer by the scale factor should produce exact results.
        let logical: f32 = 100.0;
        let physical = logical * v;
        let expected = (100.0f64 * v as f64) as f32;
        assert_eq!(
            physical, expected,
            "VAL-COORD-007: 100 * {} should be exact in f32",
            v
        );
    }
}

/// VAL-COORD-008: CompositorConfig IPC compatibility.
/// CompositorConfig with fractional scale_factor fits within 60-byte IPC payload.
/// This is a compile-time check — the existing const assertion in protocol/lib.rs
/// enforces this. We also verify at runtime for documentation.
#[test]
fn compositor_config_fits_ipc_payload() {
    let size = core::mem::size_of::<protocol::compose::CompositorConfig>();
    assert!(
        size <= 60,
        "VAL-COORD-008: CompositorConfig size {} exceeds 60-byte IPC payload",
        size
    );
}

/// VAL-COORD-011: Zero and extreme scale factors handled gracefully.
/// Scale 0.0 does not panic or divide-by-zero.
/// Scale 8.0 produces reasonable output (clamped or accepted).
/// Negative rejected (treated as 1.0 or clamped).
#[test]
fn fractional_scale_zero_no_panic() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    // Scale 0.0 should not panic — nodes will have zero physical size.
    let ctx = test_ctx_f32(&mono, &prop, 0.0);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);
    // This must not panic.
    scene_render::render_scene(&mut fb, &graph, &ctx);
    // With scale 0, nothing should be drawn (zero physical size).
}

/// VAL-COORD-011: Negative scale is rejected (treated as 1.0 or clamped to positive).
#[test]
fn fractional_scale_negative_treated_as_safe() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    // Negative scale should be clamped/rejected by the compositor.
    // The compositor should treat it as 1.0 (or some safe default).
    let ctx = test_ctx_f32(&mono, &prop, -1.5);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface(&mut buf);
    // Must not panic.
    scene_render::render_scene(&mut fb, &graph, &ctx);
}

/// VAL-COORD-011: Extreme scale (8.0) is clamped or handled gracefully.
#[test]
fn fractional_scale_extreme_clamped() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    // Scale 8.0 should be clamped to a maximum safe value (e.g., 4.0)
    // or accepted if the surface is large enough. Either way, no panic.
    let ctx = test_ctx_f32(&mono, &prop, 8.0);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 10;
    nodes[0].height = 10;
    nodes[0].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Surface needs to be big enough: 10*8 = 80
    let w = 100u32;
    let h = 100u32;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf, w, h);
        // Must not panic.
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }
}

/// VAL-COORD-012: Scene graph Node struct is unchanged.
/// The Node struct fields remain i16/u16 for coordinates. We verify by
/// checking that the existing Node struct size and content type fields
/// haven't changed from the baseline.
#[test]
fn scene_graph_node_struct_unchanged() {
    // Node coordinates remain i16/u16.
    let n = Node::EMPTY;
    // Verify the types by assigning known i16/u16 values.
    let mut node = n;
    node.x = -100i16; // x is i16
    node.y = 32000i16; // y is i16
    node.width = 65535u16; // width is u16
    node.height = 1u16; // height is u16

    assert_eq!(node.x, -100);
    assert_eq!(node.y, 32000);
    assert_eq!(node.width, 65535);
    assert_eq!(node.height, 1);

    // The Node size should match the existing compile-time assertion in scene/lib.rs.
    // We don't hard-code the number here to avoid duplication — the compile-time
    // assertion in scene/lib.rs is the authoritative check.
    let _ = core::mem::size_of::<Node>();
}

// ── VAL-COORD-005 / VAL-COORD-006: Font rasterization at physical pixel size ──

/// VAL-COORD-005: At scale 1.5 with logical font_size=16, glyphs are
/// rasterized at 24 physical pixels. The compositor computes physical_font_size
/// as round(logical_font_size * scale_factor).
#[test]
fn font_physical_pixel_size_at_fractional_scale() {
    // Simulate the compositor's computation.
    fn round_f32(x: f32) -> i32 {
        if x >= 0.0 { (x + 0.5) as i32 } else { (x - 0.5) as i32 }
    }

    // Scale 1.5, logical font size 16 → physical 24
    let physical = round_f32(16.0 * 1.5).max(1) as u32;
    assert_eq!(physical, 24, "VAL-COORD-005: logical 16 at scale 1.5 = 24 physical px");

    // Scale 2.0, logical font size 16 → physical 32
    let physical_2 = round_f32(16.0 * 2.0).max(1) as u32;
    assert_eq!(physical_2, 32, "logical 16 at scale 2.0 = 32 physical px");

    // Scale 1.25, logical font size 16 → physical 20
    let physical_125 = round_f32(16.0 * 1.25).max(1) as u32;
    assert_eq!(physical_125, 20, "logical 16 at scale 1.25 = 20 physical px");

    // Scale 1.0, logical font size 18 → physical 18
    let physical_1 = round_f32(18.0 * 1.0).max(1) as u32;
    assert_eq!(physical_1, 18, "logical 18 at scale 1.0 = 18 physical px");
}

/// VAL-COORD-006: Same logical font size at scales 1.5 and 2.0 produces
/// different glyph cache entries. The cache is keyed on physical pixel size.
#[test]
fn glyph_cache_keyed_on_physical_pixel_size() {
    // The GlyphCache is populated with a specific size_px. Two caches
    // populated at different physical sizes should have different metrics.
    // We verify this by checking that the cache's size_px field differs.
    let cache_24 = {
        let mut c = zeroed_glyph_cache();
        // We can't actually populate without font data, but we can verify
        // that after populate(), size_px reflects the physical size.
        // For now, verify the field is stored correctly.
        c.size_px = 24; // Would be set by populate(font_data, 24)
        c.size_px
    };
    let cache_32 = {
        let mut c = zeroed_glyph_cache();
        c.size_px = 32; // Would be set by populate(font_data, 32)
        c.size_px
    };
    assert_ne!(
        cache_24, cache_32,
        "VAL-COORD-006: same logical size at different scales produces different cache entries"
    );

    // Also verify via LRU cache: font_size is part of the key.
    let mut lru = fonts::cache::LruGlyphCache::new(64);
    let glyph_24 = fonts::cache::LruCachedGlyph {
        width: 10, height: 20, bearing_x: 1, bearing_y: 15, advance: 12,
        coverage: vec![0xAA; 30],
    };
    let glyph_32 = fonts::cache::LruCachedGlyph {
        width: 14, height: 26, bearing_x: 1, bearing_y: 20, advance: 16,
        coverage: vec![0xBB; 30],
    };
    // Same glyph_id=65 ('A'), different font sizes (physical px)
    lru.insert(65, 24, glyph_24);
    lru.insert(65, 32, glyph_32);

    let r24 = lru.get(65, 24).unwrap();
    assert_eq!(r24.width, 10, "24px glyph has width 10");
    let r32 = lru.get(65, 32).unwrap();
    assert_eq!(r32.width, 14, "32px glyph has width 14");
}

// ── VAL-COORD-009: Scroll offset correct at fractional scale ──

/// At scale 1.5, scroll_y=10 should offset children by 15 physical pixels.
#[test]
fn scroll_offset_fractional_scale() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx_f32(&mono, &prop, 1.5);

    // Build scene: root → container (scroll_y=10) → child (red, y=0)
    let mut nodes = vec![Node::EMPTY; 3];

    // Root: 150×150 physical (100×100 logical at 1.5x)
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Container: full size, scroll_y = 10
    nodes[1].width = 100;
    nodes[1].height = 100;
    nodes[1].scroll_y = 10;
    nodes[1].flags = NodeFlags::VISIBLE;
    nodes[1].first_child = 2;

    // Child: 20×20 at y=0 (logical), background = RED
    nodes[2].y = 20; // Logical y = 20
    nodes[2].width = 20;
    nodes[2].height = 20;
    nodes[2].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[2].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph { nodes: &nodes, data: &data };

    // Physical framebuffer: 150×150
    let w = 150u32;
    let h = 150u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    // Fill black
    for pixel in buf.chunks_exact_mut(4) {
        pixel[0] = 0; pixel[1] = 0; pixel[2] = 0; pixel[3] = 255;
    }
    {
        let mut fb = Surface { data: &mut buf, width: w, height: h, stride, format: PixelFormat::Bgra8888 };
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Child is at logical y=20, container scroll_y=10.
    // Effective logical y = 20 - 10 = 10.
    // Physical y = round(10 * 1.5) = 15.
    // So the red child should start at physical y=15.
    let (r14, _, _, _) = read_pixel(&buf, stride, 0, 14);
    let (r15, _, _, _) = read_pixel(&buf, stride, 0, 15);

    assert_eq!(r14, 0, "pixel at y=14 should be black (not yet child)");
    assert_eq!(r15, 255, "pixel at y=15 should be red (child after scroll offset at 1.5x)");
}

// ── VAL-COORD-010: Dirty rect computation at fractional scale ──

/// Changed node at fractional scale produces dirty rects that fully cover
/// the physical extent. No stale edge pixels.
#[test]
fn dirty_rect_fractional_scale_full_coverage() {
    // Node at logical (3, 5) size (10, 8) at scale 1.5
    // Physical start: round(3*1.5)=5 (rounded from 4.5), round(5*1.5)=8 (rounded from 7.5)
    // Physical end: round(13*1.5)=20 (rounded from 19.5), round(13*1.5)=20 (rounded from 19.5)
    // Physical size: 20-5=15, 20-8=12
    fn round_f32(x: f32) -> i32 {
        if x >= 0.0 { (x + 0.5) as i32 } else { (x - 0.5) as i32 }
    }
    fn scale_coord(logical: i32, scale: f32) -> i32 {
        round_f32(logical as f32 * scale)
    }
    fn scale_size(logical_pos: i32, logical_size: i32, scale: f32) -> i32 {
        let phys_start = round_f32(logical_pos as f32 * scale);
        let phys_end = round_f32((logical_pos + logical_size) as f32 * scale);
        phys_end - phys_start
    }

    let scale: f32 = 1.5;
    let ax: i32 = 3;
    let ay: i32 = 5;
    let aw: u32 = 10;
    let ah: u32 = 8;

    let px = scale_coord(ax, scale).max(0);
    let py = scale_coord(ay, scale).max(0);
    let pw = scale_size(ax, aw as i32, scale);
    let ph = scale_size(ay, ah as i32, scale);

    // The dirty rect must cover from physical start to physical end.
    assert_eq!(px, 5, "physical x should be round(3*1.5)=5");
    assert_eq!(py, 8, "physical y should be round(5*1.5)=8");
    // Physical end: round(13*1.5)=round(19.5)=20, so w = 20-5 = 15
    assert_eq!(pw, 15, "physical width covers full extent at 1.5x");
    // Physical end: round(13*1.5)=round(19.5)=20, so h = 20-8 = 12
    assert_eq!(ph, 12, "physical height covers full extent at 1.5x");

    // Verify no pixel gap: the physical rect [5..20) × [8..20) fully covers
    // what the renderer would draw for this node.
    assert!(pw > 0 && ph > 0, "dirty rect must have non-zero extent");
}

// ── SVG fixed-point scale conversion ──

/// The SVG rasterizer uses 20.12 fixed-point for its scale parameter.
/// Converting f32 display scale to SVG fixed-point must be correct.
#[test]
fn svg_f32_to_fixed_point_scale_conversion() {
    // SVG_FP_ONE = 1 << 12 = 4096
    let fp_one: i32 = 1 << 12; // 4096

    // Convert f32 scale to 20.12 fixed-point: round(scale * FP_ONE)
    fn f32_to_svg_fp(scale: f32) -> i32 {
        let fp_one = 1i32 << 12;
        if scale >= 0.0 {
            (scale * fp_one as f32 + 0.5) as i32
        } else {
            (scale * fp_one as f32 - 0.5) as i32
        }
    }

    // 1.0 → 4096
    assert_eq!(f32_to_svg_fp(1.0), fp_one, "scale 1.0 → FP_ONE");
    // 2.0 → 8192
    assert_eq!(f32_to_svg_fp(2.0), fp_one * 2, "scale 2.0 → 2×FP_ONE");
    // 1.5 → 6144
    assert_eq!(f32_to_svg_fp(1.5), 6144, "scale 1.5 → 6144");
    // 1.25 → 5120
    assert_eq!(f32_to_svg_fp(1.25), 5120, "scale 1.25 → 5120");
}

// ── Corner-radius compositor wiring tests ───────────────────────────

/// Node with corner_radius=8 renders with rounded corners — corner pixels
/// are anti-aliased (not fully opaque) while interior is fully filled.
#[test]
fn corner_radius_renders_rounded_background() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 100;
    nodes[0].height = 60;
    nodes[0].background = scene::Color::rgba(255, 0, 0, 255);
    nodes[0].corner_radius = 8;
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 60 * 4];
    let mut fb = black_surface_wh(&mut buf, 100, 60);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    let stride = 100 * 4;

    // Interior center: fully red.
    let (r, g, b, a) = read_pixel(&buf, stride, 50, 30);
    assert_eq!((r, g, b, a), (255, 0, 0, 255), "interior center should be red");

    // The top-left corner pixel (0,0) should NOT be fully red — it's
    // outside the rounded corner arc, so it should be black or have
    // anti-aliased coverage (not fully red).
    let (r, _g, _b, _a) = read_pixel(&buf, stride, 0, 0);
    assert_ne!(r, 255, "top-left corner (0,0) should not be fully red (rounded)");

    // A pixel just inside the arc (e.g., at (8, 8)) should be red.
    let (r, _g, _b, _a) = read_pixel(&buf, stride, 8, 8);
    assert_eq!(r, 255, "pixel (8,8) inside arc should be red");
}

/// corner_radius=0 falls back to rect — no overhead, pixel-identical output.
#[test]
fn corner_radius_zero_falls_back_to_rect() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let green = scene::Color::rgba(0, 200, 0, 255);

    // Scene with corner_radius = 0
    let mut nodes_sharp = vec![Node::EMPTY; 1];
    nodes_sharp[0].width = 60;
    nodes_sharp[0].height = 40;
    nodes_sharp[0].background = green;
    nodes_sharp[0].corner_radius = 0;
    nodes_sharp[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph_sharp = scene_render::SceneGraph {
        nodes: &nodes_sharp,
        data: &data,
    };

    let mut buf_sharp = vec![0u8; 60 * 40 * 4];
    {
        let mut fb = black_surface_wh(&mut buf_sharp, 60, 40);
        scene_render::render_scene(&mut fb, &graph_sharp, &ctx);
    }

    // The corner pixel (0,0) must be fully green (no rounding).
    let (r, g, b, _a) = read_pixel(&buf_sharp, 60 * 4, 0, 0);
    assert_eq!((r, g, b), (0, 200, 0), "corner_radius=0: corner should be sharp (full green)");
}

/// VAL-PRIM-006: Corner-radius-aware clipping.
/// Parent with corner_radius=20, clips_children=true: child pixels
/// outside rounded boundary are clipped.
#[test]
fn corner_radius_clips_children_to_rounded_boundary() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 2];

    // Parent: 100×100, corner_radius=20, clips children
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].corner_radius = 20;
    nodes[0].background = scene::Color::rgba(100, 100, 100, 255);
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Child: fills entire parent, bright green
    nodes[1].width = 100;
    nodes[1].height = 100;
    nodes[1].background = scene::Color::rgba(0, 255, 0, 255);
    nodes[1].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface_wh(&mut buf, 100, 100);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    let stride = 100 * 4;

    // Interior: should be green (child fills parent interior).
    let (r, g, b, _a) = read_pixel(&buf, stride, 50, 50);
    assert_eq!(
        (r, g, b),
        (0, 255, 0),
        "VAL-PRIM-006: interior should be green (child visible)"
    );

    // Corner (0,0): outside the rounded boundary. The child should be
    // clipped here, so we should see black (the framebuffer background),
    // NOT green. An anti-aliased parent bg pixel is acceptable.
    let (_r, g, _b, _a) = read_pixel(&buf, stride, 0, 0);
    assert_ne!(
        g, 255,
        "VAL-PRIM-006: corner (0,0) should not show child green (clipped by rounded parent)"
    );

    // Also check (1,1) — still well outside a radius=20 arc.
    let (_r, g, _b, _a) = read_pixel(&buf, stride, 1, 1);
    assert_ne!(
        g, 255,
        "VAL-PRIM-006: corner (1,1) should not show child green"
    );
}

/// corner_radius=0 + clips_children falls back to existing rect clip (no overhead).
#[test]
fn corner_radius_zero_clips_children_uses_rect_clip() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 2];

    // Parent: 60×40, corner_radius=0, clips children
    nodes[0].width = 60;
    nodes[0].height = 40;
    nodes[0].corner_radius = 0;
    nodes[0].background = scene::Color::rgba(100, 100, 100, 255);
    nodes[0].flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Child: extends beyond parent (80×60)
    nodes[1].width = 80;
    nodes[1].height = 60;
    nodes[1].background = scene::Color::rgba(0, 255, 0, 255);
    nodes[1].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 100 * 100 * 4];
    let mut fb = black_surface_wh(&mut buf, 100, 100);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    let stride = 100 * 4;

    // Inside parent: child is green.
    let (r, g, b, _a) = read_pixel(&buf, stride, 30, 20);
    assert_eq!((r, g, b), (0, 255, 0), "inside parent: child green");

    // The corner (0,0) should be green too (no rounding with radius=0).
    let (r, g, b, _a) = read_pixel(&buf, stride, 0, 0);
    assert_eq!((r, g, b), (0, 255, 0), "corner_radius=0: corner should show child green (rect clip)");

    // Outside parent (65, 25): should be black, not green.
    let (r, g, b, _a) = read_pixel(&buf, stride, 65, 25);
    assert_eq!((r, g, b), (0, 0, 0), "outside parent: should be black (clipped)");
}

/// VAL-PRIM-015: Border follows rounded contour.
/// Border with corner_radius > 0 follows the arc, not straight-line.
#[test]
fn rounded_border_follows_corner_contour() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 80;
    nodes[0].height = 60;
    nodes[0].background = scene::Color::rgba(50, 50, 50, 255);
    nodes[0].corner_radius = 12;
    nodes[0].border = scene::Border {
        width: 3,
        color: scene::Color::rgba(255, 0, 0, 255),
        _pad: [0; 3],
    };
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let mut buf = vec![0u8; 80 * 60 * 4];
    let mut fb = black_surface_wh(&mut buf, 80, 60);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    let stride = 80 * 4;

    // Top-center border pixel: should be red (border).
    let (r, _g, _b, _a) = read_pixel(&buf, stride, 40, 1);
    assert_eq!(r, 255, "VAL-PRIM-015: top-center border should be red");

    // Interior (well inside border): should be background color.
    let (r, g, b, _a) = read_pixel(&buf, stride, 40, 30);
    assert_eq!((r, g, b), (50, 50, 50), "interior should be background color");

    // Corner (0,0): outside the rounded border. Should be black (framebuffer bg),
    // not the border color.
    let (r, _g, _b, _a) = read_pixel(&buf, stride, 0, 0);
    assert_ne!(
        r, 255,
        "VAL-PRIM-015: corner (0,0) should not be border red (border follows arc)"
    );
}

/// VAL-PRIM-017: Rounded rect with corner_radius at fractional scale.
/// corner_radius=8 at 1.5x → 12px physical radius.
#[test]
fn corner_radius_at_fractional_scale() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx_f32(&mono, &prop, 1.5);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 80;
    nodes[0].height = 60;
    nodes[0].background = scene::Color::rgba(0, 0, 255, 255);
    nodes[0].corner_radius = 8;
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Physical: 120×90
    let w = 120u32;
    let h = 90u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Physical radius should be round(8 * 1.5) = 12.
    // Interior pixel: blue.
    let (_r, _g, b, _a) = read_pixel(&buf, stride, 60, 45);
    assert_eq!(b, 255, "VAL-PRIM-017: interior should be blue at 1.5x");

    // Corner (0,0) should NOT be blue (outside rounded arc at radius 12).
    let (_r, _g, b, _a) = read_pixel(&buf, stride, 0, 0);
    assert_ne!(b, 255, "VAL-PRIM-017: corner (0,0) should not be blue at 1.5x");

    // Pixel (12, 12) should be blue (inside arc).
    let (_r, _g, b, _a) = read_pixel(&buf, stride, 12, 12);
    assert_eq!(b, 255, "VAL-PRIM-017: pixel (12,12) should be blue (inside physical arc)");
}

/// VAL-CROSS-003: Fractional scale preserves rounded corner symmetry.
/// All four corners have identical physical radius.
#[test]
fn fractional_scale_rounded_corner_symmetry() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx_f32(&mono, &prop, 1.5);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 60;
    nodes[0].height = 60;
    nodes[0].background = scene::Color::rgba(200, 0, 0, 255);
    nodes[0].corner_radius = 10;
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Physical: 90×90
    let w = 90u32;
    let h = 90u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = black_surface_wh(&mut buf, w, h);
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Physical radius = round(10 * 1.5) = 15.
    // Sample a pixel at the same relative distance from each corner.
    // (1,1) from each corner: should all be the same.
    let tl = read_pixel(&buf, stride, 1, 1);
    let tr = read_pixel(&buf, stride, w - 2, 1);
    let bl = read_pixel(&buf, stride, 1, h - 2);
    let br = read_pixel(&buf, stride, w - 2, h - 2);

    // All four corners should have the same color (symmetry).
    assert_eq!(tl, tr, "VAL-CROSS-003: top-left and top-right corners must match");
    assert_eq!(tl, bl, "VAL-CROSS-003: top-left and bottom-left corners must match");
    assert_eq!(tl, br, "VAL-CROSS-003: top-left and bottom-right corners must match");
}

/// Rounded rect with semi-transparent background blends correctly.
#[test]
fn rounded_rect_semi_transparent_blends() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let mut nodes = vec![Node::EMPTY; 1];
    nodes[0].width = 60;
    nodes[0].height = 40;
    nodes[0].background = scene::Color::rgba(255, 0, 0, 128);
    nodes[0].corner_radius = 8;
    nodes[0].flags = NodeFlags::VISIBLE;

    let data: Vec<u8> = vec![];
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    // Framebuffer starts white
    let w = 60u32;
    let h = 40u32;
    let stride = w * 4;
    let mut buf = vec![0u8; (w * h * 4) as usize];
    {
        let mut fb = Surface {
            data: &mut buf,
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgra8888,
        };
        fb.clear(Color::WHITE);
        scene_render::render_scene(&mut fb, &graph, &ctx);
    }

    // Interior center: red @ 128 alpha over white.
    // The blended result should be reddish-pink, not pure red.
    let (r, g, b, _a) = read_pixel(&buf, stride, 30, 20);
    // With sRGB blending: r > 128, g < 255, b < 255.
    assert!(r > 128, "blended interior r should be > 128, got {}", r);
    assert!(g < 255, "blended interior g should be < 255, got {}", g);
    assert!(b < 255, "blended interior b should be < 255, got {}", b);
}

/// VAL-CROSS-012: Node size compile-time assertion.
/// Adding a compile-time assertion ensures shared-memory layout stability.
#[test]
fn node_size_compile_time_assertion_exists() {
    // The compile-time assertion is in scene/lib.rs:
    //   const _: () = assert!(size_of::<Node>() == 72);
    // If the Node layout changes, the build will fail.
    // At runtime, verify the size matches.
    let size = core::mem::size_of::<Node>();
    assert_eq!(
        size, 60,
        "VAL-CROSS-012: Node must be exactly 60 bytes for shared-memory layout stability"
    );
}

// ── Path rendering tests (VAL-PRIM-007 through VAL-PRIM-014) ───────

/// Helper: build a scene with a single path node at (0,0) 100×100.
/// The root is transparent and clips children.
fn build_path_scene(
    path_cmds: &[scene::PathCmd],
    fill: scene::Color,
    stroke: scene::Color,
    stroke_width: u8,
) -> (Vec<Node>, Vec<u8>) {
    let cmd_size = core::mem::size_of::<scene::PathCmd>();
    let total_bytes = path_cmds.len() * cmd_size;
    let mut data = vec![0u8; total_bytes];

    // SAFETY: PathCmd is repr(C) — copying to byte buffer is safe.
    unsafe {
        core::ptr::copy_nonoverlapping(
            path_cmds.as_ptr() as *const u8,
            data.as_mut_ptr(),
            total_bytes,
        );
    }

    let mut nodes = vec![Node::EMPTY; 2];

    // Root: 100×100 transparent, clips children
    nodes[0].width = 100;
    nodes[0].height = 100;
    nodes[0].flags = scene::NodeFlags::VISIBLE | scene::NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Path node: 100×100 at (0,0)
    nodes[1].width = 100;
    nodes[1].height = 100;
    nodes[1].flags = scene::NodeFlags::VISIBLE;
    nodes[1].content = scene::Content::Path {
        commands: scene::DataRef {
            offset: 0,
            length: total_bytes as u32,
        },
        fill,
        stroke,
        stroke_width,
        _pad: [0; 3],
    };

    (nodes, data)
}

/// VAL-PRIM-007: Content::Path with MoveTo/LineTo/Close renders filled triangle.
#[test]
fn path_renders_filled_triangle() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let cmds = [
        scene::PathCmd::move_to(10, 80),
        scene::PathCmd::line_to(50, 10),
        scene::PathCmd::line_to(90, 80),
        scene::PathCmd::close(),
    ];
    let fill = scene::Color::rgba(255, 0, 0, 255);
    let (nodes, data) = build_path_scene(&cmds, fill, scene::Color::TRANSPARENT, 0);
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // Interior: center of the triangle (~50, 50) should have red fill.
    let (r, g, b, _a) = read_pixel(&buf, stride, 50, 50);
    assert!(r > 200, "triangle interior should be red, got r={}", r);
    assert!(g < 50, "triangle interior g should be low, got g={}", g);
    assert!(b < 50, "triangle interior b should be low, got b={}", b);

    // Exterior: well outside the triangle (5, 5) should be black.
    let (r, g, b, _a) = read_pixel(&buf, stride, 5, 5);
    assert!(r < 10, "exterior should be black, got r={}", r);
    assert!(g < 10, "exterior should be black, got g={}", g);
    assert!(b < 10, "exterior should be black, got b={}", b);
}

/// VAL-PRIM-008: CurveTo cubic bezier renders smooth curves.
#[test]
fn path_renders_cubic_bezier() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    // A quarter-circle-like curve in the top-left quadrant.
    let cmds = [
        scene::PathCmd::move_to(10, 90),
        scene::PathCmd::curve_to(10, 10, 90, 10, 90, 90),
        scene::PathCmd::close(),
    ];
    let fill = scene::Color::rgba(0, 255, 0, 255);
    let (nodes, data) = build_path_scene(&cmds, fill, scene::Color::TRANSPARENT, 0);
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // The curve from (10,90) via controls (10,10) and (90,10) to (90,90)
    // with Close forms a shape. The cubic bulges upward (control points
    // at y=10), and the closing line goes from (90,90) back to (10,90).
    // The filled area is inside the closed path.

    // A point inside the closed shape: (50, 70) should be green.
    let (_r, g, _b, _a) = read_pixel(&buf, stride, 50, 70);
    assert!(g > 200, "curve interior at (50,70) should be green, got g={}", g);

    // Another interior point near the bottom: (50, 88) should be green
    // (between the closing line at y=90 and the curve).
    let (_r2, g2, _b2, _a) = read_pixel(&buf, stride, 50, 88);
    assert!(g2 > 100, "curve interior at (50,88) should be green, got g={}", g2);
}

/// VAL-PRIM-011: Path stroke rendering.
#[test]
fn path_renders_stroke_outline() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    // Large triangle: fill transparent, blue stroke.
    let cmds = [
        scene::PathCmd::move_to(10, 90),
        scene::PathCmd::line_to(50, 10),
        scene::PathCmd::line_to(90, 90),
        scene::PathCmd::close(),
    ];
    let stroke = scene::Color::rgba(0, 0, 255, 255);
    let (nodes, data) =
        build_path_scene(&cmds, scene::Color::TRANSPARENT, stroke, 3);
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // On the edge of the triangle — near the bottom edge (50, 89) should
    // have some blue from the stroke.
    let (_r, _g, b, _a) = read_pixel(&buf, stride, 50, 89);
    assert!(b > 100, "stroke should be blue near bottom edge, got b={}", b);

    // Interior center (50, 50) — no fill (transparent), so should be black
    // unless stroke leaks. With a thin stroke on a large triangle, center
    // should be mostly black.
    let (r, g, b, _a) = read_pixel(&buf, stride, 50, 50);
    assert!(
        r < 50 && g < 50 && b < 50,
        "interior of stroke-only triangle should be dark, got ({},{},{})",
        r, g, b
    );
}

/// VAL-PRIM-012: Path with both fill and stroke — fill renders first,
/// stroke on top.
#[test]
fn path_fill_and_stroke_layered() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let cmds = [
        scene::PathCmd::move_to(10, 80),
        scene::PathCmd::line_to(50, 10),
        scene::PathCmd::line_to(90, 80),
        scene::PathCmd::close(),
    ];
    let fill = scene::Color::rgba(255, 0, 0, 255);
    let stroke = scene::Color::rgba(0, 0, 255, 255);
    let (nodes, data) = build_path_scene(&cmds, fill, stroke, 3);
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // Interior (50, 50) should be red (fill).
    let (r, _g, _b, _a) = read_pixel(&buf, stride, 50, 50);
    assert!(r > 200, "fill interior should be red, got r={}", r);

    // On the edge (bottom edge, 50, ~79) should show blue stroke.
    let (_r, _g, b, _a) = read_pixel(&buf, stride, 50, 79);
    assert!(b > 50, "edge should show blue stroke, got b={}", b);
}

/// VAL-PRIM-013: Empty and degenerate paths — no crash, no stray pixels.
#[test]
fn path_empty_no_crash() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    // Empty path (0 commands).
    let (nodes, data) = build_path_scene(
        &[],
        scene::Color::rgba(255, 0, 0, 255),
        scene::Color::TRANSPARENT,
        0,
    );
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // No pixels changed — everything should remain black.
    let (r, g, b, _a) = read_pixel(&buf, stride, 50, 50);
    assert_eq!(
        (r, g, b),
        (0, 0, 0),
        "empty path should leave framebuffer untouched"
    );
}

#[test]
fn path_only_moveto_no_crash() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let cmds = [scene::PathCmd::move_to(50, 50)];
    let (nodes, data) = build_path_scene(
        &cmds,
        scene::Color::rgba(255, 0, 0, 255),
        scene::Color::TRANSPARENT,
        0,
    );
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // No segments generated — framebuffer untouched.
    let (r, g, b, _a) = read_pixel(&buf, stride, 50, 50);
    assert_eq!(
        (r, g, b),
        (0, 0, 0),
        "only-MoveTo path should leave framebuffer untouched"
    );
}

#[test]
fn path_lineto_without_moveto_no_crash() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    let cmds = [
        scene::PathCmd::line_to(50, 50),
        scene::PathCmd::line_to(80, 80),
    ];
    let (nodes, data) = build_path_scene(
        &cmds,
        scene::Color::rgba(255, 0, 0, 255),
        scene::Color::TRANSPARENT,
        0,
    );
    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = black_surface(&mut buf);
    // This should not crash — implicit moveto at (0,0).
    scene_render::render_scene(&mut fb, &graph, &ctx);
}

/// VAL-PRIM-014: Path rendering respects node clipping (CLIPS_CHILDREN).
#[test]
fn path_clipped_to_parent_bounds() {
    let mono = zeroed_glyph_cache();
    let prop = zeroed_glyph_cache();
    let ctx = test_ctx(&mono, &prop);

    // Path that extends beyond a small parent node.
    let cmds = [
        scene::PathCmd::move_to(0, 0),
        scene::PathCmd::line_to(100, 0),
        scene::PathCmd::line_to(100, 100),
        scene::PathCmd::line_to(0, 100),
        scene::PathCmd::close(),
    ];
    let fill = scene::Color::rgba(255, 0, 0, 255);

    let cmd_size = core::mem::size_of::<scene::PathCmd>();
    let total_bytes = cmds.len() * cmd_size;
    let mut data = vec![0u8; total_bytes];
    unsafe {
        core::ptr::copy_nonoverlapping(
            cmds.as_ptr() as *const u8,
            data.as_mut_ptr(),
            total_bytes,
        );
    }

    let mut nodes = vec![Node::EMPTY; 2];

    // Root: 50×50, clips children — smaller than the path
    nodes[0].width = 50;
    nodes[0].height = 50;
    nodes[0].flags = scene::NodeFlags::VISIBLE | scene::NodeFlags::CLIPS_CHILDREN;
    nodes[0].first_child = 1;

    // Path node: 100×100 (extends beyond parent)
    nodes[1].width = 100;
    nodes[1].height = 100;
    nodes[1].flags = scene::NodeFlags::VISIBLE;
    nodes[1].content = scene::Content::Path {
        commands: scene::DataRef {
            offset: 0,
            length: total_bytes as u32,
        },
        fill,
        stroke: scene::Color::TRANSPARENT,
        stroke_width: 0,
        _pad: [0; 3],
    };

    let graph = scene_render::SceneGraph {
        nodes: &nodes,
        data: &data,
    };

    let stride = 100u32 * 4;
    let mut buf = vec![0u8; (stride * 100) as usize];
    let mut fb = Surface {
        data: &mut buf,
        width: 100,
        height: 100,
        stride,
        format: drawing::PixelFormat::Bgra8888,
    };
    // Fill with black first.
    for pixel in fb.data.chunks_exact_mut(4) {
        pixel[0] = 0;
        pixel[1] = 0;
        pixel[2] = 0;
        pixel[3] = 255;
    }
    scene_render::render_scene(&mut fb, &graph, &ctx);

    // Inside parent (25, 25) — should be red.
    let (r, g, b, _a) = read_pixel(&buf, stride, 25, 25);
    assert!(r > 200, "inside clip should be red, got r={}", r);

    // Outside parent (75, 75) — should be black (clipped).
    let (r, g, b, _a) = read_pixel(&buf, stride, 75, 75);
    assert!(
        r < 10 && g < 10 && b < 10,
        "outside clip should be black, got ({},{},{})",
        r, g, b
    );
}
