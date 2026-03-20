# Incremental Pipeline Wiring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the incremental rendering pipeline by wiring clipped rendering, per-node cache, scroll blit-shift, and virgil-render scissor rects into the actual render services.

**Architecture:** The infrastructure is already built (dirty bitmap, IncrementalState, NodeCache, DamageTracker, `render_scene_clipped*`). This plan wires it into the render loops so the CPU/GPU only processes changed regions. The render walk already clips to a `ClipRect` — we just need to call it per dirty rect instead of per full screen.

**Tech Stack:** Rust `no_std`, bare-metal aarch64, QEMU virt. Tests: `cd system/test && cargo test -- --test-threads=1`.

**Spec:** `design/incremental-scene-pipeline.md` (Sections 5-6), `design/content-transform.md`

---

## File Structure

| File | Change | Responsibility |
|------|--------|---------------|
| `libraries/render/scene_render/walk.rs` | Modify | Add `render_scene_clipped_full()` (with pool + LRU). Existing `render_scene_clipped` and `render_scene_clipped_with_pool` are already present. |
| `libraries/render/lib.rs` | Modify | Update `CpuBackend::render()` to accept optional dirty rects. Or add `render_incremental()` method. |
| `services/drivers/cpu-render/main.rs` | Modify | Call clipped render per dirty rect instead of full render. Wire scroll blit-shift. Integrate NodeCache. |
| `services/drivers/virgil-render/main.rs` | Modify | Add Gallium3D scissor rect commands before draw calls when dirty rects are available. |
| `services/drivers/virgil-render/scene_walk.rs` | Modify | May need scissor state in the walk, or scissor can be set in the command buffer before the walk. |
| `test/tests/incremental.rs` | Modify | Add tests for clipped render producing correct results. |

---

## Task 1: Add render_scene_clipped_full to walk.rs

The existing `render_scene_full` passes the entire framebuffer as the clip rect. We need a variant that accepts a `DirtyRect` as the clip — with pool + LRU support (matching `render_scene_full`).

**Files:**
- Modify: `system/libraries/render/scene_render/walk.rs`

- [ ] **Step 1: Add `render_scene_clipped_full()`**

```rust
/// Render only the region within `dirty`, with SurfacePool + LRU rasterizer.
/// This is the incremental rendering entry point for CpuBackend.
pub fn render_scene_clipped_full(
    fb: &mut Surface,
    graph: &SceneGraph,
    ctx: &RenderCtx,
    dirty: &protocol::DirtyRect,
    pool: &mut SurfacePool,
    lru: &mut LruRasterizer,
) {
    if graph.nodes.is_empty() || dirty.w == 0 || dirty.h == 0 {
        return;
    }
    let clip = ClipRect {
        x: dirty.x as i32,
        y: dirty.y as i32,
        w: dirty.w as i32,
        h: dirty.h as i32,
    };
    render_node(fb, graph, ctx, 0, 0, 0, clip, Some(pool), Some(lru));
}
```

- [ ] **Step 2: Run tests, verify pass**

- [ ] **Step 3: Commit**

```
feat: add render_scene_clipped_full for incremental rendering with LRU
```

---

## Task 2: cpu-render Clipped Rendering

Replace the full `backend.render()` call with per-dirty-rect clipped rendering. The CPU now only walks nodes that intersect dirty rects.

**Files:**
- Modify: `system/services/drivers/cpu-render/main.rs`
- Modify: `system/libraries/render/lib.rs` (if CpuBackend needs a new method)

- [ ] **Step 1: Update the render loop**

Currently (after Task 8 of the incremental pipeline), cpu-render does:
1. Read dirty bitmap
2. Skip if all zero
3. Compute dirty rects
4. **Full render** (`backend.render(&graph, &mut make_fb(render_buf))`)
5. Partial GPU transfer per dirty rect

Change step 4: instead of full render, render per dirty rect:

```rust
// Instead of: backend.render(&graph, &mut make_fb(render_buf));
// Do:
if let Some(ref damage) = damage {
    if let Some(rects) = damage.dirty_rects() {
        for r in rects {
            if r.w > 0 && r.h > 0 {
                render_scene_clipped_full(
                    &mut make_fb(render_buf), &graph, &ctx,
                    r, &mut pool, &mut lru,
                );
            }
        }
    } else {
        // Full-screen fallback (overflow or all-dirty)
        render_scene_full(&mut make_fb(render_buf), &graph, &ctx, &mut pool, &mut lru);
    }
} else {
    // No damage computed (first incremental frame) — full render
    render_scene_full(&mut make_fb(render_buf), &graph, &ctx, &mut pool, &mut lru);
}
```

**Key issue: retained framebuffer.** For clipped rendering to work, the framebuffer must retain its contents from the previous frame. Currently cpu-render uses double buffering (`render_buf = 1 - presented_buf`) — the non-presented buffer may be stale (it was last rendered 2 frames ago, or never).

**Solution:** Before clipped rendering, copy the presented buffer to the render buffer:

```rust
// Copy presented_buf → render_buf so unchanged regions are preserved.
if !is_full_render {
    let src = make_fb(presented_buf);
    let dst_ptr = make_fb_ptr(render_buf);
    // SAFETY: both buffers are fb_size bytes, non-overlapping DMA regions.
    unsafe {
        core::ptr::copy_nonoverlapping(src.data.as_ptr(), dst_ptr, fb_size as usize);
    }
}
```

This is a full-framebuffer copy (~3 MiB) which partially negates the savings. An optimization: skip the copy for full renders, and for incremental renders the copy ensures correctness. The GPU transfer savings (partial vs full) still dominate since GPU bus bandwidth is the bottleneck.

**Alternative:** Render to the presented buffer directly (avoiding the copy but risking tearing). For now, do the copy — correctness first.

- [ ] **Step 2: Wire the SurfacePool and LruRasterizer**

The CpuBackend already has pool and LRU. Check if they're accessible for the per-rect render calls. If `backend.render()` encapsulates them, we may need to expose them or add a `render_dirty_rects()` method to CpuBackend.

- [ ] **Step 3: Run all tests + release build**

- [ ] **Step 4: Visual verification in QEMU**

Build and run. Type characters — verify only the changed line + cursor region is re-rendered (check via timing or serial output logging the dirty rect sizes).

- [ ] **Step 5: Commit**

```
feat: cpu-render clipped rendering per dirty rect
```

---

## Task 3: Scroll Blit-Shift in cpu-render

When the incremental state detects a scroll (pure translation change on a container), shift existing framebuffer pixels instead of re-rendering the scrolled region.

**Files:**
- Modify: `system/services/drivers/cpu-render/main.rs`

- [ ] **Step 1: Detect scroll after computing dirty rects**

```rust
let scroll = incr_state.detect_scroll(nodes, &dirty_bits);
```

- [ ] **Step 2: Implement blit-shift**

If `detect_scroll` returns `Some((container_id, delta_tx, delta_ty))`:

1. Round deltas to integer pixels: `let dy = (delta_ty * scale) as i32`. If fractional, skip blit-shift (full render for the container region).
2. Compute the container's screen bounds from `prev_bounds[container_id]`.
3. Blit-shift: `memmove` within the framebuffer, shifting the container region by `-dy` scanlines.
   - Scroll down (negative delta_ty): pixels move up. Copy from `src_y = container_y + abs(dy)` to `dst_y = container_y`, height = `container_h - abs(dy)`.
   - Scroll up (positive delta_ty): pixels move down. Copy from `src_y = container_y` to `dst_y = container_y + abs(dy)`.
4. Add the newly exposed strip to the dirty rect list (at the top or bottom edge of the container).
5. Continue with clipped rendering for the exposed strip + any other dirty rects.

- [ ] **Step 3: Run tests + visual verification**

Scroll through a document. Verify: content shifts smoothly, newly exposed lines appear correctly at the edge, no visual artifacts.

- [ ] **Step 4: Commit**

```
feat: scroll blit-shift optimization in cpu-render
```

---

## Task 4: virgil-render Scissor Rects

Add Gallium3D scissor state to limit GPU fragment work to dirty regions.

**Files:**
- Modify: `system/services/drivers/virgil-render/main.rs`
- Possibly modify: `system/libraries/protocol/virgl.rs` (if `cmd_set_scissor_state` doesn't exist)

- [ ] **Step 1: Check if scissor command exists**

Search for `scissor` in the protocol/virgl command encoder. Gallium3D has `VIRGL_CCMD_SET_SCISSOR_STATE`. If the command isn't encoded yet, add it.

- [ ] **Step 2: Compute dirty rects in virgil-render**

After reading dirty bits (which already happens), call `incr_state.compute_dirty_rects()` to get a `DamageTracker`. Compute a bounding box of all dirty rects.

- [ ] **Step 3: Set scissor before draw calls**

Before the draw commands in the command buffer:
```rust
if let Some(ref damage) = damage {
    let bbox = damage.bounding_box();
    cmdbuf.cmd_set_scissor_state(bbox.x as u32, bbox.y as u32, bbox.w as u32, bbox.h as u32);
}
```

This limits GPU fragment processing to the bounding box of dirty rects. The GPU still processes all vertices (the walk still runs), but fragments outside the scissor are discarded — significant savings for small changes like cursor blink.

- [ ] **Step 4: Restore full scissor for full-repaint frames**

When `compute_dirty_rects` returns None (full repaint), set scissor to the full viewport or disable scissor test.

- [ ] **Step 5: Run tests + release build + visual verification**

Build with virgl QEMU. Type characters, verify correct rendering.

- [ ] **Step 6: Commit**

```
feat: virgil-render scissor rects for incremental rendering
```

---

## Task 5: Per-Node Cache Integration (Deferred)

**This task is documented but deferred.** The per-node `NodeCache` (Task 7 of the incremental pipeline) is built and tested but not wired into the render walk. Wiring it requires modifying the tree walk to:

1. Check `cache.get(node_id, content_hash)` before rendering
2. If hit: blit cached bitmap at node position (skip rasterization)
3. If miss: render normally, then `cache.store(node_id, content_hash, w, h, pixels)`

This is a larger change to `walk.rs` that touches the render path for every node. It should be done after Tasks 1-4 are validated in QEMU, since incorrect cache behavior could cause subtle rendering bugs.

---

## Task Dependencies

```
Task 1 (clipped_full) → Task 2 (cpu-render clipped rendering)
                       → Task 3 (scroll blit-shift)
Task 1 → Task 4 (virgil scissor) [independent of Tasks 2-3]
Task 5 (per-node cache) deferred
```

---

## Verification Checklist

- [ ] `cargo test -- --test-threads=1` — all tests pass
- [ ] `cargo build --release` — builds clean
- [ ] QEMU cpu-render: cursor blink only re-renders cursor region (not full screen)
- [ ] QEMU cpu-render: typing re-renders only the changed line + cursor
- [ ] QEMU cpu-render: scroll blit-shifts content, renders only exposed strip
- [ ] QEMU virgil-render: scissor limits GPU fragment work to dirty regions
- [ ] No visual artifacts in any scenario
