# Rendering Pipeline: Completion Checklist

**Date:** 2026-03-19
**Context:** The rendering pipeline is the current foundation layer being built out completely. Layers above (text editor, content types, path rendering) are test harnesses exercising this layer. This document tracks what remains to make the pipeline layer complete.

---

## Pipeline Architecture (for reference)

```text
Input -> Core (shaping, layout, scene building) -> Scene Graph (shared memory) -> Render Service (tree walk, rasterization, compositing, present) -> Display
```

Two render backends: `cpu-render` (software) and `virgil-render` (GPU via Virgl3D).

---

## A. Performance Architecture — SUBSUMED (2026-03-19)

**All items below are subsumed by the incremental scene pipeline design.** See `design/incremental-scene-pipeline.md` (spec) and `design/plan-incremental-scene-pipeline.md` (implementation plan). The incremental pipeline addresses A1 (dirty rects), A2 (hot-path allocations via incremental building), A3 (full reshaping via per-line updates), A4 (shadow caching via per-node render cache), and A5 (change list overflow via dirty bitmap).

The pipeline's data flow is correct but has structural performance issues that would be painful to retrofit later.

### A1. Dirty-rect pipeline is designed but unwired

The infrastructure exists — `DamageTracker`, `change_count`, `content_hash`, `changed_nodes` in the scene header — but nothing consumes it. Every frame is a full repaint + full-screen GPU transfer (~3 MiB).

**What exists:**

- Scene header tracks up to 24 changed node IDs per frame
- Each node has a `content_hash` for change detection
- `DamageTracker` in the render library computes dirty rects
- `PresentPayload` supports per-rect transfer

**What's missing:**

- Render backend doesn't read changed_nodes — always walks full tree
- cpu-render always transfers full framebuffer to GPU
- Scene header overflows at 24 changed nodes (scrolling changes 40+), forcing FULL_REPAINT
- No dirty bitmap alternative (512 bits = 64 bytes would cover all nodes)

**Impact:** A cursor blink redraws ~3 MiB. Typing a character redraws ~3 MiB. The pipeline does O(entire screen) work for O(single character) changes.

### A2. Hot-path allocations in scene building

`layout_mono_lines()`, `shape_text()`, and `line_glyph_refs` allocate `Vec` on every frame. In bare-metal no_std, every `Vec` allocation hits a linked-list GlobalAlloc which may trigger a `memory_alloc` syscall.

**Impact:** Allocation overhead on every keystroke. Not catastrophic (the allocator works), but unnecessary when the buffer sizes are bounded and predictable.

**Fix:** Pre-allocate fixed-capacity buffers in `CoreState`, reuse across frames.

### A3. Full scene reshaping on every text change

`update_document_content` reshapes ALL visible text lines (title, clock, every document line) even when only one changed. `shape_text` is called per line per frame.

**Impact:** O(visible_lines) shaping work for O(1) text change. Shaping is the most expensive per-line operation (HarfBuzz allocation + glyph lookup).

**Fix:** Track which lines changed (byte range of edit vs line boundaries), only reshape those lines. `content_hash` on nodes could detect unchanged lines.

### A4. Shadow buffers allocated per frame

`render_shadow()` allocates 3 temporary buffers (up to 4 MiB each) per frame via `Vec`. Shadows are identical when only text content changes (shadow is on the chrome, not the text).

**Impact:** Up to 12 MiB of allocation + deallocation per frame for unchanging visual content.

**Fix:** Cache shadow results in `SurfacePool` keyed on node dimensions + shadow params. Invalidate only when those change.

### A5. Change list overflow forces full repaint

Scene header has space for 24 changed node IDs. Scrolling changes 40+ nodes, overflowing the list and triggering `FULL_REPAINT`. This defeats incremental rendering for the most common user operation.

**Fix:** Replace the 24-entry changed-node list with a dirty bitmap (512 bits = 64 bytes, one bit per MAX_NODES). Or increase the list. Or use a hybrid (list for small changes, bitmap flag for "too many").

---

## B. Render Pipeline Features — DONE (2026-03-19)

All items completed. See `plan-rendering-pipeline-completion.md` for implementation details.

- **B1. Non-ASCII glyph rendering** — DONE. `LruGlyphCache` wired as three-tier fallback in cpu-render: ASCII cache → LRU cache → on-demand rasterization. `LruRasterizer` struct separates mutable LRU state from immutable caches for split borrows through the tree walk.
- **B2. Bilinear interpolation** — DONE. Gamma-correct: linearize 4 samples via `SRGB_TO_LINEAR`, interpolate in linear space, re-encode via `LINEAR_TO_SRGB`.
- **B3. NEON horizontal blur** — RENAMED to `blur_horizontal_scalar_4x` (name collision with existing fallback). Actual NEON port deferred — moderate effort for marginal cpu-render-only benefit.
- **B4. Alpha rounding** — DONE. Removed `+ 127` double-rounding from 4 call sites in `draw_coverage`. Consistent with `Color::blend_over` and all other `div255` call sites.
- **B5. Font size/DPI config** — DONE. Added `font_size: u16` and `screen_dpi: u16` to `CompositorConfig` (replaced `frame_rate` + `_pad`). Init populates, both render services read from config.
- **B6. Selection data leak** — DONE. Removed `update_clock_inline` call from selection update path. Clock updates only via `update_document_content` (timer-driven, which calls `reset_data()`).

---

## C. System Robustness — DONE (2026-03-19)

- **C1. Crash detection** — DONE. Init collects all child process handles into a monitoring array. Idle loop replaced with `sys::wait()` on child handles — logs process name on exit, removes from array, continues monitoring remaining children.
- **C2. Boot timeouts** — DONE. All three wait loops (display info, GPU ready, font read) use finite timeouts (10s boot, 5s font) with 3 retries. Display/GPU timeout is fatal; font timeout falls back gracefully (bitmap font).

---

## D. Code Organization — DONE (2026-03-19)

Four files split into well-scoped modules:

| Original file            | Lines | Split into | Modules                                                   |
| ------------------------ | ----- | ---------- | --------------------------------------------------------- |
| `virgil-render/main.rs`  | 2,060 | 5 files    | wire, device, resources, pipeline, main                   |
| `scene/lib.rs`           | 1,805 | 7 files    | primitives, transform, node, writer, reader, triple, diff |
| `render/scene_render.rs` | 1,824 | 4 files    | coords, path_raster, content, walk (directory module)     |
| `core/scene_state.rs`    | 1,200 | 3 files    | scene_state, layout, test_gen                             |

Files NOT split (assessed and deemed well-organized): `core/main.rs` (1,054), `init/main.rs` (1,056), `virgil-render/scene_walk.rs` (1,042 — single responsibility).

---

## E. Above the Pipeline (correctly deferred)

These belong to layers above the rendering pipeline:

| Item                                      | Layer            | Why deferred                     |
| ----------------------------------------- | ---------------- | -------------------------------- |
| Pointer button only processes button 0    | Input/editor     | Editor-level input handling      |
| Raw message forwarding to editor          | Editor protocol  | Editor IPC design                |
| Test content generators always run        | Test scaffolding | Exists to exercise pipeline      |
| Fallback shapes full text with every font | Text shaping     | Optimization for large documents |
| Path centroid uses simple average         | Path content     | Accuracy for complex paths       |
| `byte_to_line_col` is O(n)                | Text layout      | Optimization for large documents |
