# services

Platform services that run as userspace processes. Each is a `#![no_std]` ELF binary embedded into init at build time.

| Service                   | Purpose                                                                                | Status      |
| ------------------------- | -------------------------------------------------------------------------------------- | ----------- |
| `init/`                   | Proto-OS-service: reads device manifest, spawns drivers, orchestrates display pipeline | Scaffolding |
| `compositor/`             | Event loop: receives input via IPC, renders text demo, signals GPU                     | Scaffolding |
| `drivers/virtio-blk/`     | Block device driver (reads sector 0 as proof of life)                                  | Scaffolding |
| `drivers/virtio-console/` | Console driver (placeholder)                                                           | Scaffolding |
| `drivers/virtio-gpu/`     | GPU driver: allocates framebuffer, runs present loop                                   | Scaffolding |
| `drivers/virtio-input/`   | Keyboard driver: reads evdev events, forwards to compositor via IPC                    | Scaffolding |

## Conventions

- Services communicate via IPC channels (kernel-managed shared memory)
- Init is the only process the kernel spawns; it spawns everything else
- Each driver receives its MMIO base address and IRQ via an IPC config message from init
- "Scaffolding" means the architecture is right but the implementation will be rewritten
