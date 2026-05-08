extern crate alloc;

use alloc::{vec, vec::Vec};

use crate::{
    diff::{abs_bounds, build_parent_map},
    node::*,
    primitives::*,
    reader::SceneReader,
    stroke::expand_stroke,
    svg_path::parse_svg_path,
    transform::AffineTransform,
    triple::{TripleWriter, TRIPLE_SCENE_SIZE},
    writer::SceneWriter,
};

// ── Millipoint conversions ─────────────────────────────────────────

#[test]
fn millipoint_round_trip() {
    assert_eq!(pt(1), MPT_PER_PT);
    assert_eq!(pt(10), 10 * MPT_PER_PT);
    assert_eq!(pt(-5), -5 * MPT_PER_PT);
    assert_eq!(upt(1), MPT_PER_PT as u32);
}

#[test]
fn millipoint_f32_conversion() {
    let mpt = pt(10);
    let f = mpt_to_f32(mpt);

    assert!((f - 10.0).abs() < 1e-5);
    assert_eq!(f32_to_mpt(10.0), pt(10));
}

#[test]
fn millipoint_rounding() {
    assert_eq!(mpt_round_pt(0), 0);
    assert_eq!(mpt_round_pt(pt(3)), pt(3));
    assert_eq!(mpt_round_pt(pt(3) + MPT_PER_PT / 2), pt(4));
    assert_eq!(mpt_round_pt(pt(3) + MPT_PER_PT / 2 - 1), pt(3));
    assert_eq!(mpt_round_pt(pt(-3)), pt(-3));
    assert_eq!(mpt_round_pt(pt(-3) - MPT_PER_PT / 2), pt(-4));
}

// ── Node layout ────────────────────────────────────────────────────

#[test]
fn node_size_is_144() {
    assert_eq!(core::mem::size_of::<Node>(), 144);
}

#[test]
fn header_size_is_80() {
    assert_eq!(core::mem::size_of::<SceneHeader>(), 80);
}

#[test]
fn content_size_is_24() {
    assert_eq!(core::mem::size_of::<Content>(), 24);
}

#[test]
fn shaped_glyph_size_is_16() {
    assert_eq!(core::mem::size_of::<ShapedGlyph>(), 16);
}

#[test]
fn scene_size_consistent() {
    assert_eq!(NODES_OFFSET, core::mem::size_of::<SceneHeader>());
    assert_eq!(DATA_OFFSET, NODES_OFFSET + MAX_NODES * 144);
    assert_eq!(SCENE_SIZE, DATA_OFFSET + DATA_BUFFER_SIZE);
}

#[test]
fn node_empty_default() {
    let n = Node::EMPTY;
    assert_eq!(n.first_child, NULL);
    assert_eq!(n.next_sibling, NULL);
    assert_eq!(n.x, 0);
    assert_eq!(n.y, 0);
    assert_eq!(n.width, 0);
    assert_eq!(n.height, 0);
    assert_eq!(n.opacity, 255);
    assert!(n.visible());
    assert!(!n.clips_children());
    assert_eq!(n.background, Color::TRANSPARENT);
    assert_eq!(n.role, ROLE_NONE);
    assert_eq!(n.content, Content::None);
}

// ── Color ──────────────────────────────────────────────────────────

#[test]
fn color_constructors() {
    let c = Color::rgb(255, 128, 0);
    assert_eq!(c.r, 255);
    assert_eq!(c.g, 128);
    assert_eq!(c.b, 0);
    assert_eq!(c.a, 255);

    let c2 = Color::rgba(10, 20, 30, 128);

    assert_eq!(c2.a, 128);
    assert_eq!(Color::TRANSPARENT.a, 0);
}

// ── DataRef ────────────────────────────────────────────────────────

#[test]
fn dataref_empty() {
    assert!(DataRef::EMPTY.is_empty());
    assert_eq!(DataRef::EMPTY.offset, 0);
    assert_eq!(DataRef::EMPTY.length, 0);
}

// ── FNV-1a ─────────────────────────────────────────────────────────

#[test]
fn fnv1a_known_values() {
    assert_eq!(fnv1a(b""), 0x811c_9dc5);
    assert_eq!(fnv1a(b"a"), 0xe40c_292c);
    assert_eq!(fnv1a(b"ab"), 0x4d25_05ca);

    let h1 = fnv1a(b"hello");
    let h2 = fnv1a(b"hello");

    assert_eq!(h1, h2);
    assert_ne!(fnv1a(b"hello"), fnv1a(b"world"));
}

// ── SceneWriter + SceneReader ──────────────────────────────────────

fn make_scene_buf() -> Vec<u8> {
    vec![0u8; SCENE_SIZE]
}

#[test]
fn writer_init_and_alloc() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    assert_eq!(w.node_count(), 0);
    assert_eq!(w.generation(), 0);
    assert_eq!(w.root(), NULL);
    assert_eq!(w.data_used(), 0);

    let id = w.alloc_node().unwrap();

    assert_eq!(id, 0);
    assert_eq!(w.node_count(), 1);

    let id2 = w.alloc_node().unwrap();

    assert_eq!(id2, 1);
    assert_eq!(w.node_count(), 2);
}

#[test]
fn writer_alloc_max_nodes() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    for i in 0..MAX_NODES {
        assert!(w.alloc_node().is_some(), "failed at node {}", i);
    }

    assert!(w.alloc_node().is_none());
}

#[test]
fn writer_tree_operations() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child1 = w.alloc_node().unwrap();
    let child2 = w.alloc_node().unwrap();

    w.set_root(root);
    w.add_child(root, child1);
    w.add_child(root, child2);

    assert_eq!(w.root(), root);
    assert_eq!(w.node(root).first_child, child1);
    assert_eq!(w.node(child1).next_sibling, child2);
    assert_eq!(w.node(child2).next_sibling, NULL);
}

#[test]
fn writer_push_data() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let data = b"hello world";
    let dref = w.push_data(data);

    assert_eq!(dref.offset, 0);
    assert_eq!(dref.length, data.len() as u32);
    assert_eq!(w.data_used(), data.len() as u32);

    let data2 = b"more data";
    let dref2 = w.push_data(data2);

    assert_eq!(dref2.offset, data.len() as u32);
    assert_eq!(dref2.length, data2.len() as u32);
}

#[test]
fn writer_push_shaped_glyphs() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let glyphs = [
        ShapedGlyph {
            glyph_id: 42,
            _pad: 0,
            x_advance: 1024 << 16,
            x_offset: 0,
            y_offset: 0,
        },
        ShapedGlyph {
            glyph_id: 100,
            _pad: 0,
            x_advance: 512 << 16,
            x_offset: 0,
            y_offset: 0,
        },
    ];
    let dref = w.push_shaped_glyphs(&glyphs);

    assert_eq!(dref.length, (glyphs.len() * 16) as u32);
}

#[test]
fn writer_commit_increments_generation() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    assert_eq!(w.generation(), 0);

    w.commit();

    assert_eq!(w.generation(), 1);

    w.commit();

    assert_eq!(w.generation(), 2);
}

#[test]
fn writer_clear_resets_state() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    w.alloc_node();
    w.alloc_node();
    w.push_data(b"test data");
    w.set_root(0);
    w.commit();
    w.clear();

    assert_eq!(w.node_count(), 0);
    assert_eq!(w.data_used(), 0);
    assert_eq!(w.root(), NULL);
    assert_eq!(w.generation(), 1);
}

#[test]
fn writer_node_mutation() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();

    w.node_mut(id).x = pt(100);
    w.node_mut(id).y = pt(200);
    w.node_mut(id).width = upt(50);
    w.node_mut(id).height = upt(30);
    w.node_mut(id).background = Color::rgb(255, 0, 0);
    w.node_mut(id).opacity = 200;

    assert_eq!(w.node(id).x, pt(100));
    assert_eq!(w.node(id).y, pt(200));
    assert_eq!(w.node(id).width, upt(50));
    assert_eq!(w.node(id).height, upt(30));
    assert_eq!(w.node(id).background, Color::rgb(255, 0, 0));
    assert_eq!(w.node(id).opacity, 200);
}

#[test]
fn writer_set_node_count_cleans_dangling() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();
    let grandchild = w.alloc_node().unwrap();

    w.add_child(root, child);
    w.add_child(child, grandchild);
    w.set_node_count(2);

    assert_eq!(w.node(child).first_child, NULL);
}

// ── Reader/Writer round-trip ───────────────────────────────────────

#[test]
fn reader_roundtrip() {
    let mut buf = make_scene_buf();

    {
        let mut w = SceneWriter::new(&mut buf);
        let root = w.alloc_node().unwrap();
        let child = w.alloc_node().unwrap();

        w.set_root(root);
        w.add_child(root, child);
        w.node_mut(root).x = pt(10);
        w.node_mut(child).background = Color::rgb(0, 255, 0);

        let text = b"test text data";
        let dref = w.push_data(text);

        w.node_mut(child).name = dref;
        w.commit();
    }

    let r = SceneReader::new(&buf);

    assert_eq!(r.generation(), 1);
    assert_eq!(r.node_count(), 2);
    assert_eq!(r.root(), 0);

    let root = r.node(0);

    assert_eq!(root.x, pt(10));
    assert_eq!(root.first_child, 1);

    let child = r.node(1);

    assert_eq!(child.background, Color::rgb(0, 255, 0));

    let name_data = r.data(child.name);

    assert_eq!(name_data, b"test text data");
}

#[test]
fn reader_glyph_roundtrip() {
    let mut buf = make_scene_buf();

    {
        let mut w = SceneWriter::new(&mut buf);
        let id = w.alloc_node().unwrap();
        let glyphs = [
            ShapedGlyph {
                glyph_id: 72,
                _pad: 0,
                x_advance: 600 << 16,
                x_offset: 10,
                y_offset: -5,
            },
            ShapedGlyph {
                glyph_id: 101,
                _pad: 0,
                x_advance: 550 << 16,
                x_offset: 0,
                y_offset: 0,
            },
        ];
        let dref = w.push_shaped_glyphs(&glyphs);

        w.node_mut(id).content = Content::Glyphs {
            color: Color::rgb(0, 0, 0),
            glyphs: dref,
            glyph_count: 2,
            font_size: 18,
            style_id: 1,
        };
        w.commit();
    }

    let r = SceneReader::new(&buf);

    if let Content::Glyphs {
        glyphs,
        glyph_count,
        ..
    } = r.node(0).content
    {
        let g = r.shaped_glyphs(glyphs, glyph_count);

        assert_eq!(g.len(), 2);
        assert_eq!(g[0].glyph_id, 72);
        assert_eq!(g[0].x_advance, 600 << 16);
        assert_eq!(g[1].glyph_id, 101);
    } else {
        panic!("expected Glyphs content");
    }
}

// ── Dirty bitmap ───────────────────────────────────────────────────

#[test]
fn dirty_bitmap() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..10 {
        w.alloc_node();
    }

    assert!(!w.is_dirty(0));
    assert!(!w.is_dirty(5));

    w.mark_dirty(3);
    w.mark_dirty(7);

    assert!(w.is_dirty(3));
    assert!(w.is_dirty(7));
    assert!(!w.is_dirty(0));
    assert!(!w.is_dirty(4));
    assert_eq!(w.dirty_count(), 2);

    w.set_all_dirty();

    assert!(w.is_dirty(0));
    assert!(w.is_dirty(9));

    w.clear_dirty();

    assert!(!w.is_dirty(3));
    assert_eq!(w.dirty_count(), 0);
}

// ── Data buffer operations ─────────────────────────────────────────

#[test]
fn push_path_commands_aligned() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);

    w.push_data(b"x");

    assert_eq!(w.data_used(), 1);

    let path_data = vec![0u8; 12];
    let dref = w.push_path_commands(&path_data);

    assert_eq!(dref.offset % 4, 0);
}

#[test]
fn update_data_same_length() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"AAAA");

    assert!(w.update_data(dref, b"BBBB"));
    assert_eq!(w.data_buf()[..4], *b"BBBB");
}

#[test]
fn update_data_wrong_length_fails() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"AAAA");

    assert!(!w.update_data(dref, b"BB"));
}

#[test]
fn reset_data_clears_content() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let dref = w.push_data(b"glyph data");

    w.node_mut(id).content = Content::Glyphs {
        color: Color::rgb(0, 0, 0),
        glyphs: dref,
        glyph_count: 1,
        font_size: 12,
        style_id: 0,
    };
    w.reset_data();

    assert_eq!(w.data_used(), 0);
    assert_eq!(w.node(id).content, Content::None);
}

#[test]
fn has_data_space() {
    let mut buf = make_scene_buf();
    let w = SceneWriter::new(&mut buf);

    assert!(w.has_data_space(DATA_BUFFER_SIZE));
    assert!(!w.has_data_space(DATA_BUFFER_SIZE + 1));
}

// ── Sibling iteration ──────────────────────────────────────────────

#[test]
fn sibling_iteration() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let c0 = w.alloc_node().unwrap();
    let c1 = w.alloc_node().unwrap();
    let c2 = w.alloc_node().unwrap();

    w.set_root(root);
    w.add_child(root, c0);
    w.add_child(root, c1);
    w.add_child(root, c2);

    let children: Vec<NodeId> = w.siblings(w.node(root).first_child).collect();

    assert_eq!(children, vec![c0, c1, c2]);

    let partial: Vec<NodeId> = w.children_until(w.node(root).first_child, c2).collect();

    assert_eq!(partial, vec![c0, c1]);
}

// ── Triple buffer ──────────────────────────────────────────────────

#[test]
fn triple_buffer_publish_acquire() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let mut tw = TripleWriter::new(&mut buf);

    {
        let mut scene = tw.acquire();
        let root = scene.alloc_node().unwrap();

        scene.set_root(root);
        scene.node_mut(root).background = Color::rgb(255, 0, 0);
        scene.commit();
    }

    tw.publish();

    assert_eq!(tw.generation(), 1);
    assert_eq!(tw.latest_nodes().len(), 1);
    assert_eq!(tw.latest_nodes()[0].background, Color::rgb(255, 0, 0));
}

#[test]
fn triple_buffer_multiple_publishes() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let mut tw = TripleWriter::new(&mut buf);

    for i in 0..5 {
        let mut scene = tw.acquire();

        scene.clear();

        let root = scene.alloc_node().unwrap();

        scene.set_root(root);
        scene.node_mut(root).opacity = (50 + i * 10) as u8;
        scene.commit();
        tw.publish();
    }

    assert_eq!(tw.generation(), 5);
    assert_eq!(tw.latest_nodes()[0].opacity, 90);
}

#[test]
fn triple_buffer_acquire_copy() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let mut tw = TripleWriter::new(&mut buf);

    {
        let mut scene = tw.acquire();
        let root = scene.alloc_node().unwrap();
        let child = scene.alloc_node().unwrap();

        scene.set_root(root);
        scene.add_child(root, child);
        scene.node_mut(root).background = Color::rgb(100, 100, 100);
        scene.node_mut(child).x = pt(50);
        scene.commit();
    }

    tw.publish();

    {
        let mut scene = tw.acquire_copy();
        assert_eq!(scene.node_count(), 2);
        assert_eq!(scene.node(0).background, Color::rgb(100, 100, 100));
        assert_eq!(scene.node(1).x, pt(50));

        scene.node_mut(1).x = pt(75);
        scene.commit();
    }

    tw.publish();

    assert_eq!(tw.latest_nodes()[1].x, pt(75));
    assert_eq!(tw.latest_nodes()[0].background, Color::rgb(100, 100, 100));
}

#[test]
fn triple_reader_claims_latest() {
    let mut buf = vec![0u8; TRIPLE_SCENE_SIZE];
    let mut tw = TripleWriter::new(&mut buf);

    {
        let mut scene = tw.acquire();
        let root = scene.alloc_node().unwrap();

        scene.set_root(root);
        scene.node_mut(root).width = upt(640);
        scene.commit();
    }

    tw.publish();

    // SAFETY: buf is valid and large enough.
    let reader = unsafe { crate::triple::TripleReader::new(buf.as_mut_ptr(), buf.len()) };

    assert_eq!(reader.front_generation(), 1);
    assert_eq!(reader.front_nodes().len(), 1);
    assert_eq!(reader.front_nodes()[0].width, upt(640));
}

// ── Parent map and absolute bounds ─────────────────────────────────

#[test]
fn parent_map_basic() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();
    let grandchild = w.alloc_node().unwrap();

    w.add_child(root, child);
    w.add_child(child, grandchild);

    let pmap = build_parent_map(w.nodes(), w.node_count() as usize);

    assert_eq!(pmap[root as usize], NULL);
    assert_eq!(pmap[child as usize], root);
    assert_eq!(pmap[grandchild as usize], child);
}

#[test]
fn abs_bounds_accumulates() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();

    w.add_child(root, child);
    w.node_mut(root).x = pt(10);
    w.node_mut(root).y = pt(20);
    w.node_mut(child).x = pt(5);
    w.node_mut(child).y = pt(3);
    w.node_mut(child).width = upt(100);
    w.node_mut(child).height = upt(50);

    let pmap = build_parent_map(w.nodes(), w.node_count() as usize);
    let (bx, by, bw, bh) = abs_bounds(w.nodes(), &pmap, child as usize);

    assert_eq!(bx, pt(15));
    assert_eq!(by, pt(23));
    assert_eq!(bw, upt(100));
    assert_eq!(bh, upt(50));
}

#[test]
fn abs_bounds_with_child_offset() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();

    w.add_child(root, child);
    w.node_mut(root).x = pt(0);
    w.node_mut(root).y = pt(0);
    w.node_mut(root).child_offset_y = -50.0;
    w.node_mut(child).x = pt(10);
    w.node_mut(child).y = pt(100);
    w.node_mut(child).width = upt(200);
    w.node_mut(child).height = upt(30);

    let pmap = build_parent_map(w.nodes(), w.node_count() as usize);
    let (bx, by, _bw, _bh) = abs_bounds(w.nodes(), &pmap, child as usize);

    assert_eq!(bx, pt(10));

    let expected_y = pt(100) + f32_to_mpt(-50.0);

    assert_eq!(by, expected_y);
}

#[test]
fn abs_bounds_with_shadow() {
    let mut buf = make_scene_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();

    w.node_mut(id).x = pt(100);
    w.node_mut(id).y = pt(100);
    w.node_mut(id).width = upt(200);
    w.node_mut(id).height = upt(100);
    w.node_mut(id).shadow_color = Color::rgba(0, 0, 0, 128);
    w.node_mut(id).shadow_blur_radius = 5;
    w.node_mut(id).shadow_offset_y = 2;

    let pmap = build_parent_map(w.nodes(), w.node_count() as usize);
    let (bx, by, bw, bh) = abs_bounds(w.nodes(), &pmap, id as usize);

    assert!(bx < pt(100));
    assert!(by < pt(100));
    assert!(bw > upt(200));
    assert!(bh > upt(100));
}

// ── AffineTransform ────────────────────────────────────────────────

#[test]
fn transform_identity() {
    let t = AffineTransform::identity();

    assert!(t.is_identity());
    assert!(t.is_pure_translation());
    assert!(t.is_integer_translation());

    let (x, y) = t.transform_point(10.0, 20.0);

    assert!((x - 10.0).abs() < 1e-5);
    assert!((y - 20.0).abs() < 1e-5);
}

#[test]
fn transform_translate() {
    let t = AffineTransform::translate(5.0, 10.0);

    assert!(!t.is_identity());
    assert!(t.is_pure_translation());

    let (x, y) = t.transform_point(1.0, 2.0);

    assert!((x - 6.0).abs() < 1e-5);
    assert!((y - 12.0).abs() < 1e-5);
}

#[test]
fn transform_scale() {
    let t = AffineTransform::scale(2.0, 3.0);

    assert!(!t.is_identity());
    assert!(!t.is_pure_translation());

    let (x, y) = t.transform_point(10.0, 10.0);

    assert!((x - 20.0).abs() < 1e-5);
    assert!((y - 30.0).abs() < 1e-5);
}

#[test]
fn transform_compose() {
    let t1 = AffineTransform::translate(10.0, 0.0);
    let t2 = AffineTransform::scale(2.0, 2.0);
    let composed = t1.compose(t2);
    let (x, y) = composed.transform_point(5.0, 5.0);

    assert!((x - 20.0).abs() < 1e-4);
    assert!((y - 10.0).abs() < 1e-4);
}

#[test]
fn transform_inverse() {
    let t = AffineTransform::translate(10.0, 20.0);
    let inv = t.inverse().unwrap();
    let composed = t.compose(inv);

    assert!(
        composed.is_identity() || {
            (composed.a - 1.0).abs() < 1e-5
                && composed.b.abs() < 1e-5
                && composed.c.abs() < 1e-5
                && (composed.d - 1.0).abs() < 1e-5
                && composed.tx.abs() < 1e-3
                && composed.ty.abs() < 1e-3
        }
    );
}

#[test]
fn transform_rotate_90() {
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_2);
    let (x, y) = t.transform_point(1.0, 0.0);

    assert!(x.abs() < 1e-4);
    assert!((y - 1.0).abs() < 1e-4);
}

#[test]
fn transform_aabb() {
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_4);
    let (bx, _by, bw, bh) = t.transform_aabb(0.0, 0.0, 10.0, 10.0);

    // 10×10 rect at origin rotated 45°: corners at (0,0), (~7.07,~7.07),
    // (~0,~14.14), (~-7.07,~7.07). AABB is wider and taller than 10.
    assert!(bw > 10.0);
    assert!(bh > 10.0);
    assert!(bx < 0.0);
}

#[test]
fn transform_singular_has_no_inverse() {
    let t = AffineTransform::scale(0.0, 1.0);

    assert!(t.inverse().is_none());
}

// ── Path commands ──────────────────────────────────────────────────

#[test]
fn path_commands_roundtrip() {
    let mut buf = Vec::new();

    path_move_to(&mut buf, 10.0, 20.0);
    path_line_to(&mut buf, 30.0, 40.0);
    path_close(&mut buf);

    assert_eq!(
        buf.len(),
        PATH_MOVE_TO_SIZE + PATH_LINE_TO_SIZE + PATH_CLOSE_SIZE
    );

    let tag = u32::from_le_bytes(buf[0..4].try_into().unwrap());

    assert_eq!(tag, PATH_MOVE_TO);

    let x = f32::from_le_bytes(buf[4..8].try_into().unwrap());
    let y = f32::from_le_bytes(buf[8..12].try_into().unwrap());

    assert!((x - 10.0).abs() < 1e-5);
    assert!((y - 20.0).abs() < 1e-5);
}

#[test]
fn path_cubic_command() {
    let mut buf = Vec::new();

    path_cubic_to(&mut buf, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0);

    assert_eq!(buf.len(), PATH_CUBIC_TO_SIZE);

    let tag = u32::from_le_bytes(buf[0..4].try_into().unwrap());

    assert_eq!(tag, PATH_CUBIC_TO);
}

// ── Point-in-path (winding number) ─────────────────────────────────

#[test]
fn point_in_path_square() {
    let mut path = Vec::new();

    path_move_to(&mut path, 0.0, 0.0);
    path_line_to(&mut path, 10.0, 0.0);
    path_line_to(&mut path, 10.0, 10.0);
    path_line_to(&mut path, 0.0, 10.0);
    path_close(&mut path);

    assert_ne!(path_winding_number(&path, 5.0, 5.0), 0);
    assert_eq!(path_winding_number(&path, 15.0, 5.0), 0);
    assert_eq!(path_winding_number(&path, -1.0, 5.0), 0);
}

#[test]
fn point_in_path_empty() {
    assert_eq!(path_winding_number(&[], 0.0, 0.0), 0);
}

// ── SVG path parser ────────────────────────────────────────────────

#[test]
fn svg_parse_move_line_close() {
    let data = parse_svg_path("M 0 0 L 10 0 L 10 10 Z");

    assert!(!data.is_empty());

    let tag0 = u32::from_le_bytes(data[0..4].try_into().unwrap());

    assert_eq!(tag0, PATH_MOVE_TO);
}

#[test]
fn svg_parse_relative_commands() {
    let data = parse_svg_path("M 10 10 l 5 0 l 0 5 z");

    assert!(!data.is_empty());

    let min_bytes = PATH_MOVE_TO_SIZE + 2 * PATH_LINE_TO_SIZE + PATH_CLOSE_SIZE;

    assert!(data.len() >= min_bytes);
}

#[test]
fn svg_parse_horizontal_vertical() {
    let data = parse_svg_path("M 0 0 H 10 V 10 h -10 v -10 Z");
    assert!(!data.is_empty());
}

#[test]
fn svg_parse_cubic() {
    let data = parse_svg_path("M 0 0 C 1 2 3 4 5 6");
    assert!(!data.is_empty());
    let expected_len = PATH_MOVE_TO_SIZE + PATH_CUBIC_TO_SIZE;
    assert_eq!(data.len(), expected_len);
}

#[test]
fn svg_parse_smooth_cubic() {
    let data = parse_svg_path("M 0 0 C 1 2 3 4 5 6 S 8 9 10 11");
    assert!(!data.is_empty());
    let expected_len = PATH_MOVE_TO_SIZE + 2 * PATH_CUBIC_TO_SIZE;
    assert_eq!(data.len(), expected_len);
}

#[test]
fn svg_parse_quadratic() {
    let data = parse_svg_path("M 0 0 Q 5 10 10 0");
    assert!(!data.is_empty());
    let expected_len = PATH_MOVE_TO_SIZE + PATH_CUBIC_TO_SIZE;
    assert_eq!(data.len(), expected_len);
}

#[test]
fn svg_parse_arc() {
    let data = parse_svg_path("M 10 80 A 25 25 0 0 1 50 80");
    assert!(!data.is_empty());
    assert!(data.len() > PATH_MOVE_TO_SIZE);
}

#[test]
fn svg_parse_empty_returns_empty() {
    let data = parse_svg_path("");
    assert!(data.is_empty());
}

#[test]
fn svg_parse_real_icon() {
    let data = parse_svg_path(
        "M12 3c.132 0 .263 0 .393 0a7.5 7.5 0 0 0 7.92 12.446a9 9 0 1 1 -8.313 -12.454z",
    );
    assert!(!data.is_empty());
}

// ── Stroke expansion ───────────────────────────────────────────────

#[test]
fn stroke_simple_line() {
    let mut path = Vec::new();
    path_move_to(&mut path, 0.0, 0.0);
    path_line_to(&mut path, 10.0, 0.0);

    let stroked = expand_stroke(&path, 2.0);
    assert!(!stroked.is_empty());
}

#[test]
fn stroke_closed_triangle() {
    let mut path = Vec::new();
    path_move_to(&mut path, 0.0, 0.0);
    path_line_to(&mut path, 10.0, 0.0);
    path_line_to(&mut path, 5.0, 10.0);
    path_close(&mut path);

    let stroked = expand_stroke(&path, 1.0);
    assert!(!stroked.is_empty());
}

#[test]
fn stroke_zero_width_returns_empty() {
    let mut path = Vec::new();
    path_move_to(&mut path, 0.0, 0.0);
    path_line_to(&mut path, 10.0, 0.0);

    let stroked = expand_stroke(&path, 0.0);
    assert!(stroked.is_empty());
}

#[test]
fn stroke_empty_input_returns_empty() {
    let stroked = expand_stroke(&[], 2.0);
    assert!(stroked.is_empty());
}

#[test]
fn stroke_dot_produces_circle() {
    let mut path = Vec::new();
    path_move_to(&mut path, 5.0, 5.0);
    path_line_to(&mut path, 5.0, 5.0);

    let stroked = expand_stroke(&path, 2.0);
    assert!(!stroked.is_empty());
    let first_tag = u32::from_le_bytes(stroked[0..4].try_into().unwrap());
    assert_eq!(first_tag, PATH_MOVE_TO);
}

// ── Node semantic fields ───────────────────────────────────────────

#[test]
fn node_has_shadow() {
    let mut n = Node::EMPTY;
    assert!(!n.has_shadow());

    n.shadow_color = Color::rgba(0, 0, 0, 128);
    n.shadow_blur_radius = 5;
    assert!(n.has_shadow());

    n.shadow_color = Color::TRANSPARENT;
    assert!(!n.has_shadow());
}

#[test]
fn node_flags() {
    let flags = NodeFlags::VISIBLE.union(NodeFlags::CLIPS_CHILDREN);
    assert!(flags.contains(NodeFlags::VISIBLE));
    assert!(flags.contains(NodeFlags::CLIPS_CHILDREN));

    let empty = NodeFlags::empty();
    assert!(!empty.contains(NodeFlags::VISIBLE));
}

// ── Content variants ───────────────────────────────────────────────

#[test]
fn content_variants_eq() {
    let c1 = Content::None;
    let c2 = Content::None;
    assert_eq!(c1, c2);

    let c3 = Content::Image {
        content_id: 42,
        src_width: 100,
        src_height: 100,
    };
    let c4 = Content::Image {
        content_id: 42,
        src_width: 100,
        src_height: 100,
    };
    assert_eq!(c3, c4);
}

// ── Edge cases ─────────────────────────────────────────────────────

#[test]
fn reader_out_of_bounds_data_ref() {
    let mut buf = make_scene_buf();
    let _ = SceneWriter::new(&mut buf);

    let r = SceneReader::new(&buf);
    let bad_ref = DataRef {
        offset: DATA_BUFFER_SIZE as u32 + 100,
        length: 10,
    };
    assert!(r.data(bad_ref).is_empty());
}

#[test]
fn reader_empty_shaped_glyphs() {
    let mut buf = make_scene_buf();
    let _ = SceneWriter::new(&mut buf);
    let r = SceneReader::new(&buf);
    let g = r.shaped_glyphs(DataRef::EMPTY, 0);
    assert!(g.is_empty());
}

#[test]
fn from_existing_preserves_state() {
    let mut buf = make_scene_buf();
    {
        let mut w = SceneWriter::new(&mut buf);
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        w.node_mut(root).x = pt(42);
        w.commit();
    }

    let w2 = SceneWriter::from_existing(&mut buf);
    assert_eq!(w2.generation(), 1);
    assert_eq!(w2.node_count(), 1);
    assert_eq!(w2.node(0).x, pt(42));
}
