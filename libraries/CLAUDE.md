# libraries

Shared `#![no_std]` libraries used by userspace services and programs.

| Library      | Purpose                                                                                                        | Status       |
| ------------ | -------------------------------------------------------------------------------------------------------------- | ------------ |
| `sys/`       | Syscall wrappers + userspace GlobalAlloc (heap via `memory_alloc` syscall)                                     | Foundational |
| `protocol/`  | IPC message types, payload structs (10 boundaries), Content Region layout + allocator, decode protocol         | Foundational |
| `virtio/`    | Virtio MMIO device initialization + virtqueue management                                                       | Foundational |
| `drawing/`   | Surfaces, colors, Porter-Duff compositing, sRGB blending, palette                                              | Foundational |
| `fonts/`     | TrueType rasterizer, analytic coverage, outline dilation (stem darkening), glyph cache                         | Foundational |
| `animation/` | Animation library: easing functions, spring physics, timeline sequencing                                       | Foundational |
| `ipc/`       | Lock-free SPSC ring buffer for 64-byte IPC messages, `recv_blocking` for synchronous RPC                       | Foundational |
| `layout/`    | Unified text layout engine: one function for mono + proportional via `FontMetrics` trait                       | Foundational |
| `render/`    | Shared rendering infra: scene tree walk, frame scheduler, path rasterizer, coordinate helpers                  | Foundational |
| `icons/`     | Named vector icons: `get(name, mimetype)` lookup, pre-compiled Tabler SVGs, layer annotations                  | Foundational |
| `scene/`     | Scene graph types, triple-buffered shared memory layout, writer/reader APIs for core ↔ render services         | Foundational |
| `fs/`        | COW filesystem: BlockDevice trait, superblock ring, free-extent allocator, inodes, snapshots, two-flush commit | Foundational |
| `store/`     | Document store metadata layer: catalog, media types, attributes, queries. Wraps `Box<dyn Files>`               | Foundational |
| `link.ld`    | Linker script for all userspace ELF binaries (code at 4 MiB, stack at 2 GiB)                                   | Foundational |

## Conventions

- All libraries are `#![no_std]` with optional `alloc` support
- Libraries are compiled as static libs and linked into each userspace binary
- The build orchestration is in `build.rs`, not per-library Cargo.toml
