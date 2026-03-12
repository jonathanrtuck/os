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

## Font Pipeline

- TrueType parser → glyph outline extraction → scanline rasterizer (4x vertical oversampling) → coverage map → alpha blending onto surface
- GlyphCache: pre-rasterizes printable ASCII (0x20–0x7E) at startup, 48×48 max coverage buffers
- Currently: Source Code Pro Regular, 16px, loaded via 9p

## Drawing Library

- `Surface<'a>`: wraps a pixel buffer (BGRA8888), provides drawing primitives
- `Color`: BGRA8888 with `blend_over()` (Porter-Duff source-over, integer math)
- Primitives: `fill_rect`, `fill_rect_blend`, `draw_line`, `blit`, `blit_blend`, `draw_coverage`
- No rounded rects, no blur, no gradients yet
