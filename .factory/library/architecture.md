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

- `scene::Node` — 72 bytes (verify with compile-time assertion). Fields: tree links, geometry (i16/u16 logical), scroll_y (i32), background (Color), border (Border), corner_radius (u8), opacity (u8), flags, content_hash, content variant.
- `scene::Content` — None | Text{runs, run_count} | Image{data, src_w, src_h} | Path{commands, fill, stroke, stroke_width}
- `drawing::Surface` — borrowed pixel buffer with BGRA8888 format
- `drawing::Color` — RGBA u8×4 with sRGB gamma-correct blend_over

## Double-Buffer Protocol

1. Core calls `copy_front_to_back()` — copies current front to back, resets change list
2. Core mutates specific nodes in back buffer, calls `mark_changed(node_id)` for each
3. Core calls `swap()` — bumps generation counter, back becomes new front
4. Compositor reads front buffer via `DoubleReader` (acquire fence on generation)
5. Generation counter determines which buffer is front (higher gen = front)

## Scale Factor Flow

1. Init computes scale from framebuffer resolution
2. Sent to compositor via CompositorConfig IPC message
3. Compositor stores in RenderCtx.scale
4. render_node multiplies logical coords by scale for all positions/sizes
5. Font sizes: physical_px = logical_size × scale_factor
