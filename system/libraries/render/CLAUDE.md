# render

Shared rendering infrastructure: scene graph tree walk, content rendering, compositing, and offscreen buffer management. Pure rendering library with no dependency on `sys` or `ipc`. `no_std` with `alloc`.

## Key Files

- `lib.rs` -- `LruRasterizer` (on-demand glyph rasterization with LRU cache), coordinate scaling helpers
- `scene_render/mod.rs` -- `SceneGraph` struct (nodes + data + content region), `RenderCtx` (glyph caches + scale factor)
- `scene_render/walk.rs` -- Recursive tree walk: `render_scene`, `render_scene_clipped`, `render_scene_with_pool` variants. Handles backgrounds, borders, rounded corners, clipping, opacity
- `scene_render/content.rs` -- Content rendering: `Glyphs` (shaped text), `Image`/`InlineImage` (pixel blits), `Path` (vector fill/stroke)
- `scene_render/coords.rs` -- `round_f32`, `scale_coord`, `scale_size` for point-to-pixel conversion
- `scene_render/path_raster.rs` -- Rasterize path commands to coverage maps for vector content and clip masks. Used by presenter (loading screen).
- `frame_scheduler.rs` -- Configurable-cadence frame scheduler: event coalescing, idle optimization, frame budgeting. Used by metal-render.
- `cache.rs` -- Per-node render cache (bitmap keyed by node_id + content_hash, O(1) lookup, invalidated on hash change)
- `clip_mask.rs` -- `ClipMaskCache`: rasterized 8bpp alpha masks for path clipping (16 slots, LRU eviction)
- `damage.rs` -- `DamageTracker`: dirty rectangle collection with full-screen fallback (max 32 rects)
- `incremental.rs` -- `IncrementalState`: per-node tracking across frames for dirty rect computation from scene graph diffs
- `surface_pool.rs` -- `SurfacePool`: reusable offscreen buffer pool with 32 MiB memory budget

## Active consumers (post cpu-render/virgil-render removal)

- `metal-render` -- `frame_scheduler::frame_period_ns`
- `presenter` -- `scene_render::path_raster::render_path_data` (loading screen)
- `test/` -- `render_scene_render.rs` exercises scene walk, clip skip, damage tracking

## Dependencies

- `drawing` -- Surface and color primitives, blending, blur
- `scene` -- Node types, triple-buffer reader, data buffer access
- `protocol` -- DirtyRect, Content Region layout
- `fonts` -- Glyph cache types, rasterizer for on-demand glyphs

## Conventions

- Points are the coordinate system above the render boundary; pixels are physical framebuffer coordinates
- Scale factor converts points to pixels at render time (1.0 standard, 2.0 Retina)
- Only cache nodes with content (Glyphs, Image, Path) -- pure containers use `fill_rect` directly
- Frame scheduler is a pure state machine; compositor drives it with callbacks
