# Architecture

## Current Mission: Rendering Pipeline Triple Buffering

Replacing double-buffered scene graph with triple buffering (mailbox semantics) and adding GPU completion flow control to fix 6 transport bugs.

### Content Type Field Layout
- **FillRect:** Color (4B). Position/size from Node geometry.
- **Glyphs:** color(4B) + DataRef(8B) + glyph_count(u16, 2B) + font_size(u16, 2B) + axis_hash(u32, 4B) = 20B. Single run per node.
- **Image:** DataRef(8B) + src_width(u16) + src_height(u16) = 12B. Unchanged.

### Node Child Ordering (N_DOC_TEXT)
Per-line Glyphs nodes first, then N_CURSOR (FillRect), then selection rects (FillRect). set_node_count truncates both line nodes and selection rects.

Architectural decisions, patterns, and constraints for the rendering pipeline.

---

## Rendering Pipeline

```
Core (OS service) → Scene Graph (shared memory) → Compositor (pixel pump) → GPU Driver → Display
```

- **Core:** Owns document state, text layout, scene graph construction. Builds scene in logical coordinates.
- **Scene Graph:** Double-buffered shared memory. Flat array of fixed-size repr(C) Nodes + 64KB data buffer. Change list (24 entries) for incremental updates.
- **Compositor:** Reads scene graph, calls render backend to produce framebuffer. Content-agnostic thin event loop — no font knowledge, no content dispatch, no SVG. Owns the render backend instance.
- **GPU Driver:** Transfers dirty rects from guest framebuffer to host display via virtio-gpu MMIO.

## Key Types

- `scene::Node` — 72 bytes (verified with compile-time assertion in scene/lib.rs). Fields: tree links, geometry (i16/u16 logical), scroll_y (i32), background (Color), border (Border), corner_radius (u8), opacity (u8), shadow fields (shadow_color, shadow_offset_x/y, shadow_blur_radius, shadow_spread), flags, content_hash, content variant.
- `scene::Content` — None | FillRect{color} | Image{data, src_w, src_h} | Glyphs{color, glyphs DataRef, glyph_count, font_size, axis_hash}
- `drawing::Surface` — borrowed pixel buffer with BGRA8888 format
- `drawing::Color` — RGBA u8×4 with sRGB gamma-correct blend_over

## Triple-Buffer Protocol (replacing Double-Buffer)

The old double-buffer protocol dropped frames when the writer was faster than the reader (copy_front_to_back returned false). The new triple-buffer protocol uses mailbox semantics:

1. Core calls `acquire()` — always returns a free buffer (the third buffer that neither the reader nor the writer's last published buffer). Never fails.
2. Core mutates nodes in the acquired buffer, calls `mark_changed(node_id)` for each
3. Core calls `publish()` — atomically makes this buffer the "latest". Intermediate unpublished frames from the old "latest" slot become the new free buffer.
4. Compositor reads the latest published buffer via `TripleReader` (acquire fence)
5. Compositor calls `finish_read(gen)` after reading — releases the buffer back to the free pool
6. If the writer publishes multiple times before the reader reads, only the latest is seen (mailbox semantics — correct for interactive UI)

**Three buffers:** One held by reader, one "latest" (published), one free for writer. Roles rotate atomically.

**Synchronization:** Same release-acquire fence pairs as before. No mutexes — atomics only (bare-metal constraint).

**Memory:** TRIPLE_SCENE_SIZE = 3 * SCENE_SIZE + control region (~48 KiB more than double-buffer).

## Damage Tracking — PREV_BOUNDS

The `CpuBackend` (in `libraries/render/`) maintains per-node previous-frame physical bounds (`prev_bounds` field, stored as `(i32, i32, u16, u16)`) to damage old positions when nodes move:

- **On render:** After rendering a node, store its physical (x, y, w, h) in prev_bounds[node_id]
- **On partial update:** Damage BOTH the old position (from prev_bounds) AND the new position
- **Type constraint:** Physical coordinates = logical × scale_factor. At scale≥2, logical i16 values produce physical coordinates exceeding i16::MAX (32767). prev_bounds uses i32 for x/y to avoid truncation. The new-position damage path clamps i32→u16 with `.min(fbw)` guards.
- **Ownership:** Formerly a compositor static mut; now encapsulated inside `render::CpuBackend` as part of the Phase 1 render backend extraction.

## Frame Scheduler

The compositor uses a frame scheduler (`frame_scheduler.rs`) that replaces the old event-driven render-on-every-update pattern with configurable-cadence rendering:

- **Timer-driven:** A one-shot kernel timer fires at the configured cadence (default 60fps = 16.67ms). The compositor recreates the timer after each tick (same pattern as core's clock timer).
- **Two-handle wait:** The compositor's main loop waits on BOTH `CORE_HANDLE` (scene updates) and the frame timer handle. On core signal → set dirty flag (+ check idle-to-active wakeup). On timer tick → render if dirty, skip if clean.
- **Event coalescing:** Multiple scene updates between timer ticks produce a single render reading the latest scene state.
- **Idle optimization:** When nothing changes, the frame timer fires but the compositor skips rendering entirely (no wasted GPU transfers).
- **Frame budgeting:** If rendering takes >2× frame period, the scheduler skips overdue timer ticks (where `last_render_end_ns > tick_time`) — no back-to-back catch-up renders. Uses `sys::counter()` timestamps converted to nanoseconds.
- **Idle-to-active wakeup:** When a scene update arrives after idle (last timer tick was >half-period ago), the compositor renders immediately instead of waiting for the next tick. Reduces perceived input latency after idle periods.
- **Configurable cadence:** `CompositorConfig.frame_rate` (u16, default 60) controls the frame timer period. Supports 30fps, 60fps, 120fps, and arbitrary values. `FrameScheduler::set_cadence()` allows runtime changes.
- **Pure state machine:** `FrameScheduler` struct tracks dirty flag, timestamps (last_tick_ns, last_render_end_ns), and tick/render/present/overrun counters. Testable on the host without kernel syscalls via `on_timer_tick_at(now)` and `on_render_complete_at(now)`.

The initial frame is rendered outside the scheduler loop (before it starts) to ensure immediate display on boot.

## Scale Factor Flow

Scale factor is **f32** (fractional) throughout the pipeline, supporting 1.0, 1.25, 1.5, 1.75, 2.0 etc.

1. Init computes scale as f32 from framebuffer resolution (≥2048px → 2.0, else 1.0)
2. Sent to compositor via CompositorConfig.scale_factor (f32)
3. Compositor validates: 0.0/negative/NaN → 1.0, >4.0 → clamped to 4.0
4. Compositor stores in RenderCtx.scale (f32)
5. render_node uses gap-free rounding: position = round(logical × scale), size = round((pos+size) × scale) - round(pos × scale)
6. Borders snap to whole physical pixels (minimum 1px)
7. Font sizes: physical_px = round(logical_size × scale_factor)
8. fb_stride is NOT in CompositorConfig — derived as fb_width × 4 (BGRA8888)
9. Manual `round_f32()` used instead of `f32::round()` for no_std compatibility

## Drawing Library Constraints

- **Pixel format:** The drawing library exclusively assumes **Bgra8888** (Blue, Green, Red, Alpha byte order). All blending functions (`fill_rect_blend_scalar_1px`, `neon_blend_const_4px`, `rounded_rect_write_aa_pixel`, `blend_pixel`) hard-code this byte order. If a second pixel format is ever needed, all blending functions must be audited.


## SurfacePool (Offscreen Buffer Management)

`libraries/render/surface_pool.rs` provides pool-based allocation of temporary `Surface` objects for offscreen rendering (group opacity, future blur/shadow).

**Lifecycle contract:**
1. `pool.acquire(width, height)` → returns `PoolHandle` + clears buffer to transparent
2. Use the buffer for rendering (access via `pool.surface(handle)` / `pool.surface_mut(handle)`)
3. `pool.release(handle)` — marks buffer as reusable
4. `pool.end_frame()` — frees unused buffers, reclaims memory. **All handles must be released before calling end_frame.** Uses `swap_remove` internally — handle indices are invalidated after `end_frame`.

**Constraints:**
- Budget: 32 MiB total. `acquire()` returns `None` if budget exceeded.
- Size-matched: buffers reused only for exact width×height match.
- MAX_ENTRIES: 32 pooled buffers maximum.
- Dimensions = node logical size × scale factor (physical pixels).

**Current status (compositing-model milestone):** SurfacePool exists and is tested (16 tests), but the opacity rendering path allocates via `vec![0u8; ...]` instead of pool. This is a deliberate simplification — the borrow checker prevents passing `&mut pool` while a buffer acquired from it is in use. Integration is a future follow-up.

## Gaussian Blur

`drawing::blur_surface()` implements separable Gaussian blur (two-pass: horizontal then vertical).

- **Trait interface:** `blur_surface` is the standard entry point. A scalar fallback `blur_surface_scalar` exists for testing NEON paths.
- **NEON acceleration:** The vertical pass uses actual NEON SIMD intrinsics (`vmlaq_u32`, `vld1q_u8`, etc.). The horizontal pass uses scalar `[u64; 4]` arrays despite the `_neon` suffix.
- **Radius cap:** CPU blur radius capped at 16px. Larger radii silently clamped.
- **Kernel:** Padé[0/3] approximation of `exp(-t)` for weight computation. Weights are normalized, so the effective kernel is slightly wider than true Gaussian. Acceptable for visual blur.
- **Scratch buffer:** Callers must provide a tmp buffer ≥ dst_stride × height. Undersized buffers cause silent no-op.

## Shadow Rendering

`libraries/render/scene_render.rs::render_shadow()` renders box shadows:

1. **Hard shadow** (blur_radius=0): Direct `fill_rect_blend` or `fill_rounded_rect_blend` at shadow offset
2. **Blurred shadow:** Allocate offscreen buffer (node size + spread + blur padding on each side), fill shape in shadow color, apply `blur_surface`, blit result at shadow offset
3. **4 MiB cap** per shadow buffer prevents OOM — falls back to hard shadow if exceeded
4. **Damage tracking:** `abs_bounds()` in scene/lib.rs expands logical bounds by shadow overflow (offset + blur_radius + spread). `shadow_overflow()` in scene_render.rs computes physical overflow at current scale.
5. **Opacity interaction:** Group opacity applies to shadow output — if parent has opacity < 255, shadow is rendered into the offscreen opacity buffer along with content, then both composited at group opacity.
6. **Shadow fields on Node:** shadow_color (Color), shadow_offset_x/y (i16), shadow_blur_radius (u8), shadow_spread (i8). Default = transparent color, all zeros = no shadow.
