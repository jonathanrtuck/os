# Content Transform

**Date:** 2026-03-19
**Status:** Design complete, pending implementation
**Depends on:** Incremental scene pipeline (complete)

---

## Problem

The Node struct has `scroll_y: i32` — a single-axis integer content offset. This is:

- **Missing horizontal scroll** — no `scroll_x` field. Wide content (code, spreadsheets, canvas) can't scroll horizontally.
- **Integer-only** — scroll moves in whole-point jumps. Smooth scroll animation requires sub-point (float) offsets.
- **Too specific** — "scroll" is really "translate children within a clipped container." Content zoom (pinch-to-zoom on documents, images, PDFs) is a scale operation on the same coordinate space but has no field to express it.

## Solution

Replace `scroll_y: i32` with `content_transform: AffineTransform` on every Node.

Two transforms per node, two coordinate spaces:

- **`transform`** (existing) — positions the node relative to its parent's content area. "Where am I?"
- **`content_transform`** (new) — transforms the coordinate space for the node's children. "What do my children see?"

Scroll is a translation in `content_transform`. Zoom is a scale. Both are first-class, expressible through the same field. The renderer already has a pure-translation fast path for `AffineTransform`, so scroll performance is preserved.

---

## Node Struct Change

```rust
// Remove:
pub scroll_y: i32,                       // 4 bytes

// Add:
pub content_transform: AffineTransform,   // 24 bytes (6 × f32)
```

Node grows from 100 to 116 bytes (removing 4 bytes, adding 24 = +20 net; no alignment padding needed since the prior fields end at 4-byte alignment and `AffineTransform` contains `f32` which requires 4-byte alignment). Update the compile-time size assertion to 116. `NODE_SIZE`, `NODES_OFFSET`, `DATA_OFFSET`, `SCENE_SIZE`, `TRIPLE_SCENE_SIZE` cascade automatically from `size_of::<Node>()`.

`Node::EMPTY` initializes `content_transform` to `AffineTransform::identity()`.

---

## Transform Stack

The renderer maintains a world transform that accumulates through the tree. At each node, the transform stack is:

```text
parent_content_world                         (accumulated from ancestors)
  → compose(node.transform)                  = node_world  ("where is this node?")
  → compose(node.content_transform)          = content_world ("where do children draw?")
    → compose(child.transform)               = child_world
    → compose(child.content_transform)       = child_content_world
      → ...
```

For each node in the tree walk:

1. `node_world = parent_content_world.compose(node.transform)` — positions the node
2. Render the node at `node_world` (background, border, content)
3. `content_world = node_world.compose(node.content_transform)` — set up child space
4. For each child: recurse with `content_world` as the parent

When `content_transform` is identity (most nodes), step 3 is a no-op (`content_world == node_world`). The renderer's existing pure-translation fast path applies: if the composed world transform is a pure translation, use integer point offsets. If it includes scale or rotation, use the offscreen-buffer AABB path.

**Performance note:** A content_transform with scale (zoom) causes all children to take the complex transform path (offscreen buffer + bilinear resampling). This is expected and correct — zoomed content requires resampling. The pure-translation path handles scroll (the common case) efficiently.

---

## Semantics

### Scroll

```rust
// Scroll down 400pt (content shifts up):
node.content_transform = AffineTransform::translate(0.0, -400.0);

// Scroll right 200pt (content shifts left):
node.content_transform = AffineTransform::translate(-200.0, 0.0);

// Both axes:
node.content_transform = AffineTransform::translate(-200.0, -400.0);
```

The sign convention matches CSS: positive scroll offset moves content in the negative direction.

### Content zoom

```rust
// 2x zoom centered at origin:
node.content_transform = AffineTransform::scale(2.0, 2.0);

// Zoom then scroll (zoom applied first, scroll in zoomed space):
node.content_transform = AffineTransform::translate(-100.0, -200.0)
    .compose(AffineTransform::scale(2.0, 2.0));
```

Composition order: `translate.compose(scale)` applies scale first, then translate. This means the scroll offset is in viewport points (post-zoom), which matches user expectation: scrolling 100pt always moves the viewport 100pt, regardless of zoom level.

### Clipping under zoom

`CLIPS_CHILDREN` clips in the _node's_ coordinate space (before `content_transform`). A 400×300 container with `CLIPS_CHILDREN` and `content_transform = scale(2.0, 2.0)` clips children to the 400×300 point boundary of the container. Children at positions beyond (200, 150) in content space are outside the clip (since 200×2 = 400). This is correct — the container's visible area is fixed, zoom changes what fits inside it.

### Identity (no scroll, no zoom)

```rust
node.content_transform = AffineTransform::identity();
```

Default for all nodes. Only scrollable/zoomable containers set a non-identity `content_transform`.

---

## Renderer Changes

### walk.rs

Replace:

```rust
let child_origin_y = draw_y - scale_coord(node.scroll_y, s);
```

With:

```rust
// Compose content_transform for the child coordinate space.
let content_world = world_xform.compose(node.content_transform);
// Pass content_world (not world_xform) as the parent transform for children.
```

Both the pure-translation path and the complex-transform path must use `content_world` when recursing into children, instead of the current `world_xform`.

### virgil-render scene_walk.rs

Same change: replace `scroll_y` offset with `content_transform` composition in the child coordinate space.

### diff.rs abs_bounds()

The function currently walks the parent chain, summing integer x/y offsets and subtracting `scroll_y`. With `content_transform`, it must compose the full affine transform from each ancestor's `content_transform` into the accumulated position.

When `content_transform` includes scale or rotation (zoom), children's absolute bounds are transformed by the matrix — the function must compute an AABB of the transformed bounds. The existing AABB computation for `node.transform` (lines 63-79 of diff.rs) provides the pattern: transform the four corners of the node's rect, then take the min/max.

For pure translations (the common case), the AABB degenerates to a simple offset addition — no performance regression.

---

## IncrementalState Changes

Replace:

```rust
pub prev_scroll_y: [i32; MAX_NODES]    // 2 KiB
```

With:

```rust
pub prev_content_transform: [AffineTransform; MAX_NODES]  // 12 KiB
```

Net growth: 10 KiB. Heap-allocated (IncrementalState is already Box'd in both render services).

### Scroll detection

`detect_scroll()` returns `Option<(NodeId, f32, f32)>` — the node ID and delta in (tx, ty). Detection logic:

1. Node is dirty and has children (`first_child != NULL`)
2. `content_transform` differs from `prev_content_transform[i]`
3. Only the translation components changed (`a`, `b`, `c`, `d` unchanged) — use `is_pure_translation_change()` helper

If the translation delta is non-integer (smooth scroll), the blit-shift optimization cannot apply (it requires integer-point shifts (which map to whole-pixel shifts at integer scale factors)). In that case, the renderer falls back to full dirty rect repaint for the container region. Blit-shift is applied only when both `delta_tx` and `delta_ty` round to integers.

Add `PartialEq` derive to `AffineTransform` for frame-to-frame comparison.

Add `is_pure_translation()` helper: `a == 1.0 && b == 0.0 && c == 0.0 && d == 1.0`.

### Dirty rect computation

Content transform changes on a container affect all children's visual positions. The existing logic (mark container dirty → damage entire old+new container bounds) handles this correctly — the container's dirty rect covers all children.

---

## Core Changes

All `scroll_y` assignments become `content_transform` translations:

```rust
// Old:
w.node_mut(N_DOC_TEXT).scroll_y = scroll_px;

// New:
w.node_mut(N_DOC_TEXT).content_transform =
    AffineTransform::translate(0.0, -(scroll_px as f32));
```

The `CLIPS_CHILDREN` flag continues to control clipping. This is set independently.

### scene_state.rs

Functions that take `scroll_y: i32` parameters (`build_editor_scene`, `update_document_content`, `update_document_incremental`, `update_document_insert_line`, `update_document_delete_line`) keep the `scroll_y: i32` parameter for now. The conversion to `AffineTransform::translate()` happens inside the layout functions. This limits the change to layout.rs and avoids cascading through main.rs dispatch logic.

---

## What Changes Where

### Scene library (`libraries/scene/`)

- `node.rs`: Remove `scroll_y: i32`. Add `content_transform: AffineTransform`. Update `Node::EMPTY`, size assertion (100 → 116).
- `transform.rs`: Add `translate(tx, ty)` and `scale(sx, sy)` constructors. Add `is_pure_translation()` helper. Derive `PartialEq`.
- `diff.rs`: Update `abs_bounds()` to compose `content_transform` from ancestors. Use AABB computation for non-translation transforms (same pattern as existing `node.transform` AABB logic).

### Render library (`libraries/render/`)

- `scene_render/walk.rs`: Replace `scroll_y` child offset with `content_transform` composition. Pass `content_world` as parent transform for child recursion.
- `incremental.rs`: Replace `prev_scroll_y` with `prev_content_transform`. Update `detect_scroll()` signature to return `Option<(NodeId, f32, f32)>`. Update `update_from_frame()`.

### Core service (`services/core/`)

- `layout.rs`: Replace all `node.scroll_y = ...` with `node.content_transform = AffineTransform::translate(...)`. Replace all `scroll_y` reads (visibility culling in `scroll_runs`, `allocate_selection_rects`) with reads from `content_transform.ty`.
- `scene_state.rs`: No signature changes. `scroll_y: i32` parameter kept; conversion happens in layout functions.

### Render services

- `cpu-render/main.rs`: No changes (IncrementalState API change is internal).
- `virgil-render/main.rs`: No changes.
- `virgil-render/scene_walk.rs`: Replace `scroll_y` offset with `content_transform` composition.

### Documentation

- Update `design/incremental-scene-pipeline.md`: replace `scroll_y` references with `content_transform` throughout (8+ locations).
- Update `system/DESIGN.md`: replace `scroll_y` reference with `content_transform`.

### Tests

- Update all tests that set/read `scroll_y` to use `content_transform`.
- Add tests for horizontal scroll (`translate(-x, 0.0)`).
- Add test for content zoom (`scale(2.0, 2.0)`) with abs_bounds verification.
- Add test for `is_pure_translation()`.
- Verify `abs_bounds()` correctness under zoom (transformed AABB).

---

## What This Enables

- **Horizontal scroll** — `content_transform.tx`, no separate field.
- **Smooth scroll** — f32 translation for subpixel animation.
- **Content zoom** — pinch-to-zoom on documents, images, PDFs. Scale in `content_transform`.
- **Unified animation** — a future property animation system can drive `content_transform` the same way it drives `transform`, `opacity`, or any other node property.
- **Content-type agnostic** — text scroll, image pan, canvas zoom, spreadsheet scroll all use the same mechanism.
