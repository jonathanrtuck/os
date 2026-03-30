# test/tests

72 host-compiled test files exercising kernel and library logic in isolation. Each file includes kernel or library source via `#[path]` with stub dependencies (mock `IrqMutex`, identity PA/VA mapping).

Run with `cargo test -- --test-threads=1` (some tests share global state).

## Naming Conventions

Test files use category prefixes for subsetting (e.g., `cargo test kernel_` runs only kernel tests):

- `kernel_*` -- core kernel subsystems (scheduling, processes, threads, handles, syscalls, sync)
- `mem_*` -- memory management (allocators, paging, heap, virtual memory, ASID, content allocator)
- `render_*` -- graphics and rendering (drawing, scene, animation, blur, shaping)
- `text_*` -- text, layout, unicode, NEON
- `ipc_*` -- IPC and channels (ring buffers, channel lifecycle, leak tests)
- `hw_*` -- hardware and devices (device tree, interrupts, MMIO, virtqueue)
- `fs_*` -- filesystem and storage (crash consistency, linked-block operations, document store)
- `codec_*` -- decoders/encoders (PNG decoder, PNG conformance)
- `stress_*` -- stress, adversarial, and integration-under-load tests
- `misc_*` -- cross-cutting / uncategorized
