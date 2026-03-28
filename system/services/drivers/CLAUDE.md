# drivers

Hardware abstraction services. Each driver runs as a sandboxed userspace process, receives its MMIO base address and IRQ from init via IPC, and translates between the OS and a specific hardware subsystem.

## Subdirectories

- `metal-render/` — Metal GPU render service (hypervisor passthrough, default backend). Has its own CLAUDE.md.
- `virgil-render/` — **DEPRECATED.** Virgl GPU render service (Gallium3D via virglrenderer in QEMU). v0.3 research spike; no longer maintained.
- `cpu-render/` — CPU software render service + virtio-gpu 2D present (QEMU fallback)
- `virtio-input/` — Keyboard and tablet input driver (evdev events to IPC)
- `virtio-blk/` — Block device driver (standalone self-test; document service owns blk in production)
- `virtio-9p/` — Host filesystem passthrough (9P2000.L protocol, font/image loading)
- `virtio-console/` — Console output driver (placeholder, validation only)

## Common Pattern

All drivers follow the same lifecycle:

1. Receive `MSG_DEVICE_CONFIG` from init (MMIO PA, IRQ)
2. Map MMIO region via `device_map` syscall
3. Negotiate virtio features
4. Register IRQ via `interrupt_register` syscall
5. Allocate DMA memory for virtqueues and data buffers
6. Enter service loop or complete self-test
