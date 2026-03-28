# virgil-render (DEPRECATED)

> **Deprecated.** This driver was a v0.3 research spike that proved GPU-accelerated rendering via QEMU's virglrenderer. metal-render is the primary render path. This driver is no longer maintained and will be removed in a future milestone. Do not add new features here.

Thick Virgil3D GPU driver — reads the scene graph from shared memory and renders using Gallium3D commands via virtio-gpu 3D mode.

## Architecture

```text
Core (layout + scene build) → Scene Graph (shared memory) → virgil-render (tree walk + Gallium3D + present) → Display
```

The scene graph is the interface. All rendering complexity (tree walk, GPU state management, glyph atlas, command buffer encoding) is internal to this driver — a leaf node behind a simple boundary.

## Key Files

- `main.rs` — entry point, constants, event loop, render loop orchestration
- `wire.rs` — FFI wire-format structs (`#[repr(C)]`), DmaBuf, `box_zeroed`, ctrl_header helpers
- `device.rs` — Phase A (virtio-gpu init, VIRGL feature negotiation) + Phase B (display query, init handshake)
- `resources.rs` — Phase C (virgl context, render target, VBO, texture resources), gpu_command/gpu_cmd_ok helpers
- `pipeline.rs` — Phase D (Gallium3D pipeline setup: blend, DSA, rasterizer, shaders, surface) + Phase E (initial clear)
- `shaders.rs` — TGSI shader text (color + textured vertex/fragment + glyph + stencil)
- `atlas.rs` — glyph texture atlas with row-based packing
- `scene_walk.rs` — scene graph tree walk emitting GPU draw calls (backgrounds, glyphs, images, paths)
- `frame_scheduler.rs` — configurable-cadence frame scheduling with event coalescing

## Dependencies

- `sys` — syscall wrappers
- `ipc` — ring buffer IPC
- `protocol` — message types + virgl command encoding
- `virtio` — virtio MMIO transport + virtqueue
- `scene` — scene graph types + TripleReader
- `drawing` — Surface type, color types
- `fonts` — glyph rasterization for atlas upload

## Testing

- Protocol encoding: `system/test/tests/virgl_protocol.rs` (host-side)
- Visual: `VIRGL=1 ./test-qemu.sh` (requires virgl-capable QEMU)
