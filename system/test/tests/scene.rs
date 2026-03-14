use scene::*;

fn make_buf() -> Vec<u8> {
    vec![0u8; SCENE_SIZE]
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
    let dref = w.push_data(text_data);
    let id = w.alloc_node().unwrap();
    {
        let n = w.node_mut(id);
        n.content = Content::Text {
            data: dref,
            font_size: 18,
            color: Color::rgb(220, 220, 220),
            cursor: 5,
            sel_start: 0,
            sel_end: 0,
        };
    }
    // Read back via SceneReader.
    let r = SceneReader::new(&buf);
    let node = r.node(id);
    match node.content {
        Content::Text { data, font_size, cursor, .. } => {
            assert_eq!(font_size, 18);
            assert_eq!(cursor, 5);
            assert_eq!(r.data(data), text_data);
        }
        _ => panic!("expected Text content"),
    }
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
        Content::Image { data, src_width, src_height } => {
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
        let dref = w.push_data(b"content");
        w.node_mut(child).content = Content::Text {
            data: dref,
            font_size: 16,
            color: Color::rgb(200, 200, 200),
            cursor: u32::MAX,
            sel_start: 0,
            sel_end: 0,
        };
        w.set_root(root);
        w.commit();
    }
    let r = SceneReader::new(&buf);
    assert_eq!(r.generation(), 1);
    assert_eq!(r.node_count(), 2);
    assert_eq!(r.root(), 0);
    assert_eq!(r.node(0).width, 800);
    assert_eq!(r.node(0).first_child, 1);
    assert_eq!(r.data(match r.node(1).content {
        Content::Text { data, .. } => data,
        _ => panic!("expected text"),
    }), b"content");
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
    let bad = DataRef { offset: 9999, length: 100 };
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
    let title_text_data = w.push_data(b"Text");
    let title_text = w.alloc_node().unwrap();
    w.node_mut(title_text).x = 12;
    w.node_mut(title_text).y = 8;
    w.node_mut(title_text).content = Content::Text {
        data: title_text_data,
        font_size: 18,
        color: Color::rgb(200, 200, 200),
        cursor: u32::MAX,
        sel_start: 0,
        sel_end: 0,
    };
    w.add_child(title, title_text);

    // Content area.
    let content = w.alloc_node().unwrap();
    w.node_mut(content).y = 36;
    w.node_mut(content).width = 1024;
    w.node_mut(content).height = 732;
    w.node_mut(content).flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    w.add_child(root, content);

    // Document text.
    let doc_data = w.push_data(b"Hello, world!\nThis is a test.");
    let doc_text = w.alloc_node().unwrap();
    w.node_mut(doc_text).x = 12;
    w.node_mut(doc_text).y = 8;
    w.node_mut(doc_text).width = 1000;
    w.node_mut(doc_text).height = u16::MAX;
    w.node_mut(doc_text).content = Content::Text {
        data: doc_data,
        font_size: 18,
        color: Color::rgb(220, 220, 220),
        cursor: 13,
        sel_start: 0,
        sel_end: 0,
    };
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
        Content::Text { data, cursor, .. } => {
            assert_eq!(r.data(data), b"Hello, world!\nThis is a test.");
            assert_eq!(cursor, 13);
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
