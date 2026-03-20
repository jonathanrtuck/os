//! Incremental scene graph updates.
//!
//! Contains `update_single_line` (single-line glyph update),
//! `insert_line` (line split on Enter), and `delete_line` (line merge
//! on Backspace at BOL). These modify the existing scene graph rather
//! than rebuilding from scratch.

use alloc::vec::Vec;

use scene::{fnv1a, Color, Content, NodeFlags, NULL};

use super::{
    allocate_selection_rects, byte_to_line_col, chars_per_line, dc, doc_width, layout_mono_lines,
    line_bytes_for_run, shape_text, update_clock_inline, SceneConfig, N_CURSOR, N_DOC_TEXT,
    WELL_KNOWN_COUNT,
};

// ── Incremental scene update ─────────────────────────────────────────

/// Incrementally update a single line's glyph content in the scene.
///
/// Assumes `acquire_copy()` was already called (previous frame preserved).
/// Returns `false` if data buffer is full (caller should fall back to
/// compaction via `build_document_content`).
///
/// Walks the sibling chain from N_DOC_TEXT.first_child to find the node
/// at position `changed_line`, reshapes only that line, pushes new glyph
/// data at the bump pointer (does NOT reset the data buffer), and updates
/// cursor position + optional clock.
#[allow(clippy::too_many_arguments)]
pub fn update_single_line(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    changed_line: usize,
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    scroll_y: i32,
    clock_text: Option<&[u8]>,
) -> bool {
    let scene_text_color = dc(cfg.text_color);
    let cpl = chars_per_line(cfg);

    // Layout all lines to find the changed line's text boundaries.
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );

    // Determine which visible lines are in the viewport.
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let scroll_pt = scroll_lines as i32 * cfg.line_height as i32;

    // Count visible runs and compare against the sibling chain length.
    // If they differ, a soft-wrap change occurred — fall back to compaction
    // to avoid stale line nodes rendering old glyphs.
    let visible_run_count = all_runs
        .iter()
        .filter(|r| r.y + cfg.line_height as i32 > scroll_pt && r.y < scroll_pt + content_h)
        .count();
    let mut chain_line_count: usize = 0;
    {
        let mut c = w.node(N_DOC_TEXT).first_child;
        while c != scene::NULL && c != N_CURSOR {
            chain_line_count += 1;
            c = w.node(c).next_sibling;
        }
    }
    if visible_run_count != chain_line_count {
        return false; // Visual line count changed (soft-wrap), fall back.
    }

    // Find the changed line's run index in the full list.
    if changed_line >= all_runs.len() {
        return false; // Line out of range, fall back.
    }

    // Check if the changed line is visible.
    let changed_run = &all_runs[changed_line];
    let run_y = changed_run.y;
    let line_h = cfg.line_height as i32;
    let is_visible = run_y + line_h > scroll_pt && run_y < scroll_pt + content_h;

    if !is_visible {
        // Changed line is off-screen. Only update cursor + clock.
        // (Cursor/selection update below handles this case.)
    } else {
        // Find the visible-line index by counting visible runs before
        // this one.
        let mut visible_index: usize = 0;
        for (i, run) in all_runs.iter().enumerate() {
            if i == changed_line {
                break;
            }
            let ry = run.y;
            if ry + line_h > scroll_pt && ry < scroll_pt + content_h {
                visible_index += 1;
            }
        }

        // Walk sibling chain from N_DOC_TEXT.first_child to find the node
        // at visible_index. Stop at N_CURSOR (it's in the chain after line
        // nodes) or NULL.
        let mut cur = w.node(N_DOC_TEXT).first_child;
        let mut idx: usize = 0;
        while cur != scene::NULL && cur != N_CURSOR && idx < visible_index {
            cur = w.node(cur).next_sibling;
            idx += 1;
        }

        if cur == scene::NULL || cur == N_CURSOR {
            return false; // Node not found, fall back to compaction.
        }

        // Shape the changed line's text.
        let line_text = line_bytes_for_run(doc_text, changed_run);
        let shaped = shape_text(cfg.font_data, line_text, cfg.font_size, cfg.upem, cfg.axes);

        // Check data space before pushing.
        let glyph_bytes = shaped.len() * core::mem::size_of::<scene::ShapedGlyph>();
        // Account for alignment padding (ShapedGlyph align is 2).
        let needed = glyph_bytes + core::mem::align_of::<scene::ShapedGlyph>();
        if !w.has_data_space(needed) {
            return false; // Data buffer full, fall back to compaction.
        }

        // Push new glyph data (old data is abandoned in the bump allocator).
        let new_ref = w.push_shaped_glyphs(&shaped);
        let new_count = shaped.len() as u16;

        // Update the line node.
        let n = w.node_mut(cur);
        n.content = Content::Glyphs {
            color: scene_text_color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = scene::fnv1a(&new_ref.offset.to_le_bytes());
        w.mark_dirty(cur);
    }

    // Update N_DOC_TEXT: content_transform, content_hash, and clear stale
    // next_sibling (build_full_scene links test content as siblings
    // that survive acquire_copy but are truncated on first compaction).
    w.node_mut(N_DOC_TEXT).content_transform =
        scene::AffineTransform::translate(0.0, -(scroll_pt as f32));
    w.node_mut(N_DOC_TEXT).next_sibling = scene::NULL;
    w.node_mut(N_DOC_TEXT).content_hash = scene::fnv1a(doc_text);
    w.mark_dirty(N_DOC_TEXT);

    // Update cursor position.
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
    }
    w.mark_dirty(N_CURSOR);

    // Truncate selection rects and rebuild from current selection state.
    // Walk the chain to find the highest node index — bump-allocated nodes
    // may have indices above WELL_KNOWN_COUNT + line_count (from prior
    // insert_line calls that created gaps). Must use max_node_idx to avoid
    // truncating live nodes at higher indices.
    let mut max_node_idx: u16 = WELL_KNOWN_COUNT.saturating_sub(1);
    let mut child = w.node(N_DOC_TEXT).first_child;
    while child != scene::NULL && child != N_CURSOR {
        if child >= WELL_KNOWN_COUNT && child > max_node_idx {
            max_node_idx = child;
        }
        child = w.node(child).next_sibling;
    }
    w.set_node_count(max_node_idx + 1);
    w.node_mut(N_CURSOR).next_sibling = scene::NULL;

    // Rebuild selection rects if needed.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };
    if sel_lo < sel_hi {
        let content_h_u32 = cfg.fb_height.saturating_sub(content_y);
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            cpl as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h_u32,
            scroll_pt,
        );
    }

    // Update clock if requested.
    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
    }

    true
}

// ── Incremental line insert / delete ─────────────────────────────────

/// Update y positions for all line nodes starting from `start_node`.
/// Walks the sibling chain, sets y = `start_y + index * line_height`.
/// Marks each repositioned node dirty (property-only — content_hash unchanged).
/// Stops at N_CURSOR or NULL.
fn update_line_positions(
    w: &mut scene::SceneWriter<'_>,
    start_node: u16,
    start_y: i32,
    line_height: i32,
) {
    let mut cur = start_node;
    let mut y = start_y;

    while cur != scene::NULL && cur != N_CURSOR {
        let old_y = w.node(cur).y;
        if old_y != y {
            w.node_mut(cur).y = y;
            w.mark_dirty(cur);
        }
        cur = w.node(cur).next_sibling;
        y = y.saturating_add(line_height);
    }
}

/// Shared tail: update cursor, selection, content_transform, N_DOC_TEXT hash,
/// and optionally the clock. Truncates old selection rects before rebuilding.
#[allow(clippy::too_many_arguments)]
fn finish_line_update(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    scroll_y: i32,
    clock_text: Option<&[u8]>,
) {
    let cpl = chars_per_line(cfg);

    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y);
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let scroll_pt = scroll_lines as i32 * cfg.line_height as i32;

    // Update N_DOC_TEXT content_transform and content hash.
    // Clear next_sibling to prevent stale pointers (the initial
    // build_full_scene links test content as siblings of N_DOC_TEXT
    // under N_CONTENT -- those nodes are gone after the first compaction
    // but the pointer survives acquire_copy).
    w.node_mut(N_DOC_TEXT).content_transform =
        scene::AffineTransform::translate(0.0, -(scroll_pt as f32));
    w.node_mut(N_DOC_TEXT).next_sibling = scene::NULL;
    w.node_mut(N_DOC_TEXT).content_hash = fnv1a(doc_text);
    w.mark_dirty(N_DOC_TEXT);

    // Update cursor position.
    let (cursor_line, cursor_col) =
        byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);
    let cursor_x = (cursor_col as u32 * cfg.char_width) as i32;
    let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;

    {
        let n = w.node_mut(N_CURSOR);
        n.x = cursor_x;
        n.y = cursor_y;
    }
    w.mark_dirty(N_CURSOR);

    // Truncate selection rects. Walk the chain to find the highest node
    // index — newly allocated nodes may have indices above the old
    // WELL_KNOWN_COUNT + line_count boundary.
    let mut max_node_idx: u16 = WELL_KNOWN_COUNT.saturating_sub(1);
    let mut child = w.node(N_DOC_TEXT).first_child;
    while child != scene::NULL && child != N_CURSOR {
        if child >= WELL_KNOWN_COUNT && child > max_node_idx {
            max_node_idx = child;
        }
        child = w.node(child).next_sibling;
    }
    // node_count must be at least max_node_idx + 1 to include all live nodes.
    w.set_node_count(max_node_idx + 1);
    w.node_mut(N_CURSOR).next_sibling = scene::NULL;

    // Rebuild selection rects.
    let (sel_lo, sel_hi) = if sel_start <= sel_end {
        (sel_start as usize, sel_end as usize)
    } else {
        (sel_end as usize, sel_start as usize)
    };
    if sel_lo < sel_hi {
        allocate_selection_rects(
            w,
            doc_text,
            sel_lo,
            sel_hi,
            cpl as usize,
            cfg.char_width,
            cfg.line_height,
            dc(cfg.sel_color),
            content_h,
            scroll_pt,
        );
    }

    // Update clock if requested.
    if let Some(ct) = clock_text {
        update_clock_inline(w, ct, cfg.font_data, cfg.font_size, cfg.upem, cfg.axes);
    }
}

/// Insert a new line node after a line split (Enter key).
///
/// Returns `false` if node/data allocation fails (caller falls back to
/// compaction via `build_document_content`).
#[allow(clippy::too_many_arguments)]
pub fn insert_line(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    scroll_y: i32,
    clock_text: Option<&[u8]>,
) -> bool {
    let scene_text_color = dc(cfg.text_color);
    let doc_width = doc_width(cfg);
    let cpl = chars_per_line(cfg);

    // Layout all lines from the new text.
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );

    // Determine visible runs.
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let scroll_pt = scroll_lines as i32 * cfg.line_height as i32;

    let visible_run_count = all_runs
        .iter()
        .filter(|r| r.y + cfg.line_height as i32 > scroll_pt && r.y < scroll_pt + content_h)
        .count();

    // Count current line nodes in the sibling chain.
    let mut chain_len: usize = 0;
    {
        let mut c = w.node(N_DOC_TEXT).first_child;
        while c != scene::NULL && c != N_CURSOR {
            chain_len += 1;
            c = w.node(c).next_sibling;
        }
    }

    // After insert, visible count should be chain_len + 1.
    // If not, something unexpected happened (soft wrap change). Fall back.
    if visible_run_count != chain_len + 1 {
        return false;
    }

    // Find the cursor line in the full run list (the new line after the split).
    let (cursor_line, _) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);

    // The line before the cursor (the modified/shortened line) needs reshaping.
    // The cursor line itself is the newly inserted line.
    // Map these to visible indices.

    // Build a map from full run index to visible run index.
    let mut visible_indices: Vec<usize> = Vec::new();
    for (i, r) in all_runs.iter().enumerate() {
        if r.y + cfg.line_height as i32 > scroll_pt && r.y < scroll_pt + content_h {
            visible_indices.push(i);
        }
    }

    // Find the visible index of cursor_line (the new line).
    let new_line_visible_idx = visible_indices
        .iter()
        .position(|&full_idx| full_idx == cursor_line);

    // If the new line is not visible, just update positions and cursor.
    // But we still need to allocate a node for it (it may scroll into view).
    // Actually, if it's not visible, the visible run count check above would
    // fail. So if we get here, it IS visible.
    let new_line_vis_idx = match new_line_visible_idx {
        Some(idx) => idx,
        None => return false, // Not visible — unexpected. Fall back.
    };

    // The modified line (before cursor) is cursor_line - 1 in the full list.
    let modified_full_idx = if cursor_line > 0 {
        cursor_line - 1
    } else {
        // Cursor is at line 0 — means Enter was pressed at position 0.
        // No previous line to reshape. The new line IS line 0.
        // This is an edge case: fall back for simplicity.
        return false;
    };

    let modified_vis_idx = visible_indices
        .iter()
        .position(|&full_idx| full_idx == modified_full_idx);

    // Walk the sibling chain to find nodes.
    let mut chain_nodes: Vec<u16> = Vec::new();
    {
        let mut c = w.node(N_DOC_TEXT).first_child;
        while c != scene::NULL && c != N_CURSOR {
            chain_nodes.push(c);
            c = w.node(c).next_sibling;
        }
    }

    // Reshape the modified line if it's visible.
    if let Some(mod_vis_idx) = modified_vis_idx {
        if mod_vis_idx >= chain_nodes.len() {
            return false;
        }
        let mod_node = chain_nodes[mod_vis_idx];
        let mod_run = &all_runs[modified_full_idx];
        let line_text = line_bytes_for_run(doc_text, mod_run);
        let shaped = shape_text(cfg.font_data, line_text, cfg.font_size, cfg.upem, cfg.axes);

        let glyph_bytes = shaped.len() * core::mem::size_of::<scene::ShapedGlyph>();
        let needed = glyph_bytes + core::mem::align_of::<scene::ShapedGlyph>();
        if !w.has_data_space(needed) {
            return false;
        }

        let new_ref = w.push_shaped_glyphs(&shaped);
        let new_count = shaped.len() as u16;

        let n = w.node_mut(mod_node);
        n.content = Content::Glyphs {
            color: scene_text_color,
            glyphs: new_ref,
            glyph_count: new_count,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(&new_ref.offset.to_le_bytes());
        w.mark_dirty(mod_node);
    }

    // Shape the new line's content.
    let new_run = &all_runs[cursor_line];
    let new_line_text = line_bytes_for_run(doc_text, new_run);
    let new_shaped = shape_text(
        cfg.font_data,
        new_line_text,
        cfg.font_size,
        cfg.upem,
        cfg.axes,
    );

    let new_glyph_bytes = new_shaped.len() * core::mem::size_of::<scene::ShapedGlyph>();
    let new_needed = new_glyph_bytes + core::mem::align_of::<scene::ShapedGlyph>();
    if !w.has_data_space(new_needed) {
        return false;
    }

    // Allocate new node.
    let new_node = match w.alloc_node() {
        Some(id) => id,
        None => return false,
    };

    let new_glyph_ref = w.push_shaped_glyphs(&new_shaped);

    {
        let n = w.node_mut(new_node);
        n.y = new_run.y;
        n.width = doc_width as u16;
        n.height = cfg.line_height as u16;
        n.content = Content::Glyphs {
            color: scene_text_color,
            glyphs: new_glyph_ref,
            glyph_count: new_shaped.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(&new_glyph_ref.offset.to_le_bytes());
        n.flags = NodeFlags::VISIBLE;
        n.next_sibling = scene::NULL;
    }
    w.mark_dirty(new_node);

    // Link into the chain. The new node goes at position new_line_vis_idx.
    // If new_line_vis_idx == 0, it becomes the first child.
    if new_line_vis_idx == 0 {
        let old_first = w.node(N_DOC_TEXT).first_child;
        w.node_mut(new_node).next_sibling = old_first;
        w.node_mut(N_DOC_TEXT).first_child = new_node;
    } else {
        // Insert after the node at position new_line_vis_idx - 1.
        let prev_node = chain_nodes[new_line_vis_idx - 1];
        let old_next = w.node(prev_node).next_sibling;
        w.node_mut(new_node).next_sibling = old_next;
        w.node_mut(prev_node).next_sibling = new_node;
    }

    // Update y positions for all nodes AFTER the new one.
    let after_new = w.node(new_node).next_sibling;
    if after_new != scene::NULL && after_new != N_CURSOR {
        let next_y = new_run.y.saturating_add(cfg.line_height as i32);
        update_line_positions(w, after_new, next_y, cfg.line_height as i32);
    }

    // Shared tail: cursor, selection, clock.
    finish_line_update(
        w, cfg, doc_text, cursor_pos, sel_start, sel_end, scroll_y, clock_text,
    );

    true
}

/// Delete a line node after a line merge (Backspace at BOL).
///
/// Returns `false` if data allocation fails (caller falls back to
/// compaction via `build_document_content`).
#[allow(clippy::too_many_arguments)]
pub fn delete_line(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    cursor_pos: u32,
    sel_start: u32,
    sel_end: u32,
    scroll_y: i32,
    clock_text: Option<&[u8]>,
) -> bool {
    let scene_text_color = dc(cfg.text_color);
    let cpl = chars_per_line(cfg);

    // Layout all lines from the new text (after deletion).
    let all_runs = layout_mono_lines(
        doc_text,
        cpl as usize,
        cfg.line_height as i32,
        scene_text_color,
        cfg.font_size,
    );

    // Determine visible runs.
    let content_y = cfg.title_bar_h + cfg.shadow_depth;
    let content_h = cfg.fb_height.saturating_sub(content_y) as i32;
    let scroll_lines = if scroll_y > 0 { scroll_y as u32 } else { 0 };
    let scroll_pt = scroll_lines as i32 * cfg.line_height as i32;

    let visible_run_count = all_runs
        .iter()
        .filter(|r| r.y + cfg.line_height as i32 > scroll_pt && r.y < scroll_pt + content_h)
        .count();

    // Count current line nodes in the sibling chain.
    let mut chain_nodes: Vec<u16> = Vec::new();
    {
        let mut c = w.node(N_DOC_TEXT).first_child;
        while c != scene::NULL && c != N_CURSOR {
            chain_nodes.push(c);
            c = w.node(c).next_sibling;
        }
    }
    let chain_len = chain_nodes.len();

    // After delete, visible count should be chain_len - 1.
    if visible_run_count != chain_len.wrapping_sub(1) {
        return false;
    }

    // Find the cursor line (the merged/surviving line).
    let (cursor_line, _) = byte_to_line_col(doc_text, cursor_pos as usize, cpl as usize);

    // Map cursor_line to visible index.
    let mut visible_indices: Vec<usize> = Vec::new();
    for (i, r) in all_runs.iter().enumerate() {
        if r.y + cfg.line_height as i32 > scroll_pt && r.y < scroll_pt + content_h {
            visible_indices.push(i);
        }
    }

    let surviving_vis_idx = match visible_indices
        .iter()
        .position(|&full_idx| full_idx == cursor_line)
    {
        Some(idx) => idx,
        None => return false,
    };

    // The deleted line was the one ABOVE the cursor in the old chain.
    // In the old chain, the surviving line was at surviving_vis_idx and the
    // deleted line was at surviving_vis_idx + 1 (because the old chain had
    // one extra). Wait — the deleted line was the one that was removed, which
    // was the line above the cursor in the previous frame. After backspace at
    // BOL, the cursor's line merged with the line above. In the OLD chain, the
    // line above was at surviving_vis_idx, and the cursor's line was at
    // surviving_vis_idx + 1. The merged result is now at surviving_vis_idx.
    // So we need to delete chain_nodes[surviving_vis_idx + 1].
    let deleted_chain_idx = surviving_vis_idx + 1;

    if deleted_chain_idx >= chain_len {
        return false;
    }

    let deleted_node = chain_nodes[deleted_chain_idx];

    // Unlink the deleted node.
    if deleted_chain_idx == 0 {
        // First child — shouldn't happen for BOL delete (there's always a line above).
        return false;
    }
    let prev_node = chain_nodes[deleted_chain_idx - 1];
    let after_deleted = w.node(deleted_node).next_sibling;
    w.node_mut(prev_node).next_sibling = after_deleted;

    // Mark deleted node invisible (dead slot).
    w.node_mut(deleted_node).flags = NodeFlags::empty();
    w.mark_dirty(deleted_node);

    // Reshape the surviving/merged line.
    if surviving_vis_idx >= chain_nodes.len() {
        return false;
    }
    let surviving_node = chain_nodes[surviving_vis_idx];
    let surviving_run = &all_runs[cursor_line];
    let line_text = line_bytes_for_run(doc_text, surviving_run);
    let shaped = shape_text(cfg.font_data, line_text, cfg.font_size, cfg.upem, cfg.axes);

    let glyph_bytes = shaped.len() * core::mem::size_of::<scene::ShapedGlyph>();
    let needed = glyph_bytes + core::mem::align_of::<scene::ShapedGlyph>();
    if !w.has_data_space(needed) {
        return false;
    }

    let new_ref = w.push_shaped_glyphs(&shaped);

    {
        let n = w.node_mut(surviving_node);
        n.content = Content::Glyphs {
            color: scene_text_color,
            glyphs: new_ref,
            glyph_count: shaped.len() as u16,
            font_size: cfg.font_size,
            axis_hash: 0,
        };
        n.content_hash = fnv1a(&new_ref.offset.to_le_bytes());
    }
    w.mark_dirty(surviving_node);

    // Update y positions for nodes after the deleted node.
    if after_deleted != scene::NULL && after_deleted != N_CURSOR {
        // The surviving node's y is correct. Shift everything after surviving.
        let next_y = surviving_run.y.saturating_add(cfg.line_height as i32);
        update_line_positions(w, after_deleted, next_y, cfg.line_height as i32);
    }

    // Shared tail: cursor, selection, clock.
    finish_line_update(
        w, cfg, doc_text, cursor_pos, sel_start, sel_end, scroll_y, clock_text,
    );

    true
}
