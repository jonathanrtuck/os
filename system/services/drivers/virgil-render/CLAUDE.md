# virgil-render

Thick Virgil3D GPU driver — reads the scene graph from shared memory and renders using Gallium3D commands via virtio-gpu 3D mode.

## Architecture

```
Core (layout + scene build) → Scene Graph (shared memory) → virgil-render (tree walk + Gallium3D + present) → Display
```

The scene graph is the interface. All rendering complexity (tree walk, GPU state management, glyph atlas, command buffer encoding) is internal to this driver — a leaf node behind a simple boundary.

## Key Files

- `main.rs` — entry point, event loop, virtio-gpu device init
- `shaders.rs` — TGSI shader text (color + textured vertex/fragment)
- `virgl.rs` — VirglContext: GPU state management (future)
- `atlas.rs` — glyph texture atlas (future)
- `scene_walk.rs` — scene graph tree walk emitting GPU draw calls (future)

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
