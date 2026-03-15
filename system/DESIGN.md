# System Design Notes

Architectural record for everything above the kernel: shared libraries, platform services, drivers, and user programs. Companion to `kernel/DESIGN.md`, which covers kernel internals.

Each section captures the goal, current approach, what's foundational (will survive into the real system), what's scaffolding (works for now, will be replaced), and what's missing. Honest about gaps.

## Design Principle: Simplicity

The system should be simple to reason about from the top down. Simple doesn't mean limited — it means minimal and coherent. The guiding rules:

- **Simple interfaces, complex implementations.** Every boundary between components should be the simplest version that works. Complexity lives inside leaf nodes, behind those boundaries. You shouldn't need to know how a component works internally to understand how the system fits together.
- **Make the happy path as wide as possible.** The goal isn't to ignore edge cases — it's to find the abstraction that genuinely absorbs them through its normal operation. A limited system with no edge cases is just avoidance. A well-designed system handles the breadth of real cases through one coherent mechanism. The few cases that truly can't be absorbed are the real pressure points — documented honestly, opted into when needed. Code implements the happy path; the design considers everything.
- **If an interface is getting messy, rethink the interface.** A growing interface is a signal that the boundary is in the wrong place or the abstraction is wrong, not that more surface area is needed.
- **Track pressure points explicitly.** Where will the clean abstraction genuinely fall short? Document these per-component so the cost of opting in is understood before it's paid. A pressure point means "the design considered this and the abstraction can't absorb it" — not "we didn't think about it."

This is Decision #4 applied to implementation: simple connective tissue, complex leaf nodes, total complexity conserved but displaced to where it's contained.

**Status key:** 🟢 Foundational — 🟡 Scaffolding — 🔴 Demo/throwaway

---

## 0. System Architecture

**How it fits together:**

```text
┌────────────────────────────────────────────────────────┐
│  User Programs     (text-editor, echo)                 │  🟡/🔴
├────────────────────────────────────────────────────────┤
│  Platform Services                                     │
│  ┌────────┐ ┌──────┐ ┌────────────┐ ┌──────────────┐  │
│  │  Init  │ │ Core │ │ Compositor │ │   Drivers    │  │  🟡/🟢
│  │ (root  │ │ (OS  │ │  (scene    │ │ (virtio-blk/ │  │
│  │  task) │ │  svc,│ │   graph    │ │  gpu/input/  │  │
│  │        │ │ sole │ │   render,  │ │  9p/console) │  │
│  │        │ │writer│ │  compose)  │ │              │  │
│  └────────┘ └──┬───┘ └─────┬──────┘ └──────┬───────┘  │
│       input→core│  core→comp│(scene)  comp→gpu│        │
│        editor↔core  (shared mem)       (IPC)           │
├────────────────────────────────────────────────────────┤
│  Libraries                                             │
│  ┌─────┐ ┌────────┐ ┌─────────┐ ┌───────┐ ┌───────┐  │  🟢 foundational
│  │ sys │ │ virtio │ │ drawing │ │ fonts │ │ scene │  │
│  └─────┘ └────────┘ └─────────┘ └───────┘ └───────┘  │
│  ┌─────┐ ┌──────────┐ ┌─────┐                         │
│  │ ipc │ │ protocol │ │ l.d │                         │
│  └─────┘ └──────────┘ └─────┘                         │
├────────────────────────────────────────────────────────┤
│  Kernel (28 syscalls, see kernel/DESIGN.md)            │  🟢 production
└────────────────────────────────────────────────────────┘
```

**Process model:** Kernel spawns only init. Init embeds all other ELF binaries and spawns everything else. Microkernel pattern (Fuchsia component_manager, seL4 root task). This pattern is foundational; init's implementation is scaffolding.

**IPC:** Kernel creates channels (two shared memory pages per channel + signal). Each page is a SPSC ring buffer of 64-byte messages (one direction). The `ipc` library provides lock-free ring buffer mechanics; the `protocol` library defines message types and payload structs for all 9 protocol boundaries. Configuration uses the same mechanism (first message on the ring). The mechanism (channels, shared memory, wait, ring buffers) is foundational.

**Memory model for userspace:** Stack (16 KiB) + static BSS + DMA buffers + shared memory from init + demand-paged heap via `memory_alloc`/`memory_free` syscalls. Heap region: 16–256 MiB VA, 32 MiB physical budget per process. Userspace `GlobalAlloc` in `sys` library (linked-list first-fit with coalescing, grows via `memory_alloc`). Programs opt in with `extern crate alloc;` to get `Vec`/`String`/`Box`.

---

## 1. Libraries

### 1.1 Syscall Library (`libraries/sys/`) 🟢

**Goal:** Safe Rust wrappers for all kernel syscalls.

**Status:** ~710 lines, covers all 28 syscalls with typed errors + `GlobalAlloc` (linked-list first-fit with coalescing). Every userspace binary links against this. Programs opt in to heap allocation with `extern crate alloc;`.

**What's foundational:**

- The syscall ABI (x0–x5 args, x8 number, `svc #0`). Standard, stable.
- The function signatures mirror the kernel's syscall interface 1:1.
- `SyscallError` enum — unified error type covering both `syscall::Error` and `handle::HandleError` from the kernel (13 variants, matching kernel's `repr(i64)` codes). Defensive: unknown codes map to `UnknownSyscall`.
- `SyscallResult<T>` — typed returns on all fallible syscalls. Return types encode meaning: `process_create → SyscallResult<u8>` (handle), `channel_create → SyscallResult<(u8, u8)>` (pair of handles), `dma_alloc → SyscallResult<usize>` (VA).
- `print()` — fire-and-forget console output (wraps `write`, discards Result). Mirrors Rust's `print!` vs `write!` pattern.
- Panic handler that calls `exit()` — correct behavior for userspace panics.
- `GlobalAlloc` — linked-list first-fit allocator with coalescing, grows on demand via `memory_alloc`. Spinlock-protected. Enables `Vec`, `String`, `Box` for all userspace programs. Zero cost if `alloc` crate is not imported.

**What's missing:**

- **Higher-level wrappers.** Raw syscalls are the right foundation, but common patterns (create channel + send handle + start process) should be composable.

---

### 1.2 Virtio Library (`libraries/virtio/`) 🟢

**Goal:** Reusable virtio MMIO transport and split virtqueue, shared across all virtio drivers.

**Status:** 373 lines. Pure library — no syscalls, no allocations. Callers provide DMA buffers.

**What's foundational:**

- MMIO register abstraction with volatile reads/writes.
- Split virtqueue implementation (descriptor table, available ring, used ring).
- Device negotiation flow (reset → acknowledge → driver → features → driver_ok).
- Clean separation: library handles the protocol, caller handles memory and I/O.

**What's temporary:**

- QEMU-specific. Real hardware uses different transports (PCI, platform bus). But the split virtqueue protocol is the same — the transport layer would be swapped, not the queue logic.

**No restrictions imposed.** Pure library with no opinions about allocation or control flow.

---

### 1.3 Drawing Library (`libraries/drawing/`) 🟢

**Goal:** Pure drawing primitives for pixel buffers. No allocations, no syscalls, no hardware — fully testable on the host.

**Status:** ~1100 lines (lib.rs + gamma_tables.rs + palette.rs). Surface abstraction, color with alpha, blending, blitting, PNG decoder, gamma-correct sRGB blending, monochrome palette.

**What's foundational:**

- `Surface<'a>` borrows `&mut [u8]` — no allocation policy. Works with any memory source (DMA, BSS, stack, shared).
- `Color` in canonical RGBA, encode/decode at the pixel boundary. Format-agnostic above the pixel level.
- Porter-Duff source-over blending with gamma-correct sRGB (blend in linear space via lookup tables).
- `blit_blend` — the core compositing operation (per-pixel alpha, clips to bounds).
- `draw_coverage` — composites a coverage map onto a surface with color modulation. The bridge between rasterizer output and the compositing pipeline.
- PNG decoder (DEFLATE, all filter types) — decodes PNG images from byte slices.
- `Palette` — monochrome palette system for consistent UI theming.
- All operations clip silently (no panics). Safe to call with any coordinates.

**Optimized paths (2026-03-15 rendering pipeline optimization):**

- **div-by-255 fast path:** Alpha blending replaced `x / 255` with `(x + 1 + (x >> 8)) >> 8` — exact for all u16 inputs, eliminates hardware divide.
- **Pre-clipped iteration:** `draw_coverage`, `blit_blend`, and `fill_rect_blend` compute clipped row/column ranges up front, handling negative coordinates and oversized offsets without per-pixel bounds checks.
- **Unsafe inner loops:** `draw_coverage`, `blit_blend`, and `fill_rect_blend` use raw pointer access (`unsafe { *ptr }`) in hot inner loops after bounds are verified at the row level. Eliminates redundant bounds checks for ~2× throughput on large surfaces.
- **NEON SIMD (aarch64):** `fill_rect` uses `vst1q_u32` to write 4 pixels per instruction for opaque fills. Alpha blending uses scalar sRGB gamma lookups combined with NEON vector operations for the linear-space blend math. Constant-color blends use a dedicated NEON path. All SIMD paths have scalar fallbacks and are tested against reference implementations.

**What's scaffolding:**

- `PixelFormat` enum has only `Bgra8888`. Trivial to extend (add variant + match arms), but currently untested with other formats.

**No restrictions imposed.** Pure library — anything built on top can use it or replace it.

---

### 1.3b Font Library (`libraries/fonts/`) 🟢

**Goal:** TrueType font parsing, rasterization, and caching. Separated from the drawing library for modularity — fonts depend on drawing (for coverage maps), but drawing doesn't depend on fonts.

**Status:** ~2750 lines (lib.rs + rasterize.rs + cache.rs). Zero-copy TTF parser, scanline rasterizer with LCD subpixel rendering, glyph cache.

**What's foundational:**

- `TrueTypeFont` — zero-copy parser for TTF files. Parses 7 required tables (head, maxp, cmap format 4, hhea, hmtx, loca, glyf). Extracts glyph outlines (quadratic bezier contours), maps codepoints via cmap, reads horizontal metrics.
- Scanline rasterizer — flattens quadratic beziers via De Casteljau subdivision, sweeps with non-zero winding rule. LCD subpixel rendering (per-channel RGB coverage, 6× horizontal oversampling), stem darkening for heavier strokes, GPOS kerning, proper hhea baseline metrics.
- Glyph cache — fixed-size LRU cache (codepoint + size → pre-rasterized bitmap) avoids re-rasterization for repeated text.

**What's scaffolding:**

- Runtime fonts (source-code-pro.ttf, nunito-sans.ttf) are loaded from the host filesystem via the 9p driver. Tests embed these same fonts for parser and rasterizer validation.

**What's missing:**

- **Compound glyph support.** TrueType compound glyphs (accented characters built from components) not yet handled. Only simple glyphs (positive contour count) are parsed.
- **Text layout.** Layout lives in the scene library (monospace) and core service, not here.

**No restrictions imposed.** Pure library with `alloc` dependency (for cache). Callers provide font data as byte slices.

---

### 1.4 Protocol Library (`libraries/protocol/`) 🟢

**Goal:** Single source of truth for all IPC message types and payload structs. Every component that sends or receives IPC messages imports from here.

**Status:** ~364 lines. Defines all 25 message type constants and all shared payload structs across 9 protocol modules, plus `CHANNEL_SHM_BASE` and `channel_shm_va()`.

**What's foundational:**

- **One module per protocol boundary.** `device` (init→drivers), `gpu` (init↔GPU), `input` (input→core), `edit` (core↔editor), `core_config` (init→core), `compose` (init→compositor), `editor` (init→editor), `present` (compositor→GPU), `fs` (init↔9p). The module structure mirrors the IPC topology.
- **All payload structs are `#[repr(C)]`** and fit within the 60-byte IPC message payload. Size guards via `const _: ()` assertions where payloads approach the limit.
- **`CHANNEL_SHM_BASE` and `channel_shm_va()`** defined once. Every userspace component imports these instead of defining local copies.
- **Zero dependencies.** Pure `no_std` library, fully testable on the host.

**No restrictions imposed.** Pure library with no opinions about transport or control flow. The `ipc` library handles ring buffer mechanics; this library defines what flows through them.

---

### 1.5 Linker Script (`libraries/link.ld`) 🟢

**Goal:** Shared ELF layout for all userspace binaries.

**Status:** 16 lines. Base VA 0x400000, page-aligned sections.

**Foundational.** Standard layout, matches kernel's ELF loader expectations. No changes needed unless the VA layout changes.

---

### 1.5 IPC Library (`libraries/ipc/`) 🟢

**Goal:** Lock-free SPSC ring buffer on shared memory pages. The structured message transport for all inter-process communication.

**Ring buffer page layout (one direction, one 4 KiB page):**

```text
┌─────────────────────────────────────────────────────────┐
│ Bytes 0–63: Producer header (one cache line)            │
│   [0..3]   head: u32 (monotonic write counter)          │
│   [4..7]   _reserved                                    │
│   [8..63]  _padding (cache line isolation)              │
├─────────────────────────────────────────────────────────┤
│ Bytes 64–127: Consumer header (one cache line)          │
│   [64..67] tail: u32 (monotonic read counter)           │
│   [68..71] _reserved                                    │
│   [72..127] _padding (cache line isolation)             │
├─────────────────────────────────────────────────────────┤
│ Bytes 128–4095: 62 message slots × 64 bytes             │
│   slot[0]:  bytes 128–191                               │
│   slot[1]:  bytes 192–255                               │
│   ...                                                   │
│   slot[61]: bytes 4032–4095                             │
└─────────────────────────────────────────────────────────┘
```

**Why head and tail on separate cache lines:** On AArch64, a cache line is 64 bytes. The producer writes head, the consumer writes tail. If they share a cache line, every write by either side bounces the line between cores (false sharing). Separate cache lines eliminate this. Same design choice as io_uring.

**Message slot (64 bytes = one cache line):**

```text
┌─────────────────────────────────────┐
│ [0..3]  msg_type: u32               │
│ [4..63] payload: [u8; 60]           │
└─────────────────────────────────────┘
```

`msg_type` determines how to interpret the payload. Each protocol defines its own type constants and payload structs.

**SPSC protocol (lock-free, no CAS):**

- Empty: `head == tail`
- Full: `head - tail >= 62`
- Produce: write payload to `slots[head % 62]`, then `head += 1` with release store
- Consume: read from `slots[tail % 62]`, then `tail += 1` with release store
- Memory ordering: producer writes payload (relaxed), increments head (release). Consumer reads head (acquire), reads payload, increments tail (release).
- Notification: after producing, call `channel_signal` (the kernel doorbell).

**Channel structure (two pages per channel):**

- Page 0: endpoint A → endpoint B ring (A produces, B consumes)
- Page 1: endpoint B → endpoint A ring (B produces, A consumes)
- Both pages mapped into both processes at `CHANNEL_SHM_BASE + ch_idx * 2 * PAGE_SIZE`
- Endpoint A: page 0 = send ring, page 1 = recv ring
- Endpoint B: page 0 = recv ring, page 1 = send ring
- The `ipc` library knows its endpoint index (0 or 1) and interprets accordingly

**What the ipc library provides:**

- `RingBuf` — typed wrapper around a shared page. Init (zero head/tail), produce, consume, is_empty, is_full.
- `Channel` — pair of `RingBuf` (send + recv). Constructed from base VA + endpoint index.
- `Message` — the 64-byte slot struct. Helpers for reading typed payloads from the 60-byte payload region.
- Memory barriers — acquire/release on head/tail via `core::sync::atomic`.

**What it does NOT provide:**

- Message type definitions — per-protocol, not in the ipc library.
- Protocol state validation — deferred until the edit protocol exists.
- Serialization framework — payloads are `#[repr(C)]` structs cast from `[u8; 60]`.
- Error handling for malformed messages — consumer trusts producer (same-OS processes). Kernel validates at trust boundaries.

**Kernel changes needed:**

- `channel::create()` allocates 2 pages instead of 1. Stores both PAs.
- `channel::setup_endpoint()` maps both pages into the target process.
- `channel::shared_info()` returns both PAs and VAs.
- Signal/wait mechanism unchanged — orthogonal to ring buffer format.
- The kernel remains ignorant of message content. It provides shared pages and doorbells.

**No restrictions imposed.** Pure `no_std` library with no syscalls, no allocations. Callers provide the shared memory page address. Fully testable on the host.

### 1.7 Scene Graph Library (`libraries/scene/`) 🟢

**Goal:** Define the scene graph data structures and shared memory layout that form the interface between the OS service (document semantics) and the compositor (pixels). The OS service builds a tree of typed visual nodes; the compositor reads the tree and renders it.

**Design decisions (from 2026-03-13 session):**

- One `Node` type with content variants: `None`, `Text`, `Image`, `Path` (Core Animation model — avoids wrapper nodes).
- Tree encoded via `first_child` / `next_sibling` (left-child right-sibling representation).
- Cursor and selection are properties of `Text` content, not separate nodes (compositor owns text layout, knows glyph positions).
- Relative positioning with `scroll_y` for scrolling.
- The scene graph is a **compiled output** of the document model, not the document model itself.

**Shared memory layout:**

```text
┌──────────┬──────────────────────────┬──────────────────────────┐
│  Header  │  Node array              │  Data buffer              │
│  64 B    │  512 × sizeof(Node)      │  64 KiB                   │
└──────────┴──────────────────────────┴──────────────────────────┘
```

- **Header (64 B):** generation counter (u32), node count (u16), root NodeId (u16), data bytes used (u32), reserved.
- **Node array:** fixed-size entries indexed by `NodeId` (u16). Each node has geometry (x, y, width, height), visual decoration (background, border, corner radius, opacity), flags (visible, clips children), and an optional `Content` variant.
- **Data buffer (64 KiB):** variable-length data (text strings, pixel buffers, path commands) referenced by offset+length (`DataRef`).

**APIs:**

- `SceneWriter` — builds/mutates a scene graph in a `&mut [u8]` buffer. Provides `alloc_node()`, `node_mut()`, `push_data()`, `add_child()`, `commit()`. Also exposes read-back via `nodes()` and `data_buf()` for single-process use.
- `SceneReader` — read-only access to a scene graph buffer. Provides `node()`, `nodes()`, `data()`, `data_buf()`. This is the API the compositor will use when reading from shared memory after the OS service / compositor process split.
- `DoubleWriter` / `DoubleReader` — double-buffered wrapper over two `SCENE_SIZE` regions (`DOUBLE_SCENE_SIZE = 2 × SCENE_SIZE`). The writer writes to the back buffer (lower generation), then `swap()` atomically publishes it as the new front by bumping its generation counter. The reader always reads the front buffer (higher generation). No locks — they never access the same buffer. A release fence before the generation write and an acquire fence after the generation read ensure cross-core visibility on AArch64.

**Monospace text layout helpers (2026-03-14):**

- `layout_mono_lines` — breaks text into visual lines using monospace line-breaking. Returns one `TextRun` per visual line with placeholder `DataRef` (offset = byte position in source text).
- `byte_to_line_col` — converts a byte offset to (visual_line, column) with soft wrap handling. Consistent with `layout_mono_lines` line assignments.
- `line_bytes_for_run` — extracts source text bytes for a run using its placeholder `DataRef`.
- `scroll_runs` — filters and repositions runs for a scrolled viewport. Takes scroll_lines and viewport height, returns only visible runs with y adjusted.

These live in the scene library so they're testable without the kernel.

**Incremental update support (2026-03-15 rendering pipeline optimization):**

- **Change list in SceneHeader:** 24-entry array of changed `NodeId`s plus a `FULL_REPAINT` sentinel (0xFFFF) for when the list overflows or a full rebuild is needed. The OS service records which nodes changed; the compositor reads the list to drive damage tracking.
- `DoubleWriter::copy_front_to_back()` — copies the current front buffer to the back buffer before mutation (copy-forward pattern). Enables incremental updates: the OS service copies the previous frame, modifies only changed nodes, and swaps. Avoids rebuilding the entire scene graph on every event.
- `SceneWriter::mark_changed(node_id)` — appends a node to the change list. If the list is full (>24 entries), sets the `FULL_REPAINT` sentinel so the compositor falls back to full-frame rendering.
- **SceneState targeted update methods:** `update_clock` (updates clock text, 0 allocations), `update_cursor` (updates cursor position/blink, 0 allocations), `update_document_content` (rebuilds text runs and selection after edits), `update_selection` (updates selection overlay). Each method uses `copy_front_to_back()` → modify specific nodes → `mark_changed()` → `swap()`.
- **Data buffer exhaustion fallback:** When the data buffer exceeds 75% capacity, the scene triggers a full rebuild to compact data references.

**No restrictions imposed.** Pure `no_std` library with no syscalls, no allocations (layout helpers require `alloc` for `Vec` return values). Callers provide the buffer. ~1074 lines, host-side tests in `system/test/`.

---

## 2. Platform Services

### 2.1 Init / Proto-OS-Service (`services/init/`) 🟡

**Goal:** Bootstrap userspace. The only process the kernel spawns directly.

**Status:** ~1300 lines (build.rs is at the system/ level). Reads device manifest, spawns drivers, orchestrates display pipeline.

**What's foundational (the pattern):**

- Kernel spawns only init → init spawns everything else. Microkernel root task.
- Init reads a device manifest from the kernel channel to discover hardware.
- Init sends handles (channels, shared memory) to child processes before starting them.

**What's scaffolding (the implementation):**

- **Embedded ELFs via `include_bytes!`.** All binaries baked into init at compile time. Requires full rebuild to change any component. Real OS loads from a filesystem.
- **Hardcoded framebuffer dimensions** (1024×768). Should come from GPU driver, not init.
- **Synchronous linear orchestration.** Spawn compositor, wait for it to finish, then start GPU driver. No event loop, no dynamic process management, no error recovery.
- **Device manifest format.** Ad hoc packed struct (u32 count + 8-byte-aligned entries with device_id, mmio_pa, mmio_size, irq). No versioning, no extensibility. Works for 2 devices.
- **Channel shared memory layout.** Raw bytes at magic offsets. Init writes `fb_va` at offset 0, `fb_width` at offset 8, etc. Each process pair has its own undocumented layout. No schema, no validation.

**Key change (2026-03-11):** Init no longer exits. After setting up all processes and cross-process channels, it idles via `yield_now()`. It creates four kinds of channels: config (init↔child), input (input driver→compositor), present (compositor→GPU driver), and editor (compositor↔text editor). The display pipeline starts four event-loop processes (GPU, input, text editor, compositor) and lets them run autonomously. Start order: GPU driver first (device setup), input driver (IRQ wait), text editor (input wait), compositor last (renders initial frame then enters event loop).

**Key constraint:** Init is still not a real OS service. It sets up the process topology but doesn't mediate runtime communication. The real OS service (renderer + metadata DB + input router + compositor) doesn't exist — init is a stand-in.

---

### 2.2 Core / OS Service (`services/core/`) 🟡

**Goal:** The OS service: sole writer to document state, scene graph builder, input router.

**Status:** ~1750 lines across 4 files (main.rs, scene_state.rs, typography.rs, fallback.rs). Builds a scene graph describing the visual structure of the document, routes input to the active editor, applies editor write requests to document state.

**What's foundational (the approach):**

- **Sole writer to document state.** Core owns the text buffer — the editor never touches it. Write requests (MSG_WRITE_INSERT, MSG_WRITE_DELETE) arrive via IPC and are applied sequentially. This is Decision #9: "editors are read-only consumers, OS service is sole writer."
- **Scene graph output.** Core compiles document structure into a scene graph (typed visual node tree) and publishes it via double-buffered shared memory. The compositor reads the scene graph and renders it — separation of document semantics from pixels.
- **Input routing.** Keyboard/mouse events from the input driver are forwarded to the editor via core's IPC channels.
- **Typography.** Monospace text layout with line-breaking, cursor positioning, selection rendering.

**Incremental scene updates (2026-03-15 rendering pipeline optimization):**

- **Targeted update dispatch.** The event loop classifies each event and dispatches to the narrowest possible update method: timer ticks → `update_clock` (clock text only), cursor blink → `update_cursor` (cursor node only), keypresses/edits → `update_document_content` (text runs + selection), selection changes → `update_selection`. Each method uses copy-forward (copy front buffer to back), mutates only affected nodes, marks them changed, and swaps.
- **Zero-allocation clock and cursor updates.** `update_clock` and `update_cursor` modify existing nodes in-place without touching the data buffer or allocating. These fire at high frequency (250 Hz timer, cursor blink) and must be cheap.

**What's scaffolding (the implementation):**

- **Static text buffer in BSS.** Real OS service reads document content from a Files-backed memory mapping.
- **No operation boundary detection.** Every write is applied immediately without snapshot/undo tracking.
- **Fallback font rendering.** Uses font library for TrueType rendering with glyph cache.

---

### 2.2b Compositor (`services/compositor/`) 🟡

**Goal:** Render the scene graph to pixels. Read the visual node tree from shared memory and composite it into the framebuffer.

**Status:** ~2580 lines across 7 files (main.rs, scene_state.rs, scene_render.rs, compositing.rs, cursor.rs, damage.rs, svg.rs). Reads scene graph, renders surfaces, composites with alpha blending, presents to GPU.

**What's foundational (the approach):**

- **Scene graph consumer.** Reads the double-buffered scene graph from shared memory and renders each node (text runs, images, rectangles, paths) to pixel surfaces.
- **Z-ordered compositing.** Surfaces composited back-to-front with Porter-Duff source-over blending.
- **Change-list-driven damage tracking (2026-03-15).** Reads the change list from the scene header to identify which nodes changed. Computes damage rects from both old (`PREV_BOUNDS`) and new node positions — the old-position damage prevents ghost artifacts when nodes move (e.g., cursor repositioning). Empty change lists skip rendering entirely (no wasted work). Falls back to full repaint on `FULL_REPAINT` sentinel.
- **Subtree clip skipping.** `render_node` checks whether each child's bounds intersect the clip rect before recursing. Children entirely outside the clip region are not visited, reducing work proportional to off-screen content (benefits scrolled documents).
- **SVG rasterization.** Parses and renders SVG paths for UI icons.
- **Procedural cursor.** Arrow cursor rendered at top z-order.

**What's scaffolding (the implementation):**

- **Chrome layout is hardcoded.** Title bar, background, drop shadows all manually positioned.
- **No dynamic surface management.** Surface count and layout are static.

**What's missing:**

- **Connection to layout engine.** Layout produces the surface tree; compositor renders it.

---

### 2.3 Virtio GPU Driver (`services/drivers/virtio-gpu/`) 🟢

**Goal:** Present pixel buffers to the QEMU virtual display.

**Status:** ~600 lines. All six core virtio-gpu 2D commands. Event loop — waits for present commands from compositor.

**What's foundational:**

- Complete 2D command implementation (create resource, attach backing, set scanout, transfer, flush, get display info).
- Interrupt-driven I/O (register IRQ → wait → ack). Correct async pattern.
- **Present loop.** After one-time device setup (create resource, attach backing, set scanout), enters an event loop: wait for `MSG_PRESENT` on compositor channel → transfer to host → flush → loop. Coalesces multiple pending presents into a single transfer+flush.
- Page-aligned MMIO mapping with sub-page offset handling.
- Reuses virtio library for transport + virtqueue.

**What's scaffolding:**

- **No surface trait.** The design thread identified `create_surface`/`present` as the right GPU abstraction, but the driver uses raw virtio-gpu commands. There's no abstract display interface that the compositor could program against.
- **Framebuffer owned by init.** Init allocates the DMA buffer and shares it. The driver just attaches it as backing. Real ownership: compositor or GPU driver owns display buffers.

**QEMU-specific but architecturally sound.** On real hardware, a different driver would implement the same abstract display interface. The virtio-gpu driver is a leaf node — replacing it doesn't affect anything above.

---

### 2.4 Virtio Block Driver (`services/drivers/virtio-blk/`) 🟢

**Goal:** Read sectors from a virtual block device.

**Status:** 211 lines. Reads sector 0, prints first 16 bytes. Interrupt-driven.

**What's foundational:**

- 3-descriptor chain pattern (header → data → status). Correct virtio-blk protocol.
- Same interrupt-driven pattern as GPU driver (wait → ack).

**What's scaffolding:**

- Reads one sector and exits. No block device abstraction (read/write at arbitrary LBAs).
- No filesystem uses it yet.

---

### 2.5 Virtio Input Driver (`services/drivers/virtio-input/`) 🟢

**Goal:** Read keyboard events from the QEMU virtual keyboard and forward to the compositor.

**Status:** ~190 lines. Interrupt-driven event loop. Translates Linux evdev keycodes to ASCII.

**What's foundational:**

- Same interrupt-driven pattern as virtio-blk/gpu (register IRQ → wait → ack → loop).
- Uses the standard virtio-input protocol: posts device-writable 8-byte event buffers on queue 0, device fills them with `{type, code, value}` evdev events.
- Cross-process IPC: sends `MSG_KEY_EVENT` messages to the compositor via a direct channel (not routed through init).
- Keycode-to-ASCII translation table (US layout, 58 keycodes including letters, digits, punctuation, space, enter, backspace).

**What's scaffolding:**

- **Single event buffer.** Posts one 8-byte buffer at a time. Sufficient for keyboard (human typing speed), but mouse/touch input at 100+ events/sec would need multiple pre-posted buffers.
- **Lowercase only.** No shift/ctrl/alt modifier state tracking. The keymap returns lowercase letters only.
- **Direct-to-compositor routing.** In the real OS, input goes to the OS service (input router), which routes to the active editor. See architecture note in source.

**What's missing:**

- **Mouse/touch input.** virtio-input supports EV_REL (relative mouse), EV_ABS (touch/tablet). Not handled.
- **Modifier keys.** Shift, ctrl, alt, meta. Requires state tracking (which modifiers are currently held).
- **Key repeat.** EV_KEY value=2 (repeat) is filtered out. OS-level key repeat with configurable delay/rate.

---

### 2.6 Virtio 9P Driver (`services/drivers/virtio-9p/`) 🟢

**Goal:** Read files from the host macOS filesystem via QEMU's 9p passthrough. Validates the Files interface design through practical use before building the real COW filesystem.

**Status:** ~450 lines. Implements 6 of ~30 9P2000.L operations (Tversion, Tattach, Twalk, Tlopen, Tread, Tclunk). Reads files from a shared host directory (`system/share/`) via virtio transport. Currently used to load the Source Code Pro font at boot (9 KB).

**What's foundational:**

- **Host filesystem passthrough pattern.** The driver bridges the gap between the OS and the host, letting userspace load files without `include_bytes!`. This is the prototype-on-host strategy from Decision #16 in action — implement Files against the host filesystem first, build the real COW FS later.
- **9P2000.L wire protocol.** Manual message encoding/decoding (MsgWriter/MsgReader) for the Plan 9 protocol. 2-descriptor virtio chain (T-message readable, R-message writable).
- **IPC request/response pattern.** Init sends MSG_FS_READ_REQUEST with shared buffer VA + filename, driver fills buffer via 9P reads, sends MSG_FS_READ_RESPONSE with byte count. Shared-memory-reference pattern for large data (§5.5).
- **Same interrupt-driven pattern** as other virtio drivers (register IRQ → wait → ack → loop).

**What's scaffolding:**

- **Single-directory flat namespace.** Only walks one path component from root. No subdirectories.
- **Read-only.** No write, create, or delete operations (only 6 of ~30 9P ops implemented).
- **Init-only client.** The event loop serves requests from init's IPC channel. No multi-client support.
- **Manual payload construction.** Large IPC payloads (FsReadRequest = 60 bytes) must be constructed with manual `write_unaligned` calls — `payload_as`/`from_payload` hangs on aarch64 bare metal for structs near the 60-byte payload limit. This is a known pressure point (§5.5).

**QEMU flags:** `-fsdev "local,id=fsdev0,path=$SHARE_DIR,security_model=none" -device "virtio-9p-device,fsdev=fsdev0,mount_tag=hostshare"` added to all QEMU scripts.

---

### 2.7 Virtio Console Driver (`services/drivers/virtio-console/`) 🟡

**Status:** 112 lines. TX-only, writes one test string. Not exercised (no QEMU device configured).

Minimal and not yet useful. Would need RX queue, proper character device interface.

---

### 2.8 Text Editor (`user/text-editor/`) 🟡

**Goal:** First editor process demonstrating the settled edit protocol: editors are read-only consumers, all writes go through the OS service.

**Status:** ~410 lines. Receives MSG_KEY_EVENT from core (OS service) via IPC. Has read-only shared memory mapping of the document buffer. Translates keypresses into write requests with cursor positioning. Sends write requests back to core. Never writes to document state directly.

**What's foundational (the pattern):**

- **Editor as read-only consumer.** The editor has a hardware-enforced read-only mapping of the document buffer. It reads content for cursor positioning and context-aware editing. All writes go through IPC. This is Decision #9 in action.
- **IPC write protocol.** MSG_WRITE_INSERT carries position + byte, MSG_WRITE_DELETE carries position, MSG_WRITE_DELETE_RANGE carries a byte range, MSG_CURSOR_MOVE carries cursor position, MSG_SELECTION_UPDATE carries selection state. All typed, all fit in 60-byte ring buffer payload.
- **Input → intent translation.** The editor's job is deciding what input means in editing context. Cursor movement, selection (shift+arrow), delete, insert — all translated to structured write/cursor operations.

**What's scaffolding (the implementation):**

- **Single-byte inserts only.** No Unicode, no multi-byte operations.
- **ASCII-only.** No Unicode text handling.

**What's missing:**

- **Operation boundary hints.** `beginOperation`/`endOperation` messages for better undo granularity.
- **Richer edit operations.** Word-level operations, find/replace.

---

### 2.8 Echo (`user/echo/`) 🔴

**Status:** 34 lines. IPC ping-pong demo. Not integrated into current boot sequence.

Pure throwaway. Demonstrates channel IPC works, nothing more.

---

## 3. Constraints & Gaps

These are the things that limit what can be built above the kernel today, ordered by how much they constrain.

### 3.1 ~~No Userspace Heap Allocator~~ ✅ Resolved

**Resolved:** `memory_alloc(page_count)` syscall #25 and `memory_free(va, page_count)` syscall #26 implemented. Demand-paged anonymous pages in the heap VA region (16–256 MiB). Per-process budget: 32 MiB. Wrappers in `sys` library.

**GlobalAlloc:** Implemented in `sys` library — linked-list first-fit allocator with coalescing, grows on demand via `memory_alloc`. Spinlock-protected (`AtomicBool` CAS) for thread safety. All userspace programs get it automatically; opt in with `extern crate alloc;` for `Vec`, `String`, `Box`.

---

### 3.2 No Filesystem

**The problem:** All binaries and data are embedded in init via `include_bytes!`. No way to load resources at runtime. Changing anything requires a full rebuild.

**Why it matters:** Font files, configuration, documents — everything the OS is designed around — can't be loaded. This doesn't block library-level work (embed data as `include_bytes!`), but blocks any system-level feature that involves opening files.

**Blocked on:** Decision #16 (COW filesystem on-disk design). Filesystem is a userspace service; kernel provides COW/VM mechanics. On-disk format not yet designed.

---

### 3.3 ~~No Input~~ ✅ Resolved

**Resolved:** virtio-input keyboard driver (`services/drivers/virtio-input/`). Same interrupt-driven pattern as virtio-blk/gpu: post device-writable event buffers on the event virtqueue, wait for IRQ, read 8-byte Linux evdev events, translate keycodes to ASCII, forward to compositor via IPC ring buffer.

**IPC topology:** Init creates a cross-process channel (input driver → compositor). Input driver sends `MSG_KEY_EVENT` messages; compositor receives and re-renders. This is scaffolding — in the real OS, input routes through the OS service to the active editor process.

**QEMU note:** `-device virtio-keyboard-device` added to run-qemu.sh. QEMU's QMP `input-send-event` does NOT route to virtio-keyboard (QEMU limitation) — must type into the display window directly.

---

### 3.4 ~~No Event Loop / Display Loop~~ ✅ Resolved

**Resolved:** Three processes now run continuous event loops:

1. **Input driver** — `wait(IRQ)` → read event → send to compositor → repost buffer → loop
2. **Compositor** — `wait(input_channel)` → receive key → update text buffer → re-render → signal GPU → loop
3. **GPU driver** — `wait(compositor_channel)` → transfer framebuffer → flush → loop

Init no longer exits — it sets up all cross-process channels, starts all processes, then idles via `yield_now()`.

**Cross-process IPC channels:** Init creates direct channels between processes (input→compositor, compositor→GPU) in addition to its own config channels. Each child gets handle 0 = init channel, handle 1+ = cross-process channels.

**Known limitation:** The kernel's `wait` syscall doesn't implement finite timeouts — only poll (`timeout=0`) or infinite block. Timer handles (`timer_create`) can be mixed into `wait` calls as a workaround.

---

### 3.5 ~~No Structured IPC Messages~~ ✅ Resolved

**Resolved:** Ring buffer IPC with fixed 64-byte messages. See §1.5 for the `ipc` library design. Implemented at `libraries/ipc/` (~297 lines). All services migrated to ring buffer messages.

**Decisions made (2026-03-10):**

- **One mechanism** — ring buffers for everything, including configuration (first message pattern, no separate config struct path). Prior art: Singularity contracts (config = opening protocol sequence).
- **Separate pages per direction** — each channel allocates two 4 KiB pages, one per direction. Each page is a textbook SPSC queue. Prior art: io_uring (separate SQ/CQ regions), LMAX Disruptor.
- **Fixed 64-byte messages** — one AArch64 cache line. 4-byte type tag + 60-byte payload. 62 slots per ring (4 KiB page − 128-byte header). Prior art: io_uring (64-byte SQE).
- **Split architecture** — shared `ipc` library for ring buffer mechanics, per-protocol payload definitions elsewhere.

**Pressure point:** Messages >60 bytes. If genuinely needed, use shared memory + ring buffer reference. Documented tension, not pre-built escape hatch.

---

### 3.6 Single Pixel Format

**The problem:** Only BGRA8888. The `PixelFormat` enum exists for extensibility but only has one variant.

**Why it matters now:** Doesn't. virtio-gpu uses BGRA8888. Real hardware might need other formats, but the drawing library's encode/decode-at-the-boundary design handles this — add a variant, add match arms.

**Not a real constraint.** Listed for completeness.

---

## 4. Component Dependency Map

```text
User Programs (text-editor)
  ├── sys (wait, channel_signal, exit, print)
  ├── ipc (Channel, Message — ring buffer messaging)
  └── protocol (edit, editor, input — message types + payload structs)

Core (OS Service)
  ├── sys (wait, channel_signal, exit)
  ├── drawing (Surface, Color, blit_blend)
  ├── fonts (TrueTypeFont, glyph cache)
  ├── scene (SceneWriter, node types, text layout)
  ├── ipc (Channel, Message — ring buffer messaging)
  └── protocol (core_config, edit, input — message types + payload structs)

Compositor
  ├── sys (wait, channel_signal, exit)
  ├── drawing (Surface, Color, blit_blend, PNG)
  ├── fonts (TrueTypeFont, glyph cache)
  ├── scene (SceneReader, node types)
  ├── ipc (Channel, Message — ring buffer messaging)
  └── protocol (compose, present — message types + payload structs)

Init
  ├── sys (process_create, channel_create, handle_send, memory_share, dma_alloc, wait, ...)
  └── protocol (device, gpu, core_config, compose, editor, fs — message types + payload structs)

Drivers (virtio-blk, virtio-gpu, virtio-input, virtio-9p, virtio-console)
  ├── sys (device_map, interrupt_register, dma_alloc, wait, ...)
  ├── virtio (MMIO transport, split virtqueue)
  ├── ipc (ring buffer messaging — for cross-process channels)
  └── protocol (device, gpu, input, present, fs — message types + payload structs)

Protocol Library
  └── (none — pure, no dependencies. Defines all IPC message types + payload structs)

Drawing Library
  └── (none — pure, no dependencies)

Font Library
  └── drawing (for coverage map compositing)

Scene Library
  └── (none — pure, no dependencies. Uses core::sync::atomic for double-buffering)

IPC Library
  └── (none — pure, no dependencies. Uses core::sync::atomic only)

Sys Library
  └── (none — inline asm only)

Virtio Library
  └── (none — pure, no dependencies)
```

**Observation:** All libraries are clean leaves with minimal dependencies (only `fonts` depends on `drawing`). The `protocol` library is the single source of truth for all IPC message types and payload structs — no component defines its own message constants or wire-format structs. The platform services depend on libraries + syscalls. User programs (text-editor) depend on `sys` + `ipc` + `protocol` — they don't touch `drawing`, `fonts`, or `virtio`. This is architecturally correct: editors don't render, the OS service does. Core (OS service) owns document semantics and scene graph construction; the compositor owns pixel rendering. The coupling between platform services (init knows about core, compositor, GPU driver, editor, etc.) is all scaffolding — the real OS service would mediate these relationships.

---

## 5. Known Pressure Points

Places where the clean abstraction will eventually face tension. Documented so the cost of opting in is understood before it's paid. None of these need to be solved now — the happy path works without them.

### ~~5.1 Drawing Library: Per-pixel blending is slow for large surfaces~~ ✅ Resolved (2026-03-15)

**Resolved:** NEON SIMD acceleration for `fill_rect` (4 pixels/instruction via `vst1q_u32`), `blit_blend` (scalar sRGB lookups + NEON vector blend), and `fill_rect_blend` (const-color NEON path). Unsafe inner loops with pre-clipped bounds for all blending operations. div-by-255 replaced with multiply-shift. The interfaces (`blit_blend`, `fill_rect`, `draw_coverage`) are unchanged — optimization is internal to the leaf node, as predicted.

### ~~5.2 Compositor: Full recomposite every frame~~ ✅ Resolved (2026-03-15)

**Resolved:** Change-list-driven damage tracking. The OS service records changed nodes in the scene header; the compositor reads the change list and computes damage rects (old + new bounds). Empty change lists skip rendering entirely. Subtree clip skipping avoids visiting children outside the damage region. The compositor's external interface is unchanged — scene graph in, pixels out. Combined with incremental scene updates in the OS service (targeted update methods for clock, cursor, text, selection), most frames touch only a small fraction of the scene and framebuffer.

### 5.3 Font rasterization: Glyph caching

Rasterizing a glyph from bezier curves every time it's drawn is expensive. A glyph cache (codepoint + size → pre-rasterized bitmap) is the standard solution. With no heap allocator, the cache must be pre-sized.

**Escape hatch:** Fixed-size LRU cache in static memory (works now), or dynamic cache once `memory_alloc` exists. Internal to the font module — callers don't know or care whether the glyph was cached.

### 5.4 GPU driver: Direct scanout vs copy

virtio-gpu copies the framebuffer from guest to host memory on every present — this is inherent to the virtual device, not our architecture. On real hardware, the display controller reads directly from the framebuffer via DMA scanout (zero-copy). Our surface-based abstraction handles both transparently — the driver decides how to present.

**Escape hatch:** None needed. The abstraction already accommodates this. Listed to document why virtio-gpu performance isn't representative of real hardware.

### 5.5 IPC: Messages that exceed 60 bytes

Ring buffer messages are fixed at 64 bytes (4-byte type + 60-byte payload). All current control message types fit comfortably (edit protocol, input events, device configuration — all <40 bytes). But some future messages may not: metadata query results with variable-length strings, error diagnostics, path references.

**Design rule (not an escape hatch):** Large data never flows through the ring buffer. It goes in shared memory; the ring carries a reference (VA + length). Documents are already memory-mapped this way. If a message type regularly needs >60 bytes, that's a signal it should use the shared-memory-reference pattern — not a signal to make messages bigger.

---

## 6. What's Next

Ordered by what unblocks the most, building the happy path first:

1. ~~**Font rasterization**~~ — **Done.** TrueType rasterizer in the font library (`libraries/fonts/`). Zero-copy parser, scanline rasterizer with LCD subpixel rendering (6× horizontal oversampling), stem darkening, glyph cache. Running on bare metal in core and compositor.
2. ~~**Syscall error types**~~ — **Done.** `SyscallError` enum (13 variants) + `SyscallResult<T>` on all 25 syscalls + `print()` convenience. All 6 userspace binaries migrated. Eliminated raw `i64` returns and ad-hoc `< 0` checks.
3. ~~**Userspace memory allocation**~~ (§3.1) — **Done.** `memory_alloc`/`memory_free` (#25/#26) + `GlobalAlloc` in `sys` library. `Vec`/`String`/`Box` available to all userspace programs.
4. ~~**Structured IPC**~~ (§3.5) — **Done.** Ring buffer library implemented (`libraries/ipc/`). Kernel allocates two pages per channel. All services migrated to ring buffer messages.
5. ~~**Input driver**~~ (§3.3) — **Done.** virtio-input keyboard driver with IPC forwarding to compositor. Cross-process channels for direct driver↔compositor communication.
6. ~~**Event loop**~~ (§3.4) — **Done.** Compositor, GPU driver, and input driver all run continuous event loops. Init stays alive.
7. ~~**Editor process separation**~~ — **Done.** Text editor process (`user/text-editor/`) receives input events from core, sends write requests back. Core is sole writer to document state. Demonstrates Decision #9 (editors as read-only consumers). Five processes in the display pipeline: GPU driver, input driver, text editor, core, compositor.
8. ~~**Read-only document mapping**~~ — **Done.** Text editor has a hardware-enforced read-only shared memory mapping of the document buffer. Reads content for cursor positioning and context-aware editing. All writes go through IPC to core (sole writer).
9. **Text layout** — connective tissue between fonts, drawing, and the compositor. This is an _interface_ question (gets the design treatment), not just an implementation. How does text flow? How does the editor specify what to render? Must be simple to reason about.
10. **Filesystem service** (§3.2) — blocked on Decision #16. Files interface designed (12 operations), macOS prototype validated at `prototype/files/` with 21 passing tests. **Partially unblocked:** virtio-9p driver (§2.6) provides runtime file loading from host filesystem during prototyping. Font loading working end-to-end.
11. ~~**Wait timeout**~~ — **Done.** For finite timeouts (0 < timeout < u64::MAX), `sys_wait` creates an internal timer, adds it to the wait set with a sentinel index. If the timer fires first, returns `WouldBlock`. Timer cleanup: immediate on non-blocked paths; deferred to next `wait` call for the blocked→woken path (stored on thread struct).
