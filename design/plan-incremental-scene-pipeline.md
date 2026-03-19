# Incremental Scene Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the rendering pipeline from full-rebuild-every-frame to incremental updates with dirty-rect rendering, per-node bitmap caching, and scroll_y-based scrolling.

**Architecture:** Producer (core) uses `acquire_copy()` to preserve the previous scene, mutates only changed nodes, and publishes a dirty bitmap. Consumer (render service) computes dirty rects from the bitmap, renders only intersecting nodes (with per-node cache for property-only changes), and transfers only dirty regions.

**Tech Stack:** Rust `no_std`, bare-metal aarch64, QEMU virt, virtio-gpu. Tests run on host via `cd system/test && cargo test -- --test-threads=1`.

**Spec:** `design/incremental-scene-pipeline.md`

---

## File Structure

### Scene library (`system/libraries/scene/`)

| File | Change | Responsibility |
|------|--------|---------------|
| `node.rs` | Modify | Replace `change_count`/`changed_nodes` with `dirty_bits: [u64; 8]` in SceneHeader. Widen Node `x`/`y` from `i16` to `i32`. Update size assertions. Remove `CHANGE_LIST_CAPACITY`, `FULL_REPAINT`. |
| `writer.rs` | Modify | Replace `mark_changed()` with `mark_dirty()`. Add `clear_dirty()`, `set_all_dirty()`. Update `clear()` to call `set_all_dirty()`. Retain `reset_data()` (still used by the compaction/full-rebuild path). |
| `reader.rs` | Modify | Update any accessors that reference `change_count`/`changed_nodes`. |
| `triple.rs` | Modify | Replace `change_list()`/`is_full_repaint()` with `dirty_bits()` accessor on TripleReader. |
| `diff.rs` | Modify | Remove `diff_scenes()`. Retain `abs_bounds()` and `build_parent_map()` (used by renderer). |
| `primitives.rs` | No change | `fnv1a()` stays as-is. |

### Core service (`system/services/core/`)

| File | Change | Responsibility |
|------|--------|---------------|
| `layout.rs` | Modify | Add incremental update functions. Change line positioning from viewport-relative to document-relative. Set `scroll_y` on container. Remove `scroll_runs()` pre-application from `build_document_content`. Widen `LayoutRun.y` from `i16` to `i32` (matches Node.y widening). Add `update_line_content()`, `insert_line_node()`, `delete_line_node()`, `update_line_positions()`. |
| `scene_state.rs` | Modify | Add incremental update entry points. Track `prev_line_count`. Route to incremental vs full rebuild. |
| `main.rs` | Modify | Update event dispatch to use incremental paths. Track state needed for incremental decisions. |

### Render library (`system/libraries/render/`)

| File | Change | Responsibility |
|------|--------|---------------|
| `damage.rs` | Modify | Increase `MAX_DIRTY_RECTS`. Add `add_dirty_from_bounds()`, coalescing logic. |
| `scene_render/walk.rs` | Modify | Add dirty-rect-clipped rendering path. Accept clip rect, skip non-intersecting nodes. |
| `scene_render/` | Create `cache.rs` | Per-node render cache: offscreen surface keyed by node ID, invalidated by `content_hash`. |
| `lib.rs` | Modify | Add `IncrementalState` struct (prev_bounds, prev_visible, prev_scroll_y, prev_content_hash). Wire dirty rect pipeline into `CpuBackend::render()`. |

### Render services

| File | Change | Responsibility |
|------|--------|---------------|
| `cpu-render/main.rs` | Modify | Retain framebuffer between frames. Read dirty bitmap. Render via dirty rects. Partial `transfer_to_host`. |
| `virgil-render/main.rs` | Modify | Read dirty bitmap. Scissor rects. |
| `virgil-render/scene_walk.rs` | Modify | Accept clip rects for scissored rendering. |

### Tests (`system/test/`)

| File | Change | Responsibility |
|------|--------|---------------|
| `tests/scene.rs` | Modify | Update for dirty bitmap API. Add dirty bitmap tests. |
| `tests/incremental.rs` | Create | Incremental pipeline tests: equivalence (incremental vs full rebuild), dirty rect correctness, scroll, line insert/delete, compaction. |

---

## Task 1: Dirty Bitmap in Scene Header

Replace the 24-entry `changed_nodes` array with a 512-bit dirty bitmap. This is the interface change that everything else depends on.

**Files:**
- Modify: `system/libraries/scene/node.rs`
- Modify: `system/libraries/scene/writer.rs`
- Modify: `system/libraries/scene/reader.rs`
- Modify: `system/libraries/scene/triple.rs`
- Modify: `system/libraries/scene/diff.rs`
- Modify: `system/test/tests/scene.rs`
- Modify: `system/services/core/layout.rs` (update `mark_changed` → `mark_dirty` call sites)

- [ ] **Step 1: Write failing tests for dirty bitmap**

Add to `system/test/tests/scene.rs`:

```rust
#[test]
fn dirty_bitmap_mark_and_test() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    w.clear_dirty();

    assert!(!w.is_dirty(0));
    assert!(!w.is_dirty(511));

    w.mark_dirty(0);
    w.mark_dirty(42);
    w.mark_dirty(511);

    assert!(w.is_dirty(0));
    assert!(w.is_dirty(42));
    assert!(w.is_dirty(511));
    assert!(!w.is_dirty(1));
    assert!(!w.is_dirty(41));
}

#[test]
fn dirty_bitmap_clear() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    w.mark_dirty(10);
    w.mark_dirty(200);
    w.clear_dirty();
    assert!(!w.is_dirty(10));
    assert!(!w.is_dirty(200));
}

#[test]
fn dirty_bitmap_set_all() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    w.set_all_dirty();
    for i in 0..512u16 {
        assert!(w.is_dirty(i));
    }
}

#[test]
fn dirty_bitmap_popcount() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    w.clear_dirty();
    assert_eq!(w.dirty_count(), 0);
    w.mark_dirty(5);
    w.mark_dirty(100);
    w.mark_dirty(300);
    assert_eq!(w.dirty_count(), 3);
}

#[test]
fn triple_reader_exposes_dirty_bits() {
    let mut buf = vec![0u8; scene::TRIPLE_SCENE_SIZE];
    let mut tw = scene::TripleWriter::new(&mut buf);
    {
        let w = tw.acquire();
        w.mark_dirty(7);
        w.mark_dirty(42);
    }
    tw.publish();

    let tr = scene::TripleReader::new(&buf);
    let bits = tr.dirty_bits();
    // Bit 7 and 42 should be set
    assert_ne!(bits[0] & (1u64 << 7), 0);
    assert_ne!(bits[0] & (1u64 << 42), 0);
    assert_eq!(bits[1], 0); // bits 64-127 clear
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd system/test && cargo test dirty_bitmap -- --test-threads=1`
Expected: Compilation errors — `mark_dirty`, `is_dirty`, `clear_dirty`, `set_all_dirty`, `dirty_count`, `dirty_bits` don't exist yet.

- [ ] **Step 3: Update SceneHeader in node.rs**

Replace `change_count` (2 bytes), `changed_nodes: [u16; 24]` (48 bytes), and `_reserved2: [u8; 2]` (2 bytes) — 52 bytes total — with `dirty_bits: [u64; 8]` (64 bytes). Net growth: 12 bytes. Remove `CHANGE_LIST_CAPACITY` and `FULL_REPAINT` constants.

The header is `#[repr(C)]`. With `dirty_bits: [u64; 8]` requiring 8-byte alignment, the compiler may add padding. Use `core::mem::size_of::<SceneHeader>()` to determine the actual new size and update the compile-time assert accordingly. `NODES_OFFSET` is already derived from `size_of::<SceneHeader>()`, so `DATA_OFFSET` and `SCENE_SIZE` will cascade correctly. Verify that `TRIPLE_SCENE_SIZE` (derived from `SCENE_SIZE`) still fits init's shared memory allocation — init uses the constant, so it adjusts automatically.

- [ ] **Step 4: Update SceneWriter in writer.rs**

Replace `mark_changed()` with:

```rust
pub fn mark_dirty(&mut self, node_id: NodeId) {
    let idx = node_id as usize;
    if idx < MAX_NODES {
        self.header_mut().dirty_bits[idx / 64] |= 1u64 << (idx % 64);
    }
}

pub fn is_dirty(&self, node_id: NodeId) -> bool {
    let idx = node_id as usize;
    if idx < MAX_NODES {
        self.header().dirty_bits[idx / 64] & (1u64 << (idx % 64)) != 0
    } else {
        false
    }
}

pub fn clear_dirty(&mut self) {
    self.header_mut().dirty_bits = [0u64; 8];
}

pub fn set_all_dirty(&mut self) {
    self.header_mut().dirty_bits = [u64::MAX; 8];
}

pub fn dirty_count(&self) -> u32 {
    self.header().dirty_bits.iter().map(|w| w.count_ones()).sum()
}
```

Remove the old `mark_changed` implementation.

- [ ] **Step 5: Update TripleWriter::acquire_copy in triple.rs**

In `copy_latest_to_acquired_inner()` (~line 387), the current code zeros `change_count` and `changed_nodes` in the destination. Replace this with `dirty_bits = [0u64; 8]` — the destination buffer must start with a clean dirty bitmap so that only newly-dirtied nodes are flagged by the producer. This is critical: without it, dirty bits from the previous frame carry over and cause spurious re-renders.

- [ ] **Step 6: Update TripleReader in triple.rs**

Replace `change_list()` and `is_full_repaint()` with:

```rust
pub fn dirty_bits(&self) -> &[u64; 8] {
    &self.header().dirty_bits
}
```

- [ ] **Step 7: Update SceneWriter::clear() in writer.rs**

`clear()` currently sets `change_count = FULL_REPAINT` to signal a full repaint. Replace with `self.set_all_dirty()` — same semantics via the new mechanism.

- [ ] **Step 8: Update diff.rs**

Remove `diff_scenes()` function. Retain `build_parent_map()` and `abs_bounds()`.

- [ ] **Step 9: Update all call sites**

In `system/services/core/layout.rs`: replace all `w.mark_changed(id)` calls with `w.mark_dirty(id)`. Search for `mark_changed` — there are calls at lines ~754, 809, 980, 994, and in selection rect allocation (~344).

In `system/test/tests/scene.rs`: update existing tests that use `mark_changed`, `change_list`, `is_full_repaint`, `FULL_REPAINT`, `change_count`. Replace with dirty bitmap equivalents.

- [ ] **Step 10: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass including new dirty bitmap tests.

- [ ] **Step 11: Commit**

```bash
git add system/libraries/scene/ system/services/core/layout.rs system/test/
git commit -m "feat: replace changed_nodes list with 512-bit dirty bitmap in scene header"
```

---

## Task 2: Widen Node Coordinates

Change Node `x`/`y` from `i16` to `i32` to support document-relative positioning for the scroll model.

**Files:**
- Modify: `system/libraries/scene/node.rs`
- Modify: `system/libraries/scene/diff.rs`
- Modify: `system/services/core/layout.rs`
- Modify: `system/services/drivers/cpu-render/main.rs` (if it reads x/y)
- Modify: `system/services/drivers/virgil-render/scene_walk.rs` (reads x/y)
- Modify: `system/libraries/render/scene_render/walk.rs` (reads x/y)
- Modify: `system/test/tests/scene.rs`

- [ ] **Step 1: Write failing test for wider coordinates**

```rust
#[test]
fn node_supports_large_coordinates() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let n = w.node_mut(id);
    n.x = 50000; // exceeds i16::MAX
    n.y = -40000; // exceeds i16::MIN
    assert_eq!(w.node(id).x, 50000);
    assert_eq!(w.node(id).y, -40000);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd system/test && cargo test node_supports_large -- --test-threads=1`
Expected: Compilation error or overflow — `50000` doesn't fit in `i16`.

- [ ] **Step 3: Widen x/y to i32 in Node struct**

In `node.rs`, change `x: i16` to `x: i32` and `y: i16` to `y: i32`. This adds 4 bytes to the Node struct. Update `Node::EMPTY` to set `x: 0, y: 0`. Update the size assertion — Node grows from 96 to 100 bytes. Update `NODE_SIZE` constant. Recalculate `NODES_OFFSET`, `DATA_OFFSET`, `SCENE_SIZE` to account for larger nodes.

- [ ] **Step 4: Widen LayoutRun.y from i16 to i32**

In `system/services/core/layout.rs` (~line 69), change `LayoutRun.y` from `i16` to `i32`. Update `layout_mono_lines()` (~line 114): change `line_y: i16` to `line_y: i32` and `saturating_add` arithmetic. Also update `TestLayoutRun.y` in `system/test/tests/scene.rs` (~line 13) from `i16` to `i32`. Without this, document-relative y positions overflow at ~1,638 lines.

- [ ] **Step 5: Update all code that reads/writes Node x/y**

Search for `.x =` and `.y =` assignments on Node in:
- `system/services/core/layout.rs` — many lines set node positions. Change `as i16` casts to `as i32`. Key sites: lines ~329, 474, 529, 537, 558, 590-591, 596, 748, 801, 985-986.
- `system/libraries/render/scene_render/walk.rs` — reads x/y for positioning. Many sites already cast to `i32` (`node.x as i32`); these become no-ops. Verify no truncation.
- `system/services/drivers/virgil-render/scene_walk.rs` — reads x/y. Same pattern.
- `system/libraries/scene/diff.rs` — `abs_bounds()` already returns `(i32, i32, u32, u32)` and casts `node.x as i32` internally. Becomes a no-op.

Grep for `as i16` in layout.rs and render code — these are the cast sites that need updating.

- [ ] **Step 6: Verify width/height unchanged**

Node `width: u16` and `height: u16` remain unchanged — viewport dimensions stay within u16 range. Only x/y change.

- [ ] **Step 7: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass.

- [ ] **Step 8: Visual verification**

Build and run QEMU to verify display is pixel-identical after the coordinate widening:

```bash
cd system && cargo build --release
# Launch QEMU, send keystrokes, take screenshot, compare to baseline
```

- [ ] **Step 9: Commit**

```bash
git add system/libraries/scene/ system/services/ system/test/
git commit -m "feat: widen Node x/y from i16 to i32 for document-relative positioning"
```

---

## Task 3: Scroll Model Change

Move from viewport-relative to document-relative line positioning. Set `scroll_y` on the container node. Remove `scroll_runs()` pre-application from the build paths.

**Files:**
- Modify: `system/services/core/layout.rs`
- Modify: `system/services/core/scene_state.rs`
- Modify: `system/services/core/main.rs`
- Modify: `system/test/tests/scene.rs` (or `tests/incremental.rs`)

- [ ] **Step 1: Write failing tests for document-relative positioning**

```rust
#[test]
fn lines_positioned_at_document_relative_y() {
    // Build a scene with scroll_offset = 5 lines
    // Verify line nodes have y = line_index * line_height (not viewport-adjusted)
    // Verify N_DOC_TEXT.scroll_y = scroll_offset * line_height
}

#[test]
fn scroll_change_only_dirties_container_and_new_lines() {
    // Build scene at scroll=0, then rebuild at scroll=1
    // Verify only N_DOC_TEXT and newly-visible line nodes are dirty
    // Verify previously-visible line nodes are NOT dirty
}
```

- [ ] **Step 2: Run tests to verify they fail**

Expected: Fails — lines are still viewport-relative, scroll_y is still 0.

- [ ] **Step 3: Modify `build_document_content` to use document-relative coords**

In `layout.rs`, change line node positioning from:
```rust
// Current: viewport-relative via scroll_runs()
let visible_runs = scroll_runs(all_runs, scroll_lines, cfg.line_height, content_h);
```
to:
```rust
// New: document-relative, all runs kept, scroll_y on container
// Set scroll_y on N_DOC_TEXT instead of filtering runs
w.node_mut(N_DOC_TEXT).scroll_y = scroll_lines as i32 * cfg.line_height as i32;
```

Line nodes get `y = line_index * line_height` (document-relative). Only allocate nodes for visible lines + overscan. The cursor is still positioned relative to the container (after scroll_y adjustment by the renderer).

- [ ] **Step 4: Update cursor positioning**

The cursor y must be document-relative too:
```rust
// Current: cursor_y = cursor_line * line_height - scroll_px
// New: cursor_y = cursor_line * line_height (document-relative, scroll_y handles offset)
let cursor_y = (cursor_line as i32 * cfg.line_height as i32) as i32;
```

- [ ] **Step 5: Update selection rect positioning**

Selection rects in `build_selection_update` and `build_selection_rects` also use scroll-adjusted coordinates. Change to document-relative.

- [ ] **Step 6: Verify renderer applies scroll_y**

Check that `system/libraries/render/scene_render/walk.rs` applies `parent.scroll_y` when positioning children. The existing code should already do this — `scroll_y` is a field on every node and the tree walk applies it. Verify by reading the walk code.

- [ ] **Step 7: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass.

- [ ] **Step 8: Visual verification**

Build and run QEMU. Type enough text to cause scrolling. Verify text scrolls correctly with the new coordinate model.

- [ ] **Step 9: Commit**

```bash
git add system/services/core/ system/test/
git commit -m "feat: switch to document-relative line positioning with scroll_y on container"
```

---

## Task 4: Incremental Scene Building in Core

Replace `reset_data()` + full rebuild with `acquire_copy()` + incremental mutation for text edits. This is the main producer-side change.

**Files:**
- Modify: `system/services/core/layout.rs`
- Modify: `system/services/core/scene_state.rs`
- Modify: `system/services/core/main.rs`
- Create: `system/test/tests/incremental.rs`
- Modify: `system/test/Cargo.toml` (add test module)

- [ ] **Step 1: Write tests at the scene library level**

**Host-testability constraint:** Core service (`system/services/core/`) compiles for `aarch64-unknown-none` and cannot be imported by the host test crate. Test incremental behavior at the scene library primitive level (SceneWriter, TripleWriter, dirty bitmap, node allocation, data buffer). Full-pipeline equivalence is verified via QEMU visual tests (Step 9).

Create `system/test/tests/incremental.rs`:

```rust
//! Incremental pipeline primitive tests.
//! Test scene library operations that underpin incremental updates.

#[test]
fn acquire_copy_preserves_nodes_and_data() {
    // 1. Build a scene with 3 nodes and glyph data via SceneWriter
    // 2. publish()
    // 3. acquire_copy() into back buffer
    // 4. Verify nodes and data in back buffer match front
    // 5. Verify dirty_bits are all zero (cleared by acquire_copy)
}

#[test]
fn incremental_data_push_preserves_old_data() {
    // 1. Build scene, push glyph data for node A at offset 0
    // 2. publish(), acquire_copy()
    // 3. Push NEW glyph data for node B (at bump pointer)
    // 4. Verify node A's DataRef still points to valid data (from the copy)
    // 5. Verify node B's DataRef points to new data
}

#[test]
fn cursor_move_is_property_only() {
    // 1. Build scene with cursor node at (10, 20)
    // 2. publish(), acquire_copy()
    // 3. Move cursor to (30, 40), mark_dirty(N_CURSOR)
    // 4. Verify only N_CURSOR dirty bit set
    // 5. Verify cursor content_hash unchanged (no content, property-only)
}

#[test]
fn dead_slot_node_invisible_after_delete() {
    // 1. Alloc nodes A, B, C. Link A→B→C as siblings.
    // 2. "Delete" B: clear VISIBLE flag, relink A→C
    // 3. mark_dirty(B)
    // 4. Verify B is not visible, A→C chain intact
    // 5. Verify node_count unchanged (dead slot, not reclaimed)
}

#[test]
fn compaction_after_dead_slots() {
    // 1. Build scene with dead slots
    // 2. clear() + rebuild from scratch
    // 3. Verify node_count is minimal (no dead slots)
    // 4. Verify all dirty bits set (full repaint)
}

#[test]
fn all_zero_dirty_bits_means_no_change() {
    // 1. publish() a scene
    // 2. acquire_copy() — dirty bits should be all zero
    // 3. Verify: no bits set, dirty_count() == 0
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd system/test && cargo test incremental -- --test-threads=1`
Expected: Compilation errors — incremental update functions don't exist yet.

- [ ] **Step 3: Add `prev_line_count` tracking to CoreState**

In `main.rs`, add `prev_line_count: usize` to the state. Initialize to 0. Update after each scene build.

Note: The spec also lists `prev_cursor_line` as core state. In practice, cursor position changes are detected from the existing `cursor_pos` vs the event that triggered the update — no separate `prev_cursor_line` tracking is needed. The spec overspecified this; the plan omits it intentionally.

- [ ] **Step 4: Create `update_line_content()` in layout.rs**

New function that reshapes and updates a single line node:

```rust
/// Incrementally update a single line node's glyph content.
/// Pushes new glyph data at the bump pointer (does NOT reset data buffer).
/// Returns the node ID of the updated line, or None if the line node wasn't found.
pub fn update_line_content(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    line_index: usize,
    line_node_id: NodeId,
) -> bool { ... }
```

This function: shapes one line of text, pushes glyph data via `w.push_data()`, updates the line node's `Content::Glyphs` and `content_hash`, marks the node dirty. Does NOT call `reset_data()`.

- [ ] **Step 5: Create incremental entry point in scene_state.rs**

Add `update_document_incremental()` alongside the existing `update_document_content()`:

```rust
/// Incremental text update: reshapes only the changed line(s).
/// Called when text changed but line count is the same.
/// Falls back to full rebuild (compaction) if data buffer is full.
pub fn update_document_incremental(
    &mut self,
    doc_text: &[u8],
    cursor_pos: u32,
    changed_line: usize,
    // ... other params
) { ... }
```

This function:
1. Calls `tw.acquire_copy()` (not `tw.acquire()`)
2. Calls `update_line_content()` for the changed line
3. Updates cursor position (property-only, mark dirty)
4. Publishes

- [ ] **Step 6: Add compaction fallback**

If `push_data()` would overflow the data buffer (check `data_used + size > DATA_BUFFER_SIZE`), fall back to full rebuild:

```rust
// In the incremental path, before pushing data:
if !w.has_data_space(estimated_glyph_bytes) {
    // Compaction: fall back to full rebuild
    self.update_document_content(/* full rebuild params */);
    return;
}
```

Add `has_data_space(bytes: usize) -> bool` to SceneWriter.

- [ ] **Step 7: Route events to incremental path in main.rs**

In the event dispatch section (~line 940), when `text_changed` is true:

```rust
if text_changed {
    let new_line_count = count_lines(doc_text);
    if new_line_count == prev_line_count {
        // Same line count — incremental update
        let changed_line = byte_to_line_col(doc_text, cursor_pos).0;
        scene_state.update_document_incremental(..., changed_line, ...);
    } else {
        // Line count changed — full rebuild (compaction)
        scene_state.update_document_content(...);
    }
    prev_line_count = new_line_count;
}
```

- [ ] **Step 8: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`
Expected: All tests pass including equivalence tests.

- [ ] **Step 9: Visual verification**

Build and run QEMU. Type characters — each keystroke should work identically to before. Serial output can log "incremental" vs "full rebuild" to verify the correct path is taken.

- [ ] **Step 10: Commit**

```bash
git add system/services/core/ system/test/ system/libraries/scene/
git commit -m "feat: incremental scene building for same-line-count text edits"
```

---

## Task 5: Line Insert and Delete

Handle Enter (line insert) and Backspace-at-BOL (line delete) incrementally instead of falling back to full rebuild.

**Files:**
- Modify: `system/services/core/layout.rs`
- Modify: `system/services/core/scene_state.rs`
- Modify: `system/services/core/main.rs`
- Modify: `system/test/tests/incremental.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn incremental_line_insert_allocates_new_node() {
    // Start with "abc\ndef", insert '\n' at pos 1 → "a\nbc\ndef"
    // Verify: 3 line nodes exist (was 2)
    // Verify: first line content is "a", second is "bc", third is "def"
    // Verify: dirty bits set for modified line, new line, repositioned lines, cursor
}

#[test]
fn incremental_line_delete_removes_node() {
    // Start with "abc\ndef", delete '\n' at pos 3 → "abcdef"
    // Verify: 1 line node (was 2)
    // Verify: deleted node is invisible
    // Verify: surviving line content is "abcdef"
}

#[test]
fn line_insert_shifts_subsequent_y_positions() {
    // Verify lines below insertion point shift down by line_height
    // Verify those lines' content_hash is unchanged (property-only)
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement `insert_line_node()` in layout.rs**

```rust
/// Insert a new line node into the sibling chain after the split point.
/// Allocates a node at the bump pointer, shapes the new line's content,
/// links it into the chain, and updates subsequent lines' y positions.
pub fn insert_line_node(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    doc_text: &[u8],
    split_line_index: usize,
    new_line_index: usize,
) -> Option<NodeId> { ... }
```

Steps inside:
1. `alloc_node()` — if returns None, caller falls back to compaction
2. Shape the new line's text content
3. Push glyph data
4. Set node properties (y = new_line_index * line_height, content, hash)
5. Link into sibling chain: find the node for `split_line_index`, insert new node as next_sibling
6. `mark_dirty(new_node_id)`
7. Call `update_line_positions()` for all subsequent lines (shifts y += line_height, marks dirty)

- [ ] **Step 4: Implement `delete_line_node()` in layout.rs**

```rust
/// Remove a line node from the sibling chain.
/// The node is marked invisible (dead slot). Subsequent lines shift up.
pub fn delete_line_node(
    w: &mut scene::SceneWriter<'_>,
    cfg: &SceneConfig,
    deleted_line_index: usize,
) { ... }
```

Steps inside:
1. Find the node for `deleted_line_index` by walking the sibling chain from N_DOC_TEXT
2. Unlink from chain (previous sibling's next_sibling = deleted node's next_sibling)
3. Mark deleted node invisible (clear VISIBLE flag)
4. `mark_dirty(deleted_node_id)`
5. Call `update_line_positions()` for subsequent lines (shifts y -= line_height, marks dirty)

- [ ] **Step 5: Implement `update_line_positions()`**

```rust
/// Update y positions for all line nodes starting at `from_index`.
/// Each node's y is set to `from_index * line_height + offset`.
/// Marks each repositioned node dirty. Does not change content.
fn update_line_positions(
    w: &mut scene::SceneWriter<'_>,
    line_height: i32,
    start_node_id: NodeId,
    start_line_index: usize,
) { ... }
```

Walk the sibling chain from `start_node_id`, set `y = line_index * line_height`, `mark_dirty()`. Content_hash is unchanged — renderer detects property-only.

- [ ] **Step 6: Route line count changes to incremental path**

In `main.rs`, replace the `new_line_count != prev_line_count` branch:

```rust
if new_line_count > prev_line_count {
    // Line(s) inserted — find which line, split, insert node
    let changed_line = byte_to_line_col(doc_text, cursor_pos).0;
    scene_state.update_document_insert_line(..., changed_line);
} else if new_line_count < prev_line_count {
    // Line(s) deleted — find which line, merge, delete node
    let changed_line = byte_to_line_col(doc_text, cursor_pos).0;
    scene_state.update_document_delete_line(..., changed_line);
} else {
    // Same line count — incremental content update
    scene_state.update_document_incremental(...);
}
```

If the insert/delete functions can't allocate (node array full, data buffer full), they fall back to compaction.

- [ ] **Step 7: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`

- [ ] **Step 8: Visual verification**

Build and run QEMU. Press Enter to insert lines, Backspace at line start to delete lines. Verify correct behavior.

- [ ] **Step 9: Commit**

```bash
git add system/services/core/ system/test/
git commit -m "feat: incremental line insert and delete with sibling chain management"
```

---

## Task 6: Renderer Dirty Rect Infrastructure

Add the consumer-side state tracking and dirty rect computation to the render library.

**Files:**
- Modify: `system/libraries/render/damage.rs`
- Modify: `system/libraries/render/lib.rs`
- Create: `system/libraries/render/incremental.rs`
- Create: `system/test/tests/render_incremental.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn dirty_rect_from_moved_node() {
    // prev_bounds[5] = (100, 200, 50, 20)
    // curr_bounds[5] = (100, 220, 50, 20) (moved down 20px)
    // dirty rect should be (100, 200, 50, 40) — union of old and new
}

#[test]
fn dirty_rect_from_new_node() {
    // prev_visible bit 10 = false
    // curr node 10 visible at (50, 100, 200, 30)
    // dirty rect should be (50, 100, 200, 30)
}

#[test]
fn dirty_rect_from_deleted_node() {
    // prev_visible bit 10 = true, prev_bounds[10] = (50, 100, 200, 30)
    // curr node 10 invisible
    // dirty rect should be (50, 100, 200, 30) — prev bounds only
}

#[test]
fn scroll_produces_blit_shift_and_strip() {
    // prev_scroll_y = 0, curr_scroll_y = 20 (scrolled down 1 line)
    // Container bounds: (0, 50, 1024, 700)
    // Expected: blit-shift delta = -20, exposed strip at bottom (0, 730, 1024, 20)
}

#[test]
fn dirty_rects_coalesce_overlapping() {
    // Two overlapping rects merge into one
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Create `IncrementalState` struct**

In new file `system/libraries/render/incremental.rs`:

```rust
use scene::{MAX_NODES, Node, NodeFlags};

pub struct IncrementalState {
    pub prev_bounds: [(i32, i32, u32, u32); MAX_NODES],
    pub prev_visible: [u64; 8],
    pub prev_scroll_y: [i32; MAX_NODES],
    pub prev_content_hash: [u32; MAX_NODES],
    pub first_frame: bool,
}
```

With methods:
- `new() -> Self` — all zeros, `first_frame = true`
- `compute_dirty_rects(nodes, dirty_bits, parent_map) -> Vec<DirtyRect>` — the main computation
- `detect_scroll(nodes, dirty_bits) -> Option<(NodeId, i32)>` — returns (container_id, delta)
- `update_from_frame(nodes)` — copies current state to prev arrays. Specifically:
  - For each node: if `VISIBLE` flag set, compute `abs_bounds()` → `prev_bounds[id]`, set `prev_visible` bit, copy `content_hash` → `prev_content_hash[id]`, copy `scroll_y` → `prev_scroll_y[id]`
  - If not visible: clear `prev_visible` bit, zero `prev_bounds[id]`
  - This is the sole place where `prev_visible` is populated — called at the end of every frame

- [ ] **Step 4: Increase `MAX_DIRTY_RECTS` in damage.rs**

Change from 6 to 32 (or larger). Add coalescing logic that merges overlapping rects.

- [ ] **Step 5: Implement `compute_dirty_rects()`**

For each set bit in `dirty_bits`:
1. Check `prev_visible` vs current visibility
2. Compute `abs_bounds()` for current node (if visible)
3. Union with `prev_bounds[id]` (if prev_visible)
4. Add to DamageTracker
5. Handle container position/size changes — if a dirty container's bounds changed, mark entire old+new bounds
6. Coalesce overlapping rects

- [ ] **Step 6: Implement scroll detection**

Compare each dirty container node's `scroll_y` to `prev_scroll_y[id]`. If different, return the delta.

- [ ] **Step 7: Wire into render library**

Modify `lib.rs` to add `IncrementalState` as a field on `CpuBackend`. Expose it for the render services to use.

- [ ] **Step 8: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`

- [ ] **Step 9: Commit**

```bash
git add system/libraries/render/ system/test/
git commit -m "feat: dirty rect computation infrastructure with incremental state tracking"
```

---

## Task 7: Per-Node Render Cache

Add offscreen surface caching per node, invalidated by `content_hash`.

**Files:**
- Create: `system/libraries/render/cache.rs`
- Modify: `system/libraries/render/lib.rs`
- Modify: `system/test/tests/render.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn cache_stores_and_retrieves_surface() {
    let mut cache = NodeCache::new();
    let surface_data = vec![0xFFu8; 100 * 20 * 4]; // 100x20 BGRA
    cache.store(5, 0xABCD, 100, 20, &surface_data);
    assert!(cache.get(5, 0xABCD).is_some());
}

#[test]
fn cache_invalidates_on_hash_change() {
    let mut cache = NodeCache::new();
    let surface_data = vec![0xFFu8; 100 * 20 * 4];
    cache.store(5, 0xABCD, 100, 20, &surface_data);
    // Different hash — cache miss
    assert!(cache.get(5, 0x1234).is_none());
}

#[test]
fn cache_clear_removes_all() {
    let mut cache = NodeCache::new();
    let data = vec![0u8; 40];
    cache.store(1, 0x1111, 10, 1, &data);
    cache.store(2, 0x2222, 10, 1, &data);
    cache.clear();
    assert!(cache.get(1, 0x1111).is_none());
    assert!(cache.get(2, 0x2222).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Implement `NodeCache`**

In `system/libraries/render/cache.rs`:

```rust
/// Per-node render cache. Stores rendered bitmaps keyed by (node_id, content_hash).
/// Invalidated when content_hash changes. Cleared on compaction.
pub struct NodeCache {
    entries: [CacheEntry; MAX_NODES],
}

struct CacheEntry {
    content_hash: u32,
    width: u32,
    height: u32,
    data: Vec<u8>, // BGRA pixel data
    valid: bool,
}
```

Methods:
- `new() -> Self`
- `get(node_id, content_hash) -> Option<&CacheEntry>` — returns Some if valid and hash matches
- `store(node_id, content_hash, w, h, data)` — stores rendered output
- `clear()` — invalidate all entries
- `evict(node_id)` — invalidate one entry

- [ ] **Step 4: Integrate cache into render walk**

In `scene_render/walk.rs`, the render path for a node within a dirty rect:
1. Check `cache.get(node_id, node.content_hash)`
2. If hit → blit cached bitmap at node's current position
3. If miss → render normally, then `cache.store(node_id, node.content_hash, ...)`

This applies only to nodes with `Content::Glyphs`, `Content::Image`, `Content::Path`. `Content::None` nodes are always re-rendered (fill_rect is cheaper than blit).

- [ ] **Step 5: Run all tests**

Run: `cd system/test && cargo test -- --test-threads=1`

- [ ] **Step 6: Commit**

```bash
git add system/libraries/render/ system/test/
git commit -m "feat: per-node render cache with content_hash invalidation"
```

---

## Task 8: cpu-render Incremental Rendering

Wire the dirty rect infrastructure and per-node cache into cpu-render.

**Files:**
- Modify: `system/services/drivers/cpu-render/main.rs`
- Modify: `system/libraries/render/scene_render/walk.rs`
- Modify: `system/libraries/render/lib.rs`

- [ ] **Step 1: Retain framebuffer between frames**

In cpu-render's main loop, stop clearing the framebuffer each frame. The retained framebuffer is the baseline for incremental updates.

- [ ] **Step 2: Read dirty bitmap from scene header**

After `TripleReader::new()`, read `tr.dirty_bits()`. If all zeros, skip the frame entirely (no `render()` call, no transfer).

- [ ] **Step 3: Compute dirty rects**

Call `incremental_state.compute_dirty_rects(nodes, dirty_bits, parent_map)`. If all-dirty (first frame, compaction), fall back to full render.

- [ ] **Step 4: Add clip rect to render walk**

Modify `render_scene()` / `render_node_transformed()` to accept an optional clip rect. Nodes entirely outside the clip rect are skipped. Drawing operations are clipped to the rect boundary. For full repaint, clip rect is the entire screen.

- [ ] **Step 5: Handle scroll blit-shift**

If `incremental_state.detect_scroll()` returns a delta:
1. Blit-shift the container's framebuffer region by `-delta` pixels. For scroll-down (positive delta): pixels move UP in the framebuffer. Use `memmove` (or equivalent byte copy) from `src_y = container_y + delta` to `dst_y = container_y`, copying `container_height - delta` rows. The copy direction matters: when shifting up, copy from lower to higher addresses to avoid overwriting source data.
2. Add the newly exposed strip to the dirty rect list. For scroll-down: strip at the bottom of the container (`y = container_y + container_height - delta`, height = `delta`).
3. Continue with dirty rect rendering for the exposed strip + any other dirty rects

- [ ] **Step 6: Partial GPU transfer**

Replace the single full-screen `transfer_to_host` with per-dirty-rect transfers:

```rust
for rect in dirty_rects {
    gpu::transfer_to_host_reuse(
        &device, &mut vq, irq_handle, &present_cmd,
        gpu::FB_RESOURCE_ID,
        rect.x as u32, rect.y as u32,
        rect.w as u32, rect.h as u32,
        base_offset + (rect.y as u32 * stride + rect.x as u32 * 4),
        stride,
    );
}
gpu::resource_flush_reuse(..., width, height);
```

Note: The virtio-gpu 2D `TRANSFER_TO_HOST_2D` command has separate `r.x, r.y, r.width, r.height` fields AND an `offset` field. The `offset` is the byte offset into the backing store where the rectangle starts. For a partial transfer: `offset = rect.y * stride + rect.x * 4`, and `r` specifies the rectangle dimensions. Check the current `transfer_to_host_reuse` wrapper to verify it exposes both fields correctly.

- [ ] **Step 7: Update incremental state**

After rendering, call `incremental_state.update_from_frame(nodes)`.

- [ ] **Step 8: Visual verification**

Build and run QEMU with cpu-render (non-virgl QEMU). Type characters, move cursor, scroll. Verify:
- Characters appear correctly
- Cursor blink doesn't cause full repaint (check serial output or timing)
- Scroll works correctly
- No visual artifacts

- [ ] **Step 9: Commit**

```bash
git add system/services/drivers/cpu-render/ system/libraries/render/
git commit -m "feat: cpu-render incremental rendering with dirty rects and partial transfer"
```

---

## Task 9: virgil-render Incremental Rendering

Wire dirty rect infrastructure into virgil-render with GPU-specific optimizations.

**Files:**
- Modify: `system/services/drivers/virgil-render/main.rs`
- Modify: `system/services/drivers/virgil-render/scene_walk.rs`
- Modify: `system/services/drivers/virgil-render/pipeline.rs`

- [ ] **Step 1: Read dirty bitmap**

After `TripleReader::new()`, read `tr.dirty_bits()`. If all zeros, skip the frame.

- [ ] **Step 2: Add incremental state**

Add `IncrementalState` to virgil-render's persistent state. Compute dirty rects from dirty bitmap.

- [ ] **Step 3: Scissor rects for GPU rendering**

For each dirty rect, set a Gallium3D scissor rect before the draw calls:

```rust
// Set scissor to dirty rect before rendering
cmdbuf.set_scissor_state(rect.x, rect.y, rect.w, rect.h);
```

The GPU clips all fragment processing to the scissor rect. Unchanged regions are not touched.

- [ ] **Step 4: Handle scroll**

Detect scroll delta. For GPU rendering, scroll blit-shift can be done with a textured quad copy (blit the framebuffer texture shifted by delta), then render the exposed strip.

Alternatively, if the GPU command buffer is rebuilt anyway, scissor to the exposed strip for the scroll case.

- [ ] **Step 5: Per-node GPU texture cache**

The glyph atlas already persists across frames. Extend this to per-node render-to-texture:
1. First render of a node → render to texture, store texture ID + content_hash
2. Property-only change → composite existing texture at new position
3. Content change → re-render to texture, update cache

**Deferred:** Per-node GPU render-to-texture is a significant undertaking (requires Gallium3D FBO setup, texture lifecycle management, render target switching). Start with scissor-only optimization — this already provides significant savings by limiting GPU fragment work to dirty regions. Per-node GPU textures can be added in a follow-up task once scissor-based rendering is validated.

- [ ] **Step 6: Update incremental state**

After rendering, call `incremental_state.update_from_frame(nodes)`.

- [ ] **Step 7: Visual verification**

Build and run QEMU with virgl-capable QEMU. Type, scroll, insert/delete lines. Verify correct rendering and no artifacts.

- [ ] **Step 8: Commit**

```bash
git add system/services/drivers/virgil-render/
git commit -m "feat: virgil-render incremental rendering with scissor rects"
```

---

## Task Dependencies

```
Task 1 (dirty bitmap) ──┬──► Task 3 (scroll model) ──► Task 4 (incremental building) ──► Task 5 (line ins/del)
                         │
Task 2 (node widening) ──┘
                                                         Task 6 (dirty rect infra) ──► Task 7 (node cache) ──┬──► Task 8 (cpu-render)
                                                                                                              └──► Task 9 (virgil-render)

Tasks 1-2 are prerequisites for everything.
Tasks 3-5 (producer) and Tasks 6-7 (consumer) can be developed in parallel.
Tasks 8-9 depend on both producer and consumer being complete.
```

---

## Verification Checklist

After all tasks are complete:

- [ ] `cd system/test && cargo test -- --test-threads=1` — all tests pass
- [ ] Equivalence tests verify incremental == full rebuild for all event types
- [ ] Visual verification: QEMU with both cpu-render and virgil-render
- [ ] Scroll stress test: scroll through 500+ lines, verify no artifacts or crashes
- [ ] Compaction verified: serial output shows compaction triggers at expected frequency
- [ ] Cursor blink verified: serial output shows skip-frame or small dirty rect, not full repaint
- [ ] Enter/Backspace verified: lines insert and delete correctly with visible y-shifts
- [ ] No regressions: existing display pipeline behavior preserved
