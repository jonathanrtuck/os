# Content Transform Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `scroll_y: i32` with `content_transform: AffineTransform` on Node, enabling 2D scroll, smooth scroll, and content zoom through a single general-purpose field.

**Architecture:** Field substitution on the Node struct + updating all 11 files that reference `scroll_y`. The renderer's transform composition gets a second stage: after computing `node_world` from the node's own `transform`, compose `content_transform` to get `content_world` for children. Most helpers already exist on `AffineTransform` (`translate`, `scale`, `compose`, `transform_aabb`).

**Tech Stack:** Rust `no_std`, bare-metal aarch64, QEMU virt. Tests: `cd system/test && cargo test -- --test-threads=1`.

**Spec:** `design/content-transform.md`

---

## File Structure

| File | Change | Responsibility |
|------|--------|---------------|
| `libraries/scene/node.rs` | Modify | Remove `scroll_y: i32`, add `content_transform: AffineTransform`. Update `Node::EMPTY`, size assertion (100→116). |
| `libraries/scene/transform.rs` | Modify | Add `PartialEq` derive. Add `is_pure_translation()` helper. |
| `libraries/scene/diff.rs` | Modify | Update `abs_bounds()` to compose `content_transform` from ancestors (AABB for non-translation). |
| `libraries/render/scene_render/walk.rs` | Modify | Use `content_world = world_xform.compose(node.content_transform)` for child recursion. |
| `libraries/render/incremental.rs` | Modify | Replace `prev_scroll_y: [i32; MAX_NODES]` with `prev_content_transform: [AffineTransform; MAX_NODES]`. Update `detect_scroll()` and `update_from_frame()`. |
| `services/core/layout.rs` | Modify | Replace `scroll_y = ...` with `content_transform = AffineTransform::translate(...)`. Update visibility culling to read `content_transform.ty`. |
| `services/core/scene_state.rs` | Modify | Keep `scroll_y: i32` parameter signatures. No structural change — conversion happens inside layout. |
| `services/drivers/virgil-render/scene_walk.rs` | Modify | Replace `scroll_y` offset with `content_transform` composition. |
| `test/tests/scene.rs` | Modify | Update all `scroll_y` references. Add content_transform tests. |
| `test/tests/scene_render.rs` | Modify | Update `scroll_y` references. |
| `test/tests/incremental.rs` | Modify | Update `prev_scroll_y` references. Add horizontal scroll + zoom tests. |
| `DESIGN.md` | Modify | Update `scroll_y` reference to `content_transform`. |
| `design/incremental-scene-pipeline.md` | Modify | Update 8+ `scroll_y` references to `content_transform`. |

---

## Task 1: Scene Library — Node + Transform

Update the Node struct and AffineTransform helpers.

**Files:**
- Modify: `system/libraries/scene/node.rs`
- Modify: `system/libraries/scene/transform.rs`
- Modify: `system/test/tests/scene.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn node_has_content_transform_field() {
    let mut buf = vec![0u8; scene::SCENE_SIZE];
    let mut w = scene::SceneWriter::new(&mut buf);
    let id = w.alloc_node().unwrap();
    let ct = AffineTransform::translate(-200.0, -400.0);
    w.node_mut(id).content_transform = ct;
    assert_eq!(w.node(id).content_transform.tx, -200.0);
    assert_eq!(w.node(id).content_transform.ty, -400.0);
}

#[test]
fn affine_transform_is_pure_translation() {
    assert!(AffineTransform::translate(10.0, 20.0).is_pure_translation());
    assert!(AffineTransform::identity().is_pure_translation());
    assert!(!AffineTransform::scale(2.0, 2.0).is_pure_translation());
    assert!(!AffineTransform::rotate(0.5).is_pure_translation());
}

#[test]
fn affine_transform_partial_eq() {
    let a = AffineTransform::translate(1.0, 2.0);
    let b = AffineTransform::translate(1.0, 2.0);
    let c = AffineTransform::translate(1.0, 3.0);
    assert_eq!(a, b);
    assert_ne!(a, c);
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Add `PartialEq` derive and `is_pure_translation()` to transform.rs**

```rust
#[derive(Clone, Copy, Debug, PartialEq)]  // add PartialEq
pub struct AffineTransform { ... }

/// Returns true if this is a pure translation (no rotation, scale, or skew).
/// Both identity and non-zero translations return true.
pub fn is_pure_translation(&self) -> bool {
    self.a == 1.0 && self.b == 0.0 && self.c == 0.0 && self.d == 1.0
}
```

- [ ] **Step 4: Update Node struct in node.rs**

Remove `scroll_y: i32`. Add `content_transform: AffineTransform` in its place. Update `Node::EMPTY` to set `content_transform: AffineTransform::identity()`. Update compile-time size assertion to 116 bytes.

- [ ] **Step 5: Update all `scroll_y` references in test files**

Search and replace `scroll_y` in `system/test/tests/scene.rs` and `system/test/tests/scene_render.rs`. Replace `node.scroll_y = N` with `node.content_transform = AffineTransform::translate(0.0, -(N as f32))`. Replace reads of `scroll_y` with reads of `content_transform.ty`.

Note: the sign flips — `scroll_y = 400` (old) becomes `content_transform.ty = -400.0` (new). Scrolling down means content moves up.

- [ ] **Step 6: Run all tests, verify pass**

- [ ] **Step 7: Commit**

```
feat: replace scroll_y with content_transform on Node
```

---

## Task 2: Scene Library — abs_bounds Update

Update `abs_bounds()` in diff.rs to compose `content_transform` from ancestors.

**Files:**
- Modify: `system/libraries/scene/diff.rs`
- Modify: `system/test/tests/scene.rs` or `system/test/tests/incremental.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn abs_bounds_with_content_transform_scroll() {
    // Parent at (0, 100) with content_transform = translate(0, -50)
    // Child at (10, 20) — absolute should be (10, 100 + 20 - 50) = (10, 70)
}

#[test]
fn abs_bounds_with_content_transform_zoom() {
    // Parent at (0, 0) with content_transform = scale(2, 2)
    // Child at (10, 20) with size (30, 40)
    // Absolute should be AABB of scaled rect: (20, 40, 60, 80)
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Update `abs_bounds()` in diff.rs**

The function walks the parent chain accumulating position. Currently it subtracts `scroll_y`. Replace with `content_transform` composition:

- When walking up the parent chain, if a parent has a non-identity `content_transform`, apply it to the accumulated (x, y, w, h) via `transform_aabb()`.
- For pure translations (`is_pure_translation()`), this degenerates to adding `tx`/`ty` — same performance as the old `scroll_y` subtraction.
- For scale/rotation, use the full AABB computation.

- [ ] **Step 4: Run all tests**

- [ ] **Step 5: Commit**

```
feat: compose content_transform in abs_bounds for zoom support
```

---

## Task 3: Renderer Walk — Content Transform Composition

Update the CPU render walk and virgil-render scene walk to use `content_transform` for child coordinate space.

**Files:**
- Modify: `system/libraries/render/scene_render/walk.rs`
- Modify: `system/services/drivers/virgil-render/scene_walk.rs`

- [ ] **Step 1: Update walk.rs**

Find the two sites where `scroll_y` is used (~lines 758 and 824):
```rust
let child_origin_y_off = 0i32 - scale_coord(node.scroll_y, s);
let child_origin_y = draw_y - scale_coord(node.scroll_y, s);
```

Replace with `content_transform` composition. The content_world transform becomes the parent transform passed to children:
```rust
let content_world = world_xform.compose(node.content_transform);
```

Then use `content_world` instead of `world_xform` when recursing into children. Both the pure-translation path and the complex-transform path need this change.

- [ ] **Step 2: Update virgil-render/scene_walk.rs**

Find `scroll_y` usage (2 occurrences) and replace with `content_transform` composition, same pattern.

- [ ] **Step 3: Run all tests + release build**

- [ ] **Step 4: Commit**

```
feat: use content_transform for child coordinate space in render walks
```

---

## Task 4: IncrementalState — Replace prev_scroll_y

Update the incremental rendering state to track `content_transform` instead of `scroll_y`.

**Files:**
- Modify: `system/libraries/render/incremental.rs`
- Modify: `system/test/tests/incremental.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn detect_scroll_with_content_transform() {
    // Container with content_transform changed from translate(0, 0) to translate(0, -20)
    // detect_scroll should return Some((node_id, 0.0, -20.0))
}

#[test]
fn detect_scroll_horizontal() {
    // content_transform changed from identity to translate(-100, 0)
    // detect_scroll returns Some((node_id, -100.0, 0.0))
}

#[test]
fn detect_scroll_ignores_zoom_change() {
    // content_transform changed from identity to scale(2, 2)
    // This is NOT a scroll — detect_scroll returns None
}
```

- [ ] **Step 2: Run tests to verify they fail**

- [ ] **Step 3: Replace `prev_scroll_y` with `prev_content_transform`**

In `IncrementalState`:
- Remove `prev_scroll_y: [i32; MAX_NODES]`
- Add `prev_content_transform: [AffineTransform; MAX_NODES]`
- Update `new()`: initialize to `[AffineTransform::identity(); MAX_NODES]`
- Update `update_from_frame()`: copy `node.content_transform` instead of `node.scroll_y`
- Update `detect_scroll()`: compare `content_transform` against `prev_content_transform[i]`. Return `Option<(NodeId, f32, f32)>`. Only report as scroll when `is_pure_translation()` holds for both old and new (scale/rotation changes are not scrolls).

- [ ] **Step 4: Update all `prev_scroll_y` references in test files**

In `system/test/tests/incremental.rs`, replace all `scroll_y` references with `content_transform`.

- [ ] **Step 5: Run all tests**

- [ ] **Step 6: Commit**

```
feat: track content_transform in IncrementalState for 2D scroll detection
```

---

## Task 5: Core — Layout Updates

Replace all `scroll_y` assignments in the core service with `content_transform` translations.

**Files:**
- Modify: `system/services/core/layout.rs` (22 occurrences)
- Modify: `system/services/core/scene_state.rs` (keep parameter names, update internal usage)
- Modify: `system/test/tests/scene.rs` (remaining scroll_y in test helpers)

- [ ] **Step 1: Update layout.rs**

Replace all `node.scroll_y = scroll_px` with:
```rust
node.content_transform = AffineTransform::translate(0.0, -(scroll_px as f32));
```

Replace all reads of `scroll_y` for visibility culling (in `scroll_runs`, `allocate_selection_rects`, `update_single_line`, `insert_line`, `delete_line`) with reads from `content_transform.ty`. Note the sign: `scroll_px` was positive (scroll offset), `content_transform.ty` is negative (content shifts up). So visibility checks like `run.y + line_h > scroll_px` become `run.y + line_h > (-content_transform.ty) as i32` or better, compute `scroll_px` from `content_transform.ty` once at the top of each function.

- [ ] **Step 2: Update scene_state.rs**

Keep `scroll_y: i32` in function parameter signatures (no cascading change through main.rs). The `scroll_y` parameter is converted to `AffineTransform::translate()` inside layout functions, same as today.

Remove any direct `scroll_y` field references if they exist.

- [ ] **Step 3: Update remaining test helpers**

In `system/test/tests/scene.rs`, update `build_test_editor_scene` and any other helpers that set `scroll_y`.

- [ ] **Step 4: Run all tests + release build**

- [ ] **Step 5: Commit**

```
feat: use content_transform translations for scroll in core layout
```

---

## Task 6: Documentation Updates

Update design documents and DESIGN.md.

**Files:**
- Modify: `system/DESIGN.md`
- Modify: `design/incremental-scene-pipeline.md`

- [ ] **Step 1: Update DESIGN.md**

Replace the `scroll_y` reference with `content_transform`.

- [ ] **Step 2: Update incremental-scene-pipeline.md**

Replace all 8+ `scroll_y` references with `content_transform` terminology.

- [ ] **Step 3: Commit**

```
docs: update design docs for content_transform (replaces scroll_y)
```

---

## Task Dependencies

```
Task 1 (Node + Transform) → Task 2 (abs_bounds) → Task 3 (Renderer walk)
                                                  → Task 4 (IncrementalState)
                                                  → Task 5 (Core layout)
Task 5 → Task 6 (Docs)
```

Task 1 is the foundation. Tasks 2-5 can be done in any order after Task 1 (they all depend on the new field existing). Task 6 is last.

---

## Verification Checklist

- [ ] `cargo test -- --test-threads=1` — all tests pass
- [ ] `cargo build --release` — builds clean
- [ ] Zero occurrences of `scroll_y` in any `.rs` file (grep to verify)
- [ ] `content_transform` used correctly: negative ty for scroll-down
- [ ] `abs_bounds` handles zoom (scale content_transform) correctly
- [ ] Render walks compose content_transform for child coordinate space
- [ ] `detect_scroll` returns (NodeId, f32, f32) and ignores zoom changes
- [ ] Node size assertion is exactly 116 bytes
