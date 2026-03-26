# services

Platform services that run as userspace processes. Each is a `#![no_std]` ELF binary embedded into init at build time.

| Service                   | Purpose                                                                                           | Status      |
| ------------------------- | ------------------------------------------------------------------------------------------------- | ----------- |
| `init/`                   | Root task: reads device manifest, spawns drivers, orchestrates display pipeline                   | Scaffolding |
| `core/`                   | OS service: sole writer, scene graph builder, navigation/selection, input router                  | Scaffolding |
| `drivers/metal-render/`   | Metal render service: native Metal GPU via hypervisor (default, 4x MSAA)                          | Scaffolding |
| `drivers/cpu-render/`     | CPU render service: CpuBackend + virtio-gpu 2D present (QEMU fallback)                            | Scaffolding |
| `drivers/virgil-render/`  | GPU render service: Gallium3D via virglrenderer (virgl-capable QEMU)                              | Scaffolding |
| `drivers/virtio-blk/`     | Block device driver: read/write/flush with VIRTIO_BLK_F_FLUSH negotiation (self-test, standalone) | Scaffolding |
| `drivers/virtio-console/` | Console driver (placeholder)                                                                      | Scaffolding |
| `drivers/virtio-input/`   | Keyboard + tablet driver: reads evdev events, forwards to core via IPC                            | Scaffolding |
| `drivers/virtio-9p/`      | Host filesystem passthrough: 9P2000.L protocol, loads fonts/images/icons                          | Scaffolding |
| `filesystem/`             | COW filesystem service: owns virtio-blk device, format/mount, IPC commit loop with core           | Scaffolding |
| `decoders/png/`           | PNG decoder service: sandboxed, uses generic decoder harness                                      | Scaffolding |

## Service Categories

- **`drivers/`** — Hardware abstraction (GPU, block, input, filesystem, console)
- **`decoders/`** — Content transformation (PNG, future: JPEG, WebP). Each decoder is a sandboxed process behind the generic decode protocol (`protocol/decode.rs`). Format-specific code only; all IPC plumbing lives in `decoders/harness.rs`.

## Conventions

- Services communicate via IPC channels (kernel-managed shared memory)
- Init is the only process the kernel spawns; it spawns everything else
- Each driver receives its MMIO base address and IRQ via an IPC config message from init
- Decoder services receive File Store (RO) + Content Region (RW) mappings from init
- "Scaffolding" means the architecture is right but the implementation will be rewritten
