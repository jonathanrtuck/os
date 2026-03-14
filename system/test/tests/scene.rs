use scene::*;

fn make_buf() -> Vec<u8> {
    vec![0u8; SCENE_SIZE]
}

/// Build a monospace Content::Text from raw UTF-8 bytes.
/// Each byte is treated as a glyph ID with uniform advance.
fn make_mono_text(
    w: &mut SceneWriter,
    text: &[u8],
    font_size: u16,
    color: Color,
    advance: u16,
) -> Content {
    let run = TextRun {
        glyphs: w.push_data(text),
        glyph_count: text.len() as u16,
        x: 0,
        y: 0,
        color,
        advance,
        font_size,
    };
    let (runs, run_count) = w.push_text_runs(&[run]);
    Content::Text {
        runs,
        run_count,
        _pad: [0; 2],
    }
}

// ── SceneWriter basics ──────────────────────────────────────────────

#[test]
fn writer_new_empty_state() {
    let mut buf = make_buf();
    let w = SceneWriter::new(&mut buf);
    assert_eq!(w.node_count(), 0);
    assert_eq!(w.data_used(), 0);
    assert_eq!(w.generation(), 0);
}

#[test]
fn writer_alloc_node_returns_sequential_ids() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let n0 = w.alloc_node().unwrap();
    let n1 = w.alloc_node().unwrap();
    let n2 = w.alloc_node().unwrap();
    assert_eq!(n0, 0);
    assert_eq!(n1, 1);
    assert_eq!(n2, 2);
    assert_eq!(w.node_count(), 3);
}

#[test]
fn writer_alloc_node_initialized_to_empty() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let node = w.node(id);
    assert_eq!(node.first_child, NULL);
    assert_eq!(node.next_sibling, NULL);
    assert!(node.visible());
    assert_eq!(node.opacity, 255);
}

#[test]
fn writer_node_mutation() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    {
        let n = w.node_mut(id);
        n.x = 10;
        n.y = 20;
        n.width = 100;
        n.height = 50;
        n.background = Color::rgb(255, 0, 0);
    }
    assert_eq!(w.node(id).x, 10);
    assert_eq!(w.node(id).width, 100);
    assert_eq!(w.node(id).background, Color::rgb(255, 0, 0));
}

#[test]
fn writer_push_data_returns_valid_ref() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"hello world");
    assert_eq!(dref.offset, 0);
    assert_eq!(dref.length, 11);
    assert_eq!(w.data_used(), 11);
}

#[test]
fn writer_push_data_sequential_offsets() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let d1 = w.push_data(b"abc");
    let d2 = w.push_data(b"defgh");
    assert_eq!(d1.offset, 0);
    assert_eq!(d1.length, 3);
    assert_eq!(d2.offset, 3);
    assert_eq!(d2.length, 5);
    assert_eq!(w.data_used(), 8);
}

#[test]
fn writer_set_root() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.set_root(root);
    assert_eq!(w.root(), root);
}

#[test]
fn writer_commit_increments_generation() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    assert_eq!(w.generation(), 0);
    w.commit();
    assert_eq!(w.generation(), 1);
    w.commit();
    assert_eq!(w.generation(), 2);
}

#[test]
fn writer_clear_resets_state() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.alloc_node();
    w.alloc_node();
    w.push_data(b"some data");
    w.commit();
    assert_eq!(w.node_count(), 2);
    assert!(w.data_used() > 0);

    w.clear();
    assert_eq!(w.node_count(), 0);
    assert_eq!(w.data_used(), 0);
    // Generation preserved across clear.
    assert_eq!(w.generation(), 1);
}

// ── Tree structure ──────────────────────────────────────────────────

#[test]
fn writer_add_child_links_nodes() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let parent = w.alloc_node().unwrap();
    let child1 = w.alloc_node().unwrap();
    let child2 = w.alloc_node().unwrap();
    w.add_child(parent, child1);
    w.add_child(parent, child2);
    assert_eq!(w.node(parent).first_child, child1);
    assert_eq!(w.node(child1).next_sibling, child2);
    assert_eq!(w.node(child2).next_sibling, NULL);
}

#[test]
fn writer_add_child_single() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let parent = w.alloc_node().unwrap();
    let child = w.alloc_node().unwrap();
    w.add_child(parent, child);
    assert_eq!(w.node(parent).first_child, child);
    assert_eq!(w.node(child).next_sibling, NULL);
}

// ── Text content ────────────────────────────────────────────────────

#[test]
fn writer_text_node_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let text_data = b"Hello, OS!";
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = make_mono_text(&mut w, text_data, 18, Color::rgb(220, 220, 220), 8);
    // Read back via SceneReader.
    let r = SceneReader::new(&buf);
    let node = r.node(id);
    match node.content {
        Content::Text {
            runs, run_count, ..
        } => {
            assert_eq!(run_count, 1);
            let text_runs = r.text_runs(runs);
            assert_eq!(text_runs.len(), 1);
            assert_eq!(text_runs[0].font_size, 18);
            assert_eq!(text_runs[0].advance, 8);
            assert_eq!(r.data(text_runs[0].glyphs), text_data);
        }
        _ => panic!("expected Text content"),
    }
}

#[test]
fn writer_text_runs_multiple_lines() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let line1 = b"Hello";
    let line2 = b"World";
    let d1 = w.push_data(line1);
    let d2 = w.push_data(line2);
    let runs = [
        TextRun {
            glyphs: d1,
            glyph_count: 5,
            x: 0,
            y: 0,
            color: Color::rgb(200, 200, 200),
            advance: 8,
            font_size: 16,
        },
        TextRun {
            glyphs: d2,
            glyph_count: 5,
            x: 0,
            y: 18,
            color: Color::rgb(200, 200, 200),
            advance: 8,
            font_size: 16,
        },
    ];
    let (runs_ref, count) = w.push_text_runs(&runs);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text {
        runs: runs_ref,
        run_count: count,
        _pad: [0; 2],
    };

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 2);
    assert_eq!(r.data(text_runs[0].glyphs), b"Hello");
    assert_eq!(r.data(text_runs[1].glyphs), b"World");
    assert_eq!(text_runs[1].y, 18);
}

#[test]
fn push_text_runs_round_trips_struct_fields() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let d = w.push_data(b"x");
    let run = TextRun {
        glyphs: d,
        glyph_count: 1,
        x: -5,
        y: 100,
        color: Color::rgba(10, 20, 30, 40),
        advance: 12,
        font_size: 24,
    };
    let (runs_ref, count) = w.push_text_runs(&[run]);
    assert_eq!(count, 1);
    let r = SceneReader::new(&buf);
    let read_runs = r.text_runs(runs_ref);
    assert_eq!(read_runs[0].x, -5);
    assert_eq!(read_runs[0].y, 100);
    assert_eq!(read_runs[0].color, Color::rgba(10, 20, 30, 40));
    assert_eq!(read_runs[0].advance, 12);
    assert_eq!(read_runs[0].font_size, 24);
}

// ── Image content ───────────────────────────────────────────────────

#[test]
fn writer_image_node_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let pixels: Vec<u8> = (0..64).collect();
    let dref = w.push_data(&pixels);
    let id = w.alloc_node().unwrap();
    {
        let n = w.node_mut(id);
        n.content = Content::Image {
            data: dref,
            src_width: 4,
            src_height: 4,
        };
    }
    let r = SceneReader::new(&buf);
    let node = r.node(id);
    match node.content {
        Content::Image {
            data,
            src_width,
            src_height,
        } => {
            assert_eq!(src_width, 4);
            assert_eq!(src_height, 4);
            assert_eq!(r.data(data).len(), 64);
        }
        _ => panic!("expected Image content"),
    }
}

// ── SceneReader ─────────────────────────────────────────────────────

#[test]
fn reader_reads_writer_output() {
    let mut buf = make_buf();
    {
        let mut w = SceneWriter::new(&mut buf);
        let root = w.alloc_node().unwrap();
        let child = w.alloc_node().unwrap();
        w.node_mut(root).width = 800;
        w.node_mut(root).height = 600;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.add_child(root, child);
        w.node_mut(child).content =
            make_mono_text(&mut w, b"content", 16, Color::rgb(200, 200, 200), 8);
        w.set_root(root);
        w.commit();
    }
    let r = SceneReader::new(&buf);
    assert_eq!(r.generation(), 1);
    assert_eq!(r.node_count(), 2);
    assert_eq!(r.root(), 0);
    assert_eq!(r.node(0).width, 800);
    assert_eq!(r.node(0).first_child, 1);
    match r.node(1).content {
        Content::Text {
            runs, run_count, ..
        } => {
            assert_eq!(run_count, 1);
            let text_runs = r.text_runs(runs);
            assert_eq!(r.data(text_runs[0].glyphs), b"content");
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn reader_nodes_slice() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.alloc_node();
    w.alloc_node();
    w.alloc_node();
    let r = SceneReader::new(&buf);
    assert_eq!(r.nodes().len(), 3);
}

#[test]
fn reader_data_buf_slice() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.push_data(b"abc");
    w.push_data(b"def");
    let r = SceneReader::new(&buf);
    assert_eq!(r.data_buf(), b"abcdef");
}

// ── Overflow handling ───────────────────────────────────────────────

#[test]
fn writer_node_overflow_returns_none() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    for _ in 0..MAX_NODES {
        assert!(w.alloc_node().is_some());
    }
    assert!(w.alloc_node().is_none());
}

#[test]
fn writer_data_overflow_truncates() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    // Fill most of the buffer.
    let big = vec![0xABu8; DATA_BUFFER_SIZE - 10];
    let d1 = w.push_data(&big);
    assert_eq!(d1.length as usize, DATA_BUFFER_SIZE - 10);
    // Try to push 20 bytes — only 10 fit.
    let d2 = w.push_data(&[0xCD; 20]);
    assert_eq!(d2.length, 10);
    assert_eq!(w.data_used() as usize, DATA_BUFFER_SIZE);
}

#[test]
fn writer_data_empty_push() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"");
    assert_eq!(dref.offset, 0);
    assert_eq!(dref.length, 0);
}

// ── Edge cases ──────────────────────────────────────────────────────

#[test]
fn reader_data_invalid_ref_returns_empty() {
    let mut buf = make_buf();
    let _ = SceneWriter::new(&mut buf);
    let r = SceneReader::new(&buf);
    // Reference beyond data_used.
    let bad = DataRef {
        offset: 9999,
        length: 100,
    };
    assert_eq!(r.data(bad).len(), 0);
}

#[test]
fn writer_build_typical_editor_scene() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Root (full screen background).
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 1024;
    w.node_mut(root).height = 768;
    w.node_mut(root).background = Color::rgb(30, 30, 30);
    w.set_root(root);

    // Title bar.
    let title = w.alloc_node().unwrap();
    w.node_mut(title).width = 1024;
    w.node_mut(title).height = 36;
    w.node_mut(title).background = Color::rgba(20, 20, 20, 200);
    w.add_child(root, title);

    // Title text.
    let title_text = w.alloc_node().unwrap();
    w.node_mut(title_text).x = 12;
    w.node_mut(title_text).y = 8;
    w.node_mut(title_text).content =
        make_mono_text(&mut w, b"Text", 18, Color::rgb(200, 200, 200), 8);
    w.add_child(title, title_text);

    // Content area.
    let content = w.alloc_node().unwrap();
    w.node_mut(content).y = 36;
    w.node_mut(content).width = 1024;
    w.node_mut(content).height = 732;
    w.node_mut(content).flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    w.add_child(root, content);

    // Document text.
    let doc_text = w.alloc_node().unwrap();
    w.node_mut(doc_text).x = 12;
    w.node_mut(doc_text).y = 8;
    w.node_mut(doc_text).width = 1000;
    w.node_mut(doc_text).height = u16::MAX;
    w.node_mut(doc_text).content = make_mono_text(
        &mut w,
        b"Hello, world!\nThis is a test.",
        18,
        Color::rgb(220, 220, 220),
        8,
    );
    w.add_child(content, doc_text);

    w.commit();

    // Verify the tree structure via reader.
    let r = SceneReader::new(&buf);
    assert_eq!(r.node_count(), 5);
    assert_eq!(r.root(), 0);

    // Root -> title, content
    let root_node = r.node(0);
    assert_eq!(root_node.first_child, 1); // title
    let title_node = r.node(1);
    assert_eq!(title_node.next_sibling, 3); // content
    let content_node = r.node(3);
    assert_eq!(content_node.first_child, 4); // doc_text
    assert!(content_node.clips_children());

    // Verify text content.
    match r.node(4).content {
        Content::Text {
            runs, run_count, ..
        } => {
            assert_eq!(run_count, 1);
            let text_runs = r.text_runs(runs);
            assert_eq!(
                r.data(text_runs[0].glyphs),
                b"Hello, world!\nThis is a test."
            );
        }
        _ => panic!("expected Text"),
    }
}

// ── replace_data ────────────────────────────────────────────────────

#[test]
fn writer_replace_data_appends_new() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let d1 = w.push_data(b"old");
    let d2 = w.replace_data(b"new content");
    // Old data abandoned, new data appended.
    assert_eq!(d2.offset, 3);
    assert_eq!(d2.length, 11);
    let r = SceneReader::new(&buf);
    assert_eq!(r.data(d1), b"old");
    assert_eq!(r.data(d2), b"new content");
}

// ── update_data ─────────────────────────────────────────────────────

#[test]
fn writer_update_data_in_place() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"12345678");
    assert!(w.update_data(dref, b"ABCDEFGH"));
    let r = SceneReader::new(&buf);
    assert_eq!(r.data(dref), b"ABCDEFGH");
}

#[test]
fn writer_update_data_wrong_length_fails() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_data(b"12345678");
    // Wrong length — should fail.
    assert!(!w.update_data(dref, b"ABC"));
}

// ── reset_data ──────────────────────────────────────────────────────

#[test]
fn writer_reset_data_clears_usage() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    w.push_data(b"some data here");
    assert!(w.data_used() > 0);
    w.reset_data();
    assert_eq!(w.data_used(), 0);
}

// ── DoubleWriter / DoubleReader ─────────────────────────────────────

fn make_double_buf() -> Vec<u8> {
    vec![0u8; DOUBLE_SCENE_SIZE]
}

#[test]
fn double_writer_initial_state() {
    let mut buf = make_double_buf();
    let dw = DoubleWriter::new(&mut buf);
    // Both buffers start at generation 0.
    assert_eq!(dw.front_generation(), 0);
}

#[test]
fn double_writer_first_frame() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    // Write to back buffer.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 800;
        w.set_root(root);
    }
    // Swap makes back the new front.
    dw.swap();
    assert_eq!(dw.front_generation(), 1);
    assert_eq!(dw.front_nodes().len(), 1);
    assert_eq!(dw.front_nodes()[0].width, 800);
}

#[test]
fn double_writer_alternates_buffers() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    // Frame 1: write "first".
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).content = make_mono_text(&mut w, b"first", 16, Color::rgb(255, 255, 255), 8);
        w.set_root(n);
    }
    dw.swap();
    assert_eq!(dw.front_generation(), 1);
    // Frame 2: write "second" to the OTHER buffer.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).content = make_mono_text(&mut w, b"second", 16, Color::rgb(255, 255, 255), 8);
        w.set_root(n);
    }
    dw.swap();
    assert_eq!(dw.front_generation(), 2);
    // Front now has "second".
    match dw.front_nodes()[0].content {
        Content::Text { runs, .. } => {
            let text_runs = dw.front_text_runs(runs);
            assert_eq!(dw.front_data(text_runs[0].glyphs), b"second");
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn double_writer_old_front_becomes_back() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    // Frame 1.
    {
        let mut w = dw.back();
        w.clear();
        w.alloc_node().unwrap();
        w.set_root(0);
    }
    dw.swap(); // buf 0 = gen 1, buf 1 = gen 0
               // Frame 2 should write to buf 1 (gen 0 = back).
    {
        let mut w = dw.back();
        w.clear();
        let n0 = w.alloc_node().unwrap();
        let n1 = w.alloc_node().unwrap();
        w.set_root(n0);
        w.add_child(n0, n1);
    }
    dw.swap(); // buf 1 = gen 2, buf 0 = gen 1
               // Front is buf 1 with 2 nodes.
    assert_eq!(dw.front_nodes().len(), 2);
    assert_eq!(dw.front_generation(), 2);
}

#[test]
fn double_writer_many_frames() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    for i in 0u32..20 {
        {
            let mut w = dw.back();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = (i + 1) as u16;
            w.set_root(n);
        }
        dw.swap();
        assert_eq!(dw.front_generation(), i + 1);
        assert_eq!(dw.front_nodes()[0].width, (i + 1) as u16);
    }
}

#[test]
fn double_reader_reads_front() {
    let mut buf = make_double_buf();
    {
        let mut dw = DoubleWriter::new(&mut buf);
        {
            let mut w = dw.back();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).content =
                make_mono_text(&mut w, b"visible", 14, Color::rgb(200, 200, 200), 8);
            w.set_root(n);
        }
        dw.swap();
    }
    // Read-only access.
    let dr = DoubleReader::new(&buf);
    assert_eq!(dr.front_generation(), 1);
    assert_eq!(dr.front_nodes().len(), 1);
    match dr.front_nodes()[0].content {
        Content::Text { runs, .. } => {
            let text_runs = dr.front_text_runs(runs);
            assert_eq!(dr.front_data(text_runs[0].glyphs), b"visible");
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn double_reader_sees_latest_after_two_swaps() {
    let mut buf = make_double_buf();
    {
        let mut dw = DoubleWriter::new(&mut buf);
        // Frame 1.
        {
            let mut w = dw.back();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 100;
            w.set_root(n);
        }
        dw.swap();
        // Frame 2.
        {
            let mut w = dw.back();
            w.clear();
            let n = w.alloc_node().unwrap();
            w.node_mut(n).width = 200;
            w.set_root(n);
        }
        dw.swap();
    }
    let dr = DoubleReader::new(&buf);
    assert_eq!(dr.front_nodes()[0].width, 200);
    assert_eq!(dr.front_generation(), 2);
}

#[test]
fn double_writer_front_data_resolves_refs() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).content = make_mono_text(&mut w, b"hello world", 16, Color::rgb(0, 0, 0), 8);
        w.set_root(n);
    }
    dw.swap();
    match dw.front_nodes()[0].content {
        Content::Text { runs, .. } => {
            let text_runs = dw.front_text_runs(runs);
            assert_eq!(dw.front_data(text_runs[0].glyphs), b"hello world");
        }
        _ => panic!("expected text"),
    }
}

#[test]
fn double_writer_back_does_not_corrupt_front() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);
    // Frame 1: commit a scene.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).width = 111;
        w.set_root(n);
    }
    dw.swap();
    // Start writing frame 2 to back buffer but DON'T swap.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).width = 222;
        w.set_root(n);
    }
    // Front still shows frame 1.
    assert_eq!(dw.front_nodes()[0].width, 111);
    assert_eq!(dw.front_generation(), 1);
}

// ── Monospace layout ────────────────────────────────────────────────

const WHITE: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};

#[test]
fn layout_mono_basic_lines() {
    let text = b"hello\nworld";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].y, 0);
    assert_eq!(runs[1].y, 20);
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"hello");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"world");
}

#[test]
fn layout_mono_trailing_newline() {
    let text = b"hello\nworld\n";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    // Trailing newline: "hello", "world" — newline consumed, no empty line after.
    assert_eq!(runs.len(), 2);
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"hello");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"world");
}

#[test]
fn layout_mono_soft_wrap() {
    let text = b"abcdefghij"; // 10 chars, wrap at 4
    let runs = layout_mono_lines(text, 4, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 3); // "abcd", "efgh", "ij"
    assert_eq!(line_bytes_for_run(text, &runs[0]), b"abcd");
    assert_eq!(line_bytes_for_run(text, &runs[1]), b"efgh");
    assert_eq!(line_bytes_for_run(text, &runs[2]), b"ij");
    assert_eq!(runs[2].y, 40);
}

#[test]
fn layout_mono_empty_text() {
    let runs = layout_mono_lines(b"", 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].glyph_count, 0);
}

#[test]
fn byte_to_line_col_basic() {
    let text = b"hello\nworld";
    // Byte 0 = (0, 0), byte 5 = end of "hello" = (0, 5)
    assert_eq!(byte_to_line_col(text, 0, 80), (0, 0));
    assert_eq!(byte_to_line_col(text, 5, 80), (0, 5));
    // Byte 6 = start of "world" = (1, 0)
    assert_eq!(byte_to_line_col(text, 6, 80), (1, 0));
    // Byte 11 = end of "world" = (1, 5)
    assert_eq!(byte_to_line_col(text, 11, 80), (1, 5));
}

#[test]
fn byte_to_line_col_soft_wrap() {
    let text = b"abcdefgh"; // 8 chars, wrap at 4
                            // "abcd" is line 0, "efgh" is line 1
    assert_eq!(byte_to_line_col(text, 0, 4), (0, 0));
    assert_eq!(byte_to_line_col(text, 3, 4), (0, 3));
    assert_eq!(byte_to_line_col(text, 4, 4), (1, 0));
    assert_eq!(byte_to_line_col(text, 7, 4), (1, 3));
}

// ── Scroll filtering ────────────────────────────────────────────────

#[test]
fn scroll_runs_no_scroll() {
    let text = b"a\nb\nc";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 3);
    let visible = scroll_runs(runs, 0, 20, 100);
    assert_eq!(visible.len(), 3);
    assert_eq!(visible[0].y, 0);
    assert_eq!(visible[1].y, 20);
    assert_eq!(visible[2].y, 40);
}

#[test]
fn scroll_runs_filters_above_viewport() {
    // 10 lines of text (no trailing newline), scroll = 5, viewport = 60px.
    let mut text = Vec::new();
    for i in 0u8..10 {
        if i > 0 {
            text.push(b'\n');
        }
        text.push(b'a' + i);
    }
    let runs = layout_mono_lines(&text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 10);
    let visible = scroll_runs(runs, 5, 20, 60);
    // Lines 5, 6, 7 visible (y = 0, 20, 40). Lines 0-4 above, 8-9 below.
    assert_eq!(visible.len(), 3);
    assert_eq!(visible[0].y, 0);
    assert_eq!(visible[1].y, 20);
    assert_eq!(visible[2].y, 40);
    assert_eq!(line_bytes_for_run(&text, &visible[0]), &[b'f']); // line 5 = 'f'
}

#[test]
fn scroll_runs_cursor_at_bottom_forces_scroll() {
    // 40 lines, viewport 30 lines, scroll = 6.
    let mut text = Vec::new();
    for i in 0u8..40 {
        if i > 0 {
            text.push(b'\n');
        }
        text.push(b'x');
    }
    let runs = layout_mono_lines(&text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 40);
    let visible = scroll_runs(runs, 6, 20, 600); // 600px = 30 lines
                                                 // First visible line should be line 6 at y=0.
    assert_eq!(visible[0].y, 0);
    // Last visible line should be line 35 at y = 29*20 = 580.
    let last = visible.last().unwrap();
    assert_eq!(last.y, (visible.len() as i16 - 1) * 20);
    // Nothing should have y >= 600 (viewport height).
    for run in &visible {
        assert!(run.y < 600, "run.y={} exceeds viewport", run.y);
    }
}

#[test]
fn scroll_runs_empty_text_with_scroll() {
    let runs = layout_mono_lines(b"", 80, 20, WHITE, 8, 16);
    let visible = scroll_runs(runs, 0, 20, 600);
    assert_eq!(visible.len(), 1); // empty placeholder run
}

#[test]
fn byte_to_line_col_cursor_consistency_with_layout() {
    let text = b"aaa\nbbb\nccc\nddd";
    let runs = layout_mono_lines(text, 80, 20, WHITE, 8, 16);
    assert_eq!(runs.len(), 4);
    // byte_to_line_col should agree with layout_mono_lines on line assignments.
    assert_eq!(byte_to_line_col(text, 0, 80).0, 0); // 'a' on line 0
    assert_eq!(byte_to_line_col(text, 4, 80).0, 1); // 'b' on line 1
    assert_eq!(byte_to_line_col(text, 8, 80).0, 2); // 'c' on line 2
    assert_eq!(byte_to_line_col(text, 12, 80).0, 3); // 'd' on line 3
    assert_eq!(byte_to_line_col(text, 15, 80).0, 3); // end of text, still line 3
}
