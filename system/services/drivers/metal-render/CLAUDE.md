# metal-render

Metal GPU driver — reads the scene graph from shared memory and renders using serialized Metal commands sent over a custom virtio device (device ID 22).

## Architecture

```text
Core (layout + scene build) → Scene Graph (shared memory) → metal-render (tree walk + Metal commands) → virtio → Host VirtioMetal → Metal API → Display
```

The scene graph is the interface. All rendering complexity (tree walk, glyph atlas, command buffer encoding) is internal to this driver — a leaf node behind a simple boundary.

## Key Design

- **Inline vertex data** via `set_vertex_bytes` (Metal's 4KB limit per call, ~21 quads per batch)
- **Two virtqueues:** queue 0 (setup: shaders, pipelines, textures), queue 1 (per-frame rendering)
- **DRAWABLE_HANDLE (0xFFFFFFFF):** special handle that acquires the host CAMetalLayer drawable
- **4x MSAA:** render to MSAA texture, resolve to drawable on present
- **sRGB render target:** TEX_MSAA and CAMetalLayer use `bgra8Unorm_srgb`. Hardware blender operates in linear space for gamma-correct compositing. All fragment shaders linearize sRGB color inputs via `srgb_to_linear()`.
- **Analytical Gaussian shadows:** `fragment_shadow` evaluates the exact Gaussian integral per-pixel — separable erf for rectangles, SDF+erfc for rounded rects. No offscreen textures or compute passes.
- **MSL shaders:** embedded as source text, compiled at startup via CMD_COMPILE_LIBRARY. Includes solid, textured, glyph, rounded-rect (SDF), shadow (analytical Gaussian), stencil, and separable box-blur (H+V compute) shaders.

## Dependencies

- `sys` — syscall wrappers
- `ipc` — ring buffer IPC
- `protocol` — message types + `metal::CommandBuffer` builder
- `virtio` — virtio MMIO transport + virtqueue
- `scene` — scene graph types + TripleReader
- `drawing` — color types
- `fonts` — glyph rasterization for atlas
- `render` — frame_period_ns for cadence

## Testing

- Protocol encoding: `system/test/` (host-side, via protocol crate tests)
- Visual: run via hypervisor (separate repo: `~/Sites/hypervisor/`)
