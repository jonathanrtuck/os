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
┌────────────────────────────────────────────────┐
│  User Programs         (echo, future editors)  │  🔴 demos
├────────────────────────────────────────────────┤
│  Platform Services                             │
│  ┌──────────┐  ┌────────────┐  ┌────────────┐  │
│  │   Init   │  │ Compositor │  │  Drivers   │  │  🟡/🟢 mixed
│  │  (proto  │  │   (toy,    │  │ (virtio-   │  │
│  │   OS     │  │   real     │  │  blk/gpu/  │  │
│  │ service) │  │ compositing│  │  console)  │  │
│  └──────────┘  └────────────┘  └────────────┘  │
├────────────────────────────────────────────────┤
│  Libraries                                     │
│  ┌─────┐  ┌────────┐  ┌─────────┐  ┌────────┐  │  🟢 foundational
│  │ sys │  │ virtio │  │ drawing │  │link.ld │  │
│  └─────┘  └────────┘  └─────────┘  └────────┘  │
├────────────────────────────────────────────────┤
│  Kernel (25 syscalls, see kernel/DESIGN.md)    │  🟢 production
└────────────────────────────────────────────────┘
```

**Process model:** Kernel spawns only init. Init embeds all other ELF binaries and spawns everything else. Microkernel pattern (Fuchsia component_manager, seL4 root task). This pattern is foundational; init's implementation is scaffolding.

**IPC:** Kernel creates channels (shared memory page + signal). Processes communicate by reading/writing raw bytes at agreed offsets. No structured message format yet. The mechanism (channels, shared memory, wait) is foundational; the ad hoc byte layouts are scaffolding.

**Memory model for userspace:** Stack (16 KiB) + static BSS + DMA buffers + shared memory from init. No heap allocator. No `mmap` equivalent. This is the most significant constraint on what can be built above the kernel today.

---

## 1. Libraries

### 1.1 Syscall Library (`library/sys/`) 🟢

**Goal:** Safe Rust wrappers for all kernel syscalls.

**Status:** ~380 lines, covers all 25 syscalls with typed errors. Every userspace binary links against this.

**What's foundational:**

- The syscall ABI (x0–x5 args, x8 number, `svc #0`). Standard, stable.
- The function signatures mirror the kernel's syscall interface 1:1.
- `SyscallError` enum — unified error type covering both `syscall::Error` and `handle::HandleError` from the kernel (13 variants, matching kernel's `repr(i64)` codes). Defensive: unknown codes map to `UnknownSyscall`.
- `SyscallResult<T>` — typed returns on all fallible syscalls. Return types encode meaning: `process_create → SyscallResult<u8>` (handle), `channel_create → SyscallResult<(u8, u8)>` (pair of handles), `dma_alloc → SyscallResult<usize>` (VA).
- `print()` — fire-and-forget console output (wraps `write`, discards Result). Mirrors Rust's `print!` vs `write!` pattern.
- Panic handler that calls `exit()` — correct behavior for userspace panics.

**What's missing:**

- **Higher-level wrappers.** Raw syscalls are the right foundation, but common patterns (create channel + send handle + start process) should be composable.

---

### 1.2 Virtio Library (`library/virtio/`) 🟢

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

### 1.3 Drawing Library (`library/drawing/`) 🟢

**Goal:** Pure drawing primitives for pixel buffers. No allocations, no syscalls, no hardware — fully testable on the host.

**Status:** ~1600 lines (lib.rs + truetype.rs + rasterizer.rs + font_data.rs), 83 tests. Surface abstraction, color with alpha, blending, blitting, bitmap font, TrueType font rasterizer.

**What's foundational:**

- `Surface<'a>` borrows `&mut [u8]` — no allocation policy. Works with any memory source (DMA, BSS, stack, shared).
- `Color` in canonical RGBA, encode/decode at the pixel boundary. Format-agnostic above the pixel level.
- Porter-Duff source-over blending — correct, integer-only, with fast paths.
- `blit_blend` — the core compositing operation (per-pixel alpha, clips to bounds).
- `TrueTypeFont` — zero-copy parser for TTF files. Parses 7 required tables (head, maxp, cmap format 4, hhea, hmtx, loca, glyf). Extracts glyph outlines (quadratic bezier contours), maps codepoints via cmap, reads horizontal metrics.
- Scanline rasterizer — flattens quadratic beziers via De Casteljau subdivision, sweeps with non-zero winding rule, 4× vertical oversampling for anti-aliasing. Integer/fixed-point math only. Produces coverage maps (0–255 per pixel) that feed into the existing alpha blending pipeline.
- `draw_coverage` — composites a coverage map onto a surface with color modulation. The bridge between rasterizer output and the compositing pipeline.
- All operations clip silently (no panics). Safe to call with any coordinates.

**What's scaffolding:**

- `PixelFormat` enum has only `Bgra8888`. Trivial to extend (add variant + match arms), but currently untested with other formats.
- `BitmapFont` is an embedded 8×16 VGA font covering ASCII 0x20–0x7E. Fine as a fallback, not a real text solution.
- ProggyClean.ttf (40 KiB, MIT license) is the embedded TrueType font. A vectorized bitmap font — exercises the full parser and rasterizer but glyphs are mostly straight lines. A font with real curves (variable-width, bezier-heavy) would test the rasterizer more thoroughly.

**What's missing:**

- **Text layout.** `draw_text` is left-to-right fixed-pitch ASCII. No variable-width glyphs, no word wrap, no line breaking, no Unicode. The TrueType rasterizer returns per-glyph advance widths, enabling variable-pitch rendering, but there's no layout engine to position glyphs properly.
- **Anti-aliased line/shape drawing.** Lines and rectangles are pixel-exact with no smoothing. The rasterizer's coverage map approach could be extended to arbitrary shapes.
- **Compound glyph support.** TrueType compound glyphs (accented characters built from components) not yet handled. Only simple glyphs (positive contour count) are parsed.
- **Glyph caching.** Each `rasterize()` call re-rasterizes from outlines. A fixed-size LRU cache (codepoint + size → pre-rasterized bitmap) would improve performance for repeated text.

**No restrictions imposed.** Pure library — anything built on top can use it or replace it.

---

### 1.4 Linker Script (`library/link.ld`) 🟢

**Goal:** Shared ELF layout for all userspace binaries.

**Status:** 16 lines. Base VA 0x400000, page-aligned sections.

**Foundational.** Standard layout, matches kernel's ELF loader expectations. No changes needed unless the VA layout changes.

---

## 2. Platform Services

### 2.1 Init / Proto-OS-Service (`platform/init/`) 🟡

**Goal:** Bootstrap userspace. The only process the kernel spawns directly.

**Status:** 326 lines + build.rs. Reads device manifest, spawns drivers, orchestrates display pipeline.

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

**Key constraint:** Init does everything synchronously and then exits. There's no long-running OS service process yet. The real OS service (renderer + metadata DB + input router + compositor) doesn't exist — init is a stand-in.

---

### 2.2 Compositor (`platform/compositor/`) 🟡

**Goal:** Composite multiple surfaces into a final framebuffer.

**Status:** 254 lines. Draws three demo panels into separate BSS buffers, composites them with alpha blending in z-order.

**What's foundational (the approach):**

- **Separate surface buffers, composited back-to-front.** This is how a real compositor works. Each surface has its own pixel data; the compositor blits them in z-order with per-pixel alpha.
- **Surface tree → pixel buffer.** The compositor is a pure function of its inputs (surface buffers + positions + z-order) → output (framebuffer). Matches the design thread's model.
- **Alpha blending demonstrates the compositing model.** Semi-transparent backgrounds + opaque content pixels. The blending math is reusable.

**What's scaffolding (the implementation):**

- **Static BSS buffers.** Three 400×260 panels allocated at compile time. Real compositor receives surfaces from editor processes via shared memory — surface count and dimensions are dynamic.
- **Hardcoded scene.** Panel content, positions, z-order all baked in. Real compositor reads from a surface tree (layout engine output).
- **Fire-once.** Draws one frame and exits. No damage tracking, no incremental updates, no display loop.
- **No input.** Can't respond to mouse/keyboard events.

**What's missing:**

- **Dynamic surface management.** Register/unregister surfaces, resize, reorder.
- **Damage tracking.** Only redraw pixels that changed. The design thread identified this as "React-style reconciliation" — diff the surface tree, commit minimal updates.
- **Display loop.** Continuous rendering, vsync, buffer swapping.
- **Connection to layout engine.** Layout produces the surface tree; compositor renders it. That interface doesn't exist.

---

### 2.3 Virtio GPU Driver (`platform/drivers/virtio-gpu/`) 🟢

**Goal:** Present pixel buffers to the QEMU virtual display.

**Status:** 603 lines. All six core virtio-gpu 2D commands. Interrupt-driven.

**What's foundational:**

- Complete 2D command implementation (create resource, attach backing, set scanout, transfer, flush, get display info).
- Interrupt-driven I/O (register IRQ → wait → ack). Correct async pattern.
- Page-aligned MMIO mapping with sub-page offset handling.
- Reuses virtio library for transport + virtqueue.

**What's scaffolding:**

- **Fire-once.** Presents one framebuffer and exits. Real GPU driver is long-running — continuously presents new frames.
- **No surface trait.** The design thread identified `create_surface`/`present` as the right GPU abstraction, but the driver uses raw virtio-gpu commands. There's no abstract display interface that the compositor could program against.
- **Framebuffer owned by init.** Init allocates the DMA buffer and shares it. The driver just attaches it as backing. Real ownership: compositor or GPU driver owns display buffers.

**QEMU-specific but architecturally sound.** On real hardware, a different driver would implement the same abstract display interface. The virtio-gpu driver is a leaf node — replacing it doesn't affect anything above.

---

### 2.4 Virtio Block Driver (`platform/drivers/virtio-blk/`) 🟢

**Goal:** Read sectors from a virtual block device.

**Status:** 211 lines. Reads sector 0, prints first 16 bytes. Interrupt-driven.

**What's foundational:**

- 3-descriptor chain pattern (header → data → status). Correct virtio-blk protocol.
- Same interrupt-driven pattern as GPU driver (wait → ack).

**What's scaffolding:**

- Reads one sector and exits. No block device abstraction (read/write at arbitrary LBAs).
- No filesystem uses it yet.

---

### 2.5 Virtio Console Driver (`platform/drivers/virtio-console/`) 🟡

**Status:** 112 lines. TX-only, writes one test string. Not exercised (no QEMU device configured).

Minimal and not yet useful. Would need RX queue, proper character device interface.

---

### 2.6 Echo (`user/echo/`) 🔴

**Status:** 34 lines. IPC ping-pong demo. Not integrated into current boot sequence.

Pure throwaway. Demonstrates channel IPC works, nothing more.

---

## 3. Constraints & Gaps

These are the things that limit what can be built above the kernel today, ordered by how much they constrain.

### 3.1 No Userspace Heap Allocator ⚠️ Critical

**The problem:** Userspace has no dynamic allocation. No `Vec`, `String`, `Box`. Memory sources are stack (16 KiB), static BSS (sized at compile time), DMA (for devices), and shared memory (provided by init). There's no `mmap`/`brk` syscall to request more pages.

**Why it matters:** Any non-trivial data structure needs dynamic memory. A font rasterizer needs variable-size glyph outlines and a glyph cache. A real compositor needs dynamic surface lists. A filesystem service needs buffers.

**Options:**

1. **Bump allocator over static BSS.** Embedded-systems approach — pre-size a large `static mut [u8; N]`, hand out slices from it. No free, no reuse. Works for demos, not sustainable.
2. **`memory_alloc(pages)` syscall.** Kernel maps anonymous zero-pages into the process at a bump-allocated VA. Pairs with `memory_free`. More foundational — unlocks a proper `GlobalAlloc` implementation in userspace. Moderate kernel work (new syscall + VMA management for anonymous mappings).
3. **Arena per task.** Allocate a region at process creation, userspace manages it internally. Middle ground.

**Recommendation:** Option 2 is the right long-term investment. Option 1 is acceptable as a stopgap for a specific spike (e.g., TTF rasterizer).

---

### 3.2 No Filesystem

**The problem:** All binaries and data are embedded in init via `include_bytes!`. No way to load resources at runtime. Changing anything requires a full rebuild.

**Why it matters:** Font files, configuration, documents — everything the OS is designed around — can't be loaded. This doesn't block library-level work (embed data as `include_bytes!`), but blocks any system-level feature that involves opening files.

**Blocked on:** Decision #16 (COW filesystem on-disk design). Filesystem is a userspace service; kernel provides COW/VM mechanics. On-disk format not yet designed.

---

### 3.3 No Input

**The problem:** No keyboard, mouse, or touch drivers. QEMU provides PS/2 or virtio-input, but nothing reads them. The display pipeline is output-only.

**Why it matters:** Can draw to the screen but can't respond to the user. Blocks interactive demos, editor prototypes, shell exploration (Decision #17).

**What it would take:** A virtio-input driver (same pattern as virtio-blk/gpu — MMIO + interrupt-driven) plus input event routing from init/OS service to the active process.

---

### 3.4 No Event Loop / Display Loop

**The problem:** Every process runs once and exits. Init orchestrates a linear boot sequence, compositor draws one frame, GPU driver presents once. No process runs continuously.

**Why it matters:** Interactive anything (text cursor blinking, scroll, window resize) requires a process that loops on events. The kernel's `wait` syscall supports this (multiplexed wait on channels + timers), but no userspace code uses the pattern.

**What it would take:** Any one process converted to an event loop. The compositor is the natural candidate — wait for input events or surface updates, recomposite, present, repeat.

---

### 3.5 No Structured IPC Messages

**The problem:** Processes communicate by reading/writing raw bytes at magic offsets in shared memory. Each process pair has its own undocumented layout. No serialization format, no versioning, no validation.

**Why it matters:** Adding a new field or changing a layout requires synchronized changes in both processes. The design settled on ring buffer IPC carrying control messages — the channels exist, but the message format doesn't.

**What it would take:** Define a minimal message envelope (type tag + length + payload). Could be as simple as a fixed header struct. Doesn't need to be a general serialization framework.

---

### 3.6 Single Pixel Format

**The problem:** Only BGRA8888. The `PixelFormat` enum exists for extensibility but only has one variant.

**Why it matters now:** Doesn't. virtio-gpu uses BGRA8888. Real hardware might need other formats, but the drawing library's encode/decode-at-the-boundary design handles this — add a variant, add match arms.

**Not a real constraint.** Listed for completeness.

---

## 4. Component Dependency Map

```text
User Programs
  └── sys (syscalls)

Compositor
  ├── sys (channel_signal, exit)
  └── drawing (Surface, Color, blit_blend, fonts)

Init
  └── sys (process_create, channel_create, handle_send, memory_share, dma_alloc, wait, ...)

Drivers (virtio-blk, virtio-gpu, virtio-console)
  ├── sys (device_map, interrupt_register, dma_alloc, wait, ...)
  └── virtio (MMIO transport, split virtqueue)

Drawing Library
  └── (none — pure, no dependencies)

Sys Library
  └── (none — inline asm only)

Virtio Library
  └── (none — pure, no dependencies)
```

**Observation:** The libraries are clean leaves with no dependencies. The platform services depend on libraries + syscalls. The coupling between platform services (init knows about compositor, GPU driver, etc.) is all scaffolding — a real OS service would mediate these relationships.

---

## 5. Known Pressure Points

Places where the clean abstraction will eventually face tension. Documented so the cost of opting in is understood before it's paid. None of these need to be solved now — the happy path works without them.

### 5.1 Drawing Library: Per-pixel blending is slow for large surfaces

`blit_blend` reads and writes every pixel individually. For full-screen compositing at 60fps (1280×800 = ~1M pixels × 4 bytes × 60 = 245 MB/s), this is tight on real hardware. The clean path (per-pixel blend_over) is correct but not fast.

**Escape hatch:** SIMD (NEON on aarch64) can blend 4 pixels at a time. GPU-accelerated compositing bypasses the CPU entirely. Both are leaf-node optimizations — the interface (`blit_blend`) stays the same.

### 5.2 Compositor: Full recomposite every frame

Currently redraws the entire framebuffer. With 3-4 surfaces this is fine. With complex documents (many nested content parts), damage tracking becomes necessary — only recomposite the changed region.

**Escape hatch:** Dirty rectangles or tile-based damage. The compositor's interface to the rest of the system doesn't change — surfaces still go in, pixels still come out. The optimization is internal.

### 5.3 Font rasterization: Glyph caching

Rasterizing a glyph from bezier curves every time it's drawn is expensive. A glyph cache (codepoint + size → pre-rasterized bitmap) is the standard solution. With no heap allocator, the cache must be pre-sized.

**Escape hatch:** Fixed-size LRU cache in static memory (works now), or dynamic cache once `memory_alloc` exists. Internal to the font module — callers don't know or care whether the glyph was cached.

### 5.4 GPU driver: Direct scanout vs copy

virtio-gpu copies the framebuffer from guest to host memory on every present — this is inherent to the virtual device, not our architecture. On real hardware, the display controller reads directly from the framebuffer via DMA scanout (zero-copy). Our surface-based abstraction handles both transparently — the driver decides how to present.

**Escape hatch:** None needed. The abstraction already accommodates this. Listed to document why virtio-gpu performance isn't representative of real hardware.

### 5.5 IPC: Zero-copy for large payloads

Ring buffer messages work for control messages (edit protocol, input events). Large data (images, document content) should never flow through the ring buffer — it should be memory-mapped. The current architecture already does this (documents are memory-mapped, ring buffers carry control only), but the boundary isn't enforced by any interface.

**Escape hatch:** If a message type needs large payloads, the answer is always "put it in shared memory, send a reference through the ring buffer." This should probably become an explicit design rule rather than a pressure point.

---

## 6. What's Next

Ordered by what unblocks the most, building the happy path first:

1. ~~**Font rasterization**~~ — **Done.** TrueType rasterizer in the drawing library. Zero-copy parser, scanline rasterizer with 4× oversampling, coverage map output. Simple interface: `TrueTypeFont::rasterize(codepoint, size, buffer, scratch) → GlyphMetrics`. 21 tests. Running on bare metal in the compositor.
2. ~~**Syscall error types**~~ — **Done.** `SyscallError` enum (13 variants) + `SyscallResult<T>` on all 25 syscalls + `print()` convenience. All 6 userspace binaries migrated. Eliminated raw `i64` returns and ad-hoc `< 0` checks.
3. **Userspace memory allocation** (§3.1) — `memory_alloc`/`memory_free` syscalls + userspace `GlobalAlloc`. Unlocks `Vec`, `String`, real Rust crates. Critical constraint.
4. **Structured IPC** (§3.5) — replace ad hoc byte offsets with typed messages. Unblocks adding new message types without cross-process breakage.
5. **Input driver** (§3.3) — unblocks interactive demos. virtio-input follows the same pattern as existing drivers.
6. **Event loop** (§3.4) — convert compositor or init to loop on `wait`. Unblocks continuous rendering.
7. **Text layout** — connective tissue between fonts, drawing, and the compositor. This is an _interface_ question (gets the design treatment), not just an implementation. How does text flow? How does the editor specify what to render? Must be simple to reason about.
8. **Filesystem service** (§3.2) — blocked on Decision #16. Unblocks runtime resource loading, documents, everything the OS is about.
