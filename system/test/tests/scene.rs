use scene::*;

// ── Local copies of layout helpers (moved from scene to Core) ───────

/// Convert a byte offset to (visual_line, column) with monospace wrapping.
fn byte_to_line_col(text: &[u8], byte_offset: usize, chars_per_line: usize) -> (usize, usize) {
    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut pos: usize = 0;

    while pos < text.len() && pos < byte_offset {
        if text[pos] == b'\n' {
            line += 1;
            col = 0;
            pos += 1;
        } else {
            col += 1;
            pos += 1;

            if col >= chars_per_line && pos < text.len() && text[pos] != b'\n' {
                line += 1;
                col = 0;
            }
        }
    }

    (line, col)
}

/// Break text into visual lines using monospace line-breaking.
fn layout_mono_lines(
    text: &[u8],
    chars_per_line: usize,
    line_height: i16,
    color: Color,
    advance: u16,
    font_size: u16,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut line_y: i16 = 0;
    let mut pos: usize = 0;

    while pos < text.len() {
        let remaining = &text[pos..];
        let line_end = if let Some(nl) = remaining.iter().position(|&b| b == b'\n') {
            if nl <= chars_per_line {
                pos + nl
            } else {
                pos + chars_per_line
            }
        } else if remaining.len() <= chars_per_line {
            text.len()
        } else {
            pos + chars_per_line
        };
        let line_len = line_end - pos;

        runs.push(TextRun {
            glyphs: DataRef {
                offset: pos as u32,
                length: line_len as u32,
            },
            glyph_count: line_len as u16,
            x: 0,
            y: line_y,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });

        line_y = line_y.saturating_add(line_height);
        pos = if line_end < text.len() && text[line_end] == b'\n' {
            line_end + 1
        } else {
            line_end
        };
    }

    if runs.is_empty() {
        runs.push(TextRun {
            glyphs: DataRef {
                offset: 0,
                length: 0,
            },
            glyph_count: 0,
            x: 0,
            y: 0,
            color,
            advance,
            font_size,
            axis_hash: 0,
        });
    }

    runs
}

/// Extract source text bytes for a run using its placeholder DataRef.
fn line_bytes_for_run<'a>(text: &'a [u8], run: &TextRun) -> &'a [u8] {
    let start = run.glyphs.offset as usize;
    let len = run.glyphs.length as usize;

    if start + len <= text.len() {
        &text[start..start + len]
    } else {
        &[]
    }
}

/// Filter and reposition runs for a scrolled viewport.
fn scroll_runs(
    runs: Vec<TextRun>,
    scroll_lines: u32,
    line_height: u32,
    viewport_height_px: i32,
) -> Vec<TextRun> {
    let scroll_px = scroll_lines as i32 * line_height as i32;

    runs.into_iter()
        .filter_map(|mut run| {
            let adjusted_y = run.y as i32 - scroll_px;

            if adjusted_y + line_height as i32 <= 0 {
                return None;
            }
            if adjusted_y >= viewport_height_px {
                return None;
            }

            run.y = adjusted_y as i16;

            Some(run)
        })
        .collect()
}

/// Convert raw ASCII text bytes into ShapedGlyph arrays for monospace rendering.
fn bytes_to_shaped_glyphs(text: &[u8], advance: u16) -> Vec<ShapedGlyph> {
    text.iter()
        .map(|&ch| ShapedGlyph {
            glyph_id: ch as u16,
            x_advance: advance as i16,
            x_offset: 0,
            y_offset: 0,
        })
        .collect()
}

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
        axis_hash: 0,
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
            assert_eq!(text_runs[0].axis_hash, 0);
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
            axis_hash: 0,
        },
        TextRun {
            glyphs: d2,
            glyph_count: 5,
            x: 0,
            y: 18,
            color: Color::rgb(200, 200, 200),
            advance: 8,
            font_size: 16,
            axis_hash: 0,
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
        axis_hash: 0,
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
    assert_eq!(read_runs[0].axis_hash, 0);
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

// ── ShapedGlyph struct layout (VAL-SCENE-004) ───────────────────────

#[test]
fn shaped_glyph_is_repr_c_with_size_assertion() {
    // ShapedGlyph must be #[repr(C)] with a compile-time size assertion.
    // The compile-time assertion is in scene/lib.rs itself; this test
    // verifies the runtime size matches expectations.
    let size = core::mem::size_of::<ShapedGlyph>();
    assert_eq!(size, 8, "ShapedGlyph should be 8 bytes: u16 + i16 + i16 + i16");
}

#[test]
fn shaped_glyph_field_access() {
    let g = ShapedGlyph {
        glyph_id: 42,
        x_advance: 600,
        x_offset: -10,
        y_offset: 5,
    };
    assert_eq!(g.glyph_id, 42);
    assert_eq!(g.x_advance, 600);
    assert_eq!(g.x_offset, -10);
    assert_eq!(g.y_offset, 5);
}

// ── ShapedGlyph round-trip via SceneWriter/SceneReader (VAL-SCENE-001) ──

#[test]
fn shaped_glyph_single_run_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [
        ShapedGlyph { glyph_id: 72, x_advance: 600, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 101, x_advance: 550, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 108, x_advance: 250, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 108, x_advance: 250, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 111, x_advance: 560, x_offset: 0, y_offset: 0 },
    ];
    let dref = w.push_shaped_glyphs(&glyphs);

    let run = TextRun {
        glyphs: dref,
        glyph_count: glyphs.len() as u16,
        x: 10,
        y: 20,
        color: Color::rgb(220, 220, 220),
        advance: 0, // 0 means per-glyph advances in ShapedGlyph
        font_size: 18,
        axis_hash: 0,
    };
    let (runs_ref, count) = w.push_text_runs(&[run]);

    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text { runs: runs_ref, run_count: count, _pad: [0; 2] };
    w.set_root(id);
    w.commit();

    // Read back
    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 1);
    assert_eq!(text_runs[0].glyph_count, 5);
    assert_eq!(text_runs[0].advance, 0);

    let read_glyphs = r.shaped_glyphs(text_runs[0].glyphs, text_runs[0].glyph_count);
    assert_eq!(read_glyphs.len(), 5);
    for (orig, read) in glyphs.iter().zip(read_glyphs.iter()) {
        assert_eq!(orig.glyph_id, read.glyph_id);
        assert_eq!(orig.x_advance, read.x_advance);
        assert_eq!(orig.x_offset, read.x_offset);
        assert_eq!(orig.y_offset, read.y_offset);
    }
}

// ── Multiple text nodes with varying glyph counts (VAL-SCENE-002) ──

#[test]
fn shaped_glyph_five_nodes_varying_counts_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // 5 text nodes with varying glyph counts: 1, 3, 7, 12, 20
    let glyph_counts = [1, 3, 7, 12, 20];
    let mut node_ids = Vec::new();
    let mut expected: Vec<Vec<ShapedGlyph>> = Vec::new();

    let root = w.alloc_node().unwrap();
    w.set_root(root);

    for (idx, &count) in glyph_counts.iter().enumerate() {
        let glyphs: Vec<ShapedGlyph> = (0..count).map(|i| ShapedGlyph {
            glyph_id: (idx as u16 * 100) + i as u16,
            x_advance: 500 + (i as i16 * 10),
            x_offset: if i % 2 == 0 { 0 } else { -(i as i16) },
            y_offset: if i % 3 == 0 { 2 } else { 0 },
        }).collect();

        let dref = w.push_shaped_glyphs(&glyphs);
        let run = TextRun {
            glyphs: dref,
            glyph_count: count as u16,
            x: 0,
            y: (idx as i16) * 20,
            color: Color::rgb(200, 200, 200),
            advance: 0,
            font_size: 16,
            axis_hash: 0,
        };
        let (runs_ref, run_count) = w.push_text_runs(&[run]);
        let nid = w.alloc_node().unwrap();
        w.node_mut(nid).content = Content::Text { runs: runs_ref, run_count, _pad: [0; 2] };
        w.add_child(root, nid);
        node_ids.push(nid);
        expected.push(glyphs);
    }
    w.commit();

    // Read back all 5 nodes
    let r = SceneReader::new(&buf);
    for (idx, &nid) in node_ids.iter().enumerate() {
        match r.node(nid).content {
            Content::Text { runs, run_count, .. } => {
                assert_eq!(run_count, 1);
                let text_runs = r.text_runs(runs);
                let read_glyphs = r.shaped_glyphs(text_runs[0].glyphs, text_runs[0].glyph_count);
                assert_eq!(read_glyphs.len(), expected[idx].len(),
                    "Node {} glyph count mismatch", idx);
                for (j, (orig, read)) in expected[idx].iter().zip(read_glyphs.iter()).enumerate() {
                    assert_eq!(orig.glyph_id, read.glyph_id,
                        "Node {} glyph {} glyph_id mismatch", idx, j);
                    assert_eq!(orig.x_advance, read.x_advance,
                        "Node {} glyph {} x_advance mismatch", idx, j);
                    assert_eq!(orig.x_offset, read.x_offset,
                        "Node {} glyph {} x_offset mismatch", idx, j);
                    assert_eq!(orig.y_offset, read.y_offset,
                        "Node {} glyph {} y_offset mismatch", idx, j);
                }
            }
            _ => panic!("Node {} expected Text content", idx),
        }
    }
}

// ── Boundary glyph IDs (VAL-SCENE-002, VAL-CROSS-005) ──────────────

#[test]
fn shaped_glyph_boundary_ids_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [
        ShapedGlyph { glyph_id: 0, x_advance: 100, x_offset: 0, y_offset: 0 },      // .notdef
        ShapedGlyph { glyph_id: 1, x_advance: 200, x_offset: -5, y_offset: 3 },      // first real glyph
        ShapedGlyph { glyph_id: 65534, x_advance: 300, x_offset: 10, y_offset: -10 }, // near max u16
    ];
    let dref = w.push_shaped_glyphs(&glyphs);
    let run = TextRun {
        glyphs: dref,
        glyph_count: 3,
        x: 0, y: 0,
        color: Color::rgb(255, 255, 255),
        advance: 0,
        font_size: 18,
        axis_hash: 0,
    };
    let (runs_ref, count) = w.push_text_runs(&[run]);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text { runs: runs_ref, run_count: count, _pad: [0; 2] };
    w.commit();

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    let read_glyphs = r.shaped_glyphs(text_runs[0].glyphs, text_runs[0].glyph_count);

    assert_eq!(read_glyphs.len(), 3);
    assert_eq!(read_glyphs[0].glyph_id, 0);
    assert_eq!(read_glyphs[0].x_advance, 100);
    assert_eq!(read_glyphs[1].glyph_id, 1);
    assert_eq!(read_glyphs[1].x_offset, -5);
    assert_eq!(read_glyphs[1].y_offset, 3);
    assert_eq!(read_glyphs[2].glyph_id, 65534);
    assert_eq!(read_glyphs[2].x_advance, 300);
    assert_eq!(read_glyphs[2].x_offset, 10);
    assert_eq!(read_glyphs[2].y_offset, -10);
}

// ── Data buffer capacity (VAL-SCENE-003) ────────────────────────────

#[test]
fn shaped_glyph_2000_entries_fit_in_64k_data_buffer() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Create 2000+ glyphs across multiple runs
    let total_glyphs = 2100;
    let glyphs_per_run = 100;
    let num_runs = total_glyphs / glyphs_per_run;
    let mut total_pushed = 0u32;

    let root = w.alloc_node().unwrap();
    w.set_root(root);

    for run_idx in 0..num_runs {
        let glyphs: Vec<ShapedGlyph> = (0..glyphs_per_run).map(|i| ShapedGlyph {
            glyph_id: ((run_idx * glyphs_per_run + i) % 65535) as u16,
            x_advance: 500,
            x_offset: 0,
            y_offset: 0,
        }).collect();

        let dref = w.push_shaped_glyphs(&glyphs);
        // Verify push_shaped_glyphs succeeded with full data
        assert_eq!(
            dref.length as usize,
            glyphs_per_run * core::mem::size_of::<ShapedGlyph>(),
            "Run {} data truncated — buffer overflow", run_idx
        );
        total_pushed += dref.length;

        let run = TextRun {
            glyphs: dref,
            glyph_count: glyphs_per_run as u16,
            x: 0,
            y: (run_idx as i16) * 20,
            color: Color::rgb(200, 200, 200),
            advance: 0,
            font_size: 16,
            axis_hash: 0,
        };
        let (runs_ref, count) = w.push_text_runs(&[run]);
        let nid = w.alloc_node().unwrap();
        w.node_mut(nid).content = Content::Text { runs: runs_ref, run_count: count, _pad: [0; 2] };
        w.add_child(root, nid);
    }

    // Verify total data fits within 64 KiB
    assert!(w.data_used() <= DATA_BUFFER_SIZE as u32,
        "Data used {} exceeds buffer size {}", w.data_used(), DATA_BUFFER_SIZE);
    // At 8 bytes per glyph, 2100 glyphs = 16800 bytes, well within 64 KiB
    assert!(total_pushed >= (total_glyphs * core::mem::size_of::<ShapedGlyph>()) as u32,
        "Not all glyph data was pushed");
}

// ── Byte-exact equality round-trip ──────────────────────────────────

#[test]
fn shaped_glyph_byte_exact_round_trip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [
        ShapedGlyph { glyph_id: 0xABCD, x_advance: -32000, x_offset: 32000, y_offset: -1 },
        ShapedGlyph { glyph_id: 0x0001, x_advance: 1, x_offset: -1, y_offset: 0 },
        ShapedGlyph { glyph_id: 0xFFFE, x_advance: 0, x_offset: 0, y_offset: 0 },
    ];

    // Get raw bytes of the input
    let input_bytes = unsafe {
        core::slice::from_raw_parts(
            glyphs.as_ptr() as *const u8,
            glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
        )
    };

    let dref = w.push_shaped_glyphs(&glyphs);

    // Read raw bytes back from data buffer
    let r = SceneReader::new(&buf);
    let output_bytes = r.data(dref);

    assert_eq!(input_bytes, output_bytes, "Byte-exact round-trip failed");
}

// ── Monospace text still works alongside shaped text ────────────────

#[test]
fn mono_and_shaped_text_coexist() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Monospace text run (advance > 0, raw bytes)
    let mono_text = b"Hello";
    let mono_data = w.push_data(mono_text);
    let mono_run = TextRun {
        glyphs: mono_data,
        glyph_count: 5,
        x: 0, y: 0,
        color: Color::rgb(200, 200, 200),
        advance: 8, // > 0 means monospace
        font_size: 16,
        axis_hash: 0,
    };

    // Shaped text run (advance == 0, ShapedGlyph array)
    let shaped = [
        ShapedGlyph { glyph_id: 72, x_advance: 600, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 101, x_advance: 550, x_offset: 0, y_offset: 0 },
    ];
    let shaped_ref = w.push_shaped_glyphs(&shaped);
    let shaped_run = TextRun {
        glyphs: shaped_ref,
        glyph_count: 2,
        x: 0, y: 20,
        color: Color::rgb(200, 200, 200),
        advance: 0, // shaped
        font_size: 18,
        axis_hash: 0,
    };

    let (runs_ref, count) = w.push_text_runs(&[mono_run, shaped_run]);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text { runs: runs_ref, run_count: count, _pad: [0; 2] };

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 2);

    // Mono run: raw bytes
    assert_eq!(text_runs[0].advance, 8);
    assert_eq!(r.data(text_runs[0].glyphs), b"Hello");

    // Shaped run: ShapedGlyph array
    assert_eq!(text_runs[1].advance, 0);
    let glyphs = r.shaped_glyphs(text_runs[1].glyphs, text_runs[1].glyph_count);
    assert_eq!(glyphs.len(), 2);
    assert_eq!(glyphs[0].glyph_id, 72);
    assert_eq!(glyphs[1].glyph_id, 101);
}

// ── Glyph ID boundary values survive scene graph round-trip ─────────

#[test]
fn shaped_glyph_boundary_ids_roundtrip() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Boundary glyph IDs: 0 (.notdef), 1 (first real glyph), 65534 (near u16::MAX)
    let glyphs = [
        ShapedGlyph { glyph_id: 0, x_advance: 100, x_offset: 0, y_offset: 0 },
        ShapedGlyph { glyph_id: 1, x_advance: 200, x_offset: -5, y_offset: 10 },
        ShapedGlyph { glyph_id: 65534, x_advance: 300, x_offset: 50, y_offset: -20 },
    ];
    let dref = w.push_shaped_glyphs(&glyphs);
    let run = TextRun {
        glyphs: dref,
        glyph_count: 3,
        x: 0, y: 0,
        color: Color::rgb(255, 255, 255),
        advance: 0,
        font_size: 18,
        axis_hash: 0,
    };
    let (runs_ref, _count) = w.push_text_runs(&[run]);

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 1);

    let read_glyphs = r.shaped_glyphs(text_runs[0].glyphs, text_runs[0].glyph_count);
    assert_eq!(read_glyphs.len(), 3);

    // Verify each boundary glyph ID survives exactly
    assert_eq!(read_glyphs[0].glyph_id, 0);
    assert_eq!(read_glyphs[0].x_advance, 100);
    assert_eq!(read_glyphs[1].glyph_id, 1);
    assert_eq!(read_glyphs[1].x_advance, 200);
    assert_eq!(read_glyphs[1].x_offset, -5);
    assert_eq!(read_glyphs[1].y_offset, 10);
    assert_eq!(read_glyphs[2].glyph_id, 65534);
    assert_eq!(read_glyphs[2].x_advance, 300);
    assert_eq!(read_glyphs[2].x_offset, 50);
    assert_eq!(read_glyphs[2].y_offset, -20);
}

// ── Monospace shaping: identical x_advance for all glyphs ───────────

#[test]
fn monospace_shaped_glyphs_uniform_advance() {
    // Simulate what bytes_to_shaped_glyphs does in core: each byte becomes
    // a ShapedGlyph with glyph_id = byte value and uniform advance.
    let text = b"iiiWWW";
    let advance: i16 = 10; // monospace uniform advance
    let glyphs: Vec<ShapedGlyph> = text.iter().map(|&ch| ShapedGlyph {
        glyph_id: ch as u16,
        x_advance: advance,
        x_offset: 0,
        y_offset: 0,
    }).collect();

    // All 6 glyphs should have identical x_advance
    assert_eq!(glyphs.len(), 6);
    for g in &glyphs {
        assert_eq!(g.x_advance, advance, "Monospace glyphs must have uniform advance");
    }

    // Push through scene graph and verify round-trip
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let dref = w.push_shaped_glyphs(&glyphs);

    let r = SceneReader::new(&buf);
    let read = r.shaped_glyphs(dref, glyphs.len() as u16);
    assert_eq!(read.len(), 6);
    for g in read {
        assert_eq!(g.x_advance, advance);
    }
}

// ── Proportional advance: W != i ────────────────────────────────────

#[test]
fn proportional_shaped_glyphs_different_advances() {
    let mono_font = include_bytes!("../../share/source-code-pro.ttf");

    // Shape 'W' and 'i' separately to verify mono font gives same advance
    let w_shaped = fonts::shape(mono_font, "W", &[]);
    let i_shaped = fonts::shape(mono_font, "i", &[]);

    assert!(!w_shaped.is_empty(), "W should produce glyphs");
    assert!(!i_shaped.is_empty(), "i should produce glyphs");

    // For a monospace font, W and i should have the same advance
    assert_eq!(
        w_shaped[0].x_advance, i_shaped[0].x_advance,
        "Monospace font: W and i must have same advance"
    );

    // For a proportional font (if available), they'd differ
    let prop_font = include_bytes!("../../share/nunito-sans.ttf");
    let w_prop = fonts::shape(prop_font, "W", &[]);
    let i_prop = fonts::shape(prop_font, "i", &[]);

    if !w_prop.is_empty() && !i_prop.is_empty() {
        // Proportional font: W should have a wider advance than i
        assert_ne!(
            w_prop[0].x_advance, i_prop[0].x_advance,
            "Proportional font: advance('W') != advance('i')"
        );
    }
}

// ---------------------------------------------------------------------------
// VAL-CROSS-002: Axis values flow through scene graph (axis_hash round-trip)
// ---------------------------------------------------------------------------

#[test]
fn scene_text_run_axis_hash_round_trip() {
    let mut buf = vec![0u8; SCENE_SIZE];
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [ShapedGlyph {
        glyph_id: 72,
        x_advance: 10,
        x_offset: 0,
        y_offset: 0,
    }];
    let glyph_ref = w.push_shaped_glyphs(&glyphs);

    // Create a TextRun with non-zero axis_hash (simulating wght=700).
    let axis_hash_700 = 0xABCD_1234u32;
    let run = TextRun {
        glyphs: glyph_ref,
        glyph_count: 1,
        x: 0,
        y: 0,
        color: Color::rgb(255, 255, 255),
        advance: 10,
        font_size: 18,
        axis_hash: axis_hash_700,
    };
    let (runs_ref, count) = w.push_text_runs(&[run]);

    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text {
        runs: runs_ref,
        run_count: count,
        _pad: [0; 2],
    };
    w.set_root(id);
    w.commit();

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 1);
    assert_eq!(
        text_runs[0].axis_hash, axis_hash_700,
        "axis_hash must round-trip through scene graph"
    );
}

#[test]
fn scene_text_run_different_axis_hashes_preserved() {
    let mut buf = vec![0u8; SCENE_SIZE];
    let mut w = SceneWriter::new(&mut buf);

    let glyphs = [ShapedGlyph {
        glyph_id: 65,
        x_advance: 10,
        x_offset: 0,
        y_offset: 0,
    }];

    // Two runs with different axis hashes.
    let glyph_ref1 = w.push_shaped_glyphs(&glyphs);
    let glyph_ref2 = w.push_shaped_glyphs(&glyphs);

    let run_400 = TextRun {
        glyphs: glyph_ref1,
        glyph_count: 1,
        x: 0,
        y: 0,
        color: Color::rgb(255, 255, 255),
        advance: 10,
        font_size: 18,
        axis_hash: 0x1111_0000,
    };
    let run_700 = TextRun {
        glyphs: glyph_ref2,
        glyph_count: 1,
        x: 0,
        y: 20,
        color: Color::rgb(255, 255, 255),
        advance: 10,
        font_size: 18,
        axis_hash: 0x2222_0000,
    };
    let (runs_ref, count) = w.push_text_runs(&[run_400, run_700]);

    let id = w.alloc_node().unwrap();
    w.node_mut(id).content = Content::Text {
        runs: runs_ref,
        run_count: count,
        _pad: [0; 2],
    };
    w.set_root(id);
    w.commit();

    let r = SceneReader::new(&buf);
    let text_runs = r.text_runs(runs_ref);
    assert_eq!(text_runs.len(), 2);
    assert_eq!(text_runs[0].axis_hash, 0x1111_0000);
    assert_eq!(text_runs[1].axis_hash, 0x2222_0000);
}

// ── content_hash tests ──────────────────────────────────────────────

#[test]
fn content_hash_is_zero_for_empty_node() {
    assert_eq!(Node::EMPTY.content_hash, 0);
}

#[test]
fn content_hash_stored_and_readable() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    w.node_mut(id).content_hash = scene::fnv1a(b"hello");
    w.set_root(id);
    w.commit();
    let r = SceneReader::new(&buf);
    assert_eq!(r.node(id).content_hash, scene::fnv1a(b"hello"));
}

#[test]
fn content_hash_differs_for_different_data() {
    let h1 = scene::fnv1a(b"hello");
    let h2 = scene::fnv1a(b"world");
    assert_ne!(h1, h2);
    assert_ne!(h1, 0);
}

#[test]
fn content_hash_is_deterministic() {
    assert_eq!(scene::fnv1a(b"test"), scene::fnv1a(b"test"));
}

// ── scene diffing tests ─────────────────────────────────────────────

#[test]
fn diff_identical_scenes_returns_empty() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 100;
    w.node_mut(root).height = 50;
    w.node_mut(root).background = Color::rgb(30, 30, 30);
    w.set_root(root);
    w.commit();
    let nodes = w.nodes();
    let count = w.node_count() as usize;
    let rects = scene::diff_scenes(nodes, count, nodes, count);
    assert!(rects.is_some());
    assert!(rects.unwrap().is_empty());
}

#[test]
fn diff_different_node_count_returns_none() {
    let mut buf1 = make_buf();
    let mut w1 = SceneWriter::new(&mut buf1);
    let _ = w1.alloc_node().unwrap();
    w1.commit();

    let mut buf2 = make_buf();
    let mut w2 = SceneWriter::new(&mut buf2);
    let _ = w2.alloc_node().unwrap();
    let _ = w2.alloc_node().unwrap();
    w2.commit();

    let result = scene::diff_scenes(w1.nodes(), 1, w2.nodes(), 2);
    assert!(result.is_none());
}

#[test]
fn diff_changed_background_returns_dirty_rect() {
    let mut buf1 = make_buf();
    let mut w1 = SceneWriter::new(&mut buf1);
    let root = w1.alloc_node().unwrap();
    w1.node_mut(root).x = 10;
    w1.node_mut(root).y = 20;
    w1.node_mut(root).width = 100;
    w1.node_mut(root).height = 50;
    w1.node_mut(root).background = Color::rgb(30, 30, 30);
    w1.set_root(root);
    w1.commit();

    let mut buf2 = make_buf();
    let mut w2 = SceneWriter::new(&mut buf2);
    let root2 = w2.alloc_node().unwrap();
    w2.node_mut(root2).x = 10;
    w2.node_mut(root2).y = 20;
    w2.node_mut(root2).width = 100;
    w2.node_mut(root2).height = 50;
    w2.node_mut(root2).background = Color::rgb(50, 50, 50); // changed
    w2.set_root(root2);
    w2.commit();

    let rects = scene::diff_scenes(w1.nodes(), 1, w2.nodes(), 1).unwrap();
    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0], (10, 20, 100, 50));
}

#[test]
fn diff_moved_node_returns_old_and_new_rects() {
    let mut buf1 = make_buf();
    let mut w1 = SceneWriter::new(&mut buf1);
    let root = w1.alloc_node().unwrap();
    w1.node_mut(root).x = 10;
    w1.node_mut(root).y = 20;
    w1.node_mut(root).width = 50;
    w1.node_mut(root).height = 30;
    w1.set_root(root);
    w1.commit();

    let mut buf2 = make_buf();
    let mut w2 = SceneWriter::new(&mut buf2);
    let root2 = w2.alloc_node().unwrap();
    w2.node_mut(root2).x = 100; // moved
    w2.node_mut(root2).y = 200; // moved
    w2.node_mut(root2).width = 50;
    w2.node_mut(root2).height = 30;
    w2.set_root(root2);
    w2.commit();

    let rects = scene::diff_scenes(w1.nodes(), 1, w2.nodes(), 1).unwrap();
    // Both old and new positions should be dirty.
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0], (10, 20, 50, 30));
    assert_eq!(rects[1], (100, 200, 50, 30));
}

#[test]
fn diff_content_hash_change_detected() {
    let mut buf1 = make_buf();
    let mut w1 = SceneWriter::new(&mut buf1);
    let root = w1.alloc_node().unwrap();
    w1.node_mut(root).width = 200;
    w1.node_mut(root).height = 100;
    w1.node_mut(root).content_hash = scene::fnv1a(b"hello");
    w1.set_root(root);
    w1.commit();

    let mut buf2 = make_buf();
    let mut w2 = SceneWriter::new(&mut buf2);
    let root2 = w2.alloc_node().unwrap();
    w2.node_mut(root2).width = 200;
    w2.node_mut(root2).height = 100;
    w2.node_mut(root2).content_hash = scene::fnv1a(b"world"); // different content
    w2.set_root(root2);
    w2.commit();

    let rects = scene::diff_scenes(w1.nodes(), 1, w2.nodes(), 1).unwrap();
    assert_eq!(rects.len(), 1, "content_hash change should produce a dirty rect");
}

#[test]
fn diff_child_node_includes_parent_offset() {
    let mut buf1 = make_buf();
    let mut w1 = SceneWriter::new(&mut buf1);
    let root = w1.alloc_node().unwrap();
    w1.node_mut(root).x = 50;
    w1.node_mut(root).y = 100;
    w1.node_mut(root).width = 500;
    w1.node_mut(root).height = 400;
    let child = w1.alloc_node().unwrap();
    w1.node_mut(child).x = 10;
    w1.node_mut(child).y = 20;
    w1.node_mut(child).width = 80;
    w1.node_mut(child).height = 40;
    w1.node_mut(child).background = Color::rgb(255, 0, 0);
    w1.add_child(root, child);
    w1.set_root(root);
    w1.commit();

    let mut buf2 = make_buf();
    let mut w2 = SceneWriter::new(&mut buf2);
    let root2 = w2.alloc_node().unwrap();
    w2.node_mut(root2).x = 50;
    w2.node_mut(root2).y = 100;
    w2.node_mut(root2).width = 500;
    w2.node_mut(root2).height = 400;
    let child2 = w2.alloc_node().unwrap();
    w2.node_mut(child2).x = 10;
    w2.node_mut(child2).y = 20;
    w2.node_mut(child2).width = 80;
    w2.node_mut(child2).height = 40;
    w2.node_mut(child2).background = Color::rgb(0, 255, 0); // changed
    w2.add_child(root2, child2);
    w2.set_root(root2);
    w2.commit();

    let rects = scene::diff_scenes(w1.nodes(), 2, w2.nodes(), 2).unwrap();
    assert_eq!(rects.len(), 1);
    // Child absolute position: parent(50,100) + child(10,20) = (60,120)
    assert_eq!(rects[0], (60, 120, 80, 40));
}

#[test]
fn build_parent_map_basic() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    let child1 = w.alloc_node().unwrap();
    let child2 = w.alloc_node().unwrap();
    w.add_child(root, child1);
    w.add_child(root, child2);
    w.set_root(root);
    w.commit();
    let nodes = w.nodes();
    let parent_map = scene::build_parent_map(nodes, 3);
    assert_eq!(parent_map[root as usize], scene::NULL);
    assert_eq!(parent_map[child1 as usize], root);
    assert_eq!(parent_map[child2 as usize], root);
}

#[test]
fn abs_bounds_nested_three_levels() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.node_mut(root).x = 10;
    w.node_mut(root).y = 20;
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    let mid = w.alloc_node().unwrap();
    w.node_mut(mid).x = 30;
    w.node_mut(mid).y = 40;
    w.node_mut(mid).width = 200;
    w.node_mut(mid).height = 150;
    let leaf = w.alloc_node().unwrap();
    w.node_mut(leaf).x = 5;
    w.node_mut(leaf).y = 10;
    w.node_mut(leaf).width = 50;
    w.node_mut(leaf).height = 25;
    w.add_child(root, mid);
    w.add_child(mid, leaf);
    w.set_root(root);
    w.commit();
    let nodes = w.nodes();
    let parent_map = scene::build_parent_map(nodes, 3);
    // leaf abs: root(10,20) + mid(30,40) + leaf(5,10) = (45, 70)
    let (ax, ay, aw, ah) = scene::abs_bounds(nodes, &parent_map, leaf as usize);
    assert_eq!((ax, ay, aw, ah), (45, 70, 50, 25));
}

// ── Change list and copy-forward tests ──────────────────────────────

// VAL-SCENE-001: copy_front_to_back preserves scene state
#[test]
fn copy_front_to_back_preserves_nodes_and_data() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Build a scene with multiple nodes and data.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.set_root(root);

        let child = w.alloc_node().unwrap();
        w.node_mut(child).x = 10;
        w.node_mut(child).y = 20;
        w.node_mut(child).width = 200;
        w.node_mut(child).height = 100;
        w.node_mut(child).content =
            make_mono_text(&mut w, b"Hello, world!", 16, Color::rgb(220, 220, 220), 8);
        w.add_child(root, child);
    }
    dw.swap(); // front is now gen 1

    // Copy front to back.
    dw.copy_front_to_back();

    // Verify the back buffer matches the front byte-for-byte (nodes + data).
    let front_nodes = dw.front_nodes().to_vec();
    let front_data = dw.front_data_buf().to_vec();

    let back = dw.back();
    let back_nodes = back.nodes();
    let back_data = back.data_buf();

    assert_eq!(front_nodes.len(), back_nodes.len());
    assert_eq!(front_data, back_data);

    let node_size = core::mem::size_of::<Node>();
    for (i, (f, b)) in front_nodes.iter().zip(back_nodes.iter()).enumerate() {
        // SAFETY: Node is repr(C), byte comparison is sound for equality.
        let f_bytes = unsafe {
            core::slice::from_raw_parts(f as *const Node as *const u8, node_size)
        };
        let b_bytes = unsafe {
            core::slice::from_raw_parts(b as *const Node as *const u8, node_size)
        };
        assert_eq!(f_bytes, b_bytes, "Node {} differs after copy_front_to_back", i);
    }
}

// VAL-SCENE-010: Generation counter NOT copied by copy_front_to_back
#[test]
fn copy_front_to_back_preserves_back_generation() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: write and swap.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.node_mut(n).width = 100;
        w.set_root(n);
    }
    dw.swap(); // buf 0 = gen 1, buf 1 = gen 0

    // Front is gen 1. Back is gen 0.
    let front_gen_before = dw.front_generation();
    assert_eq!(front_gen_before, 1);

    // Copy front to back. Back should still have its original generation.
    dw.copy_front_to_back();

    // The front generation should not have changed.
    assert_eq!(dw.front_generation(), 1);

    // The back buffer's generation should be less than the front's (0 < 1).
    // Verify by checking that front is still the same buffer.
    let back = dw.back();
    let back_gen = back.generation();
    assert!(
        back_gen < front_gen_before,
        "back gen {} should be < front gen {}",
        back_gen,
        front_gen_before
    );
}

// VAL-SCENE-004: Change list cleared on new frame (copy_front_to_back)
#[test]
fn copy_front_to_back_resets_change_list() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: build scene with marks.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
        w.mark_changed(0);
    }
    dw.swap();

    // Copy front to back — change list should be empty in back.
    dw.copy_front_to_back();
    {
        let back = dw.back();
        // Back header should have change_count = 0.
        assert_eq!(back.generation(), 0); // back gen preserved
    }
    // Now swap to make back the new front, then verify change list is empty.
    dw.swap();
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list();
    assert!(cl.is_some(), "change list should not be FULL_REPAINT");
    assert_eq!(cl.unwrap().len(), 0, "change list should be empty after copy_front_to_back");
}

// VAL-SCENE-002: Change list records changed node IDs
#[test]
fn mark_changed_records_node_ids() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: initial scene.
    {
        let mut w = dw.back();
        w.clear();
        for _ in 0..8 {
            w.alloc_node().unwrap();
        }
        w.set_root(0);
    }
    dw.swap();

    // Frame 2: copy forward, mark specific nodes.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        w.mark_changed(3); // clock
        w.mark_changed(7); // cursor
    }
    dw.swap();

    // Read the change list from the new front.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list();
    assert!(cl.is_some());
    let changes = cl.unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0], 3);
    assert_eq!(changes[1], 7);
    assert!(!dr.is_full_repaint());
}

// VAL-SCENE-003: Change list is readable by DoubleReader
#[test]
fn double_reader_reads_change_list_from_front() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
    }
    dw.swap();

    // Frame 2: copy-forward + mark one node.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        w.node_mut(0).background = Color::rgb(255, 0, 0);
        w.mark_changed(0);
    }
    dw.swap();

    // Now DoubleReader on the same buffer should see the change list.
    let dr = DoubleReader::new(&buf);
    assert!(!dr.is_full_repaint());
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 1);
    assert_eq!(cl[0], 0);
}

// VAL-SCENE-008: Change list capacity handles full screen update (overflow)
#[test]
fn mark_changed_overflow_sets_full_repaint() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Allocate enough nodes.
    for _ in 0..30 {
        w.alloc_node().unwrap();
    }
    w.set_root(0);

    // Mark more nodes than CHANGE_LIST_CAPACITY (24).
    for i in 0..25 {
        w.mark_changed(i as NodeId);
    }

    // 25th mark should have caused overflow → FULL_REPAINT sentinel.
    let r = SceneReader::new(&buf);
    let hdr = r.node_count(); // just verifying reader works
    assert_eq!(hdr, 30);

    // Read the header directly to check change_count.
    // We need DoubleWriter to test DoubleReader, but we can also verify
    // via the raw header.
    let hdr_ptr = buf.as_ptr() as *const scene::SceneHeader;
    let hdr = unsafe { &*hdr_ptr };
    assert_eq!(hdr.change_count, scene::FULL_REPAINT);
}

// VAL-SCENE-008: overflow via DoubleReader
#[test]
fn double_reader_full_repaint_on_overflow() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: scene with 30 nodes.
    {
        let mut w = dw.back();
        w.clear();
        for _ in 0..30 {
            w.alloc_node().unwrap();
        }
        w.set_root(0);
    }
    dw.swap();

    // Frame 2: copy-forward, mark 25 nodes (overflow).
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        for i in 0..25 {
            w.mark_changed(i as NodeId);
        }
    }
    dw.swap();

    let dr = DoubleReader::new(&buf);
    assert!(dr.is_full_repaint());
    assert!(dr.change_list().is_none());
}

// SceneWriter::clear sets FULL_REPAINT sentinel
#[test]
fn clear_sets_full_repaint() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
    }
    dw.swap();

    // Frame 2: clear (full rebuild) should signal full repaint.
    {
        let mut w = dw.back();
        w.clear();
        let n = w.alloc_node().unwrap();
        w.set_root(n);
    }
    dw.swap();

    let dr = DoubleReader::new(&buf);
    assert!(dr.is_full_repaint());
    assert!(dr.change_list().is_none());
}

// Already-overflowed mark_changed is a no-op
#[test]
fn mark_changed_after_overflow_is_noop() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..30 {
        w.alloc_node().unwrap();
    }

    // Overflow the change list.
    for i in 0..25 {
        w.mark_changed(i as NodeId);
    }

    // Further marks should be a no-op (still FULL_REPAINT, no crash).
    w.mark_changed(29);
    w.mark_changed(0);

    let hdr = unsafe { &*(buf.as_ptr() as *const scene::SceneHeader) };
    assert_eq!(hdr.change_count, scene::FULL_REPAINT);
}

// VAL-SCENE-007: Node mutation via copy-then-mutate preserves tree structure
#[test]
fn copy_then_mutate_preserves_other_nodes() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: build a tree with 8 well-known nodes.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap(); // 0
        w.node_mut(root).width = 1024;
        w.node_mut(root).height = 768;
        w.node_mut(root).background = Color::rgb(30, 30, 30);
        w.set_root(root);

        let title_bar = w.alloc_node().unwrap(); // 1
        w.node_mut(title_bar).width = 1024;
        w.node_mut(title_bar).height = 36;
        w.node_mut(title_bar).background = Color::rgba(20, 20, 20, 200);
        w.add_child(root, title_bar);

        let title_text = w.alloc_node().unwrap(); // 2
        w.node_mut(title_text).x = 12;
        w.node_mut(title_text).y = 8;
        w.add_child(title_bar, title_text);

        let clock_text = w.alloc_node().unwrap(); // 3
        w.node_mut(clock_text).x = 900;
        w.node_mut(clock_text).y = 8;
        w.add_child(title_bar, clock_text);

        let shadow = w.alloc_node().unwrap(); // 4
        w.node_mut(shadow).y = 36;
        w.node_mut(shadow).height = 4;
        w.node_mut(shadow).background = Color::rgba(0, 0, 0, 80);
        w.add_child(root, shadow);

        let content = w.alloc_node().unwrap(); // 5
        w.node_mut(content).y = 40;
        w.node_mut(content).width = 1024;
        w.node_mut(content).height = 728;
        w.add_child(root, content);

        let doc_text = w.alloc_node().unwrap(); // 6
        w.node_mut(doc_text).x = 12;
        w.node_mut(doc_text).y = 8;
        w.add_child(content, doc_text);

        let cursor = w.alloc_node().unwrap(); // 7
        w.node_mut(cursor).x = 12;
        w.node_mut(cursor).y = 8;
        w.node_mut(cursor).width = 2;
        w.node_mut(cursor).height = 20;
        w.node_mut(cursor).background = Color::rgb(200, 200, 200);
        w.add_child(content, cursor);
    }
    dw.swap();

    // Snapshot the front nodes before mutation.
    let front_nodes_before: Vec<Node> = dw.front_nodes().to_vec();

    // Frame 2: copy forward, mutate only cursor (node 7) position.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        w.node_mut(7).x = 100; // moved cursor
        w.node_mut(7).y = 48;  // moved cursor
        w.mark_changed(7);
    }
    dw.swap();

    // Verify all non-mutated nodes are identical.
    let front_nodes_after: Vec<Node> = dw.front_nodes().to_vec();
    assert_eq!(front_nodes_after.len(), 8);

    let node_size = core::mem::size_of::<Node>();
    for i in 0..8 {
        if i == 7 {
            // Cursor should have changed.
            assert_eq!(front_nodes_after[i].x, 100);
            assert_eq!(front_nodes_after[i].y, 48);
            // But tree structure preserved.
            assert_eq!(
                front_nodes_after[i].first_child,
                front_nodes_before[i].first_child
            );
            assert_eq!(
                front_nodes_after[i].next_sibling,
                front_nodes_before[i].next_sibling
            );
            assert_eq!(
                front_nodes_after[i].width,
                front_nodes_before[i].width
            );
            assert_eq!(
                front_nodes_after[i].background,
                front_nodes_before[i].background
            );
        } else {
            // All other nodes unchanged byte-for-byte.
            let before_bytes = unsafe {
                core::slice::from_raw_parts(
                    &front_nodes_before[i] as *const Node as *const u8,
                    node_size,
                )
            };
            let after_bytes = unsafe {
                core::slice::from_raw_parts(
                    &front_nodes_after[i] as *const Node as *const u8,
                    node_size,
                )
            };
            assert_eq!(
                before_bytes, after_bytes,
                "Node {} should be unchanged after mutating only cursor",
                i
            );
        }
    }

    // Verify change list only has cursor.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 1);
    assert_eq!(cl[0], 7);
}

// VAL-SCENE-009: Data buffer exhaustion detection
#[test]
fn data_buffer_exhaustion_detectable() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: fill data buffer to >75%.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        // Push enough data to exceed 75% of DATA_BUFFER_SIZE.
        let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
        let chunk = vec![0xABu8; threshold as usize + 100];
        w.push_data(&chunk);
    }
    dw.swap();

    // After copy-forward, the back buffer inherits the high data_used.
    dw.copy_front_to_back();
    {
        let back = dw.back();
        let used = back.data_used();
        let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
        assert!(
            used > threshold,
            "data_used {} should exceed 75% threshold {}",
            used,
            threshold
        );
        // This is where core would detect exhaustion and fall back to
        // full rebuild via clear() + reset_data().
    }
}

// VAL-SCENE-005: update_data with matching and mismatching lengths
#[test]
fn update_data_in_place_after_copy_forward() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: scene with data.
    let mut clock_dref = DataRef { offset: 0, length: 0 };
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        clock_dref = w.push_data(b"12:34:56");
    }
    dw.swap();

    // Frame 2: copy forward, update clock data in-place.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        assert!(w.update_data(clock_dref, b"12:35:00"));
        // Wrong length should fail.
        assert!(!w.update_data(clock_dref, b"ABC"));
        w.mark_changed(0); // mark root changed (for demo)
    }
    dw.swap();

    // Verify the updated data is readable.
    let dr = DoubleReader::new(&buf);
    let data = dr.front_data(clock_dref);
    assert_eq!(data, b"12:35:00");
}

// Verify mark_changed at exact capacity (24 entries)
#[test]
fn mark_changed_exact_capacity() {
    let mut buf = make_buf();
    {
        let mut w = SceneWriter::new(&mut buf);

        for _ in 0..30 {
            w.alloc_node().unwrap();
        }

        // Mark exactly CHANGE_LIST_CAPACITY nodes.
        for i in 0..scene::CHANGE_LIST_CAPACITY {
            w.mark_changed(i as NodeId);
        }
    }

    let hdr = unsafe { &*(buf.as_ptr() as *const scene::SceneHeader) };
    assert_eq!(hdr.change_count, scene::CHANGE_LIST_CAPACITY as u16);

    // Verify all entries.
    for i in 0..scene::CHANGE_LIST_CAPACITY {
        assert_eq!(hdr.changed_nodes[i], i as NodeId);
    }

    // One more should overflow.
    {
        let mut w = SceneWriter::from_existing(&mut buf);
        w.mark_changed(24);
    }
    let hdr = unsafe { &*(buf.as_ptr() as *const scene::SceneHeader) };
    assert_eq!(hdr.change_count, scene::FULL_REPAINT);
}

// Empty change list after initial DoubleWriter::new (both buffers empty)
#[test]
fn double_reader_initial_change_list_empty() {
    let mut buf = make_double_buf();
    {
        let _dw = DoubleWriter::new(&mut buf);
    }

    let dr = DoubleReader::new(&buf);
    // Initial state: change_count = 0 (set by SceneWriter::new).
    assert!(!dr.is_full_repaint());
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 0);
}

// Multiple frames of copy-forward + selective mutation
#[test]
fn multiple_copy_forward_frames() {
    let mut buf = make_double_buf();

    // Frame 1: initial build.
    {
        let mut dw = DoubleWriter::new(&mut buf);
        {
            let mut w = dw.back();
            w.clear();
            for i in 0..8u16 {
                let n = w.alloc_node().unwrap();
                w.node_mut(n).width = (i + 1) * 10;
            }
            w.set_root(0);
        }
        dw.swap();
    }

    // Frames 2-5: copy-forward with different mutations.
    for frame in 0..4u16 {
        {
            let mut dw = DoubleWriter::from_existing(&mut buf);
            dw.copy_front_to_back();
            {
                let mut w = dw.back();
                // Mutate a different node each frame.
                let target = (frame + 1) as NodeId; // nodes 1, 2, 3, 4
                w.node_mut(target).height = (frame + 1) * 100;
                w.mark_changed(target);
            }
            dw.swap();
        }

        // Verify change list has exactly one entry.
        let dr = DoubleReader::new(&buf);
        let cl = dr.change_list().unwrap();
        assert_eq!(cl.len(), 1, "Frame {}: expected 1 change", frame + 2);
        assert_eq!(cl[0], (frame + 1) as NodeId);

        // Verify the mutation stuck.
        assert_eq!(
            dr.front_nodes()[(frame + 1) as usize].height,
            (frame + 1) * 100
        );

        // Verify other nodes' widths are preserved from frame 1.
        for i in 0..8usize {
            assert_eq!(
                dr.front_nodes()[i].width,
                ((i as u16) + 1) * 10,
                "Frame {}: node {} width changed unexpectedly",
                frame + 2,
                i
            );
        }
    }
}

// VAL-SCENE-009: update_data doesn't grow data_used (same-length overwrite)
#[test]
fn update_data_does_not_grow_data_used() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let mut dref = DataRef { offset: 0, length: 0 };
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.set_root(root);
        dref = w.push_data(b"AAAAAAAA");
    }
    dw.swap();

    let front_data_before = dw.front_data_buf().len();

    // Copy forward and update in-place 10 times.
    for i in 0..10u8 {
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            let new_data = [b'A' + i; 8];
            assert!(w.update_data(dref, &new_data));
            assert_eq!(w.data_used() as usize, front_data_before);
        }
        dw.swap();
    }

    // data_used should be unchanged.
    let dr = DoubleReader::new(&buf);
    assert_eq!(dr.front_data_buf().len(), front_data_before);
}

// ── Targeted incremental update tests ───────────────────────────────
//
// These tests verify the incremental update patterns used by Core's
// SceneState methods (update_clock, update_cursor, update_selection,
// update_document_content). They exercise the scene graph primitives
// directly to prove correctness of the copy-forward + selective mutation
// pattern.

/// Well-known node indices (mirrors core/scene_state.rs).
const N_ROOT: u16 = 0;
const N_TITLE_BAR: u16 = 1;
const N_TITLE_TEXT: u16 = 2;
const N_CLOCK_TEXT: u16 = 3;
const N_SHADOW: u16 = 4;
const N_CONTENT: u16 = 5;
const N_DOC_TEXT: u16 = 6;
const N_CURSOR: u16 = 7;
const WELL_KNOWN_COUNT: u16 = 8;

/// Build a typical editor scene into a DoubleWriter, swap to publish.
/// Returns the glyph DataRef for the clock text (for in-place update tests).
fn build_test_editor_scene(dw: &mut DoubleWriter<'_>, doc_text: &[u8], clock_text: &[u8]) -> DataRef {
    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let chrome_bg = Color::rgba(45, 45, 48, 255);
    let clock_color = Color::rgb(130, 130, 130);

    let mut w = dw.back();
    w.clear();

    // Push clock glyph data.
    let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width);
    let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

    // Push title glyph data.
    let title_glyphs = bytes_to_shaped_glyphs(b"Text", char_width);
    let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

    // Push doc text runs.
    let chars_per_line: usize = 80;
    let all_runs = layout_mono_lines(
        doc_text, chars_per_line, line_height as i16, text_color, char_width, font_size,
    );
    let visible_runs = scroll_runs(all_runs, 0, line_height as u32, 700);
    let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
    for mut run in visible_runs {
        let line_text = line_bytes_for_run(doc_text, &run);
        let shaped = bytes_to_shaped_glyphs(line_text, char_width);
        run.glyphs = w.push_shaped_glyphs(&shaped);
        run.glyph_count = shaped.len() as u16;
        final_runs.push(run);
    }
    let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

    // Push title/clock text runs.
    let title_run = TextRun {
        glyphs: title_glyph_ref, glyph_count: 4, x: 0, y: 0,
        color: text_color, advance: char_width, font_size, axis_hash: 0,
    };
    let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

    let clock_run = TextRun {
        glyphs: clock_glyph_ref, glyph_count: clock_glyphs.len() as u16, x: 0, y: 0,
        color: clock_color, advance: char_width, font_size, axis_hash: 0,
    };
    let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

    // Allocate 8 well-known nodes.
    for _ in 0..8 {
        w.alloc_node().unwrap();
    }

    // Root.
    {
        let n = w.node_mut(N_ROOT);
        n.first_child = N_TITLE_BAR;
        n.width = 1024;
        n.height = 768;
        n.background = Color::rgb(30, 30, 30);
        n.flags = NodeFlags::VISIBLE;
    }
    // Title bar.
    {
        let n = w.node_mut(N_TITLE_BAR);
        n.first_child = N_TITLE_TEXT;
        n.next_sibling = N_SHADOW;
        n.width = 1024;
        n.height = 36;
        n.background = chrome_bg;
        n.flags = NodeFlags::VISIBLE;
    }
    // Title text.
    {
        let n = w.node_mut(N_TITLE_TEXT);
        n.next_sibling = N_CLOCK_TEXT;
        n.x = 12;
        n.y = 8;
        n.width = 512;
        n.height = line_height;
        n.content = Content::Text { runs: title_runs_ref, run_count: title_run_count, _pad: [0; 2] };
        n.content_hash = fnv1a(b"Text");
        n.flags = NodeFlags::VISIBLE;
    }
    // Clock text.
    {
        let n = w.node_mut(N_CLOCK_TEXT);
        n.x = 932;
        n.y = 8;
        n.width = 80;
        n.height = line_height;
        n.content = Content::Text { runs: clock_runs_ref, run_count: clock_run_count, _pad: [0; 2] };
        n.content_hash = fnv1a(clock_text);
        n.flags = NodeFlags::VISIBLE;
    }
    // Shadow: structural placeholder (real shadow on title bar).
    {
        let n = w.node_mut(N_SHADOW);
        n.next_sibling = N_CONTENT;
        n.y = 36;
        n.width = 1024;
        n.height = 0;
        n.background = Color::TRANSPARENT;
        n.flags = NodeFlags::VISIBLE;
    }
    // Real shadow on title bar.
    {
        let n = w.node_mut(N_TITLE_BAR);
        n.shadow_color = Color::rgba(0, 0, 0, 60);
        n.shadow_offset_x = 0;
        n.shadow_offset_y = 12;
        n.shadow_blur_radius = 8;
        n.shadow_spread = 0;
    }
    // Content.
    {
        let n = w.node_mut(N_CONTENT);
        n.first_child = N_DOC_TEXT;
        n.next_sibling = NULL;
        n.y = 48;
        n.width = 1024;
        n.height = 720;
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    // Doc text.
    {
        let n = w.node_mut(N_DOC_TEXT);
        n.first_child = N_CURSOR;
        n.x = 12;
        n.y = 8;
        n.width = 1000;
        n.height = 720;
        n.scroll_y = 0;
        n.content = Content::Text { runs: doc_runs_ref, run_count: doc_run_count, _pad: [0; 2] };
        n.content_hash = fnv1a(doc_text);
        n.flags = NodeFlags::VISIBLE | NodeFlags::CLIPS_CHILDREN;
    }
    // Cursor.
    {
        let n = w.node_mut(N_CURSOR);
        n.x = 0;
        n.y = 0;
        n.width = 2;
        n.height = line_height;
        n.background = Color::rgb(200, 200, 200);
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;
    }

    w.set_root(N_ROOT);

    clock_glyph_ref
}

// VAL-CORE-001: Clock tick updates only clock node
#[test]
fn incremental_clock_update_changes_only_clock() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let clock_glyph_ref = build_test_editor_scene(&mut dw, b"hello world", b"12:34:56");
    dw.swap();

    // Snapshot all nodes before the incremental update.
    let nodes_before: Vec<Node> = dw.front_nodes().to_vec();
    let node_size = core::mem::size_of::<Node>();

    // Incremental clock update: copy forward, update glyph data in-place.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        // Read the clock node's Content::Text to find the glyph DataRef.
        let clock_node = w.node(N_CLOCK_TEXT);
        if let Content::Text { runs, .. } = clock_node.content {
            // Read the TextRun from the data buffer.
            let data_buf = w.data_buf();
            let run_offset = runs.offset as usize;
            let run_size = core::mem::size_of::<TextRun>();
            if run_offset + run_size <= data_buf.len() {
                // SAFETY: TextRun is repr(C), data buffer is aligned.
                let run_ptr = unsafe {
                    data_buf.as_ptr().add(run_offset) as *const TextRun
                };
                let text_run = unsafe { core::ptr::read(run_ptr) };
                let glyph_dref = text_run.glyphs;

                // Build new glyphs for "12:35:00".
                let new_glyphs = bytes_to_shaped_glyphs(b"12:35:00", text_run.advance);
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };

                assert!(w.update_data(glyph_dref, new_bytes));
            }
        }

        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(b"12:35:00");
        w.mark_changed(N_CLOCK_TEXT);
    }
    dw.swap();

    // Verify: only N_CLOCK_TEXT changed, all other nodes byte-identical.
    let nodes_after: Vec<Node> = dw.front_nodes().to_vec();
    assert_eq!(nodes_after.len(), 8);

    for i in 0..8u16 {
        if i == N_CLOCK_TEXT {
            // Clock node should have new content_hash.
            assert_ne!(
                nodes_after[i as usize].content_hash,
                nodes_before[i as usize].content_hash,
                "Clock content_hash should have changed"
            );
        } else {
            // All other nodes unchanged byte-for-byte.
            let before = unsafe {
                core::slice::from_raw_parts(
                    &nodes_before[i as usize] as *const Node as *const u8, node_size,
                )
            };
            let after = unsafe {
                core::slice::from_raw_parts(
                    &nodes_after[i as usize] as *const Node as *const u8, node_size,
                )
            };
            assert_eq!(before, after, "Node {} should be unchanged after clock update", i);
        }
    }

    // Verify change list has only N_CLOCK_TEXT.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 1);
    assert_eq!(cl[0], N_CLOCK_TEXT);
}

// VAL-CORE-002: Cursor move updates only cursor node
#[test]
fn incremental_cursor_update_changes_only_cursor() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello world", b"12:34:56");
    dw.swap();

    let nodes_before: Vec<Node> = dw.front_nodes().to_vec();
    let node_size = core::mem::size_of::<Node>();

    // Incremental cursor update: move cursor to position 5.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        let doc_text = b"hello world";
        let (cursor_line, cursor_col) = byte_to_line_col(doc_text, 5, 80);
        let cursor_x = (cursor_col as u32 * 8) as i16;
        let cursor_y = (cursor_line as i32 * 20 - 0) as i16;

        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;

        w.mark_changed(N_CURSOR);
    }
    dw.swap();

    let nodes_after: Vec<Node> = dw.front_nodes().to_vec();
    assert_eq!(nodes_after.len(), 8);

    for i in 0..8u16 {
        if i == N_CURSOR {
            assert_eq!(nodes_after[i as usize].x, 40); // 5 * 8
            assert_eq!(nodes_after[i as usize].y, 0);
            // Other cursor properties unchanged.
            assert_eq!(nodes_after[i as usize].width, nodes_before[i as usize].width);
            assert_eq!(nodes_after[i as usize].background, nodes_before[i as usize].background);
        } else {
            let before = unsafe {
                core::slice::from_raw_parts(
                    &nodes_before[i as usize] as *const Node as *const u8, node_size,
                )
            };
            let after = unsafe {
                core::slice::from_raw_parts(
                    &nodes_after[i as usize] as *const Node as *const u8, node_size,
                )
            };
            assert_eq!(before, after, "Node {} should be unchanged after cursor move", i);
        }
    }

    // Verify change list has only N_CURSOR.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 1);
    assert_eq!(cl[0], N_CURSOR);
}

// VAL-CORE-006: Incremental update matches full rebuild
#[test]
fn incremental_cursor_matches_full_rebuild() {
    let doc_text = b"hello world\nsecond line\nthird line";
    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let cursor_pos: usize = 13; // start of "second"
    let scroll_px: i32 = 0;
    let chars_per_line: usize = 80;

    // Full rebuild: build from scratch with cursor at position 13.
    let mut buf_full = make_double_buf();
    let mut dw_full = DoubleWriter::new(&mut buf_full);
    {
        let mut w = dw_full.back();
        w.clear();
        for _ in 0..8 { w.alloc_node().unwrap(); }
        w.set_root(N_ROOT);
        // Set cursor to position 13.
        let (line, col) = byte_to_line_col(doc_text, cursor_pos, chars_per_line);
        let n = w.node_mut(N_CURSOR);
        n.x = (col as u32 * char_width as u32) as i16;
        n.y = (line as i32 * line_height as i32 - scroll_px) as i16;
        n.width = 2;
        n.height = line_height;
        n.background = Color::rgb(200, 200, 200);
        n.flags = NodeFlags::VISIBLE;
    }
    dw_full.swap();

    // Incremental: build initial scene with cursor at 0, then update to 13.
    let mut buf_inc = make_double_buf();
    let mut dw_inc = DoubleWriter::new(&mut buf_inc);
    {
        let mut w = dw_inc.back();
        w.clear();
        for _ in 0..8 { w.alloc_node().unwrap(); }
        w.set_root(N_ROOT);
        let n = w.node_mut(N_CURSOR);
        n.x = 0;
        n.y = 0;
        n.width = 2;
        n.height = line_height;
        n.background = Color::rgb(200, 200, 200);
        n.flags = NodeFlags::VISIBLE;
    }
    dw_inc.swap();

    // Incremental update to cursor_pos=13.
    dw_inc.copy_front_to_back();
    {
        let mut w = dw_inc.back();
        let (line, col) = byte_to_line_col(doc_text, cursor_pos, chars_per_line);
        let n = w.node_mut(N_CURSOR);
        n.x = (col as u32 * char_width as u32) as i16;
        n.y = (line as i32 * line_height as i32 - scroll_px) as i16;
        w.mark_changed(N_CURSOR);
    }
    dw_inc.swap();

    // Compare cursor nodes.
    let full_cursor = dw_full.front_nodes()[N_CURSOR as usize];
    let inc_cursor = dw_inc.front_nodes()[N_CURSOR as usize];

    assert_eq!(full_cursor.x, inc_cursor.x, "Cursor x mismatch");
    assert_eq!(full_cursor.y, inc_cursor.y, "Cursor y mismatch");
    assert_eq!(full_cursor.width, inc_cursor.width, "Cursor width mismatch");
    assert_eq!(full_cursor.height, inc_cursor.height, "Cursor height mismatch");
    assert_eq!(full_cursor.background, inc_cursor.background, "Cursor bg mismatch");
}

// VAL-CORE-004 / VAL-CORE-010: Selection update manages node count correctly
#[test]
fn incremental_selection_manages_nodes() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello world\nsecond line", b"12:34:56");
    dw.swap();

    assert_eq!(dw.front_nodes().len(), 8);

    // Add selection: bytes 0..5 (= "hello" on line 0).
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        // Truncate to well-known count first.
        w.set_node_count(WELL_KNOWN_COUNT);
        w.node_mut(N_CURSOR).next_sibling = NULL;
        w.mark_changed(N_CURSOR);

        // Add one selection rect.
        let sel_id = w.alloc_node().unwrap(); // should be 8
        assert_eq!(sel_id, 8);
        let n = w.node_mut(sel_id);
        n.x = 0;
        n.y = 0;
        n.width = 5 * 8;
        n.height = 20;
        n.background = Color::rgba(0, 100, 200, 80);
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = NULL;

        w.node_mut(N_CURSOR).next_sibling = sel_id;
        w.mark_changed(sel_id);
    }
    dw.swap();

    assert_eq!(dw.front_nodes().len(), 9); // 8 well-known + 1 selection

    // Clear selection.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();
        w.set_node_count(WELL_KNOWN_COUNT);
        w.node_mut(N_CURSOR).next_sibling = NULL;
        w.mark_changed(N_CURSOR);
    }
    dw.swap();

    assert_eq!(dw.front_nodes().len(), 8); // back to well-known only
}

// VAL-CORE-010: Selection create/destroy cycles don't leak node slots
#[test]
fn selection_cycle_no_node_leak() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello world\nsecond line\nthird line", b"12:00:00");
    dw.swap();

    // Cycle select/deselect 10 times.
    for cycle in 0..10 {
        // Add selection (2 rects across 2 lines).
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.set_node_count(WELL_KNOWN_COUNT);
            w.node_mut(N_CURSOR).next_sibling = NULL;
            w.mark_changed(N_CURSOR);

            // Selection rect on line 0.
            let s1 = w.alloc_node().unwrap();
            assert_eq!(s1, 8, "Cycle {}: first sel node should be 8", cycle);
            w.node_mut(s1).background = Color::rgba(0, 100, 200, 80);
            w.node_mut(s1).flags = NodeFlags::VISIBLE;
            w.node_mut(s1).next_sibling = NULL;
            w.node_mut(N_CURSOR).next_sibling = s1;
            w.mark_changed(s1);

            // Selection rect on line 1.
            let s2 = w.alloc_node().unwrap();
            assert_eq!(s2, 9, "Cycle {}: second sel node should be 9", cycle);
            w.node_mut(s2).background = Color::rgba(0, 100, 200, 80);
            w.node_mut(s2).flags = NodeFlags::VISIBLE;
            w.node_mut(s2).next_sibling = NULL;
            w.node_mut(s1).next_sibling = s2;
            w.mark_changed(s2);
        }
        dw.swap();

        assert_eq!(
            dw.front_nodes().len(), 10,
            "Cycle {}: expected 10 nodes (8 + 2 sel rects)", cycle
        );

        // Clear selection.
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.set_node_count(WELL_KNOWN_COUNT);
            w.node_mut(N_CURSOR).next_sibling = NULL;
            w.mark_changed(N_CURSOR);
        }
        dw.swap();

        assert_eq!(
            dw.front_nodes().len(), 8,
            "Cycle {}: expected 8 nodes after clearing selection", cycle
        );
    }
}

// VAL-SCENE-009: Data buffer exhaustion triggers full rebuild fallback
#[test]
fn data_buffer_exhaustion_triggers_full_rebuild() {
    let mut buf = make_double_buf();

    // Build initial scene with enough data to approach 75% threshold.
    {
        let mut dw = DoubleWriter::new(&mut buf);
        {
            let mut w = dw.back();
            w.clear();
            for _ in 0..8 { w.alloc_node().unwrap(); }
            w.set_root(N_ROOT);

            // Fill data buffer to just above 75%.
            let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
            let padding = vec![0xABu8; threshold as usize + 100];
            w.push_data(&padding);
        }
        dw.swap();

        let data_used = dw.front_data_buf().len();
        let threshold = (DATA_BUFFER_SIZE * 3) / 4;
        assert!(
            data_used > threshold,
            "data_used {} should exceed 75% threshold {}",
            data_used, threshold
        );
    }

    // Simulate what update_document_content does: check threshold.
    // If above threshold, a full rebuild (clear + reset_data) is needed.
    {
        let mut dw = DoubleWriter::from_existing(&mut buf);
        dw.copy_front_to_back();
        {
            let w = dw.back();
            let used = w.data_used();
            let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;

            if used > threshold {
                // Fall back to full rebuild.
                drop(w);
                let mut w2 = dw.back();
                w2.clear();
                w2.reset_data();
                let root = w2.alloc_node().unwrap();
                w2.set_root(root);
                // After clear, data_used is 0.
                assert_eq!(w2.data_used(), 0);
            }
        }
        dw.swap();
    }

    // Verify the full rebuild produced a clean scene.
    let dr = DoubleReader::new(&buf);
    assert!(dr.is_full_repaint()); // clear() sets FULL_REPAINT
    assert_eq!(dr.front_nodes().len(), 1); // only root node
}

// VAL-CORE-009: Repeated incremental cycles preserve scene integrity
// Interleaves clock updates, cursor moves, text insertions, and selection
// changes across 100 iterations to verify no state accumulation or drift.
#[test]
fn repeated_incremental_cycles_preserve_integrity() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let sel_color = Color::rgba(0, 100, 200, 80);
    let chars_per_line: usize = 80;

    // Start with initial document text.
    let mut doc_text: Vec<u8> = b"hello world\nsecond line\nthird line".to_vec();
    let mut cursor_pos: usize = 0;
    let mut clock_text: Vec<u8> = b"12:00:00".to_vec();
    let mut sel_start: usize = 0;
    let mut sel_end: usize = 0;

    build_test_editor_scene(&mut dw, &doc_text, &clock_text);
    dw.swap();

    for i in 0..100 {
        match i % 5 {
            // Clock-only update (20 iterations)
            0 => {
                let second = i % 60;
                clock_text = format!("12:{:02}:{:02}", i / 60 % 60, second)
                    .into_bytes();
                // Ensure clock_text is always 8 bytes.
                clock_text.resize(8, b'0');

                dw.copy_front_to_back();
                {
                    let mut w = dw.back();

                    let clock_node = w.node(N_CLOCK_TEXT);
                    if let Content::Text { runs, .. } = clock_node.content {
                        let data_buf = w.data_buf();
                        let run_offset = runs.offset as usize;
                        let run_size = core::mem::size_of::<TextRun>();
                        if run_offset + run_size <= data_buf.len() {
                            // SAFETY: TextRun is repr(C), data buffer is aligned.
                            let run_ptr = unsafe {
                                data_buf.as_ptr().add(run_offset) as *const TextRun
                            };
                            let text_run = unsafe { core::ptr::read(run_ptr) };
                            let glyph_dref = text_run.glyphs;

                            let new_glyphs =
                                bytes_to_shaped_glyphs(&clock_text, text_run.advance);
                            let new_bytes = unsafe {
                                core::slice::from_raw_parts(
                                    new_glyphs.as_ptr() as *const u8,
                                    new_glyphs.len()
                                        * core::mem::size_of::<ShapedGlyph>(),
                                )
                            };
                            let _ = w.update_data(glyph_dref, new_bytes);
                        }
                    }

                    w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(&clock_text);
                    w.mark_changed(N_CLOCK_TEXT);
                }
                dw.swap();
            }

            // Cursor-only move (20 iterations)
            1 => {
                cursor_pos = i % (doc_text.len() + 1);
                sel_start = cursor_pos;
                sel_end = cursor_pos;

                dw.copy_front_to_back();
                {
                    let mut w = dw.back();
                    let (line, col) =
                        byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
                    let n = w.node_mut(N_CURSOR);
                    n.x = (col as u32 * char_width as u32) as i16;
                    n.y = (line as i32 * line_height as i32) as i16;
                    w.mark_changed(N_CURSOR);
                }
                dw.swap();
            }

            // Text insertion (20 iterations)
            2 => {
                let insert_pos = cursor_pos.min(doc_text.len());
                let ch = b'a' + (i % 26) as u8;
                doc_text.insert(insert_pos, ch);
                cursor_pos = insert_pos + 1;
                sel_start = cursor_pos;
                sel_end = cursor_pos;

                dw.copy_front_to_back();
                {
                    let mut w = dw.back();
                    w.set_node_count(WELL_KNOWN_COUNT);

                    // Re-layout doc text.
                    let all_runs = layout_mono_lines(
                        &doc_text,
                        chars_per_line,
                        line_height as i16,
                        text_color,
                        char_width,
                        font_size,
                    );
                    let visible = scroll_runs(all_runs, 0, line_height as u32, 700);
                    let mut final_runs: Vec<TextRun> =
                        Vec::with_capacity(visible.len());
                    for mut run in visible {
                        let lt = line_bytes_for_run(&doc_text, &run);
                        let shaped = bytes_to_shaped_glyphs(lt, char_width);
                        run.glyphs = w.push_shaped_glyphs(&shaped);
                        run.glyph_count = shaped.len() as u16;
                        final_runs.push(run);
                    }
                    let (doc_runs_ref, doc_run_count) =
                        w.push_text_runs(&final_runs);

                    {
                        let n = w.node_mut(N_DOC_TEXT);
                        n.content = Content::Text {
                            runs: doc_runs_ref,
                            run_count: doc_run_count,
                            _pad: [0; 2],
                        };
                        n.content_hash = fnv1a(&doc_text);
                    }
                    w.mark_changed(N_DOC_TEXT);

                    // Update cursor.
                    let (line, col) =
                        byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
                    let n = w.node_mut(N_CURSOR);
                    n.x = (col as u32 * char_width as u32) as i16;
                    n.y = (line as i32 * line_height as i32) as i16;
                    n.next_sibling = NULL;
                    w.mark_changed(N_CURSOR);
                }
                dw.swap();
            }

            // Selection change (20 iterations)
            3 => {
                // Select a range around current cursor.
                sel_start = cursor_pos.saturating_sub(3).min(doc_text.len());
                sel_end = (cursor_pos + 5).min(doc_text.len());

                dw.copy_front_to_back();
                {
                    let mut w = dw.back();
                    w.set_node_count(WELL_KNOWN_COUNT);

                    // Update cursor.
                    let (line, col) =
                        byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
                    let n = w.node_mut(N_CURSOR);
                    n.x = (col as u32 * char_width as u32) as i16;
                    n.y = (line as i32 * line_height as i32) as i16;
                    n.next_sibling = NULL;
                    w.mark_changed(N_CURSOR);

                    // Add selection rects.
                    let (sl, sh) = if sel_start <= sel_end {
                        (sel_start, sel_end)
                    } else {
                        (sel_end, sel_start)
                    };
                    if sl < sh {
                        let (sl_line, sl_col) =
                            byte_to_line_col(&doc_text, sl, chars_per_line);
                        let (sh_line, sh_col) =
                            byte_to_line_col(&doc_text, sh, chars_per_line);
                        let mut prev_sel: u16 = NULL;

                        for line in sl_line..=sh_line {
                            let c_start = if line == sl_line { sl_col } else { 0 };
                            let c_end = if line == sh_line {
                                sh_col
                            } else {
                                chars_per_line
                            };
                            if c_start >= c_end {
                                continue;
                            }
                            if let Some(sid) = w.alloc_node() {
                                let sn = w.node_mut(sid);
                                sn.x = (c_start as u32 * char_width as u32) as i16;
                                sn.y = (line as i32 * line_height as i32) as i16;
                                sn.width =
                                    ((c_end - c_start) as u32 * char_width as u32) as u16;
                                sn.height = line_height;
                                sn.background = sel_color;
                                sn.flags = NodeFlags::VISIBLE;
                                sn.next_sibling = NULL;
                                w.mark_changed(sid);

                                if prev_sel == NULL {
                                    w.node_mut(N_CURSOR).next_sibling = sid;
                                } else {
                                    w.node_mut(prev_sel).next_sibling = sid;
                                }
                                prev_sel = sid;
                            }
                        }
                    }
                }
                dw.swap();
            }

            // Clear selection (20 iterations) — ensures node count returns to 8
            _ => {
                sel_start = cursor_pos;
                sel_end = cursor_pos;

                dw.copy_front_to_back();
                {
                    let mut w = dw.back();
                    w.set_node_count(WELL_KNOWN_COUNT);
                    w.node_mut(N_CURSOR).next_sibling = NULL;
                    w.mark_changed(N_CURSOR);
                }
                dw.swap();
            }
        }
    }

    // After 100 diverse cycles, build a reference scene from scratch with
    // the same final state and compare key properties.
    let (expected_line, expected_col) =
        byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
    let expected_cursor_x = (expected_col as u32 * char_width as u32) as i16;
    let expected_cursor_y = (expected_line as i32 * line_height as i32) as i16;

    let cursor = &dw.front_nodes()[N_CURSOR as usize];
    assert_eq!(cursor.x, expected_cursor_x, "Cursor x after 100 diverse cycles");
    assert_eq!(cursor.y, expected_cursor_y, "Cursor y after 100 diverse cycles");

    // Last iteration (i=99, 99%5=4) clears selection → node count should be 8.
    assert_eq!(
        dw.front_nodes().len(),
        8,
        "Node count should be 8 after selection clear"
    );

    // Chrome nodes should still be intact.
    assert_eq!(dw.front_nodes()[N_ROOT as usize].width, 1024);
    assert_eq!(dw.front_nodes()[N_TITLE_BAR as usize].height, 36);
    // N_SHADOW is now a 0-height placeholder — real shadow rendered via
    // title bar's shadow fields.
    assert_eq!(dw.front_nodes()[N_SHADOW as usize].height, 0);

    // Clock should reflect the last clock update (i=95, 95%5=0).
    // The clock_text was last set at iteration 95.
    assert_ne!(
        dw.front_nodes()[N_CLOCK_TEXT as usize].content_hash,
        fnv1a(b"12:00:00"),
        "Clock hash should differ from initial after updates"
    );
}

// VAL-CORE-008: Change list populated correctly per update type
#[test]
fn change_list_correct_per_update_type() {
    let mut buf = make_double_buf();

    // Initial scene.
    {
        let mut dw = DoubleWriter::new(&mut buf);
        build_test_editor_scene(&mut dw, b"hello", b"12:00:00");
        dw.swap();
    }

    // Clock-only update.
    {
        let mut dw = DoubleWriter::from_existing(&mut buf);
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(b"12:01:00");
            w.mark_changed(N_CLOCK_TEXT);
        }
        dw.swap();
    }
    {
        let dr = DoubleReader::new(&buf);
        let cl = dr.change_list().unwrap();
        assert_eq!(cl, &[N_CLOCK_TEXT], "Clock update should only mark N_CLOCK_TEXT");
    }

    // Cursor-only update.
    {
        let mut dw = DoubleWriter::from_existing(&mut buf);
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.node_mut(N_CURSOR).x = 40;
            w.mark_changed(N_CURSOR);
        }
        dw.swap();
    }
    {
        let dr = DoubleReader::new(&buf);
        let cl = dr.change_list().unwrap();
        assert_eq!(cl, &[N_CURSOR], "Cursor update should only mark N_CURSOR");
    }

    // Document update: marks N_DOC_TEXT + N_CURSOR.
    {
        let mut dw = DoubleWriter::from_existing(&mut buf);
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.node_mut(N_DOC_TEXT).content_hash = fnv1a(b"hello!");
            w.mark_changed(N_DOC_TEXT);
            w.node_mut(N_CURSOR).x = 48;
            w.mark_changed(N_CURSOR);
        }
        dw.swap();
    }
    {
        let dr = DoubleReader::new(&buf);
        let cl = dr.change_list().unwrap();
        assert_eq!(cl.len(), 2);
        assert!(cl.contains(&N_DOC_TEXT));
        assert!(cl.contains(&N_CURSOR));
    }
}

// VAL-CORE-003: Character insert updates doc text and cursor
#[test]
fn incremental_doc_update_changes_doc_text_and_cursor() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let old_text = b"hello";
    build_test_editor_scene(&mut dw, old_text, b"12:00:00");
    dw.swap();

    let nodes_before: Vec<Node> = dw.front_nodes().to_vec();
    let node_size = core::mem::size_of::<Node>();

    // Simulate typing 'x' at position 5 → "hellox"
    let new_text = b"hellox";

    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        // Truncate selection nodes.
        w.set_node_count(WELL_KNOWN_COUNT);

        // Re-layout and push new doc text data.
        let text_color = Color::rgb(220, 220, 220);
        let runs = layout_mono_lines(new_text, 80, 20, text_color, 8, 16);
        let visible = scroll_runs(runs, 0, 20, 700);
        let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible.len());
        for mut run in visible {
            let line_text = line_bytes_for_run(new_text, &run);
            let shaped = bytes_to_shaped_glyphs(line_text, 8);
            run.glyphs = w.push_shaped_glyphs(&shaped);
            run.glyph_count = shaped.len() as u16;
            final_runs.push(run);
        }
        let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

        // Update doc text node.
        {
            let n = w.node_mut(N_DOC_TEXT);
            n.content = Content::Text { runs: doc_runs_ref, run_count: doc_run_count, _pad: [0; 2] };
            n.content_hash = fnv1a(new_text);
        }
        w.mark_changed(N_DOC_TEXT);

        // Update cursor position (after 'x' = position 6).
        let (line, col) = byte_to_line_col(new_text, 6, 80);
        let n = w.node_mut(N_CURSOR);
        n.x = (col as u32 * 8) as i16;
        n.y = (line as i32 * 20) as i16;
        n.next_sibling = NULL;
        w.mark_changed(N_CURSOR);
    }
    dw.swap();

    let nodes_after: Vec<Node> = dw.front_nodes().to_vec();

    // Doc text should have changed.
    assert_ne!(
        nodes_after[N_DOC_TEXT as usize].content_hash,
        nodes_before[N_DOC_TEXT as usize].content_hash,
        "Doc text content_hash should have changed"
    );

    // Cursor should have moved.
    assert_eq!(nodes_after[N_CURSOR as usize].x, 48); // 6 * 8

    // Chrome nodes should be unchanged.
    for &i in &[N_ROOT, N_TITLE_BAR, N_TITLE_TEXT, N_CLOCK_TEXT, N_SHADOW, N_CONTENT] {
        let before = unsafe {
            core::slice::from_raw_parts(
                &nodes_before[i as usize] as *const Node as *const u8, node_size,
            )
        };
        let after = unsafe {
            core::slice::from_raw_parts(
                &nodes_after[i as usize] as *const Node as *const u8, node_size,
            )
        };
        assert_eq!(before, after, "Chrome node {} should be unchanged after doc update", i);
    }

    // Change list should have N_DOC_TEXT and N_CURSOR.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 2);
    assert!(cl.contains(&N_DOC_TEXT));
    assert!(cl.contains(&N_CURSOR));
}

// VAL-SCENE-009 test: update_data (in-place) does not grow data_used
#[test]
fn clock_update_data_used_unchanged() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello", b"12:00:00");
    dw.swap();

    let data_used_before = dw.front_data_buf().len();

    // Incremental clock update via in-place glyph overwrite.
    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        let clock_node = w.node(N_CLOCK_TEXT);
        if let Content::Text { runs, .. } = clock_node.content {
            let data_buf = w.data_buf();
            let run_offset = runs.offset as usize;
            let run_size = core::mem::size_of::<TextRun>();
            if run_offset + run_size <= data_buf.len() {
                let run_ptr = unsafe { data_buf.as_ptr().add(run_offset) as *const TextRun };
                let text_run = unsafe { core::ptr::read(run_ptr) };
                let glyph_dref = text_run.glyphs;

                let new_glyphs = bytes_to_shaped_glyphs(b"12:01:00", text_run.advance);
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };
                assert!(w.update_data(glyph_dref, new_bytes));
            }
        }

        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(b"12:01:00");
        w.mark_changed(N_CLOCK_TEXT);

        // data_used should be the same as before.
        assert_eq!(w.data_used() as usize, data_used_before, "Clock update should not grow data_used");
    }
    dw.swap();

    // Verify data_used hasn't grown in the new front buffer.
    assert_eq!(dw.front_data_buf().len(), data_used_before);
}

// VAL-CORE-007: Scroll change updates only doc text runs and cursor
#[test]
fn incremental_scroll_updates_only_doc_and_cursor() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Build a scene with multi-line text (enough to scroll).
    let mut long_text = Vec::new();
    for i in 0..50u8 {
        if i > 0 {
            long_text.push(b'\n');
        }
        // Each line: "Line XX some padding text here!"
        long_text.extend_from_slice(b"Line ");
        long_text.push(b'0' + i / 10);
        long_text.push(b'0' + i % 10);
        long_text.extend_from_slice(b" some padding text here!");
    }

    build_test_editor_scene(&mut dw, &long_text, b"12:00:00");
    dw.swap();

    // Snapshot all nodes before the scroll update.
    let nodes_before: Vec<Node> = dw.front_nodes().to_vec();
    let node_size = core::mem::size_of::<Node>();

    // Incremental scroll update: same text, different scroll_y.
    // This mirrors what update_document_content does when only scroll changes.
    let scroll_lines: u32 = 5;
    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let chars_per_line: usize = 80;
    let scroll_px = scroll_lines as i32 * line_height as i32;

    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        // Truncate selection nodes (none in this case, but matches real code path).
        w.set_node_count(WELL_KNOWN_COUNT);

        // Re-layout visible text with new scroll offset.
        let all_runs = layout_mono_lines(
            &long_text,
            chars_per_line,
            line_height as i16,
            text_color,
            char_width,
            font_size,
        );
        let visible_runs = scroll_runs(all_runs, scroll_lines, line_height as u32, 700);

        let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
        for mut run in visible_runs {
            let line_text = line_bytes_for_run(&long_text, &run);
            let shaped = bytes_to_shaped_glyphs(line_text, char_width);
            run.glyphs = w.push_shaped_glyphs(&shaped);
            run.glyph_count = shaped.len() as u16;
            final_runs.push(run);
        }
        let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

        // Update N_DOC_TEXT with new scroll-adjusted content.
        {
            let n = w.node_mut(N_DOC_TEXT);
            n.content = Content::Text {
                runs: doc_runs_ref,
                run_count: doc_run_count,
                _pad: [0; 2],
            };
            n.content_hash = fnv1a(&long_text);
        }
        w.mark_changed(N_DOC_TEXT);

        // Update cursor position for new scroll offset.
        // Cursor at position 0, now adjusted for scroll.
        let (cursor_line, cursor_col) = byte_to_line_col(&long_text, 0, chars_per_line);
        let cursor_x = (cursor_col as u32 * char_width as u32) as i16;
        let cursor_y = (cursor_line as i32 * line_height as i32 - scroll_px) as i16;
        {
            let n = w.node_mut(N_CURSOR);
            n.x = cursor_x;
            n.y = cursor_y;
            n.next_sibling = NULL;
        }
        w.mark_changed(N_CURSOR);
    }
    dw.swap();

    let nodes_after: Vec<Node> = dw.front_nodes().to_vec();
    assert_eq!(nodes_after.len(), 8);

    // Chrome nodes (root, title bar, title text, clock text, shadow, content)
    // should be byte-identical — scroll only affects doc text and cursor.
    let chrome_indices = [N_ROOT, N_TITLE_BAR, N_TITLE_TEXT, N_CLOCK_TEXT, N_SHADOW, N_CONTENT];
    for &i in &chrome_indices {
        let before = unsafe {
            core::slice::from_raw_parts(
                &nodes_before[i as usize] as *const Node as *const u8,
                node_size,
            )
        };
        let after = unsafe {
            core::slice::from_raw_parts(
                &nodes_after[i as usize] as *const Node as *const u8,
                node_size,
            )
        };
        assert_eq!(
            before, after,
            "Chrome node {} should be byte-identical after scroll-only update",
            i
        );
    }

    // N_DOC_TEXT should have changed (new text runs for scrolled viewport).
    let doc_before = unsafe {
        core::slice::from_raw_parts(
            &nodes_before[N_DOC_TEXT as usize] as *const Node as *const u8,
            node_size,
        )
    };
    let doc_after = unsafe {
        core::slice::from_raw_parts(
            &nodes_after[N_DOC_TEXT as usize] as *const Node as *const u8,
            node_size,
        )
    };
    assert_ne!(
        doc_before, doc_after,
        "N_DOC_TEXT should have changed after scroll (new text runs)"
    );

    // N_CURSOR should have changed (y adjusted for scroll offset).
    let cursor_after = &nodes_after[N_CURSOR as usize];
    // Cursor at byte 0 = line 0, col 0. With scroll_lines=5, y = 0*20 - 5*20 = -100.
    assert_eq!(cursor_after.y, -100, "Cursor y should be adjusted for scroll");

    // Change list should contain exactly N_DOC_TEXT and N_CURSOR.
    let dr = DoubleReader::new(&buf);
    let cl = dr.change_list().unwrap();
    assert_eq!(cl.len(), 2, "Change list should have exactly 2 entries");
    assert!(cl.contains(&N_DOC_TEXT), "Change list should contain N_DOC_TEXT");
    assert!(cl.contains(&N_CURSOR), "Change list should contain N_CURSOR");
}

// set_node_count unit test
#[test]
fn set_node_count_truncates() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    for _ in 0..10 {
        w.alloc_node().unwrap();
    }
    assert_eq!(w.node_count(), 10);

    w.set_node_count(8);
    assert_eq!(w.node_count(), 8);

    // Can alloc again after truncate (reuses slot 8).
    let new_id = w.alloc_node().unwrap();
    assert_eq!(new_id, 8);
    assert_eq!(w.node_count(), 9);
}

// ── FULL_REPAINT sentinel tests (VAL-PIPE-001, VAL-PIPE-002) ───────

/// VAL-PIPE-001: After build_editor_scene (via clear + rebuild + swap),
/// the published front buffer's change_count must equal FULL_REPAINT.
#[test]
fn full_rebuild_sets_full_repaint_sentinel() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Build initial scene (like build_editor_scene: clear, alloc, swap).
    build_test_editor_scene(&mut dw, b"hello world", b"12:34:56");
    dw.swap();

    // The front buffer should have change_count == FULL_REPAINT.
    let dr = DoubleReader::new(&buf);
    assert!(
        dr.is_full_repaint(),
        "After full rebuild (clear + write + swap), is_full_repaint() must be true"
    );
    assert!(
        dr.change_list().is_none(),
        "After full rebuild, change_list() must return None"
    );
}

/// VAL-PIPE-001: After a second full rebuild, FULL_REPAINT is still set.
#[test]
fn second_full_rebuild_also_sets_full_repaint() {
    let mut buf = make_double_buf();

    // First build.
    {
        let mut dw = DoubleWriter::new(&mut buf);
        build_test_editor_scene(&mut dw, b"hello world", b"12:34:56");
        dw.swap();

        // Incremental update (copy-forward + mark_changed).
        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.node_mut(N_CURSOR).x = 10;
            w.mark_changed(N_CURSOR);
        }
        dw.swap();
    }

    // After incremental update, should NOT be full repaint.
    {
        let dr = DoubleReader::new(&buf);
        assert!(!dr.is_full_repaint(), "incremental update should not be FULL_REPAINT");
    }

    // Second full rebuild (simulating data buffer exhaustion fallback).
    {
        let mut dw = DoubleWriter::from_existing(&mut buf);
        build_test_editor_scene(&mut dw, b"hello world again", b"12:35:00");
        dw.swap();
    }

    // Should be full repaint again.
    let dr = DoubleReader::new(&buf);
    assert!(
        dr.is_full_repaint(),
        "Second full rebuild must also set FULL_REPAINT"
    );
}

/// VAL-PIPE-002: Compositor decision logic — is_full_repaint returns true,
/// change_list returns None. The compositor must never skip a full-rebuild
/// frame via the empty-change-list early-exit.
#[test]
fn compositor_never_skips_full_rebuild_frame() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Full rebuild.
    build_test_editor_scene(&mut dw, b"test text", b"00:00:00");
    dw.swap();

    let dr = DoubleReader::new(&buf);

    // The compositor's decision path checks:
    // 1. if dr.is_full_repaint() → damage.mark_full_screen() → render everything
    // 2. else match dr.change_list() { Some([]) => skip, Some(list) => partial, None => full }
    //
    // After a full rebuild, path 1 fires. Verify the conditions:
    assert!(dr.is_full_repaint());
    assert!(dr.change_list().is_none());

    // Specifically, change_list() must NOT return Some(empty_slice), which
    // would cause the compositor to skip the frame.
    if let Some(cl) = dr.change_list() {
        panic!(
            "change_list() returned Some({:?}) after full rebuild — compositor would skip frame!",
            cl
        );
    }
}

// ── Timer-only update tests (VAL-PIPE-009) ──────────────────────────

/// VAL-PIPE-009: Timer-only update produces change_count==1 with N_CLOCK_TEXT.
#[test]
fn timer_only_update_produces_change_count_1() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Initial scene build.
    build_test_editor_scene(&mut dw, b"hello", b"12:34:56");
    dw.swap();

    // Simulate timer-only update (like update_clock in SceneState).
    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        // Read the clock node's glyph DataRef and update in-place.
        let clock_node = w.node(N_CLOCK_TEXT);
        if let Content::Text { runs, .. } = clock_node.content {
            let data_buf = w.data_buf();
            let run_offset = runs.offset as usize;
            let run_size = core::mem::size_of::<TextRun>();
            if run_offset + run_size <= data_buf.len() {
                let run_ptr = unsafe {
                    data_buf.as_ptr().add(run_offset) as *const TextRun
                };
                let text_run = unsafe { core::ptr::read(run_ptr) };
                let new_glyphs = bytes_to_shaped_glyphs(b"12:35:00", text_run.advance);
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };
                assert!(w.update_data(text_run.glyphs, new_bytes));
            }
        }
        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(b"12:35:00");
        w.mark_changed(N_CLOCK_TEXT);
    }
    dw.swap();

    // Verify change_count == 1, NOT FULL_REPAINT.
    let dr = DoubleReader::new(&buf);
    assert!(
        !dr.is_full_repaint(),
        "Timer-only update must NOT set FULL_REPAINT"
    );
    let cl = dr.change_list().expect("Timer-only update must have a change list");
    assert_eq!(cl.len(), 1, "Timer-only update must have exactly 1 changed node");
    assert_eq!(cl[0], N_CLOCK_TEXT, "The changed node must be N_CLOCK_TEXT");
}

// ── PREV_BOUNDS large coordinate tests (VAL-PIPE-013) ──────────────

/// VAL-PIPE-013: At scale=2, nodes near the framebuffer edge produce
/// physical coordinates that may exceed i16 range (32767). The damage
/// tracking types must handle this without truncation.
///
/// This tests the abs_bounds function output range — values can exceed
/// i16 when multiplied by scale. PREV_BOUNDS must use a wide enough
/// type (i32 for x/y) to avoid truncation.
#[test]
fn abs_bounds_large_coords_no_truncation() {
    // Node at logical position (500, 400) with scale=2 → physical (1000, 800).
    // These fit in i16 (max 32767). But at scale=2 with a 2048-wide display,
    // a node at logical x=16000 would overflow i16.
    //
    // Test: verify abs_bounds returns correct values for large logical coords.
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 2048;
    w.node_mut(root).height = 1536;
    w.set_root(root);

    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = i16::MAX; // 32767
    w.node_mut(child).y = i16::MAX;
    w.node_mut(child).width = 100;
    w.node_mut(child).height = 50;
    w.add_child(root, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 2);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // Physical coords at scale=2 would be 32767*2 = 65534, which doesn't
    // fit in i16 but does fit in i32/u16. The abs_bounds returns i32 values.
    assert_eq!(ax, 32767, "abs_bounds should return full i32 values");
    assert_eq!(ay, 32767);
    assert_eq!(aw, 100);
    assert_eq!(ah, 50);

    // At scale=2, physical x = ax * 2 = 65534. This exceeds i16::MAX (32767).
    // The compositor's PREV_BOUNDS must use i32 for x/y to avoid truncation.
    let physical_x = ax * 2i32;
    assert_eq!(physical_x, 65534);
    assert!(physical_x > i16::MAX as i32, "physical coord exceeds i16 range");
    // But it fits in i32 and can be safely clamped to u16 for damage rects.
    assert!(physical_x >= 0);
    assert!(physical_x <= u16::MAX as i32);
}

// ── Data buffer compaction tests (VAL-PIPE-004, VAL-PIPE-005, VAL-PIPE-006, VAL-CROSS-013) ──

/// Helper: simulate what update_document_content does with compaction.
/// After copy_front_to_back, resets data buffer and re-pushes all text
/// data (title, clock, document). This is the fixed behavior.
fn incremental_update_with_compaction(
    dw: &mut DoubleWriter<'_>,
    doc_text: &[u8],
    clock_text: &[u8],
    title_text: &[u8],
) {
    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let clock_color = Color::rgb(130, 130, 130);
    let chars_per_line: usize = 80;

    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        // Truncate selection nodes.
        w.set_node_count(WELL_KNOWN_COUNT);

        // ── Compaction: reset data and re-push everything ──
        w.reset_data();

        // Re-push title glyph data.
        let title_glyphs = bytes_to_shaped_glyphs(title_text, char_width);
        let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

        // Re-push clock glyph data.
        let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width);
        let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

        // Re-layout and push doc text.
        let all_runs = layout_mono_lines(
            doc_text,
            chars_per_line,
            line_height as i16,
            text_color,
            char_width,
            font_size,
        );
        let visible_runs = scroll_runs(all_runs, 0, line_height as u32, 700);
        let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
        for mut run in visible_runs {
            let line_text = line_bytes_for_run(doc_text, &run);
            let shaped = bytes_to_shaped_glyphs(line_text, char_width);
            run.glyphs = w.push_shaped_glyphs(&shaped);
            run.glyph_count = shaped.len() as u16;
            final_runs.push(run);
        }
        let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

        // Re-push title/clock TextRuns.
        let title_run = TextRun {
            glyphs: title_glyph_ref,
            glyph_count: title_glyphs.len() as u16,
            x: 0,
            y: 0,
            color: text_color,
            advance: char_width,
            font_size,
            axis_hash: 0,
        };
        let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

        let clock_run = TextRun {
            glyphs: clock_glyph_ref,
            glyph_count: clock_glyphs.len() as u16,
            x: 0,
            y: 0,
            color: clock_color,
            advance: char_width,
            font_size,
            axis_hash: 0,
        };
        let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

        // Update node content references.
        {
            let n = w.node_mut(N_DOC_TEXT);
            n.content = Content::Text {
                runs: doc_runs_ref,
                run_count: doc_run_count,
                _pad: [0; 2],
            };
            n.content_hash = fnv1a(doc_text);
        }
        w.mark_changed(N_DOC_TEXT);

        {
            let n = w.node_mut(N_TITLE_TEXT);
            n.content = Content::Text {
                runs: title_runs_ref,
                run_count: title_run_count,
                _pad: [0; 2],
            };
        }

        {
            let n = w.node_mut(N_CLOCK_TEXT);
            n.content = Content::Text {
                runs: clock_runs_ref,
                run_count: clock_run_count,
                _pad: [0; 2],
            };
        }

        // Update cursor.
        let cursor_pos = doc_text.len();
        let (line, col) = byte_to_line_col(doc_text, cursor_pos, chars_per_line);
        let n = w.node_mut(N_CURSOR);
        n.x = (col as u32 * char_width as u32) as i16;
        n.y = (line as i32 * line_height as i32) as i16;
        n.next_sibling = NULL;
        w.mark_changed(N_CURSOR);
    }
    dw.swap();
}

/// VAL-PIPE-004: After 50 single-char insertions, data_used < 50% of DATA_BUFFER_SIZE.
#[test]
fn data_buffer_compaction_50_inserts_under_50_percent() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let mut doc_text: Vec<u8> = b"hello world".to_vec();
    let clock_text = b"12:00:00";
    let title_text = b"Text";

    // Build initial scene.
    build_test_editor_scene(&mut dw, &doc_text, clock_text);
    dw.swap();

    // 50 single-char insertions with compaction.
    for i in 0..50 {
        let ch = b'a' + (i % 26) as u8;
        doc_text.push(ch);
        incremental_update_with_compaction(&mut dw, &doc_text, clock_text, title_text);
    }

    let data_used = dw.front_data_buf().len();
    let threshold_50 = DATA_BUFFER_SIZE / 2;
    assert!(
        data_used < threshold_50,
        "VAL-PIPE-004: After 50 inserts, data_used {} should be < 50% of {} (threshold {})",
        data_used,
        DATA_BUFFER_SIZE,
        threshold_50
    );
}

/// VAL-PIPE-005: data_used at update 100 < 2x data_used at update 10.
/// Uses a narrow viewport (10 chars/line, 5 visible lines) so visible
/// content is bounded even as total text grows — data_used should stabilize.
#[test]
fn data_buffer_usage_stable_under_sustained_typing() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let clock_color = Color::rgb(130, 130, 130);
    let chars_per_line: usize = 10;
    let viewport_height_px: i32 = 5 * line_height as i32; // 5 visible lines

    let mut doc_text: Vec<u8> = b"hello".to_vec();
    let clock_text = b"12:00:00";
    let title_text = b"Text";

    // Build initial scene.
    build_test_editor_scene(&mut dw, &doc_text, clock_text);
    dw.swap();

    let mut data_used_at_10: usize = 0;

    for i in 0..100 {
        let ch = b'a' + (i % 26) as u8;
        doc_text.push(ch);

        // Compute auto-scroll to keep cursor visible.
        let cursor_pos = doc_text.len();
        let (cursor_line, _cursor_col) =
            byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
        let visible_lines = (viewport_height_px / line_height as i32) as u32;
        let scroll_lines = if cursor_line as u32 >= visible_lines {
            cursor_line as u32 - visible_lines + 1
        } else {
            0
        };

        dw.copy_front_to_back();
        {
            let mut w = dw.back();
            w.set_node_count(WELL_KNOWN_COUNT);

            // Compaction: reset data and re-push everything.
            w.reset_data();

            let title_glyphs = bytes_to_shaped_glyphs(title_text, char_width);
            let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

            let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width);
            let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

            let all_runs = layout_mono_lines(
                &doc_text,
                chars_per_line,
                line_height as i16,
                text_color,
                char_width,
                font_size,
            );
            let visible_runs =
                scroll_runs(all_runs, scroll_lines, line_height as u32, viewport_height_px);
            let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
            for mut run in visible_runs {
                let line_text = line_bytes_for_run(&doc_text, &run);
                let shaped = bytes_to_shaped_glyphs(line_text, char_width);
                run.glyphs = w.push_shaped_glyphs(&shaped);
                run.glyph_count = shaped.len() as u16;
                final_runs.push(run);
            }
            let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

            let title_run = TextRun {
                glyphs: title_glyph_ref,
                glyph_count: title_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: text_color,
                advance: char_width,
                font_size,
                axis_hash: 0,
            };
            let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

            let clock_run = TextRun {
                glyphs: clock_glyph_ref,
                glyph_count: clock_glyphs.len() as u16,
                x: 0,
                y: 0,
                color: clock_color,
                advance: char_width,
                font_size,
                axis_hash: 0,
            };
            let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

            {
                let n = w.node_mut(N_DOC_TEXT);
                n.content = Content::Text {
                    runs: doc_runs_ref,
                    run_count: doc_run_count,
                    _pad: [0; 2],
                };
                n.content_hash = fnv1a(&doc_text);
            }
            w.mark_changed(N_DOC_TEXT);

            {
                let n = w.node_mut(N_TITLE_TEXT);
                n.content = Content::Text {
                    runs: title_runs_ref,
                    run_count: title_run_count,
                    _pad: [0; 2],
                };
            }

            {
                let n = w.node_mut(N_CLOCK_TEXT);
                n.content = Content::Text {
                    runs: clock_runs_ref,
                    run_count: clock_run_count,
                    _pad: [0; 2],
                };
            }

            let scroll_px = scroll_lines as i32 * line_height as i32;
            let (line, col) =
                byte_to_line_col(&doc_text, cursor_pos, chars_per_line);
            let n = w.node_mut(N_CURSOR);
            n.x = (col as u32 * char_width as u32) as i16;
            n.y = (line as i32 * line_height as i32 - scroll_px) as i16;
            n.next_sibling = NULL;
            w.mark_changed(N_CURSOR);
        }
        dw.swap();

        if i == 9 {
            data_used_at_10 = dw.front_data_buf().len();
        }
    }

    let data_used_at_100 = dw.front_data_buf().len();
    assert!(
        data_used_at_100 < data_used_at_10 * 2,
        "VAL-PIPE-005: data_used at 100 ({}) should be < 2x data_used at 10 ({})",
        data_used_at_100,
        data_used_at_10
    );
}

/// VAL-PIPE-006: Zero build_editor_scene fallbacks during 100 chars of typing.
/// With compaction, the data buffer never exceeds 75% so the fallback is never triggered.
#[test]
fn zero_full_rebuild_fallbacks_during_100_chars() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let mut doc_text: Vec<u8> = b"hello".to_vec();
    let clock_text = b"12:00:00";
    let title_text = b"Text";

    build_test_editor_scene(&mut dw, &doc_text, clock_text);
    dw.swap();

    let threshold = (DATA_BUFFER_SIZE as u32 * 3) / 4;
    let mut fallback_count = 0u32;

    for i in 0..100 {
        let ch = b'a' + (i % 26) as u8;
        doc_text.push(ch);

        // Check if fallback would be triggered (before compaction fix, it would be).
        let front_data_used = dw.front_data_buf().len() as u32;
        if front_data_used > threshold {
            fallback_count += 1;
        }

        incremental_update_with_compaction(&mut dw, &doc_text, clock_text, title_text);
    }

    assert_eq!(
        fallback_count, 0,
        "VAL-PIPE-006: Expected zero fallbacks but got {} during 100 chars of typing",
        fallback_count
    );
}

/// VAL-CROSS-013: Under sustained typing, data_used never permanently exceeds 75%.
#[test]
fn data_buffer_growth_bounded_per_frame() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let mut doc_text: Vec<u8> = b"start".to_vec();
    let clock_text = b"12:00:00";
    let title_text = b"Text";

    build_test_editor_scene(&mut dw, &doc_text, clock_text);
    dw.swap();

    let threshold_75 = (DATA_BUFFER_SIZE * 3) / 4;

    // Simulate 200 typing events (sustained typing).
    for i in 0..200 {
        let ch = b'a' + (i % 26) as u8;
        doc_text.push(ch);
        incremental_update_with_compaction(&mut dw, &doc_text, clock_text, title_text);

        let data_used = dw.front_data_buf().len();
        assert!(
            data_used <= threshold_75,
            "VAL-CROSS-013: At update {}, data_used {} exceeds 75% threshold {}",
            i,
            data_used,
            threshold_75
        );
    }
}

/// Verify that after compaction, text content is still correctly readable.
/// The compositor should be able to resolve all DataRefs and read valid glyphs.
#[test]
fn compacted_data_readable_by_reader() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    let mut doc_text: Vec<u8> = b"hello world".to_vec();
    let clock_text = b"12:00:00";
    let title_text = b"Text";

    build_test_editor_scene(&mut dw, &doc_text, clock_text);
    dw.swap();

    // Insert 10 characters with compaction.
    for _ in 0..10 {
        doc_text.push(b'x');
        incremental_update_with_compaction(&mut dw, &doc_text, clock_text, title_text);
    }

    // Read back via DoubleReader and verify data is valid.
    let dr = DoubleReader::new(&buf);
    let nodes = dr.front_nodes();

    // Verify doc text node has valid content.
    let doc_node = &nodes[N_DOC_TEXT as usize];
    if let Content::Text { runs, run_count, .. } = doc_node.content {
        assert!(run_count > 0, "doc text should have at least one run");
        let run_bytes = dr.front_data(runs);
        assert!(
            !run_bytes.is_empty(),
            "doc text runs DataRef should resolve to non-empty data"
        );

        // Verify first TextRun's glyph data resolves.
        let text_runs = dr.front_text_runs(runs);
        assert!(!text_runs.is_empty(), "should have at least one TextRun");
        let first_run = &text_runs[0];
        let glyphs = dr.front_shaped_glyphs(first_run.glyphs, first_run.glyph_count);
        assert!(
            !glyphs.is_empty(),
            "first run should have resolvable glyph data"
        );
    } else {
        panic!("N_DOC_TEXT should have Content::Text");
    }

    // Verify clock text node has valid content.
    let clock_node = &nodes[N_CLOCK_TEXT as usize];
    if let Content::Text { runs, run_count, .. } = clock_node.content {
        assert!(run_count > 0, "clock should have at least one run");
        let run_bytes = dr.front_data(runs);
        assert!(!run_bytes.is_empty(), "clock runs should resolve");
    } else {
        panic!("N_CLOCK_TEXT should have Content::Text");
    }

    // Verify title text node has valid content.
    let title_node = &nodes[N_TITLE_TEXT as usize];
    if let Content::Text { runs, run_count, .. } = title_node.content {
        assert!(run_count > 0, "title should have at least one run");
        let run_bytes = dr.front_data(runs);
        assert!(!run_bytes.is_empty(), "title runs should resolve");
    } else {
        panic!("N_TITLE_TEXT should have Content::Text");
    }
}

// ── VAL-PIPE-007: Double-buffer read consistency ────────────────────
//
// When core calls copy_front_to_back() + modify + swap(), the published
// front buffer must contain a complete, consistent frame — all fields
// from the same write cycle. Exercises 10,000 write-read cycles.
//
// The protocol is designed for cross-process shared memory (separate
// address spaces). In a single-process test, concurrent &mut/& references
// to the same buffer constitute Rust aliasing UB, which causes the
// compiler to miscompile consistency checks. We therefore test the
// protocol sequentially: write → swap → read-via-writer → ack → repeat.
// This validates the double-buffer invariant (copy, modify, swap, read
// produces consistent frames) without scheduling or aliasing issues.

#[test]
fn double_buffer_read_consistency_race() {
    let len = DOUBLE_SCENE_SIZE;
    let mut buf = vec![0u8; len];
    let mut dw = DoubleWriter::new(&mut buf);

    // Write initial frame: one node with width=1, background red.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 1;
        w.node_mut(root).background = Color::rgb(255, 0, 0);
        w.set_root(root);
    }
    dw.swap();
    dw.ack_reader(dw.front_generation());

    let mut inconsistencies = 0u64;
    let mut reads = 0u64;
    let mut last_gen = 0u32;

    for frame_id in 2u32..10_002 {
        // Writer phase: copy front → back, modify, swap.
        let copied = dw.copy_front_to_back();
        assert!(
            copied,
            "copy_front_to_back should succeed after ack (frame {})",
            frame_id
        );
        {
            let mut w = dw.back();
            let marker = (frame_id & 0xFFFF) as u16;
            w.node_mut(0).width = marker;
            w.node_mut(0).height = marker;
            w.node_mut(0).background = Color::rgb(
                (marker & 0xFF) as u8,
                ((marker >> 8) & 0xFF) as u8,
                0,
            );
        }
        dw.swap();

        // Reader phase: read front buffer via DoubleWriter's front
        // accessors (avoids aliasing UB from concurrent &mut/&).
        let gen = dw.front_generation();
        assert!(gen > 0, "generation should be non-zero at frame {}", frame_id);

        let nodes = dw.front_nodes();
        assert!(
            !nodes.is_empty(),
            "front should have nodes at frame {}",
            frame_id
        );

        let node = &nodes[0];
        let w = node.width;
        let h = node.height;
        let bg_marker =
            (node.background.r as u16) | ((node.background.g as u16) << 8);

        // All three fields must agree (same frame).
        if w != h || w != bg_marker {
            inconsistencies += 1;
        }

        // Generation must monotonically increase.
        assert!(
            gen >= last_gen,
            "generation went backwards: {} -> {} at frame {}",
            last_gen,
            gen,
            frame_id
        );
        last_gen = gen;
        reads += 1;

        // Acknowledge the read so writer can reuse this buffer.
        dw.ack_reader(gen);
    }

    assert!(reads > 0, "reader should have completed at least one read");
    assert_eq!(
        inconsistencies, 0,
        "reader saw {} torn frames out of {} reads",
        inconsistencies, reads
    );
}

// ── VAL-PIPE-008: Rapid swap cycles preserve latest frame ───────────
//
// If core performs two complete swap cycles before the compositor reads,
// the compositor sees the second (latest) frame. Generation counter
// monotonically increases.

#[test]
fn rapid_swap_cycles_preserve_latest_frame() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: write width=100.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 100;
        w.node_mut(root).height = 100;
        w.set_root(root);
    }
    dw.swap();
    let gen1 = dw.front_generation();

    // Acknowledge frame 1 so frame 2 can proceed.
    dw.ack_reader(gen1);

    // Frame 2: write width=200.
    assert!(dw.copy_front_to_back(), "copy should succeed after ack");
    {
        let mut w = dw.back();
        w.node_mut(0).width = 200;
        w.node_mut(0).height = 200;
    }
    dw.swap();
    let gen2 = dw.front_generation();

    // Acknowledge frame 2.
    dw.ack_reader(gen2);

    // Frame 3: write width=300.
    assert!(dw.copy_front_to_back(), "copy should succeed after ack");
    {
        let mut w = dw.back();
        w.node_mut(0).width = 300;
        w.node_mut(0).height = 300;
    }
    dw.swap();
    let gen3 = dw.front_generation();

    // Verify monotonically increasing generations.
    assert!(gen2 > gen1, "gen2 ({}) should be > gen1 ({})", gen2, gen1);
    assert!(gen3 > gen2, "gen3 ({}) should be > gen2 ({})", gen3, gen2);

    // Reader sees the latest (frame 3) content — drop dw to allow immutable borrow.
    drop(dw);
    let dr = DoubleReader::new(&buf);
    let gen = dr.front_generation();
    assert_eq!(gen, gen3);
    let nodes = dr.front_nodes();
    assert_eq!(nodes[0].width, 300, "should see latest frame (width=300)");
    assert_eq!(nodes[0].height, 300, "should see latest frame (height=300)");
}

// ── VAL-PIPE-008b: Two rapid swaps without ack — second copy blocked ──
//
// If core does two swap cycles without the compositor acknowledging the
// first, the second copy_front_to_back should return false to prevent
// overwriting the buffer the compositor may still be reading.

#[test]
fn rapid_swap_without_ack_blocks_copy() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Frame 1: write to back, swap.
    {
        let mut w = dw.back();
        w.clear();
        let root = w.alloc_node().unwrap();
        w.node_mut(root).width = 100;
        w.set_root(root);
    }
    dw.swap();
    let gen1 = dw.front_generation();

    // Compositor acknowledges frame 1.
    dw.ack_reader(gen1);

    // Frame 2: copy, modify, swap. Do NOT acknowledge.
    assert!(dw.copy_front_to_back());
    {
        let mut w = dw.back();
        w.node_mut(0).width = 200;
    }
    dw.swap();
    // gen2 is now front. The old front (gen1 buffer) is now back.
    // The reader has acknowledged gen1, so back is safe.

    // Frame 3: copy should work because reader ack'd gen1 (which is now back).
    assert!(
        dw.copy_front_to_back(),
        "copy should succeed because reader ack'd the gen that is now back"
    );
    {
        let mut w = dw.back();
        w.node_mut(0).width = 300;
    }
    dw.swap();
    // Now gen3 is front. Gen2 buffer is back.
    // Reader has NOT acknowledged gen2.

    // Frame 4: copy should FAIL because reader hasn't ack'd gen2 (now back).
    assert!(
        !dw.copy_front_to_back(),
        "copy should fail — reader hasn't ack'd the buffer that is now back"
    );
}

// ── Timer+input coincidence tests (VAL-PIPE-010) ───────────────────

/// VAL-PIPE-010: When timer fires simultaneously with text_changed, the
/// incremental path (update_document_content) must also update the clock
/// — both in a single copy/swap cycle. The change list must include both
/// N_CLOCK_TEXT and document nodes. Must NOT use full rebuild.
#[test]
fn timer_plus_text_changed_uses_incremental_path_with_clock() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    // Build initial scene with doc "hello" and clock "12:00:00".
    build_test_editor_scene(&mut dw, b"hello", b"12:00:00");
    dw.swap();

    // Snapshot the clock hash before the update.
    let clock_hash_before = dw.front_nodes()[N_CLOCK_TEXT as usize].content_hash;

    // Acknowledge initial frame (needed by double-buffer protocol).
    let gen = dw.front_generation();
    dw.ack_reader(gen);

    // Simulate timer+text_changed coincidence using the incremental
    // compaction path (same as update_document_content), but also
    // updating clock text and marking N_CLOCK_TEXT changed.
    let new_doc = b"hellox";
    let new_clock = b"12:00:01";

    assert!(dw.copy_front_to_back());
    {
        let mut w = dw.back();

        w.set_node_count(WELL_KNOWN_COUNT);
        w.reset_data();

        let char_width: u16 = 8;
        let line_height: u16 = 20;
        let font_size: u16 = 16;
        let text_color = Color::rgb(220, 220, 220);
        let clock_color = Color::rgb(130, 130, 130);
        let chars_per_line: usize = 80;

        // Re-push title.
        let title_glyphs = bytes_to_shaped_glyphs(b"Text", char_width);
        let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);
        let title_run = TextRun {
            glyphs: title_glyph_ref, glyph_count: 4, x: 0, y: 0,
            color: text_color, advance: char_width, font_size, axis_hash: 0,
        };
        let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

        // Re-push clock with NEW time.
        let clock_glyphs = bytes_to_shaped_glyphs(new_clock, char_width);
        let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);
        let clock_run = TextRun {
            glyphs: clock_glyph_ref, glyph_count: clock_glyphs.len() as u16, x: 0, y: 0,
            color: clock_color, advance: char_width, font_size, axis_hash: 0,
        };
        let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

        // Re-layout doc text.
        let all_runs = layout_mono_lines(
            new_doc, chars_per_line, line_height as i16, text_color, char_width, font_size,
        );
        let visible_runs = scroll_runs(all_runs, 0, line_height as u32, 700);
        let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
        for mut run in visible_runs {
            let line_text = line_bytes_for_run(new_doc, &run);
            let shaped = bytes_to_shaped_glyphs(line_text, char_width);
            run.glyphs = w.push_shaped_glyphs(&shaped);
            run.glyph_count = shaped.len() as u16;
            final_runs.push(run);
        }
        let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

        // Update all text content references.
        {
            let n = w.node_mut(N_TITLE_TEXT);
            n.content = Content::Text { runs: title_runs_ref, run_count: title_run_count, _pad: [0; 2] };
            n.content_hash = fnv1a(b"Text");
        }
        {
            let n = w.node_mut(N_CLOCK_TEXT);
            n.content = Content::Text { runs: clock_runs_ref, run_count: clock_run_count, _pad: [0; 2] };
            n.content_hash = fnv1a(new_clock);
        }
        {
            let n = w.node_mut(N_DOC_TEXT);
            n.content = Content::Text { runs: doc_runs_ref, run_count: doc_run_count, _pad: [0; 2] };
            n.content_hash = fnv1a(new_doc);
        }

        // Mark both document and clock as changed.
        w.mark_changed(N_DOC_TEXT);
        w.mark_changed(N_CURSOR);
        w.mark_changed(N_CLOCK_TEXT);
    }
    dw.swap();

    // Verify: NOT full repaint (incremental path used).
    let dr = DoubleReader::new(&buf);
    assert!(
        !dr.is_full_repaint(),
        "Timer+text_changed must use incremental path, not full rebuild"
    );

    // Verify: change list includes both document and clock nodes.
    let cl = dr.change_list().expect("Must have a change list");
    assert!(
        cl.contains(&N_DOC_TEXT),
        "Change list must include N_DOC_TEXT, got {:?}", cl
    );
    assert!(
        cl.contains(&N_CLOCK_TEXT),
        "Change list must include N_CLOCK_TEXT, got {:?}", cl
    );
    assert!(
        cl.contains(&N_CURSOR),
        "Change list must include N_CURSOR, got {:?}", cl
    );

    // Verify: clock content hash updated.
    let clock_hash_after = dr.front_nodes()[N_CLOCK_TEXT as usize].content_hash;
    assert_ne!(
        clock_hash_before, clock_hash_after,
        "Clock content_hash must change when timer+input coincidence updates clock"
    );
    assert_eq!(clock_hash_after, fnv1a(new_clock));

    // Verify: doc content hash updated.
    let doc_hash = dr.front_nodes()[N_DOC_TEXT as usize].content_hash;
    assert_eq!(doc_hash, fnv1a(new_doc));
}

/// VAL-PIPE-010b: When timer fires simultaneously with selection_changed,
/// the incremental path must update both selection and clock in one frame.
#[test]
fn timer_plus_selection_changed_uses_incremental_path_with_clock() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello world", b"12:00:00");
    dw.swap();

    let clock_hash_before = dw.front_nodes()[N_CLOCK_TEXT as usize].content_hash;

    let gen = dw.front_generation();
    dw.ack_reader(gen);

    // Simulate timer+selection_changed: update cursor, selection rects,
    // AND clock all in one copy/swap cycle.
    let new_clock = b"12:00:01";

    assert!(dw.copy_front_to_back());
    {
        let mut w = dw.back();

        w.set_node_count(WELL_KNOWN_COUNT);

        // Update cursor position.
        let n = w.node_mut(N_CURSOR);
        n.x = 40;  // Move cursor.
        n.y = 0;
        n.next_sibling = NULL;
        w.mark_changed(N_CURSOR);

        // Update clock in-place (same technique as update_clock).
        let clock_node = w.node(N_CLOCK_TEXT);
        if let Content::Text { runs, .. } = clock_node.content {
            let data_buf = w.data_buf();
            let run_offset = runs.offset as usize;
            let run_size = core::mem::size_of::<TextRun>();
            if run_offset + run_size <= data_buf.len() {
                let run_ptr = unsafe {
                    data_buf.as_ptr().add(run_offset) as *const TextRun
                };
                let text_run = unsafe { core::ptr::read(run_ptr) };
                let new_glyphs = bytes_to_shaped_glyphs(new_clock, text_run.advance);
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };
                assert!(w.update_data(text_run.glyphs, new_bytes));
            }
        }
        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(new_clock);
        w.mark_changed(N_CLOCK_TEXT);
    }
    dw.swap();

    // Verify incremental (not full repaint).
    let dr = DoubleReader::new(&buf);
    assert!(
        !dr.is_full_repaint(),
        "Timer+selection_changed must use incremental path"
    );

    // Change list includes both cursor and clock.
    let cl = dr.change_list().expect("Must have a change list");
    assert!(cl.contains(&N_CURSOR), "Change list must include N_CURSOR, got {:?}", cl);
    assert!(cl.contains(&N_CLOCK_TEXT), "Change list must include N_CLOCK_TEXT, got {:?}", cl);

    // Clock actually updated.
    let clock_hash_after = dr.front_nodes()[N_CLOCK_TEXT as usize].content_hash;
    assert_ne!(clock_hash_before, clock_hash_after);
    assert_eq!(clock_hash_after, fnv1a(new_clock));
}

/// VAL-PIPE-010c: When timer fires simultaneously with cursor-only change,
/// the incremental path must update both cursor and clock in one frame.
#[test]
fn timer_plus_cursor_only_uses_incremental_path_with_clock() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello", b"12:00:00");
    dw.swap();

    let clock_hash_before = dw.front_nodes()[N_CLOCK_TEXT as usize].content_hash;

    let gen = dw.front_generation();
    dw.ack_reader(gen);

    // Simulate timer+cursor_only: update cursor position AND clock.
    let new_clock = b"12:00:01";

    assert!(dw.copy_front_to_back());
    {
        let mut w = dw.back();

        // Update cursor position.
        let n = w.node_mut(N_CURSOR);
        n.x = 24;  // Move cursor.
        n.y = 0;
        w.mark_changed(N_CURSOR);

        // Update clock in-place.
        let clock_node = w.node(N_CLOCK_TEXT);
        if let Content::Text { runs, .. } = clock_node.content {
            let data_buf = w.data_buf();
            let run_offset = runs.offset as usize;
            let run_size = core::mem::size_of::<TextRun>();
            if run_offset + run_size <= data_buf.len() {
                let run_ptr = unsafe {
                    data_buf.as_ptr().add(run_offset) as *const TextRun
                };
                let text_run = unsafe { core::ptr::read(run_ptr) };
                let new_glyphs = bytes_to_shaped_glyphs(new_clock, text_run.advance);
                let new_bytes = unsafe {
                    core::slice::from_raw_parts(
                        new_glyphs.as_ptr() as *const u8,
                        new_glyphs.len() * core::mem::size_of::<ShapedGlyph>(),
                    )
                };
                assert!(w.update_data(text_run.glyphs, new_bytes));
            }
        }
        w.node_mut(N_CLOCK_TEXT).content_hash = fnv1a(new_clock);
        w.mark_changed(N_CLOCK_TEXT);
    }
    dw.swap();

    // Verify incremental (not full repaint).
    let dr = DoubleReader::new(&buf);
    assert!(
        !dr.is_full_repaint(),
        "Timer+cursor_only must use incremental path"
    );

    // Change list includes both cursor and clock.
    let cl = dr.change_list().expect("Must have a change list");
    assert!(cl.contains(&N_CURSOR), "Must include N_CURSOR, got {:?}", cl);
    assert!(cl.contains(&N_CLOCK_TEXT), "Must include N_CLOCK_TEXT, got {:?}", cl);

    // Clock updated.
    let clock_hash_after = dr.front_nodes()[N_CLOCK_TEXT as usize].content_hash;
    assert_ne!(clock_hash_before, clock_hash_after);
    assert_eq!(clock_hash_after, fnv1a(new_clock));
}

/// VAL-PIPE-010d: Timer+input coincidence change_count ≤ max incremental
/// (not FULL_REPAINT). Verify the scene is renderable by the compositor.
#[test]
fn timer_plus_input_change_count_bounded() {
    let mut buf = make_double_buf();
    let mut dw = DoubleWriter::new(&mut buf);

    build_test_editor_scene(&mut dw, b"hello", b"12:00:00");
    dw.swap();

    let gen = dw.front_generation();
    dw.ack_reader(gen);

    // Simulate timer+text_changed with full data compaction.
    let new_doc = b"hello!";
    let new_clock = b"12:00:01";

    incremental_update_with_compaction_and_clock(
        &mut dw, new_doc, new_clock, b"Text",
    );

    let dr = DoubleReader::new(&buf);

    // Must not be full repaint.
    assert!(!dr.is_full_repaint());

    // Change list should have at most a few nodes (doc, cursor, clock).
    let cl = dr.change_list().expect("Must have a change list");
    assert!(cl.len() <= 10, "Change count {} is too high", cl.len());
    assert!(cl.contains(&N_CLOCK_TEXT), "Must include clock");
    assert!(cl.contains(&N_DOC_TEXT), "Must include doc text");
}

/// Helper: incremental update with compaction AND clock update.
/// This is what the fixed `update_document_content` should do when
/// timer_fired is true.
fn incremental_update_with_compaction_and_clock(
    dw: &mut DoubleWriter<'_>,
    doc_text: &[u8],
    clock_text: &[u8],
    title_text: &[u8],
) {
    let char_width: u16 = 8;
    let line_height: u16 = 20;
    let font_size: u16 = 16;
    let text_color = Color::rgb(220, 220, 220);
    let clock_color = Color::rgb(130, 130, 130);
    let chars_per_line: usize = 80;

    dw.copy_front_to_back();
    {
        let mut w = dw.back();

        w.set_node_count(WELL_KNOWN_COUNT);
        w.reset_data();

        // Re-push title.
        let title_glyphs = bytes_to_shaped_glyphs(title_text, char_width);
        let title_glyph_ref = w.push_shaped_glyphs(&title_glyphs);

        // Re-push clock.
        let clock_glyphs = bytes_to_shaped_glyphs(clock_text, char_width);
        let clock_glyph_ref = w.push_shaped_glyphs(&clock_glyphs);

        // Re-layout doc text.
        let all_runs = layout_mono_lines(
            doc_text, chars_per_line, line_height as i16, text_color, char_width, font_size,
        );
        let visible_runs = scroll_runs(all_runs, 0, line_height as u32, 700);
        let mut final_runs: Vec<TextRun> = Vec::with_capacity(visible_runs.len());
        for mut run in visible_runs {
            let line_text = line_bytes_for_run(doc_text, &run);
            let shaped = bytes_to_shaped_glyphs(line_text, char_width);
            run.glyphs = w.push_shaped_glyphs(&shaped);
            run.glyph_count = shaped.len() as u16;
            final_runs.push(run);
        }
        let (doc_runs_ref, doc_run_count) = w.push_text_runs(&final_runs);

        let title_run = TextRun {
            glyphs: title_glyph_ref, glyph_count: title_glyphs.len() as u16, x: 0, y: 0,
            color: text_color, advance: char_width, font_size, axis_hash: 0,
        };
        let (title_runs_ref, title_run_count) = w.push_text_runs(&[title_run]);

        let clock_run = TextRun {
            glyphs: clock_glyph_ref, glyph_count: clock_glyphs.len() as u16, x: 0, y: 0,
            color: clock_color, advance: char_width, font_size, axis_hash: 0,
        };
        let (clock_runs_ref, clock_run_count) = w.push_text_runs(&[clock_run]);

        // Update content references.
        {
            let n = w.node_mut(N_DOC_TEXT);
            n.content = Content::Text {
                runs: doc_runs_ref, run_count: doc_run_count, _pad: [0; 2],
            };
            n.content_hash = fnv1a(doc_text);
        }
        w.mark_changed(N_DOC_TEXT);

        {
            let n = w.node_mut(N_TITLE_TEXT);
            n.content = Content::Text {
                runs: title_runs_ref, run_count: title_run_count, _pad: [0; 2],
            };
        }

        {
            let n = w.node_mut(N_CLOCK_TEXT);
            n.content = Content::Text {
                runs: clock_runs_ref, run_count: clock_run_count, _pad: [0; 2],
            };
            n.content_hash = fnv1a(clock_text);
        }
        // Mark clock as changed (the key fix: timer+input marks both).
        w.mark_changed(N_CLOCK_TEXT);

        // Update cursor.
        let cursor_pos = doc_text.len();
        let (line, col) = byte_to_line_col(doc_text, cursor_pos, chars_per_line);
        let n = w.node_mut(N_CURSOR);
        n.x = (col as u32 * char_width as u32) as i16;
        n.y = (line as i32 * line_height as i32) as i16;
        n.next_sibling = NULL;
        w.mark_changed(N_CURSOR);
    }
    dw.swap();
}

// ── VAL-COORD-013: abs_bounds accounts for scroll_y from ancestor nodes ──

/// abs_bounds must subtract parent scroll_y when computing a child's absolute
/// position. A child inside a scrolled container has its effective y position
/// offset by -scroll_y.
#[test]
fn abs_bounds_accounts_for_scroll_y() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    // Root at (0, 0)
    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Scrollable container at (0, 50) with scroll_y = 10
    let container = w.alloc_node().unwrap();
    w.node_mut(container).y = 50;
    w.node_mut(container).width = 800;
    w.node_mut(container).height = 500;
    w.node_mut(container).scroll_y = 10;
    w.add_child(root, container);

    // Child inside the scrolled container at (20, 30)
    let child = w.alloc_node().unwrap();
    w.node_mut(child).x = 20;
    w.node_mut(child).y = 30;
    w.node_mut(child).width = 100;
    w.node_mut(child).height = 40;
    w.add_child(container, child);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 3);
    let (ax, ay, aw, ah) = abs_bounds(nodes, &parent_map, child as usize);

    // Expected: child.x(20) + container.x(0) + root.x(0) = 20
    assert_eq!(ax, 20, "abs_bounds x should sum parent x values");
    // Expected: child.y(30) + container.y(50) - container.scroll_y(10) + root.y(0) = 70
    // NOT 80 (which would be the result without scroll_y subtraction)
    assert_eq!(ay, 70, "abs_bounds y must subtract parent scroll_y");
    assert_eq!(aw, 100);
    assert_eq!(ah, 40);
}

/// abs_bounds with deeply nested scroll containers: scroll_y accumulates.
#[test]
fn abs_bounds_nested_scroll_containers() {
    let mut buf = make_buf();
    let mut w = SceneWriter::new(&mut buf);

    let root = w.alloc_node().unwrap();
    w.node_mut(root).width = 800;
    w.node_mut(root).height = 600;
    w.set_root(root);

    // Outer container scrolled by 5
    let outer = w.alloc_node().unwrap();
    w.node_mut(outer).y = 100;
    w.node_mut(outer).width = 800;
    w.node_mut(outer).height = 400;
    w.node_mut(outer).scroll_y = 5;
    w.add_child(root, outer);

    // Inner container scrolled by 15
    let inner = w.alloc_node().unwrap();
    w.node_mut(inner).y = 20;
    w.node_mut(inner).width = 800;
    w.node_mut(inner).height = 300;
    w.node_mut(inner).scroll_y = 15;
    w.add_child(outer, inner);

    // Leaf at y=10 inside inner
    let leaf = w.alloc_node().unwrap();
    w.node_mut(leaf).x = 5;
    w.node_mut(leaf).y = 10;
    w.node_mut(leaf).width = 50;
    w.node_mut(leaf).height = 20;
    w.add_child(inner, leaf);

    let nodes = w.nodes();
    let parent_map = build_parent_map(nodes, 4);
    let (ax, ay, _aw, _ah) = abs_bounds(nodes, &parent_map, leaf as usize);

    // x: leaf(5) + inner(0) + outer(0) + root(0) = 5
    assert_eq!(ax, 5);
    // y: scroll_y on a node offsets its CHILDREN. So:
    //   leaf.y(10) is offset by inner.scroll_y(15) → 10 - 15 = -5 (relative to inner)
    //   inner.y(20) is offset by outer.scroll_y(5) → 20 - 5 = 15 (relative to outer)
    //   outer.y(100) → 100 (relative to root, no scroll)
    //   Total: -5 + 15 + 100 = 110
    assert_eq!(ay, 110, "abs_bounds must subtract each ancestor's scroll_y");
}

// ── PathCmd round-trip tests ────────────────────────────────────────

#[test]
fn path_cmd_round_trips_through_scene_writer_reader() {
    let mut buf = vec![0u8; SCENE_SIZE];
    let mut writer = SceneWriter::new(&mut buf);

    let cmds = [
        PathCmd::move_to(10, 20),
        PathCmd::line_to(50, 20),
        PathCmd::line_to(30, 50),
        PathCmd::close(),
    ];
    let dref = writer.push_path_cmds(&cmds);

    let reader = SceneReader::new(&buf);
    let read_cmds = reader.path_cmds(dref);

    assert_eq!(read_cmds.len(), 4);
    assert_eq!(read_cmds[0], PathCmd::move_to(10, 20));
    assert_eq!(read_cmds[1], PathCmd::line_to(50, 20));
    assert_eq!(read_cmds[2], PathCmd::line_to(30, 50));
    assert_eq!(read_cmds[3], PathCmd::close());
}

#[test]
fn path_cmd_curve_to_round_trips() {
    let mut buf = vec![0u8; SCENE_SIZE];
    let mut writer = SceneWriter::new(&mut buf);

    let cmds = [
        PathCmd::move_to(0, 0),
        PathCmd::curve_to(10, 0, 20, 10, 20, 20),
        PathCmd::close(),
    ];
    let dref = writer.push_path_cmds(&cmds);

    let reader = SceneReader::new(&buf);
    let read_cmds = reader.path_cmds(dref);

    assert_eq!(read_cmds.len(), 3);
    assert_eq!(read_cmds[0].kind, PathCmdKind::MoveTo);
    assert_eq!(read_cmds[1].kind, PathCmdKind::CurveTo);
    assert_eq!(read_cmds[1].x1, 10);
    assert_eq!(read_cmds[1].y1, 0);
    assert_eq!(read_cmds[1].x2, 20);
    assert_eq!(read_cmds[1].y2, 10);
    assert_eq!(read_cmds[1].x, 20);
    assert_eq!(read_cmds[1].y, 20);
    assert_eq!(read_cmds[2].kind, PathCmdKind::Close);
}

#[test]
fn path_cmd_empty_returns_empty_slice() {
    let mut buf = vec![0u8; SCENE_SIZE];
    let _writer = SceneWriter::new(&mut buf);

    let reader = SceneReader::new(&buf);
    let dref = DataRef {
        offset: 0,
        length: 0,
    };
    let cmds = reader.path_cmds(dref);
    assert!(cmds.is_empty());
}

#[test]
fn path_cmd_size_is_14_bytes() {
    assert_eq!(core::mem::size_of::<PathCmd>(), 14);
}

// ── AffineTransform tests ───────────────────────────────────────────

#[test]
fn affine_transform_identity_is_default() {
    let t = AffineTransform::identity();
    assert_eq!(t.a, 1.0);
    assert_eq!(t.b, 0.0);
    assert_eq!(t.c, 0.0);
    assert_eq!(t.d, 1.0);
    assert_eq!(t.tx, 0.0);
    assert_eq!(t.ty, 0.0);
}

#[test]
fn affine_transform_is_repr_c() {
    // AffineTransform is repr(C) with 6 × f32 = 24 bytes.
    assert_eq!(core::mem::size_of::<AffineTransform>(), 24);
}

#[test]
fn affine_transform_translate() {
    let t = AffineTransform::translate(10.0, 5.0);
    assert_eq!(t.tx, 10.0);
    assert_eq!(t.ty, 5.0);
    assert_eq!(t.a, 1.0);
    assert_eq!(t.d, 1.0);
    assert_eq!(t.b, 0.0);
    assert_eq!(t.c, 0.0);
}

#[test]
fn affine_transform_scale() {
    let t = AffineTransform::scale(2.0, 3.0);
    assert_eq!(t.a, 2.0);
    assert_eq!(t.d, 3.0);
    assert_eq!(t.tx, 0.0);
    assert_eq!(t.ty, 0.0);
}

#[test]
fn affine_transform_rotate_90() {
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_2);
    // cos(90°) ≈ 0, sin(90°) ≈ 1
    assert!((t.a - 0.0).abs() < 1e-5, "a should be ~0, got {}", t.a);
    assert!((t.b - 1.0).abs() < 1e-5, "b should be ~1, got {}", t.b);
    assert!((t.c - (-1.0)).abs() < 1e-5, "c should be ~-1, got {}", t.c);
    assert!((t.d - 0.0).abs() < 1e-5, "d should be ~0, got {}", t.d);
}

#[test]
fn affine_transform_rotate_180() {
    let t = AffineTransform::rotate(core::f32::consts::PI);
    // cos(180°) ≈ -1, sin(180°) ≈ 0
    assert!((t.a - (-1.0)).abs() < 1e-5, "a should be ~-1, got {}", t.a);
    assert!((t.b - 0.0).abs() < 1e-4, "b should be ~0, got {}", t.b);
    assert!((t.c - 0.0).abs() < 1e-4, "c should be ~0, got {}", t.c);
    assert!((t.d - (-1.0)).abs() < 1e-5, "d should be ~-1, got {}", t.d);
}

#[test]
fn affine_transform_skew_x() {
    let angle = 0.5_f32; // ~26.6 degrees
    let t = AffineTransform::skew_x(angle);
    assert_eq!(t.a, 1.0);
    assert_eq!(t.d, 1.0);
    // c = tan(angle) ≈ 0.5463
    let expected_c = angle.tan();
    assert!((t.c - expected_c).abs() < 1e-5, "c should be tan(0.5), got {}", t.c);
    assert_eq!(t.b, 0.0);
}

#[test]
fn affine_transform_compose_translations() {
    let t1 = AffineTransform::translate(100.0, 50.0);
    let t2 = AffineTransform::translate(10.0, 5.0);
    let composed = t1.compose(t2);
    assert!((composed.tx - 110.0).abs() < 1e-5, "tx should be 110, got {}", composed.tx);
    assert!((composed.ty - 55.0).abs() < 1e-5, "ty should be 55, got {}", composed.ty);
}

#[test]
fn affine_transform_compose_scale_then_translate() {
    // Scale(2,2) then translate(10,5):
    // Result: point (x,y) → scale → (2x, 2y) → translate → (2x+10, 2y+5)
    // Matrix: parent × child = translate × scale
    // [1 0 10]   [2 0 0]   [2 0 10]
    // [0 1  5] × [0 2 0] = [0 2  5]
    // [0 0  1]   [0 0 1]   [0 0  1]
    let parent = AffineTransform::translate(10.0, 5.0);
    let child = AffineTransform::scale(2.0, 2.0);
    let composed = parent.compose(child);
    assert!((composed.a - 2.0).abs() < 1e-5);
    assert!((composed.d - 2.0).abs() < 1e-5);
    assert!((composed.tx - 10.0).abs() < 1e-5);
    assert!((composed.ty - 5.0).abs() < 1e-5);
}

#[test]
fn affine_transform_compose_three_levels() {
    // VAL-XFORM-008: Three-level nesting composes correctly.
    let level1 = AffineTransform::translate(100.0, 100.0);
    let level2 = AffineTransform::scale(2.0, 2.0);
    let level3 = AffineTransform::translate(5.0, 5.0);
    // world = level1 × level2 × level3
    let mid = level1.compose(level2);
    let world = mid.compose(level3);
    // Point (0,0) → level3.translate → (5,5) → level2.scale → (10,10) → level1.translate → (110, 110)
    let (rx, ry) = world.transform_point(0.0, 0.0);
    assert!((rx - 110.0).abs() < 1e-4, "x should be 110, got {}", rx);
    assert!((ry - 110.0).abs() < 1e-4, "y should be 110, got {}", ry);
}

#[test]
fn affine_transform_identity_is_identity() {
    let t = AffineTransform::identity();
    assert!(t.is_identity());
    let t2 = AffineTransform::translate(1.0, 0.0);
    assert!(!t2.is_identity());
}

#[test]
fn affine_transform_transform_point() {
    let t = AffineTransform::translate(10.0, 20.0);
    let (x, y) = t.transform_point(5.0, 3.0);
    assert!((x - 15.0).abs() < 1e-5);
    assert!((y - 23.0).abs() < 1e-5);
}

#[test]
fn affine_transform_aabb_identity() {
    let t = AffineTransform::identity();
    let (x, y, w, h) = t.transform_aabb(10.0, 20.0, 30.0, 40.0);
    assert!((x - 10.0).abs() < 1e-5);
    assert!((y - 20.0).abs() < 1e-5);
    assert!((w - 30.0).abs() < 1e-5);
    assert!((h - 40.0).abs() < 1e-5);
}

#[test]
fn affine_transform_aabb_90_rotation() {
    // VAL-XFORM-003: 40x20 node rotated 90° → bounding box ~20x40
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_2);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 40.0, 20.0);
    // After 90° rotation of a 40×20 rect, AABB should be ~20×40.
    assert!((w - 20.0).abs() < 1.0, "AABB width should be ~20, got {}", w);
    assert!((h - 40.0).abs() < 1.0, "AABB height should be ~40, got {}", h);
}

#[test]
fn affine_transform_aabb_45_rotation() {
    // VAL-XFORM-009: 100x100 node rotated 45° has AABB ~141x141
    let t = AffineTransform::rotate(core::f32::consts::FRAC_PI_4);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 100.0, 100.0);
    // sqrt(2) * 100 ≈ 141.42
    assert!((w - 141.42).abs() < 1.0, "AABB width should be ~141, got {}", w);
    assert!((h - 141.42).abs() < 1.0, "AABB height should be ~141, got {}", h);
}

#[test]
fn affine_transform_scale_zero_no_panic() {
    // VAL-XFORM-018: scale(0,0) produces degenerate but no panic.
    let t = AffineTransform::scale(0.0, 0.0);
    let (_, _, w, h) = t.transform_aabb(0.0, 0.0, 10.0, 10.0);
    assert_eq!(w, 0.0);
    assert_eq!(h, 0.0);
}

#[test]
fn affine_transform_negative_scale_mirror() {
    // VAL-XFORM-019: scale(-1,1) produces horizontal mirror.
    let t = AffineTransform::scale(-1.0, 1.0);
    let (px, py) = t.transform_point(10.0, 5.0);
    assert!((px - (-10.0)).abs() < 1e-5);
    assert!((py - 5.0).abs() < 1e-5);
}

#[test]
fn affine_transform_skew_x_parallelogram() {
    // VAL-XFORM-011: skew_x(0.5) on 40x40: bottom edge shifts 20px right.
    let t = AffineTransform::skew_x(0.5_f32.atan()); // tan(angle) = 0.5
    // Bottom-left corner (0, 40): x' = 0 + 0.5*40 = 20, y' = 40
    let (px, _py) = t.transform_point(0.0, 40.0);
    assert!((px - 20.0).abs() < 1e-4, "bottom-left x should shift to ~20, got {}", px);
}

#[test]
fn node_has_transform_field() {
    let node = Node::EMPTY;
    assert!(node.transform.is_identity());
}

#[test]
fn node_size_assertion_with_transform() {
    // VAL-XFORM-022: Node size compile-time assertion.
    // After adding 24-byte AffineTransform, Node should be 96 bytes.
    let size = core::mem::size_of::<Node>();
    assert_eq!(size, 96, "Node size should be 96 bytes with transform field, got {}", size);
}

#[test]
fn double_buffer_swap_preserves_transform_fields() {
    // VAL-CROSS-015: copy_front_to_back preserves transform fields.
    let mut buf = vec![0u8; DOUBLE_SCENE_SIZE];
    let mut dw = DoubleWriter::new(&mut buf);

    {
        let mut sw = dw.back();
        let n = sw.alloc_node().unwrap();
        let node = sw.node_mut(n);
        node.width = 50;
        node.height = 50;
        node.flags = NodeFlags::VISIBLE;
        node.transform = AffineTransform::translate(10.0, 20.0);
        sw.commit();
    }
    dw.swap();
    dw.copy_front_to_back();

    {
        let sw = dw.back();
        let node = sw.node(0);
        assert!((node.transform.tx - 10.0).abs() < 1e-5,
            "transform.tx must survive copy_front_to_back, got {}", node.transform.tx);
        assert!((node.transform.ty - 20.0).abs() < 1e-5,
            "transform.ty must survive copy_front_to_back, got {}", node.transform.ty);
        assert!((node.transform.a - 1.0).abs() < 1e-5,
            "transform.a must survive copy_front_to_back, got {}", node.transform.a);
    }
}
