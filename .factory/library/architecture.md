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
2. **Compositor**: Event loop waiting on input + editor + timer channels. Forwards key events to editor. Applies editor's write requests to document buffer. Manages multi-surface compositing (background, content, shadows, chrome). Supports two content modes: text editor (default) and image viewer (toggled via F1). Updates live clock from timer events. Signals GPU.
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
- Two font caches:
  - **Monospace** (Source Code Pro Regular, 16px): used for editor text. Fixed char_width via TextLayout.
  - **Proportional** (Nunito Sans Regular, 16px): used for chrome text (title bar, status bar). Per-glyph advance widths via `draw_proportional_string()`.
- All assets loaded from `system/share/` via 9p into a single 256 KiB shared buffer (mono font | prop font | PNG image | SVG icon). Init sends offsets/lengths to compositor via CompositorConfig IPC message.
- Missing glyph codepoints advance by space width fallback without crashing.

## Multi-Surface Compositing

The compositor uses a multi-surface model with z-ordered back-to-front compositing:

| Surface          | Z-order | Description |
|-----------------|---------|-------------|
| Background       | 0       | Solid dark background fill |
| Content          | 10      | Text editor or image viewer (full-screen, extends behind chrome) |
| Title shadow     | 15      | Soft gradient shadow below title bar |
| Title bar chrome | 20      | Translucent (alpha=170) with "Document OS" branding |

Each surface has a dedicated render function (e.g., `render_content_surface()`, `render_title_bar()`). Surfaces are allocated once at startup and re-rendered in-place each frame. The `composite_surfaces()` function sorts by z-order (stable sort) and composites via `blit_blend`.

Content modes: `IMAGE_MODE` global toggles between text editor (renders document text) and image viewer (renders decoded PNG). F1 key switches modes. Text state is preserved across switches.

## Editor ↔ Compositor IPC Protocol

The text editor and compositor communicate via bidirectional IPC channels using these message types:

| Type ID | Name | Direction | Payload |
|---------|------|-----------|---------|
| 1 | MSG_KEY_EVENT | compositor → editor | Keycode + shift/ctrl flags |
| 30 | MSG_WRITE_INSERT | editor → compositor | Byte position + character to insert |
| 31 | MSG_WRITE_DELETE | editor → compositor | Byte position to delete at |
| 32 | MSG_CURSOR_MOVE | editor → compositor | New cursor byte position |
| 33 | MSG_SELECTION_UPDATE | editor → compositor | Selection start + end byte positions |
| 34 | MSG_WRITE_DELETE_RANGE | editor → compositor | Start + end byte positions for bulk delete |

The editor sends multiple messages atomically (before signaling), and the compositor drains them all in one event loop iteration. Selection range normalization handles reversed anchor/cursor.

## Pointer Pipeline (Milestone 3)

The virtio-tablet device produces absolute coordinate events (EV_ABS, ABS_X/ABS_Y in 0-32767 range) and button events (EV_KEY, BTN_LEFT/BTN_RIGHT). The input driver sends these as MSG_POINTER_ABS and MSG_POINTER_BUTTON to the compositor, which:

1. Scales coordinates from [0, 32767] to screen pixel coordinates
2. Updates cursor surface position (z=30, above all other surfaces)
3. Generates dirty rects for old + new cursor positions
4. On click in content area: converts screen coords to text position (column = x / char_width, line = y / line_height) and sends cursor-position message to editor
5. Clicks in title bar are ignored (not forwarded to editor)

The tablet device appears as a second virtio-input device (device ID 18) in the kernel's device manifest. Init spawns a driver for each virtio-input device.

## Drawing Library

- `Surface<'a>`: wraps a pixel buffer (BGRA8888), provides drawing primitives
- `Color`: BGRA8888 with `blend_over()` (Porter-Duff source-over, integer math)
- Primitives: `fill_rect`, `fill_rect_blend`, `draw_line`, `blit`, `blit_blend`, `draw_coverage`, `fill_gradient_v`
- `CompositeSurface` + `composite_surfaces()`: z-ordered back-to-front compositing with negative-offset clipping and blit_blend delegation
- `png_decode()`: no-dependency PNG decoder (DEFLATE, all 5 filter types, RGB/RGBA → BGRA8888)
- `svg_parse_path()` / `svg_parse_path_into()`: SVG path data parser (M/L/C/Z, absolute + relative), returns `SvgPath` segments
- `svg_rasterize()`: Rasterizes parsed SVG paths into coverage maps using scanline/non-zero winding rule (same approach as TrueType rasterizer)
- `palette` module: 13 named color constants (dark blue-grey theme) — all UI colors centralized here
- **SVG struct sizes**: `SvgPath` (~16 KiB) and `SvgRasterScratch` (~64 KiB) exceed the 16 KiB userspace stack — must be heap-allocated via `alloc_zeroed` in bare-metal code
- No rounded rects, no blur yet
