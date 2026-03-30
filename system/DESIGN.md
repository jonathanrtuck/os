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

**Status key:** 🟢 Foundational — 🟡 Scaffolding — 🔴 Demo/throwaway — ⚫ Deprecated

---

## 0. System Architecture

**How it fits together:**

```text
┌───────────────────────────────────────────────────────────┐
│  User Programs     (text-editor, echo)                    │  🟡/🔴
├───────────────────────────────────────────────────────────┤
│  Platform Services                                        │
│  ┌────────┐ ┌─────────┐ ┌──────────────────────────────┐  │
│  │  Init  │ │  Core   │ │      Render Services         │  │  🟡/🟢
│  │ (root  │ │ (OS svc)│ │ ┌─────────────┐ ┌──────────┐ │  │
│  │  task) │ │         │ │ ┌─────────────┐              │  │
│  │        │ │   sole  │ │ │metal-render │              │  │
│  │        │ │  writer │ │ │  (Metal GPU)│              │  │
│  └────────┘ └───┬─────┘ │ └─────────────┘              │  │
│      input→core │       │                              │  │
│   editor↔core   │       │                              │  │
│                 │       │                              │  │
│          core→render    └──────────────────────────────┘  │
│          (scene graph,                                    │
│           shared mem)          ┌──────────────────┐       │
│                                │  Drivers +       │       │
│                                │  Services        │       │
│                                │ (virtio-blk/     │       │
│                                │  input/9p/       │       │
│                                │  console/        │       │
│                                │  filesystem)     │       │
│                                └──────────────────┘       │
├───────────────────────────────────────────────────────────┤
│  Libraries                                                │
│  ┌─────┐ ┌────────┐ ┌─────────┐ ┌───────┐ ┌───────┐       │  🟢 foundational
│  │ sys │ │ virtio │ │ drawing │ │ fonts │ │ scene │       │
│  └─────┘ └────────┘ └─────────┘ └───────┘ └───────┘       │
│  ┌─────┐ ┌──────────┐ ┌────────┐ ┌───────────┐ ┌────────┐ │
│  │ ipc │ │ protocol │ │ render │ │ animation │ │ layout │ │
│  └─────┘ └──────────┘ └────────┘ └───────────┘ └────────┘ │
│  ┌────┐                                                   │
│  │ fs │                                                   │
│  └────┘                                                   │
├───────────────────────────────────────────────────────────┤
│  Kernel (28 syscalls, see kernel/DESIGN.md)               │  🟢 production
└───────────────────────────────────────────────────────────┘
```

**Process model:** Kernel spawns only init. Init reads service ELFs from a memory-mapped service pack and spawns everything else. Microkernel pattern (Fuchsia component_manager, seL4 root task). This pattern is foundational; init's implementation is scaffolding.

**IPC:** Two mechanisms, matched to data semantics:

- **Event rings** (discrete events where order and count matter): Kernel creates channels (two shared memory pages per channel + signal). Each page is a SPSC ring buffer of 64-byte messages (one direction). The `ipc` library provides lock-free ring buffer mechanics; the `protocol` library defines message types and payload structs for all 9 protocol boundaries. Used for key presses, button clicks, config messages.
- **State registers** (continuous state where only the latest value matters): Init-allocated shared memory pages with atomic reads/writes. The producer overwrites the latest value (store-release); the consumer reads it once per frame (load-acquire). Zero queue, zero overflow. Used for pointer position (input driver → core). Same init-orchestrated shared memory pattern as the scene graph (core → render).
- **Content Region** (persistent decoded content): Init-allocated 4 MiB shared memory region with a `ContentRegionHeader` registry (64 entries). Contains font TTF data and decoded image pixels. Init writes font entries at boot; decoder services write decoded pixels via IPC-directed shared memory writes; core manages the registry and free-list allocator (`ContentAllocator` with first-fit, coalescing, generation-based deferred GC). Render services read-only. Compositor never sees raw encoded files (File Store is a separate 1 MiB region shared with decoder services). Write-once entry semantics for lock-free concurrent reads.

Notification for both: `channel_signal` syscall wakes the consumer from `sys::wait()`. The signal means "something changed" — the consumer checks both event rings and state registers.

**Memory model for userspace:** Stack (64 KiB) + static BSS + DMA buffers + shared memory from init + demand-paged heap via `memory_alloc`/`memory_free` syscalls. Heap region: 16–256 MiB VA, 32 MiB physical budget per process. Userspace `GlobalAlloc` in `sys` library (linked-list first-fit with coalescing, grows via `memory_alloc`). Programs opt in with `extern crate alloc;` to get `Vec`/`String`/`Box`.

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

**Status:** ~1100 lines (lib.rs + gamma_tables.rs + palette.rs). Surface abstraction, color with alpha, blending, blitting, gamma-correct sRGB blending, monochrome palette. PNG decoder moved to `services/decoders/png/` (sandboxed service).

**What's foundational:**

- `Surface<'a>` borrows `&mut [u8]` — no allocation policy. Works with any memory source (DMA, BSS, stack, shared).
- `Color` in canonical RGBA, encode/decode at the pixel boundary. Format-agnostic above the pixel level.
- Porter-Duff source-over blending with gamma-correct sRGB (blend in linear space via lookup tables).
- `blit_blend` — the core compositing operation (per-pixel alpha, clips to bounds).
- `draw_coverage` — composites a coverage map onto a surface with color modulation. The bridge between rasterizer output and the compositing pipeline.
- ~~PNG decoder~~ — moved to `services/decoders/png/` as a sandboxed decoder service (2026-03-25).
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

**Status:** ~3,500 lines across lib.rs, cache.rs, and rasterize/ (8 modules). read-fonts for outline extraction, HarfRust for OpenType shaping, analytic area coverage rasterizer, outline dilation (stem darkening), variable font support (gvar), glyph cache.

**What's foundational:**

- read-fonts + HarfRust — OpenType shaping (ligatures, kerning, contextual alternates). Glyph outline extraction from TTF/OTF (simple + composite glyphs, variable fonts via gvar).
- Analytic area coverage rasterizer — bezier flattening + exact signed-area trapezoid coverage per pixel. Grayscale anti-aliasing (1 byte/pixel). No LCD subpixel rendering (unnecessary at Retina density).
- Outline dilation (stem darkening) — macOS Core Text formula with scale-factor-aware conversion. Symmetric miter-join modification applied to glyph outlines before rasterization.
- Glyph cache — fixed ASCII cache (95 glyphs, O(1) lookup) + LRU cache for non-ASCII/ligature glyphs. Keyed by (glyph_id, font_size, axis_hash).

**What's scaffolding:**

- Runtime fonts (jetbrains-mono.ttf, inter.ttf, source-serif-4.ttf) are loaded from the host filesystem via the 9p driver. Tests embed these same fonts for parser and rasterizer validation.

**What's missing:**

- **Compound glyph support.** TrueType compound glyphs (accented characters built from components) not yet handled. Only simple glyphs (positive contour count) are parsed.
- **Text layout.** Layout lives in the core service (`scene_state.rs`), not here.

**No restrictions imposed.** Pure library with `alloc` dependency (for cache). Callers provide font data as byte slices.

---

### 1.4 Protocol Library (`libraries/protocol/`) 🟢

**Goal:** Single source of truth for all IPC message types and payload structs. Every component that sends or receives IPC messages imports from here.

**Status:** 10 protocol modules across ~1000 lines (lib.rs + external files). Defines message type constants and all shared payload structs, plus shared memory layout types (`PointerState` for the input state register), `CHANNEL_SHM_BASE` and `channel_shm_va()`.

**What's foundational:**

- **One module per protocol boundary (10 modules).** `init` (init→any service config), `device` (init→drivers), `input` (input→presenter), `edit` (editor↔document, editor↔presenter), `layout` (presenter↔layout), `view` (presenter→compositor, document↔presenter notifications), `store` (document↔store service), `decode` (document↔decoders), `content` (shared memory layout), `metal` (compositor→hypervisor, includes legacy virgl submodule). The module structure mirrors the IPC topology.
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

**Ring buffer page layout (one direction, one 16 KiB page):**

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
│ Bytes 128–16383: 254 message slots × 64 bytes           │
│   slot[0]:    bytes 128–191                             │
│   slot[1]:    bytes 192–255                             │
│   ...                                                   │
│   slot[253]:  bytes 16320–16383                         │
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

**Design decisions (from 2026-03-13 session, updated 2026-03-16):**

- One `Node` type with geometric content variants: `None`, `FillRect`, `Glyphs`, `Image` (no semantic content types — the scene graph is purely geometric).
- Tree encoded via `first_child` / `next_sibling` (left-child right-sibling representation).
- Cursor and selection are `FillRect` content nodes (explicit geometry, not implicit text properties).
- `child_offset` (f32, f32) for scrolling and document slide.
- The scene graph is a **compiled output** of the document model, not the document model itself.

**Shared memory layout:**

```text
┌──────────┬──────────────────────────┬──────────────────────────┐
│  Header  │  Node array              │  Data buffer             │
│  64 B    │  512 × sizeof(Node)      │  64 KiB                  │
└──────────┴──────────────────────────┴──────────────────────────┘
```

- **Header (64 B):** generation counter (u32), node count (u16), root NodeId (u16), data bytes used (u32), reserved.
- **Node array:** fixed-size entries indexed by `NodeId` (u16). Each node is 120 bytes. Geometry (x, y, width, height), visual decoration (background, border, corner radius, opacity, shadow), flags (visible, clips children), `clip_path` (DataRef), `child_offset` (f32, f32) for scroll/slide, `transform` (AffineTransform), `cursor_shape` (u8: 0=inherit, 1=pointer, 2=text — used by Core's hit-test, ignored by render driver), and a `Content` variant (None/Image/Path/Glyphs). **Growth policy:** add fields in 8-byte increments. `_reserved` fields (3 bytes) provide headroom for future additions without a size bump.
- **Data buffer (128 KiB):** variable-length data (shaped glyph arrays, pixel buffers, path commands) referenced by offset+length (`DataRef`).

**APIs:**

- `SceneWriter` — builds/mutates a scene graph in a `&mut [u8]` buffer. Provides `alloc_node()`, `node_mut()`, `push_data()`, `add_child()`, `commit()`. Also exposes read-back via `nodes()` and `data_buf()` for single-process use.
- `SceneReader` — read-only access to a scene graph buffer. Provides `node()`, `nodes()`, `data()`, `data_buf()`. This is the API render services (metal-render) use when reading from shared memory.
- `DoubleWriter` / `DoubleReader` — double-buffered wrapper over two `SCENE_SIZE` regions (`DOUBLE_SCENE_SIZE = 2 × SCENE_SIZE`). The writer writes to the back buffer (lower generation), then `swap()` atomically publishes it as the new front by bumping its generation counter. The reader always reads the front buffer (higher generation). No locks — they never access the same buffer. A release fence before the generation write and an acquire fence after the generation read ensure cross-core visibility on AArch64.

**Incremental update support (2026-03-15 rendering pipeline optimization):**

- **Dirty bitmap in SceneHeader:** 512-bit bitmap (`[u64; 8]`), one bit per `MAX_NODES`. The OS service marks changed nodes via `mark_dirty(node_id)`; render services read `dirty_bits()` to drive damage tracking. Never overflows — replaces the former 24-entry change list and `FULL_REPAINT` fallback.
- `TripleWriter::acquire_copy()` — copies the latest published buffer to the acquired buffer (copy-forward pattern). Enables incremental updates: the OS service copies the previous frame, modifies only changed nodes, and publishes. The dirty bitmap is zeroed in the copy so only newly-dirtied nodes are flagged.
- `SceneWriter::mark_dirty(node_id)` — sets one bit in the dirty bitmap. O(1), idempotent. Complemented by `clear_dirty()`, `set_all_dirty()`, `is_dirty()`, `dirty_count()`.
- **SceneState targeted update methods:** `update_clock` (updates clock text, 0 allocations), `update_cursor` (updates cursor position/blink, 0 allocations), `update_document_content` (rebuilds text runs and selection after edits), `update_selection` (updates selection overlay). Each method uses `acquire_copy()` → modify specific nodes → `mark_dirty()` → `publish()`.
- **Data buffer compaction:** When the data buffer is full, the scene triggers a full rebuild to compact data references. The incremental pipeline design is implemented in the scene library and core service.

**No restrictions imposed.** Pure `no_std` library with no syscalls, no allocations. Callers provide the buffer. ~1584 lines, host-side tests in `system/test/`. The scene library is purely geometric — no content-aware code (no monospace assumptions, no line breaking, no character encoding knowledge). Content-aware layout helpers (`layout_mono_lines`, `byte_to_line_col`, `scroll_runs`) live in core where they belong.

### 1.8 Render Library (`libraries/render/`) 🟢

**Goal:** Render backend that transforms a scene graph into pixels. Owns the tree walk, rasterization, compositing, damage tracking, glyph caching, and all pixel-level work. The compositor delegates all rendering to this library via the `RenderBackend` trait.

**Status:** ~2,194 lines across 6 files (lib.rs, scene_render.rs, compositing.rs, surface_pool.rs, damage.rs, cursor.rs). Extracted from the compositor in Phase 1 of the rendering architecture redesign (2026-03-16).

**What's foundational:**

- **`RenderBackend` trait.** `fn render(&mut self, scene, target)` + `fn dirty_rects()`. One call — the backend owns tree walk, transform/clip stack, glyph cache, rasterization, compositing, and damage tracking. The compositor becomes a thin event loop that calls this.
- **`CpuBackend` implementation.** Takes font data at construction, builds internal glyph caches, handles all content types (`FillRect`, `Glyphs`, `Image`). Encapsulates rendering state: glyph caches, damage tracker, surface pool, per-node previous-frame bounds (PREV_BOUNDS).
- **Content-type rendering.** `FillRect` → solid/blended rectangle fill. `Glyphs` → glyph cache lookup + coverage drawing. `Image` → bilinear resampling blit. No content-type dispatch above this layer.
- **Damage tracking.** Change-list-driven + PREV_BOUNDS for old-position damage. Supports incremental rendering (only repaint dirty rects) and full repaints.
- **Compositing.** Group opacity via offscreen buffers (SurfacePool), shadows, transforms, clip-skip optimization, rounded corner clipping.
- **Procedural cursor.** Arrow cursor rendered at top z-order.
- **Font handling boundary.** The render backend owns glyph rasterization and caching. Core owns text shaping (harfrust) and metrics. The compositor has zero font knowledge — it passes font data to `CpuBackend::new()` and never touches it again.

**What's scaffolding:**

- **Single-threaded tree walk.** Multi-core rasterization (horizontal strip parallelism) is an internal optimization — no interface changes needed.

**No restrictions imposed.** Pure `no_std` library with `alloc` dependency. Depends on drawing, scene, fonts, protocol. Host-side tests in `system/test/tests/render_scene_render.rs`.

---

### 1.9 Filesystem Library (`libraries/fs/`) 🟢

**Goal:** COW filesystem implementation. `no_std` port of the host prototype (`prototype/files/`). Provides the full stack from raw blocks to the `Files` trait.

**Status:** `BlockDevice` trait, superblock ring (16-slot with CRC32), sorted free-extent allocator with coalescing, inodes (16 KiB blocks, inline data, extent lists with birth_txg), COW write path with two-flush commit protocol, per-file and multi-file snapshots, `Files` trait with `FileId`/`SnapshotId` newtypes.

**What's foundational:**

- **`BlockDevice` trait.** Abstract block I/O — implemented by `VirtioBlockDevice` (bare-metal) and `FileBlockDevice`/`MemoryBlockDevice` (host tests).
- **Pure COW crash consistency.** Two-flush commit protocol: write all data blocks, flush, write superblock, flush. No journal needed.
- **Flat namespace.** `FileId` → inode block. No directories.
- **`Files` trait.** Object-safe (`dyn Files`) with explicit `commit()`. Full lifecycle: create, read, write, delete, snapshot, restore.

**What's scaffolding:**

- **`BTreeMap` for inode table and allocator.** `HashMap` from the host prototype replaced with `BTreeMap` for `no_std` compatibility.
- **COW-entire-file** for extent-based writes. O(file_size) per write. Acceptable for document workloads. Per-block COW is a future optimization.

**No restrictions imposed.** `no_std` library with `alloc` dependency. No syscalls, no I/O — callers provide a `BlockDevice` implementation.

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

- **Service pack.** Service ELFs packed into a flat archive linked into the kernel as a `.services` section. Init reads from a memory-mapped region at `SERVICE_PACK_BASE`. Changing one service requires only repacking + relinking. Real OS loads from a filesystem.
- **Hardcoded framebuffer dimensions** (1024×768). Should come from GPU driver, not init.
- **Linear orchestration.** Spawn render service, run 10-phase handshake, start remaining processes. No dynamic process management, no error recovery.
- **Device manifest format.** Ad hoc packed struct (u32 count + 8-byte-aligned entries with device_id, mmio_pa, mmio_size, irq). No versioning, no extensibility. Works for 2 devices.
- **Channel shared memory layout.** Raw bytes at magic offsets. Init writes `fb_va` at offset 0, `fb_width` at offset 8, etc. Each process pair has its own undocumented layout. No schema, no validation.

**Key change (2026-03-11, updated 2026-03-18):** Init no longer exits. After probing GPU capabilities and spawning the appropriate render service, it runs `setup_render_pipeline()` — a unified 10-phase handshake that sets up shared memory, spawns core + editor + input drivers, creates all cross-process channels, and starts processes. All render services follow the same handshake. Init then idles via `yield_now()`.

**Key constraint:** Init is still not a real OS service. It sets up the process topology but doesn't mediate runtime communication. The real OS service (renderer + metadata DB + input router + compositor) doesn't exist — init is a stand-in.

---

### 2.2 Presenter (`services/presenter/`) 🟡

**Goal:** The OS service: sole writer to document state, scene graph builder, input router.

**Status:** ~1750 lines across 4 files (main.rs, scene_state.rs, typography.rs, fallback.rs). Builds a scene graph describing the visual structure of the document, routes input to the active editor, applies editor write requests to document state.

**What's foundational (the approach):**

- **Sole writer to document state.** Core owns the text buffer — the editor never touches it. Write requests (MSG_WRITE_INSERT, MSG_WRITE_DELETE) arrive via IPC and are applied sequentially. This is Decision #9: "editors are read-only consumers, OS service is sole writer."
- **Scene graph output.** Core compiles document structure into a scene graph (typed visual node tree) and publishes it via double-buffered shared memory. The compositor reads the scene graph and renders it — separation of document semantics from pixels.
- **Input routing.** Keyboard/mouse events from the input driver are forwarded to the editor via core's IPC channels.
- **Typography and layout.** Monospace text layout with line-breaking, cursor positioning, selection rendering. Layout helpers (`layout_mono_lines`, `byte_to_line_col`, `scroll_runs`) live here in `scene_state.rs` — they encode monospace content knowledge that doesn't belong in the scene library.
- **Font handling boundary.** Core owns text shaping (harfrust) and font metrics. The render backend (in `libraries/render/`) owns glyph rasterization and caching. Core produces `Glyphs` scene nodes (glyph IDs + positions); the render backend turns them into pixels.

**Event-driven boot sequence:**

Core's boot phase is fully event-driven: it publishes a loading scene (Tabler loader-2 spinner, CPU-rasterized as `Content::InlineImage`) to shared memory immediately, giving the user a visible frame within milliseconds. It then enters a multiplexed wait loop: animation timer ticks rotate the spinner while async init replies (font metrics from 9p, document queries/reads from the store service, PNG decode from the decoder service, undo snapshot) arrive as IPC messages. Each reply advances a state machine (`BootState` with `DecodePhase` and `DocPhase` sub-states). The loop exits when all init completes (`all_ready()`) or a 5-second timeout fires. Core then builds the full document scene, replacing the loading scene in the triple buffer.

This pattern — show UI immediately, init async, transition on completion — avoids blocking the display pipeline during boot. The spinner uses `Content::InlineImage` (CPU-rasterized each frame) rather than `Content::Path` because it rotates each frame and path re-rasterization would be wasteful. Title bar icons use `Content::Path` with fill+stroke support (separate fill color and stroke color per path node).

**Incremental scene updates (2026-03-15 rendering pipeline optimization):**

- **Targeted update dispatch.** The event loop classifies each event and dispatches to the narrowest possible update method: timer ticks → `update_clock` (clock text only), cursor blink → `update_cursor` (cursor node only), keypresses/edits → `update_document_content` (text runs + selection), selection changes → `update_selection`. Each method uses copy-forward (copy front buffer to back), mutates only affected nodes, marks them changed, and swaps.
- **Zero-allocation clock and cursor updates.** `update_clock` and `update_cursor` modify existing nodes in-place without touching the data buffer or allocating. These fire at high frequency (250 Hz timer, cursor blink) and must be cheap.

**What's scaffolding (the implementation):**

- **Static text buffer in BSS.** Real OS service reads document content from a Files-backed memory mapping.
- **No operation boundary detection.** Every write is applied immediately without snapshot/undo tracking.
- **Single font path.** Uses font library for shaping; render backend handles rasterization via glyph cache.

---

### 2.2b–c CPU Render / Virgil Render — REMOVED (2026-03-30)

cpu-render (software via virtio-gpu 2D) and virgil-render (Gallium3D via virglrenderer) have been removed. metal-render is the sole render backend. See `design/journal.md` "Render Driver Consolidation" for rationale.

---

### 2.4 Virtio Block Driver (`services/drivers/virtio-blk/`) 🟢

**Goal:** Full block device I/O over virtio-blk transport.

**Status:** `BlkDevice` struct with `read_block`, `write_block`, and `flush` methods. Negotiates `VIRTIO_BLK_F_FLUSH` feature for persistent writes. Self-test on init (write → read-back → verify cycle). Interrupt-driven.

**What's foundational:**

- 3-descriptor chain pattern (header → data → status). Correct virtio-blk protocol.
- Same interrupt-driven pattern as GPU driver (wait → ack).
- `BlkDevice` struct encapsulates device state (MMIO base, virtqueue, DMA buffers).
- Feature negotiation: `VIRTIO_BLK_F_FLUSH` enables cache flush commands for crash consistency.
- Self-test validates read/write correctness at driver startup.

**Note:** Init now spawns the filesystem service (not the standalone blk driver) for `device_id=2`. The standalone driver remains for direct block I/O testing.

---

### 2.4b Filesystem Service (`services/filesystem/`) 🟢

**Goal:** COW filesystem over virtio-blk, persists document edits to disk.

**Status:** Owns the virtio-blk device. Formats the disk on first boot, mounts the filesystem, and runs an IPC commit loop with core. Uses the `fs` library (`system/libraries/fs/`) — a `no_std` port of the host prototype.

**What's foundational:**

- **`VirtioBlockDevice`** implements the `BlockDevice` trait over virtio transport. Uses `RefCell` for interior mutability (`read_block` takes `&self` but virtio I/O mutates internal queue state).
- **IPC commit loop:** Receives `MSG_FS_COMMIT` from core, reads the doc buffer from shared memory (read-only mapping), writes content to the filesystem via the `Files` trait, and commits (two-flush protocol for crash consistency).
- **Init orchestration (Phase 10):** Init creates the filesystem process, shares the doc buffer read-only via `memory_share` (requires unstarted process), creates a core↔filesystem channel, then starts it.

**What's scaffolding:**

- Single-document persistence. Only one FileId managed.
- Formats on every boot (no mount-on-reboot yet).

**What's missing:**

- **Mount-on-reboot:** Read back persisted content on subsequent boots.
- **Undo/redo:** Snapshot infrastructure exists in the fs library but is not wired to core.
- **Multi-document support:** Requires FileId management in core.

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

**Status:** ~450 lines. Implements 6 of ~30 9P2000.L operations (Tversion, Tattach, Twalk, Tlopen, Tread, Tclunk). Reads files from a shared host directory (`system/share/`) via virtio transport. Currently used to load three fonts (JetBrains Mono, Inter, Source Serif 4) and a PNG image at boot.

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

**Goal:** Content-type-specific input-to-write translator. Editors handle character insertion, deletion, and content-specific operations. Navigation and selection live in core (the OS service), which owns layout and provides content-type interaction primitives (cursor, selection, playhead). This is Decision #8 in action.

**Status:** ~195 lines. Receives MSG_KEY_EVENT from core via IPC. Has read-only shared memory mapping of the document buffer. Handles: character insert, backspace, forward delete, Tab (4 spaces), Shift+Tab (dedent). Sends write requests (MSG_WRITE_INSERT, MSG_WRITE_DELETE) back to core. No navigation, no selection, no modifier tracking.

**What's foundational (the pattern):**

- **Editor as read-only consumer.** The editor has a hardware-enforced read-only mapping of the document buffer. It reads content for context-aware editing. All writes go through IPC. This is Decision #9 in action.
- **Thin editor, smart core.** Navigation (arrows, Cmd+Left/Right, word boundaries, Home/End, PgUp/PgDn), selection (Shift+navigation, Cmd+A), selection-aware deletion (Opt+Backspace/Delete), and mouse click handling (double-click word select, triple-click line select) all live in core. The editor only translates content-type-specific keypresses into write operations. This split means adding a new editor for a different content type only requires writing the content-specific translation logic — navigation and selection come for free from core.
- **IPC write protocol.** MSG_WRITE_INSERT carries position + byte, MSG_WRITE_DELETE carries position, MSG_WRITE_DELETE_RANGE carries a byte range, MSG_CURSOR_MOVE carries cursor position, MSG_SELECTION_UPDATE carries selection state. All typed, all fit in 60-byte ring buffer payload.

**What's scaffolding (the implementation):**

- **Single-byte inserts only.** No Unicode, no multi-byte operations.
- **ASCII-only.** No Unicode text handling.

**What's missing:**

- **Operation boundary hints.** `beginOperation`/`endOperation` messages for better undo granularity.
- **Richer edit operations.** Find/replace.

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

### 3.2 ~~No Filesystem~~ ✅ Resolved

**Resolved:** Document service (`services/document/`) replaces the filesystem service. Two-library architecture: `fs` library (generic COW filesystem with `BlockDevice` trait, superblock ring, free-extent allocator, inodes, snapshots, `Files` trait) + `store` library (metadata layer with catalog, media types, queryable attributes, wraps `Box<dyn Files>`). Factory disk image builder (`tools/mkdisk/`) pre-populates fonts at build time. Boot loads fonts from native filesystem. Multi-document persistence (text + image). Undo/redo via COW snapshots: `UndoState` in core (64-entry ring), Cmd+Z/Cmd+Shift+Z handlers, synchronous restore + content reload. Protocol: `protocol::document` (13 message types including snapshot/restore).

---

### 3.3 ~~No Input~~ ✅ Resolved

**Resolved:** virtio-input keyboard driver (`services/drivers/virtio-input/`). Same interrupt-driven pattern as virtio-blk/gpu: post device-writable event buffers on the event virtqueue, wait for IRQ, read 8-byte Linux evdev events, translate keycodes to ASCII, forward to compositor via IPC ring buffer.

**IPC topology:** Init creates cross-process channels (input driver → core, core → render service, core ↔ editor). Input driver sends `MSG_KEY_EVENT` messages to core; core routes to editor and rebuilds the scene graph; render service reads the scene graph and presents.

**QEMU note:** `-device virtio-keyboard-device` added to run-qemu.sh. QEMU's QMP `input-send-event` does NOT route to virtio-keyboard (QEMU limitation) — must type into the display window directly.

---

### 3.4 ~~No Event Loop / Display Loop~~ ✅ Resolved

**Resolved:** Multiple processes run continuous event loops:

1. **Input driver** — `wait(IRQ)` → read event → send to core → repost buffer → loop
2. **Core** — `wait(input_channel | editor_channel)` → route events → update document → rebuild scene graph → signal render service → loop
3. **Editor** — `wait(core_channel)` → receive input → compute write requests → send to core → loop
4. **Render service** (metal-render) — `wait(scene_update_channel | frame_timer)` → read scene graph → render → present → loop

Init probes GPU, calls `setup_render_pipeline()`, starts all processes, then idles via `yield_now()`.

**Cross-process IPC channels:** Init creates direct channels between processes (input→core, core→render service, core↔editor) in addition to its own config channels. Each child gets handle 0 = init channel, handle 1+ = cross-process channels.

**Known limitation:** The kernel's `wait` syscall doesn't implement finite timeouts — only poll (`timeout=0`) or infinite block. Timer handles (`timer_create`) can be mixed into `wait` calls as a workaround.

---

### 3.5 ~~No Structured IPC Messages~~ ✅ Resolved

**Resolved:** Ring buffer IPC with fixed 64-byte messages. See §1.5 for the `ipc` library design. Implemented at `libraries/ipc/` (~297 lines). All services migrated to ring buffer messages.

**Decisions made (2026-03-10):**

- **One mechanism** — ring buffers for everything, including configuration (first message pattern, no separate config struct path). Prior art: Singularity contracts (config = opening protocol sequence).
- **Separate pages per direction** — each channel allocates two 16 KiB pages, one per direction. Each page is a textbook SPSC queue. Prior art: io_uring (separate SQ/CQ regions), LMAX Disruptor.
- **Fixed 64-byte messages** — one AArch64 cache line. 4-byte type tag + 60-byte payload. 254 slots per ring (16 KiB page − 128-byte header). Prior art: io_uring (64-byte SQE).
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
  ├── fonts (TrueTypeFont — shaping + metrics only)
  ├── scene (SceneWriter, node types)
  ├── ipc (Channel, Message — ring buffer messaging)
  └── protocol (core_config, edit, input — message types + payload structs)

Compositor
  ├── sys (wait, channel_signal, exit)
  ├── render (CpuBackend, RenderBackend — all pixel work)
  ├── scene (DoubleReader — reads scene graph from shared memory)
  ├── ipc (Channel, Message — ring buffer messaging)
  └── protocol (compose, present — message types + payload structs)

Render Library
  ├── drawing (Surface, Color, blit_blend, fill_rect, draw_coverage)
  ├── fonts (TrueTypeFont, glyph cache — rasterization)
  ├── scene (SceneReader, node types, Content variants)
  └── protocol (DirtyRect, CompositorConfig)

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

Render Library
  ├── drawing (pixel operations)
  ├── fonts (glyph rasterization + caching)
  ├── scene (node types, Content variants)
  └── protocol (DirtyRect)

Scene Library
  └── (none — pure, no dependencies. Uses core::sync::atomic for double-buffering)

IPC Library
  └── (none — pure, no dependencies. Uses core::sync::atomic only)

Sys Library
  └── (none — inline asm only)

Virtio Library
  └── (none — pure, no dependencies)
```

**Observation:** Libraries form a clean DAG: `render` depends on `drawing` + `fonts` + `scene` + `protocol`; `fonts` depends on `drawing`; all others are independent leaves. The `protocol` library is the single source of truth for all IPC message types and payload structs. User programs (text-editor) depend on `sys` + `ipc` + `protocol` — they don't touch `drawing`, `fonts`, `render`, or `virtio`. This is architecturally correct: editors don't render, the OS service does. Core (OS service) owns document semantics, text shaping, and scene graph construction. The compositor is a content-agnostic pixel pump that delegates all rendering to the render library. The render library owns the tree walk, glyph rasterization/caching, compositing, and damage tracking. Font handling has a clean boundary: core for shaping + metrics, render backend for rasterization + caching.

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

1. ~~**Font rasterization**~~ — **Done.** TrueType rasterizer in the font library (`libraries/fonts/`). read-fonts + HarfRust for shaping, analytic area coverage rasterizer, outline dilation (stem darkening), variable font support, glyph cache. Running on bare metal: core for shaping + metrics, render backend for rasterization + caching.
2. ~~**Syscall error types**~~ — **Done.** `SyscallError` enum (13 variants) + `SyscallResult<T>` on all 25 syscalls + `print()` convenience. All 6 userspace binaries migrated. Eliminated raw `i64` returns and ad-hoc `< 0` checks.
3. ~~**Userspace memory allocation**~~ (§3.1) — **Done.** `memory_alloc`/`memory_free` (#25/#26) + `GlobalAlloc` in `sys` library. `Vec`/`String`/`Box` available to all userspace programs.
4. ~~**Structured IPC**~~ (§3.5) — **Done.** Ring buffer library implemented (`libraries/ipc/`). Kernel allocates two pages per channel. All services migrated to ring buffer messages.
5. ~~**Input driver**~~ (§3.3) — **Done.** virtio-input keyboard driver with IPC forwarding to compositor. Cross-process channels for direct driver↔compositor communication.
6. ~~**Event loop**~~ (§3.4) — **Done.** Compositor, GPU driver, and input driver all run continuous event loops. Init stays alive.
7. ~~**Editor process separation**~~ — **Done.** Text editor process (`user/text-editor/`) receives input events from core, sends write requests back. Core is sole writer to document state. Demonstrates Decision #9 (editors as read-only consumers). Four processes in the display pipeline: render service, input driver, text editor, core.
8. ~~**Read-only document mapping**~~ — **Done.** Text editor has a hardware-enforced read-only shared memory mapping of the document buffer. Reads content for cursor positioning and context-aware editing. All writes go through IPC to core (sole writer).
9. **Text layout** — connective tissue between fonts, drawing, and the rendering pipeline. This is an _interface_ question (gets the design treatment), not just an implementation. How does text flow? How does the editor specify what to render? Must be simple to reason about. Currently monospace-only layout helpers live in core's `scene_state.rs`.
10. ~~**Filesystem service**~~ (§3.2) — **Done (v0.4).** Document service (`services/document/`) replaces filesystem service. Two-library architecture: `fs` library (generic COW filesystem) + `store` library (metadata layer with catalog, media types, queryable attributes, wraps `Box<dyn Files>`). Factory disk image builder (`tools/mkdisk/`) pre-populates fonts. Boot loads fonts from native filesystem (no 9p dependency). Multi-document persistence. Undo/redo via COW snapshots (64-entry ring, Cmd+Z/Cmd+Shift+Z). Protocol: `protocol::document` (13 message types).
11. ~~**Wait timeout**~~ — **Done.** For finite timeouts (0 < timeout < u64::MAX), `sys_wait` creates an internal timer, adds it to the wait set with a sentinel index. If the timer fires first, returns `WouldBlock`. Timer cleanup: immediate on non-blocked paths; deferred to next `wait` call for the blocked→woken path (stored on thread struct).
