# Architecture

## Rendering Pipeline

One-way data flow, five layers:

```
Core (OS Service) -> Scene Graph (shared memory) -> Compositor -> GPU Driver -> Display
```

- **Core:** Content understanding, text layout, input routing, scene graph building. Sole writer to document state.
- **Scene Graph:** Pure data in shared memory. Tree of Node values with geometry, decoration, content variants.
- **Compositor:** Content-agnostic pixel pump. Walks scene graph, rasterizes glyphs, composites layers.
- **GPU Driver:** Transfers pixel buffers to display hardware.

## Library Responsibilities (TARGET STATE)

| Library | Owns | Does NOT own |
|---------|------|-------------|
| `drawing` | Surface, Color, PixelFormat, blend_over, fill_rect, draw_coverage, blit, draw_line, gradients, gamma tables | Layout, caching, decoders, compositing, damage tracking |
| `fonts` | Shaping (HarfRust), rasterization (scanline + subpixel), font metrics, glyph cache (GlyphCache, LruGlyphCache), variable font axes, stem darkening | Typography policy, font selection policy, layout |
| `scene` | Node, Content, TextRun, ShapedGlyph, SceneWriter/Reader, DoubleWriter/Reader, diff_scenes | Layout computation, glyph data, content understanding |
| `protocol` | IPC message types, payload structs | Everything else |

## Build System

Custom `build.rs` at system/build.rs. Two compilation paths:
1. Direct `rustc --crate-type=rlib` for: sys, protocol, virtio, scene, ipc, drawing
2. `cargo build` for: fonts (because of harfrust/read-fonts dependency tree)

Compilation order matters (DAG): sys -> protocol, virtio(sys), scene, ipc, fonts(cargo), drawing(protocol+fonts) -> all programs.

## Incremental Scene Graph (rendering-pipeline-optimization mission)

**Before:** Core calls `build_editor_scene()` on every event, clearing and rebuilding the entire scene graph. Compositor byte-diffs all 512 nodes to find changes.

**After:** Core uses copy_front_to_back + selective mutation. Four update paths:
- `update_clock()`: In-place overwrite of 8-byte clock glyph data. 0 heap allocs.
- `update_cursor()`: Mutate N_CURSOR x,y fields. 0 heap allocs.
- `update_document_content()`: Re-layout visible text runs, update doc text + cursor. O(visible_lines) allocs.
- `update_selection()`: Rebuild selection rect nodes. Truncate node count to 8 first.

Each path records changed node IDs in the scene header's change list. Compositor reads the change list instead of diffing. Dirty rects derived from changed node positions.

**Data buffer management:** update_data() for same-length overwrites (clock, title). replace_data() for new-length data (doc text). Fallback to full rebuild when data_used > 75% of DATA_BUFFER_SIZE.

## QEMU/virtio Constraints

1. virtio-gpu 2D is copy-based (guest->host on every present). No zero-copy scanout.
2. No GPU-accelerated compositing. All blending is CPU.
3. Display resolution fixed at init time.
4. These constraints stay in the driver layer and never leak above.
