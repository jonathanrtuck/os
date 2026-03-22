# v0.3 Phase 2: Composition — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add path clipping, backdrop blur, pointer cursor, and fix document switching — the composition layer that makes the rendering pipeline feel like a real OS.

**Architecture:** Node struct grows from 120→136 bytes to carry `clip_path: DataRef` and `backdrop_blur_radius: u8`. CpuBackend gains an 8bpp clip mask cache and backdrop blur pipeline. Core gains a pointer cursor node, cursor shape switching, and image-mode scene building. Render services handle external VA image references.

**Design notes:**

- Clip masks use 8bpp alpha (not 1bpp) for anti-aliased clip edges — same scanline rasterizer as `Content::Path`.
- Backdrop blur reuses the existing NEON-accelerated `drawing::blur_surface()`.
- Pointer cursor is a well-known node at highest z-order, updated via a lightweight incremental path.
- Document switching extends `build_full_scene()` with `image_mode` — image data uses a sentinel (`data.offset == u32::MAX`) for external VA references.
- virgil-render tasks (4, 6) use GPU stencil and blur shaders — independent of CpuBackend and can be parallelized.

**Spec:** `design/v0.3-spec.md` sections 2.1–2.4 + Phase 2 demos.

**Tech Stack:** Rust `no_std`, `#![no_std]`, bare-metal aarch64. Host-side tests in `system/test/`.

---

## File Map

### New files

| File                                     | Purpose                                            |
| ---------------------------------------- | -------------------------------------------------- |
| `libraries/render/clip_mask.rs`          | 8bpp clip mask rasterizer + LRU cache (16 slots)   |
| `services/drivers/virgil-render/blur.rs` | Gaussian blur shaders + render-to-texture pipeline |
| `test/tests/clip_mask.rs`                | Clip mask unit tests                               |

### Modified files

| File                                           | Change                                                                                                                                         |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `libraries/scene/node.rs`                      | Node grows 120→136 bytes: `_pad` → `backdrop_blur_radius`, add `clip_path: DataRef`, add `_reserved: [u8; 8]`                                  |
| `libraries/scene/primitives.rs`                | `DataRef::EMPTY` constant, `DataRef::is_empty()` method                                                                                        |
| `libraries/render/lib.rs`                      | Add `mod clip_mask`, plumb clip mask cache through CpuBackend                                                                                  |
| `libraries/render/scene_render/walk.rs`        | Clip mask application during child rendering, backdrop blur extract-blur-composite                                                             |
| `libraries/render/scene_render/path_raster.rs` | Extract `rasterize_to_mask()` (8bpp alpha) from existing path fill code                                                                        |
| `libraries/protocol/lib.rs`                    | Expand `ImageConfig` with `width`/`height`. CompositorConfig unchanged — image metadata sent via separate `MSG_IMAGE_CONFIG` to render service |
| `services/core/main.rs`                        | Store image metadata, pointer cursor state, pointer hide timeout, update_pointer path                                                          |
| `services/core/layout/mod.rs`                  | New `N_POINTER` well-known node (14), `WELL_KNOWN_COUNT` → 15                                                                                  |
| `services/core/layout/full.rs`                 | Pointer cursor node setup, image-mode scene building, demo scenes                                                                              |
| `services/core/scene_state.rs`                 | New `update_pointer()`, `build_image_scene()` methods                                                                                          |
| `services/core/test_gen.rs`                    | Arrow/I-beam cursor path generators, clip demo generators                                                                                      |
| `services/drivers/cpu-render/main.rs`          | Handle external VA image reference, pass image_va to CpuBackend                                                                                |
| `services/drivers/virgil-render/main.rs`       | Handle external VA image reference                                                                                                             |
| `services/drivers/virgil-render/shaders.rs`    | Horizontal/vertical Gaussian blur fragment shaders                                                                                             |
| `services/init/main.rs`                        | Send image config with dimensions to core, send `MSG_IMAGE_CONFIG` to render service                                                           |
| `test/tests/scene.rs`                          | Update Node size assertions                                                                                                                    |

---

## Task 1: Node Struct Growth (120 → 136 bytes)

**Objective:** Add `clip_path` and `backdrop_blur_radius` fields to Node. Single coordinated commit across all size-dependent code. This is the foundation for Tasks 2–6.

**Files:**

- Modify: `system/libraries/scene/node.rs`
- Modify: `system/libraries/scene/primitives.rs`
- Modify: `system/test/tests/scene.rs` (update size assertions if any)

**Design:** Node grows by 16 bytes (one cache line on aarch64). `_pad: u8` is repurposed as `backdrop_blur_radius: u8` (no waste). `clip_path: DataRef` (8 bytes) references serialized path commands in the data buffer. `_reserved: [u8; 8]` provides headroom for future fields (z_order, accessibility_id, etc.) without another size bump.

**Byte budget (verified):** Pre-content fields sum to 96 bytes. Content is 24 bytes. Current: 96 + 24 = 120. New: 96 (existing, `_pad` → `backdrop_blur_radius` is size-neutral) + `clip_path` (8) + `_reserved` (8) + Content (24) = 136 bytes. Fields inserted between `content_hash` and `content`. The compile-time size assertion (`assert!(size_of::<Node>() == 136)`) catches any miscalculation.

- [ ] **Step 1: Add `DataRef::EMPTY` and `DataRef::is_empty()`**

In `scene/primitives.rs`, add:

```rust
impl DataRef {
    pub const EMPTY: Self = Self { offset: 0, length: 0 };

    pub const fn is_empty(&self) -> bool {
        self.length == 0
    }
}
```

- [ ] **Step 2: Modify Node struct**

In `scene/node.rs`:

1. Replace `pub _pad: u8` with `pub backdrop_blur_radius: u8`
2. Add `pub clip_path: DataRef` field after `content_hash`
3. Add `pub _reserved: [u8; 8]` for future headroom
4. Update `Node::EMPTY` — set `backdrop_blur_radius: 0`, `clip_path: DataRef::EMPTY`, `_reserved: [0; 8]`
5. Update size assertion: `assert!(core::mem::size_of::<Node>() == 136)`

The derived constants (`NODES_OFFSET`, `DATA_OFFSET`, `SCENE_SIZE`, `TRIPLE_SCENE_SIZE`) auto-update because they use `core::mem::size_of::<Node>()`.

- [ ] **Step 3: Run tests to verify nothing breaks**

Run: `cd system && cargo test --release -- --test-threads=1`

All field-based access (no offset arithmetic) means existing code compiles without changes. The only breakage is any hardcoded `120` in test assertions.

- [ ] **Step 4: Commit**

```text
feat: Node struct growth 120→136 bytes — clip_path + backdrop_blur_radius

Repurpose _pad as backdrop_blur_radius (u8). Add clip_path (DataRef, 8
bytes) for arbitrary path clipping. Add _reserved (8 bytes) for future
fields without another size bump. Node is now 136 bytes (17×8, aarch64
cache-line friendly). All derived constants auto-update via size_of.
```

---

## Task 2: Clip Mask Infrastructure (Render Library)

**Objective:** Build an 8bpp alpha mask rasterizer and LRU cache for clip paths. This is the foundation for clip path rendering (Task 3).

**Depends on:** Task 1 (clip_path field on Node).

**Files:**

- Create: `system/libraries/render/clip_mask.rs`
- Modify: `system/libraries/render/lib.rs` (add `mod clip_mask`)
- Modify: `system/libraries/render/scene_render/path_raster.rs` (extract mask rasterization)
- Create: `system/test/tests/clip_mask.rs`
- Modify: `system/test/Cargo.toml` (if needed)

**Design:** The clip mask rasterizer reuses the existing scanline path rasterizer from `path_raster.rs` but outputs to an 8bpp alpha buffer instead of compositing BGRA pixels. The LRU cache avoids re-rasterizing masks that haven't changed. Cache key: `(clip_path DataRef, node bounds, transform)`. 16 slots, ~640 KB worst-case memory.

- [ ] **Step 1: Extract `rasterize_path_to_coverage` from path_raster.rs**

The existing path rendering pipeline in `path_raster.rs` already computes per-pixel coverage before compositing. Extract this coverage computation into a standalone function:

```rust
/// Rasterize a path to an 8bpp coverage buffer (one byte per pixel).
/// Returns a Vec<u8> of `width * height` bytes, or empty if rasterization fails.
pub fn rasterize_path_to_coverage(
    path_data: &[u8],
    width: u32,
    height: u32,
    fill_rule: FillRule,
) -> Vec<u8> { ... }
```

This shares the same flattening + scanline code but writes u8 coverage instead of BGRA compositing.

- [ ] **Step 2: Write failing tests for coverage rasterizer**

In `test/tests/clip_mask.rs`:

```rust
#[test]
fn rasterize_rect_coverage_fills_interior() { ... }

#[test]
fn rasterize_circle_coverage_antialiased_edges() { ... }

#[test]
fn rasterize_empty_path_returns_empty() { ... }
```

- [ ] **Step 3: Implement `rasterize_path_to_coverage`**

Adapt the existing `render_path` flow in `path_raster.rs`:

1. Parse path commands (reuse existing parser)
2. Flatten cubics (reuse existing De Casteljau)
3. Scanline fill (reuse existing active edge logic)
4. Write coverage to u8 buffer instead of compositing to BGRA surface

- [ ] **Step 4: Run tests — verify they pass**

- [ ] **Step 5: Build the LRU clip mask cache**

In `clip_mask.rs`:

```rust
/// Cached clip mask: 8bpp alpha buffer at a specific size.
struct CachedMask {
    /// Key: hash of (clip_path DataRef + bounds + transform).
    key: u64,
    /// Alpha mask data (width × height bytes).
    data: Vec<u8>,
    width: u32,
    height: u32,
    /// LRU counter (higher = more recently used).
    last_used: u32,
}

/// LRU cache of rasterized clip masks.
pub struct ClipMaskCache {
    masks: [Option<CachedMask>; 16],
    generation: u32,
}

impl ClipMaskCache {
    pub const fn new() -> Self { ... }

    /// Get or rasterize a clip mask for the given path data and bounds.
    pub fn get_or_rasterize(
        &mut self,
        clip_path_data: &[u8],
        width: u32,
        height: u32,
        fill_rule: FillRule,
        cache_key: u64,
    ) -> Option<&[u8]> { ... }
}
```

- [ ] **Step 6: Write cache tests**

```rust
#[test]
fn cache_hit_returns_same_mask() { ... }

#[test]
fn cache_miss_rasterizes_new_mask() { ... }

#[test]
fn cache_evicts_lru_when_full() { ... }
```

- [ ] **Step 7: Run all tests**

- [ ] **Step 8: Commit**

```text
feat: clip mask infrastructure — 8bpp alpha rasterizer + LRU cache

Extracts coverage rasterization from path_raster.rs into standalone
rasterize_path_to_coverage(). Adds ClipMaskCache (16-slot LRU) for
cached clip masks. Foundation for arbitrary path clipping (next commit).
```

---

## Task 3: Clip Path Integration (CpuBackend)

**Objective:** Wire clip mask into the CpuBackend tree walk so that nodes with `clip_path` clip their children to the path shape.

**Depends on:** Task 2 (ClipMaskCache).

**Files:**

- Modify: `system/libraries/render/lib.rs` (add ClipMaskCache to CpuBackend)
- Modify: `system/libraries/render/scene_render/walk.rs` (apply clip mask during rendering)

**Design:** In `render_node_content_translated`, after rendering background but before rendering children: if `node.clip_path` is non-empty, get/rasterize the clip mask, then pass it down through child rendering. During child pixel writes, multiply destination alpha by the mask value. This is done by rendering children to an offscreen buffer, then compositing with the mask.

- [ ] **Step 1: Add ClipMaskCache to CpuBackend**

In `lib.rs`, add a `clip_cache: ClipMaskCache` field to `CpuBackend`. Initialize in `CpuBackend::new()`.

- [ ] **Step 2: Thread clip_cache through the render calls**

The tree walk functions need access to `&mut ClipMaskCache`. Add it to the mutable state passed through `render_node_transformed` → `render_node_content_translated` (alongside `pool`, `lru`, `cache`).

- [ ] **Step 3: Implement clip mask application in tree walk**

In `render_node_content_translated` (walk.rs), after the node's own background is drawn but before recursing into children:

```rust
if !node.clip_path.is_empty() {
    // 1. Rasterize/cache the clip mask at node's physical bounds
    // 2. Render children into an offscreen buffer (same as group opacity path)
    // 3. Apply clip mask: for each pixel, multiply alpha by mask[x,y]
    // 4. Composite masked buffer onto framebuffer
}
```

The clip mask application follows the same offscreen-buffer pattern already used for `opacity < 255` rendering. The key difference: instead of a uniform opacity multiplier, use per-pixel mask values.

**Implementation detail:** When both `clip_path` and `opacity < 255` are active, compose them: render to offscreen, apply clip mask, then composite with opacity. This avoids a double-offscreen-buffer penalty.

- [ ] **Step 4: Write tests for clip integration**

```rust
#[test]
fn clip_path_clips_child_content() { ... }

#[test]
fn nested_clips_intersect_correctly() { ... }

#[test]
fn clip_plus_corner_radius_both_active() { ... }

#[test]
fn clip_plus_transforms_correct() { ... }

#[test]
fn empty_clip_path_is_noop() { ... }
```

Per spec: "Nested clips. Clip + rounded corners (both active simultaneously). Clip + transforms."

- [ ] **Step 5: Visual verification**

Build the kernel, launch QEMU, take a screenshot showing clip_path in action (requires Task 9 demo nodes, but can test with a hardcoded clip path on an existing node for verification).

- [ ] **Step 6: Run all tests**

`cd system && cargo test --release -- --test-threads=1`

- [ ] **Step 7: Commit**

```text
feat: arbitrary path clipping — CpuBackend clip mask integration

Nodes with clip_path set clip their children to the path shape.
Clip mask is an 8bpp alpha buffer rasterized once and cached (16-slot
LRU). Applied via offscreen render + per-pixel mask multiplication.
Supports anti-aliased clip edges and nested clips (mask intersection).
```

---

## Task 4: Clip Path (virgil-render — Stencil Buffer)

**Objective:** Implement GPU-accelerated path clipping via the stencil buffer.

**Depends on:** Task 1 (clip_path field). Independent of Tasks 2–3.

**Files:**

- Modify: `system/services/drivers/virgil-render/main.rs` or pipeline module
- Modify: scene walk/batch rendering code in virgil-render

**Design:** Use the GPU stencil buffer for clip paths:

1. When encountering a node with `clip_path`, render the clip path geometry to the stencil buffer (increment stencil for filled pixels)
2. Enable stencil test for child rendering (draw only where stencil > 0)
3. After children are done, decrement stencil (pop) to restore parent clip
4. Nested clips: increment/decrement stencil depth — children are clipped to the intersection of all ancestor clips

**Implementation sketch:**

- Add stencil buffer allocation (same dimensions as framebuffer, 8-bit)
- Add `GL_STENCIL_TEST` enable/disable to the command stream
- Render clip path geometry using the existing path-to-triangle pipeline (stencil-then-cover)
- Track stencil depth counter for nested clips

- [ ] **Step 1: Allocate stencil buffer in virgil-render init**
- [ ] **Step 2: Implement stencil push (render clip path to stencil)**
- [ ] **Step 3: Enable stencil test during child rendering**
- [ ] **Step 4: Implement stencil pop (restore previous clip)**
- [ ] **Step 5: Test with nested clips**
- [ ] **Step 6: Visual verification via QEMU screenshot**
- [ ] **Step 7: Commit**

```text
feat: path clipping via GPU stencil buffer — virgil-render

Nodes with clip_path use stencil-then-cover: render clip path to stencil
buffer, enable stencil test for children, decrement after. Supports
nested clips via stencil depth tracking. Anti-aliased clip edges via
multisampled stencil.
```

---

## Task 5: Backdrop Blur (CpuBackend)

**Objective:** Implement the frosted-glass effect: blur content behind a translucent node.

**Depends on:** Task 1 (backdrop_blur_radius field).

**Files:**

- Modify: `system/libraries/render/scene_render/walk.rs`
- Create: `system/test/tests/backdrop_blur.rs` (or add to existing render tests)

**Design:** When a node has `backdrop_blur_radius > 0`:

1. Render the scene tree up to (but not including) the backdrop-blur node
2. Extract the rectangular region behind the node from the framebuffer
3. Apply the existing `drawing::blur_surface()` (NEON-accelerated Gaussian)
4. Composite the blurred region back at the node's position
5. Then render the node (typically translucent) on top

**Cache strategy:** Cache the blurred backdrop keyed on `(node_bounds, backdrop_blur_radius, underlying_content_hash)`. Skip re-blur when only the overlay content changed. Check dirty bitmap — if no dirty nodes are _behind_ the blur surface, use cached result.

**Rendering order:** This requires a two-pass approach for the subtree containing the blur node:

- First pass: render all siblings before the blur node (produces the "backdrop" pixels)
- Extract + blur the region under the blur node
- Second pass: render the blur node and its children on top

The tree walk already processes children in sibling order, so the "render up to this node" property is naturally satisfied — siblings rendered before this one are already in the framebuffer.

- [ ] **Step 1: Detect backdrop_blur_radius > 0 in tree walk**

In `render_node_content_translated` (walk.rs), before rendering the node's background:

```rust
if node.backdrop_blur_radius > 0 {
    let blur_radius_px = (node.backdrop_blur_radius as f32 * ctx.scale) as u32;
    let clamped = blur_radius_px.min(MAX_BACKDROP_BLUR_PX);
    // ... extract region, blur, composite
}
```

`MAX_BACKDROP_BLUR_PX = 32` for CpuBackend (single-digit milliseconds with NEON on typical region sizes).

- [ ] **Step 2: Extract backdrop region from framebuffer**

Read the rectangular region at the node's position from the current framebuffer state. This naturally contains everything rendered before this node (siblings processed earlier in sibling order).

```rust
let region = fb.extract_region(nx, ny, nw, nh);
```

If `Surface` doesn't have `extract_region`, implement it: copy a rectangular subregion into a new buffer.

- [ ] **Step 3: Apply Gaussian blur**

Reuse existing `drawing::blur_surface()`:

```rust
let sigma_fp = ((clamped * 256) / 2).max(128);
drawing::blur_surface(&region_read, &mut blurred, &mut tmp, clamped, sigma_fp);
```

- [ ] **Step 4: Composite blurred region back**

```rust
fb.blit_blend(&blurred_buf, nw as u32, nh as u32, nw as u32 * 4, nx as u32, ny as u32);
```

Then continue with normal node rendering (background, content, children) on top.

- [ ] **Step 5: Add backdrop blur cache**

Store the last blurred result per-node. On the next frame, check:

- Node bounds unchanged
- Backdrop blur radius unchanged
- No dirty nodes behind this node (check dirty bitmap for all nodes with lower z-order within the same parent)

If all conditions hold, skip re-blur and blit the cached result.

- [ ] **Step 6: Write tests**

```rust
#[test]
fn backdrop_blur_zero_radius_is_noop() { ... }

#[test]
fn backdrop_blur_applies_gaussian() { ... }

#[test]
fn backdrop_blur_clamped_to_max() { ... }
```

- [ ] **Step 7: Run all tests**

- [ ] **Step 8: Visual verification**

Add a temporary backdrop-blur node to the scene, take QEMU screenshots showing blurred content behind a translucent panel.

- [ ] **Step 9: Commit**

```text
feat: backdrop blur (frosted glass) — CpuBackend

Nodes with backdrop_blur_radius > 0 blur the framebuffer content behind
them using the existing NEON-accelerated Gaussian blur. Max 32px radius.
Cached per-node to skip re-blur when underlying content is unchanged.
Two-pass: extract backdrop, blur, composite, then render node on top.
```

---

## Task 6: Backdrop Blur (virgil-render — Blur Shaders)

**Objective:** GPU-accelerated backdrop blur via render-to-texture and separable Gaussian blur shaders.

**Depends on:** Task 1. Independent of Task 5.

**Files:**

- Create: `system/services/drivers/virgil-render/blur.rs`
- Modify: `system/services/drivers/virgil-render/shaders.rs` (new shader programs)
- Modify: `system/services/drivers/virgil-render/main.rs` (pipeline integration)

**Design:**

1. When encountering a backdrop-blur node, render the scene so far to an intermediate texture (render-to-texture)
2. Horizontal blur pass: bind intermediate texture as input, draw fullscreen quad through horizontal Gaussian fragment shader
3. Vertical blur pass: bind horizontal result as input, draw fullscreen quad through vertical Gaussian fragment shader
4. Composite blur result + node content onto final framebuffer

**New shaders (~200-300 lines):**

```tgsi
// BLUR_H_FS — Horizontal Gaussian blur fragment shader
// Samples N texels horizontally, weighted by Gaussian kernel
// Kernel size passed via uniform (constant buffer)

// BLUR_V_FS — Vertical Gaussian blur fragment shader
// Same as horizontal but samples vertically
```

- [ ] **Step 1: Define blur shaders in shaders.rs**
- [ ] **Step 2: Create intermediate render target (FBO/texture)**
- [ ] **Step 3: Implement two-pass blur pipeline in blur.rs**
- [ ] **Step 4: Wire into scene walk — detect backdrop_blur_radius > 0**
- [ ] **Step 5: Visual verification via QEMU screenshot**
- [ ] **Step 6: Commit**

```text
feat: backdrop blur via GPU — virgil-render

Two-pass separable Gaussian blur using TGSI fragment shaders.
Horizontal and vertical blur passes via render-to-texture.
Complements the CpuBackend implementation for GPU-accelerated path.
```

---

## Task 7: Pointer Cursor

**Objective:** Render a visual pointer cursor on screen that tracks mouse/trackpad position, hides after inactivity, and switches shape based on context.

**Depends on:** Task 1 (Node struct, for WELL_KNOWN_COUNT change). Uses animation library from Phase 1.

**Files:**

- Modify: `system/services/core/layout/mod.rs` (N_POINTER, WELL_KNOWN_COUNT)
- Modify: `system/services/core/layout/full.rs` (pointer node setup, cursor shape paths)
- Modify: `system/services/core/main.rs` (pointer state, MSG_POINTER_ABS handling, hide timeout)
- Modify: `system/services/core/scene_state.rs` (update_pointer method)
- Modify: `system/services/core/test_gen.rs` (arrow/I-beam path generators)
- Create: `system/test/tests/pointer.rs` (or add to existing tests)

**Design:**

- `N_POINTER = 14` — well-known node, highest z-order (last child of N_ROOT, after N_CONTENT)
- `WELL_KNOWN_COUNT = 15`
- `Content::Path` with arrow cursor shape (default) or I-beam shape (over text area)
- Position updated every MSG_POINTER_ABS via lightweight `update_pointer()` path
- Hidden when no mouse events for 3 seconds (fade out 300ms via animation timeline)
- Shown immediately on any mouse event (instant opacity=255)

**Cursor shapes (path commands, stored as constants):**

- **Arrow:** 12pt × 18pt pointer (standard arrow: tip at top-left, diagonal body, horizontal tail)
- **I-beam:** 2pt × 18pt text cursor (vertical line with small horizontal serifs at top and bottom)

**CoreState additions:**

```rust
/// True when the pointer cursor should be rendered (mouse recently moved).
pointer_visible: bool,
/// Timestamp (ms) of last pointer event, for auto-hide timeout.
pointer_last_event_ms: u64,
/// Animation ID for the pointer fade-out.
pointer_fade_id: Option<animation::AnimationId>,
/// Current pointer opacity (0-255).
pointer_opacity: u8,
/// Current cursor shape (false=arrow, true=I-beam).
pointer_is_ibeam: bool,
```

- [ ] **Step 1: Add well-known node constant**

In `layout/mod.rs`:

```rust
pub const N_POINTER: u16 = 14;
pub const WELL_KNOWN_COUNT: u16 = 15;
```

- [ ] **Step 2: Define cursor shape path generators**

In `test_gen.rs`, add:

```rust
/// Generate path commands for a standard arrow pointer cursor.
/// Size: 12pt × 18pt. Tip at (0, 0).
pub fn generate_arrow_cursor() -> Vec<u8> { ... }

/// Generate path commands for an I-beam text cursor.
/// Size: ~8pt × 18pt. Center at midpoint.
pub fn generate_ibeam_cursor() -> Vec<u8> { ... }
```

Arrow shape: MoveTo(0,0), LineTo(0,14), LineTo(4,11), LineTo(7,17), LineTo(9,16), LineTo(6,10), LineTo(10,10), Close.

- [ ] **Step 3: Add pointer node to build_full_scene**

In `layout/full.rs`, allocate N_POINTER (index 14) as a child of N_ROOT at highest z-order.

**Critical:** Link N_POINTER into the sibling chain. Currently N_CONTENT is the last child of N_ROOT (its `next_sibling` is `NULL` at line 232 of full.rs). Set `N_CONTENT.next_sibling = N_POINTER` so the pointer renders last (on top of everything).

```rust
let _pointer = w.alloc_node().unwrap(); // 14

// Link pointer as last child of root (after N_CONTENT).
w.node_mut(N_CONTENT).next_sibling = N_POINTER;

// Set up pointer node.
{
    let arrow_cmds = generate_arrow_cursor();
    let arrow_ref = w.push_path_commands(&arrow_cmds);
    let n = w.node_mut(N_POINTER);
    n.x = mouse_x as i32;
    n.y = mouse_y as i32;
    n.width = 12;
    n.height = 18;
    n.next_sibling = NULL; // Last sibling.
    n.content = Content::Path {
        color: Color::rgb(255, 255, 255),
        fill_rule: FillRule::Winding,
        contours: arrow_ref,
    };
    n.opacity = pointer_opacity;
    n.flags = NodeFlags::VISIBLE;
}
```

Link as last sibling of N_ROOT's children (after N_CONTENT).

- [ ] **Step 4: Add pointer state to CoreState**

Add the five new fields listed above. Initialize: `pointer_visible: false`, `pointer_last_event_ms: 0`, `pointer_fade_id: None`, `pointer_opacity: 0`, `pointer_is_ibeam: false`.

- [ ] **Step 5: Update MSG_POINTER_ABS handling**

In `main.rs`, when processing `MSG_POINTER_ABS`:

```rust
MSG_POINTER_ABS => {
    let ptr: PointerAbs = unsafe { msg.payload_as() };
    let s = state();
    s.mouse_x = scale_pointer_coord(ptr.x, fb_width);
    s.mouse_y = scale_pointer_coord(ptr.y, fb_height);

    // Show pointer immediately.
    if !s.pointer_visible {
        s.pointer_visible = true;
        // Cancel any pending fade-out.
        if let Some(id) = s.pointer_fade_id {
            s.timeline.cancel(id);
            s.pointer_fade_id = None;
        }
    }
    s.pointer_opacity = 255;
    s.pointer_last_event_ms = now_ms;

    // Shape switching: I-beam over text area, arrow elsewhere.
    let in_text_area = s.mouse_y >= TITLE_BAR_H + SHADOW_DEPTH
        && !s.image_mode;
    let should_be_ibeam = in_text_area;
    if should_be_ibeam != s.pointer_is_ibeam {
        s.pointer_is_ibeam = should_be_ibeam;
        // Shape change requires data buffer update → flag for full rebuild
        // or a targeted shape-swap path.
    }

    changed = true;
}
```

- [ ] **Step 6: Implement pointer hide timeout**

In the animation tick section of the event loop, after blink processing:

```rust
// Pointer auto-hide: fade out after 3 seconds of inactivity.
const POINTER_HIDE_MS: u64 = 3000;
const POINTER_FADE_MS: u64 = 300;

if s.pointer_visible && s.pointer_fade_id.is_none() {
    let idle_ms = now_ms.saturating_sub(s.pointer_last_event_ms);
    if idle_ms >= POINTER_HIDE_MS {
        s.pointer_fade_id = s.timeline
            .start(255.0, 0.0, POINTER_FADE_MS as u32, animation::Easing::EaseOut, now_ms)
            .ok();
    }
}

// Tick pointer fade.
if let Some(id) = s.pointer_fade_id {
    if s.timeline.is_active(id) {
        let new_opacity = s.timeline.value(id) as u8;
        if new_opacity != s.pointer_opacity {
            s.pointer_opacity = new_opacity;
            changed = true;
        }
    } else {
        // Fade complete.
        s.pointer_opacity = 0;
        s.pointer_visible = false;
        s.pointer_fade_id = None;
        changed = true;
    }
}
```

- [ ] **Step 7: Add update_pointer incremental path**

In `scene_state.rs`:

```rust
/// Update only the pointer cursor position and opacity. Zero-allocation path.
pub fn update_pointer(&mut self, mouse_x: u32, mouse_y: u32, opacity: u8) {
    let mut tw = self.triple();
    {
        let mut w = tw.acquire_copy();
        let n = w.node_mut(N_POINTER);
        n.x = mouse_x as i32;
        n.y = mouse_y as i32;
        n.opacity = opacity;
        w.mark_dirty(N_POINTER);
    }
    tw.publish();
}
```

Wire this into the scene dispatch — when only the pointer moved and nothing else changed, use `update_pointer()` instead of the heavier update paths.

- [ ] **Step 8: Integrate pointer opacity into scene dispatch**

Update `apply_opacity` or add a separate `apply_pointer` method that sets pointer node opacity every frame. Ensure the pointer node is marked dirty when its position or opacity changes.

Also update `build_full_scene` signature: add `mouse_x`, `mouse_y`, `pointer_opacity` parameters (or pass via SceneConfig).

- [ ] **Step 9: Write tests**

```rust
#[test]
fn pointer_visible_on_mouse_event() { ... }

#[test]
fn pointer_hidden_after_timeout() { ... }

#[test]
fn pointer_ibeam_in_text_area() { ... }

#[test]
fn pointer_arrow_in_title_bar() { ... }
```

- [ ] **Step 10: Visual verification**

QEMU with `-device virtio-tablet-device` sends pointer events. Move the mouse, verify pointer appears and tracks. Wait 3+ seconds, verify it fades out. Move again, verify instant reappearance.

- [ ] **Step 11: Run all tests**

- [ ] **Step 12: Commit**

```text
feat: pointer cursor — arrow/I-beam with auto-hide

Adds N_POINTER well-known node (14) as Content::Path at highest z-order.
Arrow cursor default, I-beam over text area. Position updates via
lightweight update_pointer() path. Auto-hides after 3s inactivity with
300ms EaseOut fade. Instantly visible on any mouse event.
```

---

## Task 8: Document Switching Fix

**Objective:** Fix Ctrl+Tab to actually show the image viewer with a centered PNG, instead of rebuilding the same text editor scene.

**Depends on:** Phase 1 fade transitions (already implemented).

**Files:**

- Modify: `system/libraries/protocol/lib.rs` (expand ImageConfig with width/height)
- Modify: `system/services/init/main.rs` (send image dimensions to core, send MSG_IMAGE_CONFIG to render service)
- Modify: `system/services/core/main.rs` (store image metadata, pass image_mode to scene builder)
- Modify: `system/services/core/layout/full.rs` (image-mode scene building)
- Modify: `system/services/core/scene_state.rs` (build_image_scene convenience)
- Modify: `system/services/drivers/cpu-render/main.rs` (external VA image handling)
- Modify: `system/services/drivers/virgil-render/main.rs` (external VA image handling)

**Design:**

**Protocol changes:**

- `ImageConfig` gains `image_width: u16` and `image_height: u16` (the decoded PNG dimensions). Remove `_pad: u32`.
- `CompositorConfig` is **NOT** expanded — it's already 44 bytes, and adding `image_va: u64` would require 8-byte alignment padding that pushes it past the 60-byte IPC payload limit (`#[repr(C)]` inserts 4 bytes of padding before the u64, making it 64 bytes). Instead, init sends a separate `MSG_IMAGE_CONFIG` to the render service.

**External VA sentinel:** When `Content::Image { data, ... }` has `data.offset == u32::MAX`, the render service uses its pre-mapped image VA instead of reading from the scene data buffer. The `src_width` and `src_height` fields tell the renderer the image dimensions. This avoids copying multi-megabyte image data into the 64KB scene data buffer.

**Image-mode scene building:** `build_full_scene` takes `image_mode: bool`, `image_width: u16`, `image_height: u16`. When true:

- Title bar shows "Image viewer | {width}×{height}" instead of "Text"
- Content area: single centered Image node with the external VA sentinel
- No cursor node, no selection, no doc text, no demo nodes
- Chrome frame (title bar, shadow, background) stays the same

- [ ] **Step 1: Expand ImageConfig with dimensions**

In `protocol/lib.rs`:

```rust
pub struct ImageConfig {
    pub image_va: u64,
    pub image_len: u32,
    pub image_width: u16,
    pub image_height: u16,
}
```

Update size assertion. Remove old `_pad` field.

- [ ] **Step 2: Send MSG_IMAGE_CONFIG to render service**

In `init/main.rs`:

1. After decoding the PNG, store width and height
2. Include dimensions in `ImageConfig` sent to core (existing channel)
3. Send the **same** `MSG_IMAGE_CONFIG` to the render service's init channel (new — send after `MSG_COMPOSITOR_CONFIG`)

The render service stores `image_va`, `image_len`, `image_width`, `image_height` from this message during its init handshake. This avoids bloating `CompositorConfig`.

- [ ] **Step 3: Store image metadata in CoreState**

In `main.rs`, when receiving `MSG_IMAGE_CONFIG`:

```rust
if img_config.image_va != 0 && img_config.image_len > 0 {
    has_image = true;
    s.image_va = img_config.image_va;
    s.image_len = img_config.image_len;
    s.image_width = img_config.image_width;
    s.image_height = img_config.image_height;
}
```

Add corresponding fields to CoreState.

- [ ] **Step 4: Update build_full_scene for image mode**

Add `image_mode`, `image_width`, `image_height` parameters. When `image_mode`:

```rust
// Title: "Image viewer | 1024×768"
let title_text = format_image_title(image_width, image_height);

// Content area: centered image node.
{
    let n = w.node_mut(N_DOC_TEXT); // Reuse doc text container
    n.content = Content::Image {
        data: DataRef { offset: u32::MAX, length: 0 }, // External VA sentinel
        src_width: image_width,
        src_height: image_height,
    };
    // Center the image in the content area.
    let content_w = cfg.fb_width;
    let content_h = cfg.fb_height - content_y;
    n.x = ((content_w as i32 - image_width as i32) / 2).max(0);
    n.y = ((content_h as i32 - image_height as i32) / 2).max(0);
    n.width = image_width;
    n.height = image_height;
    // ... no cursor, no selection, no demo nodes
}
```

- [ ] **Step 5: Handle external VA sentinel in cpu-render**

In `cpu-render/main.rs` (or in the render library's content rendering), when encountering `Content::Image` with `data.offset == u32::MAX`:

```rust
Content::Image { data, src_width, src_height } => {
    if data.offset == u32::MAX {
        // External VA reference — use pre-mapped image data.
        let image_slice = unsafe {
            core::slice::from_raw_parts(
                config.image_va as *const u8,
                (src_width as usize * src_height as usize * 4),
            )
        };
        render_image_from_slice(fb, image_slice, src_width, src_height, ...);
    } else {
        // Normal data buffer reference.
        // ... existing code
    }
}
```

The render service stores `image_va` from the `MSG_IMAGE_CONFIG` received during init (see Step 2).

- [ ] **Step 6: Handle external VA sentinel in virgil-render**

Same pattern as Step 5 but using GPU texture upload from the external VA.

- [ ] **Step 7: Update context switch in main.rs**

When `context_switched` fires and `image_mode` is now true, call `build_full_scene` with `image_mode=true`:

```rust
if context_switched {
    let s = state();
    scene.build_editor_scene(
        &scene_cfg,
        doc_content(),
        s.cursor_pos as u32,
        s.sel_start as u32,
        s.sel_end as u32,
        if s.image_mode { &title_image } else { b"Text" },
        &time_buf,
        s.scroll_offset,
        s.cursor_opacity,
        s.image_mode,     // NEW
        s.image_width,    // NEW
        s.image_height,   // NEW
    );
}
```

- [ ] **Step 8: Write tests**

```rust
#[test]
fn image_mode_scene_has_image_node() { ... }

#[test]
fn image_mode_no_cursor_or_selection() { ... }

#[test]
fn external_va_sentinel_detected() { ... }
```

- [ ] **Step 9: Visual verification**

QEMU: type some text, press Ctrl+Tab, verify image appears centered with fade transition. Press Ctrl+Tab again, verify text editor returns with scroll position preserved. Take screenshots of both states.

- [ ] **Step 10: Run all tests**

- [ ] **Step 11: Commit**

```text
feat: document switching fix — image viewer via Ctrl+Tab

build_full_scene now handles image_mode: centered PNG with external VA
sentinel (data.offset == u32::MAX), "Image viewer | WxH" title bar.
ImageConfig expanded with width/height. MSG_IMAGE_CONFIG sent separately
to render service for external VA lookup. Fade transition from Phase 1
preserved. No cursor/selection/demo in image mode.
```

---

## Task 9: Phase 2 Demo Scenes

**Objective:** Add demonstration content showcasing new capabilities: star-shaped clip, circular clip, frosted glass panel, pointer cursor.

**Depends on:** Tasks 3, 5, 7 (clip path, backdrop blur, pointer cursor).

**Files:**

- Modify: `system/services/core/layout/full.rs` (replace Phase 1 demos with Phase 2 demos)
- Modify: `system/services/core/test_gen.rs` (clip demo shape generators)

**Demo nodes (replace N_DEMO_BALL and N_DEMO_EASE_0..4):**

1. **Star clip + image (reuse N_DEMO_BALL = 8):** A container node with `clip_path` set to a 5-pointed star. Child node: the existing test image (gradient). Result: image visible only through the star shape.

2. **Circular clip + text (reuse N_DEMO_EASE_0 = 9):** A container node with `clip_path` set to a circle. Child node: text content. Result: text visible only through the circle.

3. **Frosted glass panel (reuse N_DEMO_EASE_1 = 10):** A node with `backdrop_blur_radius = 8`, translucent white background (`rgba(255, 255, 255, 180)`), positioned over the document content area. Shows blurred text behind a glass panel.

4. **Nodes 11-13:** Reserved or removed.

- [ ] **Step 1: Generate clip demo shapes**

In `test_gen.rs`:

```rust
/// Generate path commands for a circle (approximated with 4 cubic beziers).
pub fn generate_circle_clip(radius: f32) -> Vec<u8> { ... }
```

The star shape already exists (`generate_test_star`).

- [ ] **Step 2: Replace Phase 1 demos in build_full_scene**

Replace the bouncing ball and easing bar setup with:

- Star clip container (index 8) with image child
- Circle clip container (index 9) with text child
- Frosted glass panel (index 10) with backdrop_blur_radius

- [ ] **Step 3: Remove Phase 1 demo animation code from main.rs**

Remove `demo_ball_spring`, `demo_ball_at_top`, `demo_ball_y`, `demo_ease_ids`, `demo_ease_x` from CoreState. Remove the demo animation tick code and `apply_demo` calls. The Phase 2 demos are static (no animation needed).

- [ ] **Step 4: Visual verification**

QEMU screenshots showing:

- Star-shaped cutout with image visible through it
- Circular text area
- Frosted glass panel blurring underlying text
- Pointer cursor visible and tracking

- [ ] **Step 5: Run all tests**

- [ ] **Step 6: Commit**

```text
feat: Phase 2 demo scenes — star clip, circle clip, frosted glass

Replaces Phase 1 bouncing ball and easing bars with composition demos:
star-shaped clip path containing an image, circular clip path over text,
frosted glass panel (backdrop_blur_radius=8) over document content.
Phase 1 demo animation code removed.
```

---

## Build Order Summary

```text
Task 1: Node struct growth (foundation)
  ├── Task 2: Clip mask infrastructure (render library)
  │     └── Task 3: Clip path CpuBackend integration
  ├── Task 4: Clip path virgil-render (parallel with 2-3)
  ├── Task 5: Backdrop blur CpuBackend
  ├── Task 6: Backdrop blur virgil-render (parallel with 5)
  ├── Task 7: Pointer cursor (independent after Task 1)
  └── Task 8: Document switching fix (independent)
Task 9: Demo scenes (after 3, 5, 7)
```

Tasks 2-3 and 4 can be parallelized (CpuBackend vs virgil-render).
Tasks 5 and 6 can be parallelized (CpuBackend vs virgil-render).
Task 7 and Task 8 are independent of each other and of clip/blur.

**Recommended serial order:** 1 → 2 → 3 → 5 → 7 → 8 → 9 → 4 → 6

This prioritizes CpuBackend (the primary visual output) and defers virgil-render to later, since the CPU path is easier to verify visually.

---

## Acceptance Criteria

- [ ] All existing tests pass (~2,004+)
- [ ] Node struct is exactly 136 bytes (compile-time assertion)
- [ ] Clip path: content correctly clipped to non-rectangular shapes (circle, star) — screenshot verified
- [ ] Backdrop blur: visible frosted glass effect behind translucent panel — screenshot verified
- [ ] Pointer cursor: visible, tracks position, I-beam over text, auto-hides — screenshot verified
- [ ] Document switching: Ctrl+Tab shows centered image, Ctrl+Tab returns to editor — screenshot verified
- [ ] Demo scenes: star clip, circle clip, frosted glass all rendering correctly
- [ ] Both cpu-render and virgil-render handle the external VA image sentinel
