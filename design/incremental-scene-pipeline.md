# Incremental Scene Pipeline

**Date:** 2026-03-19
**Status:** Design complete, pending implementation
**Depends on:** Rendering pipeline (complete), scene graph library, core service, render services

---

## Problem

The rendering pipeline rebuilds the entire scene and repaints every pixel on every frame. A single keystroke triggers: reshape all visible text lines, rebuild all scene nodes, reset the data buffer, full tree walk in the renderer, full framebuffer transfer (~3 MiB). The pipeline does O(entire screen) work for O(single character) changes.

This is correct but structurally wasteful. Retrofitting incrementalism later would be painful — the data flow assumptions (reset-and-rebuild, full walk, full transfer) are baked into every stage. The complete incremental pipeline should be built as a foundation before layers above (more content types, compound documents, layout engine) are added.

---

## Design Principles

From `design/philosophy.md`:

- **The system is a series of data transformations.** The incremental pipeline is two transformations: `(previous scene, edit event) → (updated scene, dirty bitmap)` on the producer side, and `(scene, dirty bitmap, previous framebuffer) → (updated framebuffer, dirty rects)` on the consumer side.
- **The architecture is the interfaces, not the components.** The scene header (dirty bitmap + content_hash) is the interface between producer and consumer. Each side can evolve independently.
- **Push complexity to the leaves.** The dirty bitmap is simple connective tissue. The complexity of how core tracks line changes, or how the renderer caches rendered output, lives in the leaf components behind that simple interface.
- **Find the abstraction that absorbs the edge cases.** All event types — character edit, line insert, line delete, scroll, cursor move, selection, clock tick — go through the same mechanism (mutate nodes, set dirty bits). No special-case flags. When new content types are added, the per-node bitmap cache automatically handles their property-only changes without new code.

---

## Architecture Overview

```text
                          Scene Header
                        ┌─────────────┐
  Producer (core)       │ dirty_bits  │       Consumer (render service)
  ──────────────── ───► │ content_hash│ ───► ─────────────────────────
  • acquire_copy()      │ per node    │      • Read dirty bitmap
  • Mutate changed      └─────────────┘      • Compute dirty rects
    nodes + data                             • Scroll blit-shift
  • Set dirty bits                           • Render in dirty rects
  • publish()                                • Per-node cache (bitmap)
                                             • Partial transfer
```

Two render backends (cpu-render, virgil-render) share the same producer-side design. Each implements its own caching and transfer strategy.

---

## 1. Change Tracking

### Dirty bitmap

Replace the 24-entry `changed_nodes` array with a 512-bit dirty bitmap:

```rust
// In SceneHeader (replaces change_count + changed_nodes)
dirty_bits: [u64; 8],   // 512 bits, one per MAX_NODES
```

- Never overflows — scroll can dirty 40+ nodes without fallback.
- O(1) test: `dirty_bits[n / 64] & (1 << (n % 64))`.
- Slightly larger than the current 24-entry array (64 bytes vs 52 bytes). The `SceneHeader` grows to accommodate this; both producer and consumer must agree on the layout.
- All-zeros = no change (skip frame). All-ones = full repaint (context switch, first frame).
- `mark_dirty(id)` sets one bit. `clear_dirty()` zeros the array.

### Content hash

Each node already has a `content_hash: u32` field (FNV-1a of the node's data buffer content). This field serves two purposes:

1. **Producer side:** Core compares content_hash to decide whether a line needs reshaping. Unchanged hash → skip reshape (the expensive operation).
2. **Consumer side:** Renderer compares content_hash to `prev_content_hash[id]`. Unchanged hash on a dirty node → property-only change → blit from per-node cache instead of re-rendering.

Content hash is the sole cache invalidation signal. No heuristics, no frame-count thresholds.

---

## 2. Scene Graph Memory Model

### Persistence via triple buffer

After `acquire_copy()`, the back buffer is a complete copy of the last published scene — both the node array and the data buffer. Unchanged nodes have valid properties; unchanged DataRefs point to valid data. Core only touches what changed.

This replaces the current pattern of `reset_data()` + full rebuild on every text change. The triple buffer's copy semantics are the mechanism that makes incrementalism possible — no new infrastructure needed.

### Uniform lifecycle for nodes and data

Both the node array and data buffer follow the same lifecycle:

| Operation    | Node Array                                | Data Buffer                                   |
| ------------ | ----------------------------------------- | --------------------------------------------- |
| **Allocate** | `alloc_node()` — bump `node_count`        | `push_data()` — bump `data_used`              |
| **Update**   | Overwrite node fields in-place            | Push new data at bump pointer, update DataRef |
| **Delete**   | Unlink from sibling chain, mark invisible | Old data at old offset becomes dead space     |
| **Preserve** | `acquire_copy()` copies live nodes        | `acquire_copy()` copies all data              |
| **Compact**  | Full rebuild reclaims dead slots          | Full rebuild repacks live data                |

Dead space accumulates between compactions. The bump allocator does not reuse dead slots — `alloc_node()` always allocates at the end, and dead slots from deleted lines remain until compaction reclaims them. This is acceptable because the node array has 512 slots with ~60-70 in typical use (~440 slots of headroom). Even heavy line deletion (200 lines deleted one at a time) consumes 200 dead slots, leaving ~240 for new allocations — well before node array pressure triggers compaction. The data buffer has 64 KiB with ~15 KiB live (~49 KiB headroom). Compaction is infrequent.

---

## 3. Scroll Model

### Change from current design

**Current:** Core pre-applies scroll via `scroll_runs()`. Line nodes have viewport-relative y positions. `scroll_y` on the container (N_DOC_TEXT) is always 0.

**Proposed:** Line nodes are positioned at document-relative coordinates (`y = line_index × line_height`). The container node holds a `content_transform` (AffineTransform) — a translation for scroll offset. The renderer composes `content_transform` into the child coordinate space: `content_world = node_world.compose(node.content_transform)`. See `design/content-transform.md` for the full design.

### Coordinate range

The Node struct's `y: i16` field overflows at ~1,638 lines with 20px line height (32767 / 20). Document-relative positioning requires widening `y` to `i32` (and `x` for consistency). This changes the Node struct size from 96 bytes — the exact layout adjustment is an implementation detail, but the size change must be reflected in `NODE_SIZE`, `SCENE_SIZE`, and the scene buffer allocation in init. `content_transform` uses `f32` via `AffineTransform`, which accommodates any coordinate range.

### Why this matters

Scroll becomes a single property change on one container node + shaping any newly-visible lines at the viewport edge. The renderer can optimize this further with blit-shift (move existing pixels, render only the exposed strip). Without this change, scroll would dirty every visible line node's y position — defeating incrementalism for the most common navigation operation.

### Viewport management

Core maintains nodes for visible lines plus an overscan band (a few lines above and below the viewport). Lines beyond the overscan range don't have nodes. When scroll brings new lines into the overscan range, core allocates nodes and shapes their content. Lines leaving the far edge of the overscan keep their nodes temporarily (dead slots, reclaimed on compaction).

### Scroll and data buffer pressure

Scrolling accumulates glyph data faster than typing. Each newly-visible line adds ~320 bytes to the data buffer (80 glyphs × 4 bytes). Scrolling through 150 lines would consume the ~49 KiB of data buffer headroom, triggering compaction. This is expected behavior: compaction on data buffer pressure is a reactive trigger (Section 6), and scrolling through a long document naturally hits it periodically. The compaction cost (one frame at current latency) is imperceptible at scroll speed. Typing between scrolls does not compound the problem — the data for currently-visible lines is live; only data from previously-visible lines that scrolled off is dead space.

---

## 4. Producer Operations

Every event type goes through the same three steps:

1. Mutate node properties and/or push new data for that node
2. Set that node's dirty bit
3. `publish()`

No structural-change flags. No special-case paths.

### Complete operation table

| Event                                              | Scene mutations                                                                                                                                                     | Dirty bits                                             |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| **Character typed** (same line count)              | Reshape 1 line, push new glyphs, update line node's DataRef + content_hash. Update cursor x/y.                                                                      | Changed line + cursor                                  |
| **Line insert** (Enter key)                        | Update existing line's content (split). Alloc new node, shape new line, link into sibling chain. Update subsequent lines' y positions (property-only). Move cursor. | Modified line + new line + repositioned lines + cursor |
| **Line delete** (Backspace at BOL)                 | Update surviving line's content (merge). Unlink deleted node (mark invisible = dead slot). Update subsequent lines' y positions. Move cursor.                       | Merged line + dead node + repositioned lines + cursor  |
| **Cursor move**                                    | Update cursor x/y.                                                                                                                                                  | Cursor                                                 |
| **Selection change**                               | Update cursor. Alloc/remove/resize selection rect nodes.                                                                                                            | Cursor + selection rects                               |
| **Clock tick**                                     | Reshape clock text, push new glyphs, update clock node.                                                                                                             | Clock node                                             |
| **Scroll** (any size)                              | Update container `content_transform` (translation). Shape newly-visible edge lines (alloc nodes if needed, push glyph data). Update cursor y.                       | Container + new line nodes + cursor                    |
| **Property-only** (transform, opacity, background) | Update node properties. No data buffer work.                                                                                                                        | Changed nodes                                          |
| **Context switch**                                 | Full rebuild: clear + reshape all. Set all dirty bits.                                                                                                              | All bits                                               |

### Line insert/delete specifics

The sibling chain (`first_child`/`next_sibling`) is a linked list — node array indices don't need to be contiguous. A new line node allocated at the end of the array is linked into the middle of the chain by adjusting pointers. A deleted node is unlinked and marked invisible. Its slot is dead space, reclaimed on compaction.

Subsequent lines after an insert/delete need y-position updates (shift by ±`line_height`). This is a property-only write per node — no data buffer work, no reshaping. The renderer sees these as property-only changes via content_hash comparison and blits cached bitmaps at the new positions.

### Core state across frames

Core tracks:

- `prev_line_count: usize` — detect line add/remove (affects node allocation)
- `prev_cursor_line: usize` — detect cursor line changes

Everything else is carried by `acquire_copy()`.

---

## 5. Consumer: Dirty-Rect Rendering

### Renderer state

The renderer maintains per-node state that persists across frames:

| State                                            | Size     | Purpose                                          |
| ------------------------------------------------ | -------- | ------------------------------------------------ |
| `prev_bounds: [(i32, i32, u32, u32); MAX_NODES]` | 8 KiB    | Previous frame's absolute visual bounds per node |
| `prev_visible: [u64; 8]`                         | 64 B     | Bitmap: was node visible last frame?             |
| `prev_content_transform: [AffineTransform; MAX_NODES]` | 12 KiB | Previous frame's content_transform per node     |
| `prev_content_hash: [u32; MAX_NODES]`            | 2 KiB    | Previous frame's content_hash per node           |
| Per-node render cache (bitmaps)                  | ~2-8 MiB | Cached rendered output per node                  |

Total metadata: ~12 KiB. Render cache: bounded by viewport pixel area.

`prev_bounds` uses `(i32, i32, u32, u32)` to match `abs_bounds()` return type — document-relative coordinates in the scroll model can exceed `i16` range for long documents. `prev_visible` distinguishes new nodes (not in prev_visible, in current) from deleted nodes (in prev_visible, now invisible) from updated nodes (in both). Initialized to all-zeros; first frame sets all dirty bits and rebuilds everything.

### Per-frame pipeline

1. **Read dirty bitmap.** All zeros → skip frame entirely. All ones → full repaint.

2. **Compute dirty rects.** For each set bit: `dirty_rect = union(prev_bounds[i], curr_bounds[i])`. Detect node lifecycle via `prev_visible` bitmap: deleted nodes (prev_visible but now invisible) use `prev_bounds[i]` only; new nodes (not prev_visible but now visible) use `curr_bounds[i]` only. If a dirty node is a container whose position or size changed (not just content_transform), mark its entire old and new bounding boxes as dirty rects — this implicitly covers all children without marking them individually dirty.

3. **Detect scroll.** For each dirty container node: compare `content_transform` to `prev_content_transform[i]`. If only translation changed (pure translation both before and after), compute delta. Blit-shift the container's existing rendered region by `-delta` points. Add the newly exposed strip to the dirty rect list.

4. **Coalesce dirty rects.** Merge overlapping rects. If total dirty area exceeds a backend-tunable threshold (~75% for cpu-render, potentially higher for virgil-render where GPU scissor is nearly free), fall back to full repaint.

5. **Render dirty rects.** For each dirty rect: walk the tree, clip all drawing to the rect boundary. Skip nodes that don't intersect. For nodes that intersect:
   - If `content_hash == prev_content_hash[i]`: **property-only** — blit from per-node render cache at new position.
   - If `content_hash` differs: re-render the node (rasterize glyphs, decode images, rasterize paths), update the render cache entry.
   - Render back-to-front within each rect for correct compositing order.

6. **Transfer.** cpu-render: `transfer_to_host` with per-rect coordinates (virtio-gpu 2D supports partial transfer). virgil-render: scissor rects limit GPU fragment processing.

7. **Update state.** Copy current bounds → `prev_bounds`, `content_transform` → `prev_content_transform`, `content_hash` → `prev_content_hash`. For scroll: shift all children's `prev_bounds` by the scroll delta.

### Per-node render cache

Each node's rendered content is cached as a bitmap, keyed by node ID, invalidated when `content_hash` changes. This matters for compound documents: when a text edit causes layout reflow that shifts images and other content below, those shifted elements are property-only changes — their cached bitmaps are blit'd at new positions instead of re-rendering from source data.

| Content type | Re-render cost                           | With cache (property-only) |
| ------------ | ---------------------------------------- | -------------------------- |
| **Glyphs**   | Moderate (glyph lookup + blend)          | Blit                       |
| **Image**    | Expensive (decode + scale + interpolate) | Blit                       |
| **Path**     | Expensive (rasterize contours + stencil) | Blit                       |

Nodes with `Content::None` (pure containers with only background/border) are not cached — `fill_rect` is cheaper than a bitmap blit. The cache applies only to nodes with content (Glyphs, Image, Path).

The cache is bounded by viewport pixel area. Cache entries for nodes that leave the viewport (via scroll or layout change) are evicted. On compaction (full rebuild), the entire cache is invalidated and rebuilt.

### Backend-specific implementation

**cpu-render:** Per-node offscreen surfaces from `SurfacePool` (already exists in the render library). Render to offscreen buffer, then blit to framebuffer at position. Retain offscreen buffer as cache entry. `DamageTracker` (already exists, currently unused) collects dirty rects.

**virgil-render:** Per-node GPU textures (render-to-texture). Glyph atlas already provides per-glyph texture caching — this extends the pattern to full nodes. Scissor rects limit GPU fragment work to dirty regions. GPU memory is plentiful for 2D viewport-sized content.

---

## 6. Compaction

### Compaction is the existing full rebuild

There is no new compaction mechanism. Compaction IS the existing `build_editor_scene()` path: `clear()` + reshape all visible lines + allocate all nodes + link sibling chains. Today this runs every frame. After the incremental design, it runs only when triggered. The code already exists.

After compaction: node_count is minimal (no dead slots), data_used is minimal (no dead data), all dirty bits are set (full repaint). The renderer rebuilds its entire per-node cache. The pipeline continues incrementally from the new clean state.

### Triggers

Compaction is reactive, not periodic:

| Trigger                 | Condition                          | Rationale                                    |
| ----------------------- | ---------------------------------- | -------------------------------------------- |
| Data buffer pressure    | `data_used + next_push > capacity` | Can't fit new glyph data                     |
| Node array pressure     | `alloc_node()` returns None        | No free slots                                |
| Context switch          | Different editor/document          | Full scene change — natural compaction point |
| Renderer cache pressure | Too many stale cache entries       | Dead node IDs → orphaned cache bitmaps       |

### Frequency

With ~49 KiB of data buffer headroom and ~320 bytes of dead data per keystroke, data buffer pressure triggers compaction after ~150+ keystrokes. Line insert/delete adds ~96 bytes of dead node space per operation; node array pressure triggers after ~440 line operations (512 slots - ~70 live). Scrolling through ~150 lines triggers data buffer compaction (see Section 3). In practice, context switches (which trigger compaction naturally) will fire before buffer pressure in most editing sessions.

The cost of compaction is identical to today's per-frame cost — one frame at current latency. Imperceptible.

---

## 7. What Changes Where

### Scene library (`libraries/scene/`)

- Replace `change_count` + `changed_nodes` in `SceneHeader` with `dirty_bits: [u64; 8]`
- `SceneWriter::mark_dirty(id)` sets one bit (replaces `mark_changed`)
- `SceneWriter::clear_dirty()` zeros the bitmap
- `SceneWriter::set_all_dirty()` sets all bits (for compaction / full rebuild)
- `TripleReader`: expose `dirty_bits()` accessor
- Remove superseded fields: `change_count`, `changed_nodes`, `CHANGE_LIST_CAPACITY`, `FULL_REPAINT` sentinel
- Remove `diff_scenes()` from `diff.rs` — byte-level scene comparison is superseded by the dirty bitmap. `abs_bounds()` is retained (used by the renderer for dirty rect computation)

### Core service (`services/core/`)

- `update_document_content()` split into incremental (update changed lines only) and full rebuild (compaction) paths
- `acquire_copy()` replaces `reset_data()` + full rebuild as the default frame setup
- Track `prev_line_count` and `prev_cursor_line` for detecting line insert/delete
- Line insert: alloc node at bump pointer, link into sibling chain, update subsequent y positions
- Line delete: unlink node, mark invisible, update subsequent y positions
- Scroll: set `content_transform` translation on container, shape newly-visible edge lines
- Compaction: triggered on buffer pressure or context switch, reuses existing full-rebuild code

### Render library (`libraries/render/`)

- Wire `DamageTracker` into the render pipeline (currently exists but is unused). Increase `MAX_DIRTY_RECTS` — the current limit of 6 is insufficient for line insert/delete (which can produce 30+ dirty rects before coalescing). Coalescing reduces these to a few strips in practice, but the pre-coalescing capacity must handle the worst case
- Add per-node render cache (offscreen surface pool, keyed by node ID, invalidated by content_hash)
- `render_scene()` accepts dirty bitmap, computes dirty rects, renders only intersecting nodes
- Scroll detection: compare content_transform to previous, blit-shift + exposed strip

### cpu-render (`services/drivers/cpu-render/`)

- Retain framebuffer between frames (stop clearing every frame)
- Read dirty bitmap from scene header
- Render only dirty rects via render library
- Partial `transfer_to_host` per dirty rect (virtio-gpu 2D already supports coordinates)

### virgil-render (`services/drivers/virgil-render/`)

- Read dirty bitmap from scene header
- Per-node GPU texture cache (render-to-texture, retain across frames)
- Scissor rects for dirty regions
- Glyph atlas already retained — unchanged glyphs don't re-upload

---

## 8. What This Enables

The incremental pipeline is foundation infrastructure. Layers built on top benefit automatically:

- **Compound documents with flow layout.** A text edit that causes reflow shifts images, paths, and other content. Those shifted elements are property-only changes — cached bitmaps blit'd at new positions. Without the incremental pipeline, every reflow would re-render every element.
- **New content types.** Audio waveforms, video thumbnails, embedded charts. Their rendered output is automatically cached. Layout shifts don't trigger re-rendering.
- **Cursor blink.** Currently redraws ~3 MiB. With incrementalism: ~32 pixels.
- **Animation.** Property-only changes (opacity, transform) can animate at the cost of a blit per frame. The renderer doesn't re-rasterize animated content.
- **Multiple documents.** If the design moves to split-view or tiling, unchanged document regions stay cached while the active region updates incrementally.

---

## 9. Error Handling

### Data buffer overflow mid-frame

If `push_data()` would exceed `DATA_BUFFER_SIZE` during an incremental update, the current frame is abandoned: trigger compaction (full rebuild), which resets the buffer and re-pushes only live data. The frame is delivered at compaction latency instead of incremental latency. This is transparent to the renderer — it sees a full-dirty frame.

### Content hash collisions

`content_hash` uses FNV-1a (32-bit). False negatives (unchanged content hashed differently) cause unnecessary re-renders — wasteful but correct. False positives (different content hashed identically) would cause stale rendering — a visual bug. The hash is computed over the raw data buffer content (glyph arrays, path commands), not the source text. Two different glyph arrays producing the same FNV-1a hash is unlikely but possible. Mitigation: if visual artifacts are observed, a secondary check (data length comparison, or upgrade to 64-bit hash) can be added behind the same `content_hash` interface. The 32-bit hash is sufficient for the current scale.

### Node array exhaustion mid-frame

If `alloc_node()` returns `None` during an incremental update (e.g., line insert when array is full), trigger compaction immediately. Same pattern as data buffer overflow.

---

## 10. Undo/Redo Accommodation

Undo is not yet implemented, but the COW snapshot model means reverting to a previous document state replaces the entire document text. This is equivalent to a context switch — the document content changes wholesale, and the incremental pipeline treats it as a full rebuild (compaction + all dirty bits). No special undo handling is needed in the pipeline design. The same applies to redo.

---

## 11. Testing Strategy

### Equivalence testing

The incremental path and the full-rebuild (compaction) path must produce identical scene graphs. Property-based tests generate random sequences of edit operations and verify that after each operation, an incremental update followed by a snapshot produces the same node array and data buffer as a full rebuild from the same document state.

### Dirty rect correctness

After an incremental render, compare the framebuffer to a full repaint of the same scene. No pixel should differ. This catches missed dirty rects (visual corruption from unchanged pixels where content actually changed) and over-dirtying (correctness-preserving but verifies the optimization is actually working). Run for each event type in the operation table.

### Scroll stress

Scroll through a multi-hundred-line document. Verify: (a) compaction triggers before buffer exhaustion, (b) no visual artifacts at compaction boundaries, (c) scroll remains responsive across compaction events.

### Cache correctness

Verify that property-only changes (node moved but content_hash unchanged) produce pixel-identical output whether rendered from cache (blit) or from source (full render). This catches cache invalidation bugs.

### Regression

The existing ~1,816 tests continue to pass. Scene library tests updated for the dirty bitmap API. New tests cover: dirty bitmap set/clear/popcount, incremental node allocation with dead slots, data buffer accumulation and compaction, content_transform-based child positioning.
