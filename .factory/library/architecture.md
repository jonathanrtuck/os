# Architecture

Architectural decisions, patterns, and component relationships.

**What belongs here:** Component architecture, design patterns, inter-process communication, memory layout.

---

## Display Pipeline (4 processes)

```
virtio-input driver → compositor → text-editor → compositor → virtio-gpu driver
     (IRQ)          (event loop)   (read-only)   (sole writer)    (present)
```

1. **virtio-input**: Blocks on IRQ, reads evdev events, translates keycodes, sends MSG_KEY_EVENT to compositor
2. **Compositor**: Event loop waiting on input + editor channels. Forwards key events to editor. Applies editor's write requests to document buffer. Renders content. Signals GPU.
3. **Text editor**: Receives key events, translates to editing intent (insert/delete/cursor move), sends write requests back to compositor. Has read-only shared memory mapping of document buffer.
4. **virtio-gpu**: Waits for MSG_PRESENT, coalesces pending presents, does transfer_to_host_2d + resource_flush.

## Key Patterns

- **Compositor is sole writer**: All document mutations go through the compositor. Editors are read-only. This makes undo automatic and non-circumventable.
- **IPC via ring buffers**: SPSC ring buffers (2 pages per channel), 64-byte fixed messages (one cache line), 62 slots per ring.
- **Init orchestrates everything**: Creates channels, allocates shared memory (framebuffer, document buffer), spawns all processes with appropriate handle mappings.
- **DMA allocation**: Kernel provides `dma_alloc` syscall for physically-contiguous pages. Used for framebuffer and virtio descriptor rings.

## Memory Layout

- Framebuffer: DMA-allocated, shared between compositor (write) and GPU driver (read). Currently 1024×768 BGRA8888 (~3 MiB).
- Document buffer: 4 KiB shared page. First 64 bytes = header (length + cursor position). Rest = text content.
- Each IPC channel: 2 pages (one per direction), each a SPSC ring buffer.

## Process Startup Constraint

`handle_send` only works on unstarted processes. This is a critical constraint for IPC protocol design — if two processes need to communicate during initialization (e.g., GPU driver sending display dimensions to init), the channel must be created and one process started before the other.

## Stack Constraint

Userspace processes have a 16 KiB stack. All userspace programs and libraries are compiled with `-C opt-level=s` (in build.rs) to keep stack usage manageable. Be careful with large stack allocations, deep recursion, or unoptimized code paths that expand stack frames.

## Rendering Invariants

- **Clear before re-render**: Any region that will be drawn with alpha blending (e.g., `draw_coverage` for text) MUST be cleared to the background color before re-rendering. Alpha blending is additive — drawing the same glyph twice without clearing produces darker/heavier strokes. This applies to dirty-rect optimizations: the cleared region must cover everything that `draw_tt()` will redraw, not just the region that "changed."
- **Gamma-correct blending**: `draw_coverage()` and `blend_over()` convert sRGB→linear before blending and linear→sRGB after. Lookup tables in `gamma_tables.rs` (512 bytes + 4 KiB). Zero-coverage pixels are never modified (fast path preserved).

## DMA Allocation Limits

- `MAX_DMA_ORDER` = 11 (max 8 MiB per allocation, as of configurable-resolution feature).
- For framebuffers larger than MAX_DMA_ORDER allows in a single allocation, use two separate `dma_alloc` calls and `attach_backing` with `nr_entries=2`. The GPU driver computes transfer offsets as `buffer_index * fb_size` into the combined backing.
- Example: double buffering at 1920×1080 needs 2 × ~8 MiB = two order-11 allocations.

## Font Pipeline

- TrueType parser → glyph outline extraction → scanline rasterizer (2× horizontal + 4× vertical oversampling) → coverage map → gamma-correct alpha blending onto surface
- GlyphCache: pre-rasterizes printable ASCII (0x20–0x7E) at startup, 48×48 max coverage buffers (~430 KiB with 2D oversampling)
- Currently: Source Code Pro Regular, 16px, loaded via 9p

## Drawing Library

- `Surface<'a>`: wraps a pixel buffer (BGRA8888), provides drawing primitives
- `Color`: BGRA8888 with `blend_over()` (Porter-Duff source-over, integer math)
- Primitives: `fill_rect`, `fill_rect_blend`, `draw_line`, `blit`, `blit_blend`, `draw_coverage`
- No rounded rects, no blur, no gradients yet
