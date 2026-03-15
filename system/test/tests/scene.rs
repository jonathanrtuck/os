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

    let data_used_before = dw.front_nodes().len(); // just to read front
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
