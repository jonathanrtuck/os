# Architecture

Architectural decisions, patterns, and constraints for the rendering pipeline.

---

## Rendering Pipeline

```
Core (OS service) → Scene Graph (shared memory) → Compositor (pixel pump) → GPU Driver → Display
```

- **Core:** Owns document state, text layout, scene graph construction. Builds scene in logical coordinates.
- **Scene Graph:** Double-buffered shared memory. Flat array of fixed-size repr(C) Nodes + 64KB data buffer. Change list (24 entries) for incremental updates.
- **Compositor:** Reads scene graph, renders to framebuffer. Applies scale factor (logical→physical). Owns glyph cache and SVG rasterizer. Damage-tracked partial rendering.
- **GPU Driver:** Transfers dirty rects from guest framebuffer to host display via virtio-gpu MMIO.

## Key Types

- `scene::Node` — 60 bytes (verified with compile-time assertion in scene/lib.rs). Fields: tree links, geometry (i16/u16 logical), scroll_y (i32), background (Color), border (Border), corner_radius (u8), opacity (u8), flags, content_hash, content variant.
- `scene::Content` — None | Text{runs, run_count} | Image{data, src_w, src_h} | Path{commands, fill, stroke, stroke_width}
- `drawing::Surface` — borrowed pixel buffer with BGRA8888 format
- `drawing::Color` — RGBA u8×4 with sRGB gamma-correct blend_over

## Double-Buffer Protocol

1. Core calls `copy_front_to_back()` — copies current front to back, resets change list. **Checks `reader_done_gen`**: if compositor hasn't finished reading the current front buffer, returns `false` and the update is skipped (retried next event).
2. Core mutates specific nodes in back buffer, calls `mark_changed(node_id)` for each
3. Core calls `swap()` — bumps generation counter, back becomes new front
4. Compositor reads front buffer via `DoubleReader` (acquire fence on generation)
5. Compositor calls `finish_read(gen)` after reading all nodes/data — writes `reader_done_gen` with release fence, signaling the writer it's safe to overwrite this buffer
6. Generation counter determines which buffer is front (higher gen = front)

**Synchronization:** Release-acquire fence pairs on generation counter (write side) and reader_done_gen (read side). No mutexes — atomics only (bare-metal constraint). The u32 generation wraps after ~2.2 years at 60fps.

## Damage Tracking — PREV_BOUNDS

The compositor maintains per-node previous-frame physical bounds (`PREV_BOUNDS` array, stored as `(i32, i32, u16, u16)`) to damage old positions when nodes move:

- **On render:** After rendering a node, store its physical (x, y, w, h) in PREV_BOUNDS[node_id]
- **On partial update:** Damage BOTH the old position (from PREV_BOUNDS) AND the new position
- **Type constraint:** Physical coordinates = logical × scale_factor. At scale≥2, logical i16 values produce physical coordinates exceeding i16::MAX (32767). PREV_BOUNDS uses i32 for x/y to avoid truncation. The new-position damage path clamps i32→u16 with `.min(fbw)` guards.

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
- **Stroke width convention:** In `build_stroke_outline()` / `stroke_subpath()`, the parameter is a **half-width** (offset from center to each edge). Callers must pass `stroke_width / 2`, not the full stroke width. Note: as of the visual-primitives milestone, `render_path()` incorrectly passes the full width — a known non-blocking bug producing strokes at 2× specified width.
