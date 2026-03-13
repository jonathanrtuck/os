# libraries

Shared `#![no_std]` libraries used by userspace services and programs.

| Library      | Purpose                                                                       | Status       |
| ------------ | ----------------------------------------------------------------------------- | ------------ |
| `sys/`       | Syscall wrappers + userspace GlobalAlloc (heap via `memory_alloc` syscall)    | Foundational |
| `protocol/`  | IPC message types and payload structs — single source of truth for all boundaries | Foundational |
| `virtio/`    | Virtio MMIO device initialization + virtqueue management                      | Foundational |
| `drawing/`   | 2D rendering: surfaces, alpha blending, TrueType rasterizer                  | Foundational |
| `ipc/`       | Lock-free SPSC ring buffer for 64-byte IPC messages over shared memory        | Foundational |
| `link.ld`    | Linker script for all userspace ELF binaries (code at 4 MiB, stack at 2 GiB)  | Foundational |

## Conventions

- All libraries are `#![no_std]` with optional `alloc` support
- Libraries are compiled as static libs and linked into each userspace binary
- The build orchestration is in `system/build.rs`, not per-library Cargo.toml
