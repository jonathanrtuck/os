# cpu-render

CPU software render service: reads the triple-buffered scene graph from shared memory, rasterizes it using `CpuBackend` (libraries/render), and presents frames via the virtio-gpu 2D protocol. Fallback render path when virgl/Metal are unavailable. Self-allocates double framebuffers via DMA.

## Key Files

- `main.rs` — Entry point, 10-phase handshake with init, render loop (scene walk, incremental rendering, damage tracking, frame scheduling)
- `gpu.rs` — Virtio-GPU 2D command layer: protocol structs and command functions (create resource, attach backing, set scanout, transfer, flush)

## IPC Protocol

**Receives:**

- `MSG_DEVICE_CONFIG` — GPU MMIO address and IRQ from init (handle 0)
- `MSG_GPU_CONFIG` — Framebuffer dimensions from init (handle 0)
- `MSG_COMPOSITOR_CONFIG` — Scene graph VA, font data, scale factor from init (handle 0)
- `MSG_SCENE_UPDATED` — Scene graph change signal from core (handle 1)

**Sends:**

- `MSG_DISPLAY_INFO` — Display resolution to init (handle 0)
- `MSG_GPU_READY` — Ready signal to init (handle 0)

## Dependencies

- `sys` — Syscalls, DMA allocation
- `ipc` — Channel communication
- `protocol` — Wire format (compose, device, gpu, virgl)
- `render` — CpuBackend, incremental rendering, frame scheduler
- `scene` — Scene graph types, triple buffer reader
- `virtio` — Virtio device/virtqueue management
