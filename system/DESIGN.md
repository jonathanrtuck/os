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
│  │  (proto  │  │  (event    │  │ (virtio-   │  │
│  │   OS     │  │   loop,    │  │  blk/gpu/  │  │
│  │ service) │  │ interactive│  │  input/    │  │
│  │          │  │   text)    │  │  console)  │  │
│  └──────────┘  └──────┬─────┘  └─────┬──────┘  │
│                 input→comp      comp→gpu       │
│                  (IPC)           (IPC)         │
├────────────────────────────────────────────────┤
│  Libraries                                     │
│  ┌─────┐ ┌────────┐ ┌─────────┐ ┌─────┐ ┌───┐  │  🟢 foundational
│  │ sys │ │ virtio │ │ drawing │ │ ipc │ │l.d│  │
│  └─────┘ └────────┘ └─────────┘ └─────┘ └───┘  │
├────────────────────────────────────────────────┤
│  Kernel (27 syscalls, see kernel/DESIGN.md)    │  🟢 production
└────────────────────────────────────────────────┘
```

**Process model:** Kernel spawns only init. Init embeds all other ELF binaries and spawns everything else. Microkernel pattern (Fuchsia component_manager, seL4 root task). This pattern is foundational; init's implementation is scaffolding.

**IPC:** Kernel creates channels (two shared memory pages per channel + signal). Each page is a SPSC ring buffer of 64-byte messages (one direction). The `ipc` library provides lock-free ring buffer mechanics; per-protocol crates define message types. Configuration uses the same mechanism (first message on the ring). The mechanism (channels, shared memory, wait, ring buffers) is foundational; current services still use ad hoc byte layouts (migration to ring buffers pending).

**Memory model for userspace:** Stack (16 KiB) + static BSS + DMA buffers + shared memory from init + demand-paged heap via `memory_alloc`/`memory_free` syscalls. Heap region: 16–256 MiB VA, 32 MiB physical budget per process. Userspace `GlobalAlloc` in `sys` library (linked-list first-fit with coalescing, grows via `memory_alloc`). Programs opt in with `extern crate alloc;` to get `Vec`/`String`/`Box`.

---

## 1. Libraries

### 1.1 Syscall Library (`libraries/sys/`) 🟢

**Goal:** Safe Rust wrappers for all kernel syscalls.

**Status:** ~710 lines, covers all 27 syscalls with typed errors + `GlobalAlloc` (linked-list first-fit with coalescing). Every userspace binary links against this. Programs opt in to heap allocation with `extern crate alloc;`.

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

### 1.4 Linker Script (`libraries/link.ld`) 🟢

**Goal:** Shared ELF layout for all userspace binaries.

**Status:** 16 lines. Base VA 0x400000, page-aligned sections.

**Foundational.** Standard layout, matches kernel's ELF loader expectations. No changes needed unless the VA layout changes.

---

### 1.5 IPC Library (`libraries/ipc/`) 🟢 — designed, not yet implemented

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

---

## 2. Platform Services

### 2.1 Init / Proto-OS-Service (`services/init/`) 🟡

**Goal:** Bootstrap userspace. The only process the kernel spawns directly.

**Status:** 326 lines (build.rs is at the system/ level). Reads device manifest, spawns drivers, orchestrates display pipeline.

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

**Key change (2026-03-11):** Init no longer exits. After setting up all processes and cross-process channels, it idles via `yield_now()`. It creates three kinds of channels: config (init↔child), input (input driver→compositor), and present (compositor→GPU driver). The display pipeline starts all three event-loop processes (GPU, input, compositor) and lets them run autonomously.

**Key constraint:** Init is still not a real OS service. It sets up the process topology but doesn't mediate runtime communication. The real OS service (renderer + metadata DB + input router + compositor) doesn't exist — init is a stand-in.

---

### 2.2 Compositor (`services/compositor/`) 🟡

**Goal:** Composite multiple surfaces into a final framebuffer. Respond to input events.

**Status:** ~260 lines. Interactive text demo with event loop. Receives keyboard events from the input driver via IPC, renders typed text to the framebuffer, signals the GPU driver to present.

**What's foundational (the approach):**

- **Event loop.** Waits on input channel, processes events, re-renders, signals GPU. This is the correct reactive pattern — event-driven, not polling.
- **Cross-process IPC.** Receives events via ring buffer channel from input driver. Sends present commands to GPU driver via another channel. Demonstrates the multi-process IPC topology.
- **Full framebuffer re-render.** Each frame is a pure function of text state → framebuffer. No retained state except the text buffer.

**What's scaffolding (the implementation):**

- **Combined OS service + editor role.** In the real OS, the compositor is part of the OS service (renderer only). Input would be routed to a separate editor process which modifies document state via the edit protocol. The compositor currently plays both roles.
- **Static text buffer in BSS.** Real compositor renders from document state in shared memory.
- **Full re-render every frame.** No damage tracking. Fine for a text demo, not for complex documents.
- **Bitmap font only.** TrueType rendering removed from compositor (still available in drawing library for future use).

**What's missing:**

- **Dynamic surface management.** Register/unregister surfaces, resize, reorder.
- **Damage tracking.** Only redraw pixels that changed.
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

### 2.6 Virtio Console Driver (`services/drivers/virtio-console/`) 🟡

**Status:** 112 lines. TX-only, writes one test string. Not exercised (no QEMU device configured).

Minimal and not yet useful. Would need RX queue, proper character device interface.

---

### 2.7 Echo (`user/echo/`) 🔴

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

### 3.5 ~~No Structured IPC Messages~~ ✅ Designed (implementation pending)

**Resolved (design):** Ring buffer IPC with fixed 64-byte messages. See §1.5 for the `ipc` library design. Implementation next.

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
User Programs
  └── sys (syscalls)

Compositor
  ├── sys (channel_signal, exit)
  └── drawing (Surface, Color, blit_blend, fonts)

Init
  └── sys (process_create, channel_create, handle_send, memory_share, dma_alloc, wait, ...)

Drivers (virtio-blk, virtio-gpu, virtio-input, virtio-console)
  ├── sys (device_map, interrupt_register, dma_alloc, wait, ...)
  ├── virtio (MMIO transport, split virtqueue)
  └── ipc (ring buffer messaging — for cross-process channels)

Drawing Library
  └── (none — pure, no dependencies)

IPC Library
  └── (none — pure, no dependencies. Uses core::sync::atomic only)

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

### 5.5 IPC: Messages that exceed 60 bytes

Ring buffer messages are fixed at 64 bytes (4-byte type + 60-byte payload). All current control message types fit comfortably (edit protocol, input events, device configuration — all <40 bytes). But some future messages may not: metadata query results with variable-length strings, error diagnostics, path references.

**Design rule (not an escape hatch):** Large data never flows through the ring buffer. It goes in shared memory; the ring carries a reference (VA + length). Documents are already memory-mapped this way. If a message type regularly needs >60 bytes, that's a signal it should use the shared-memory-reference pattern — not a signal to make messages bigger.

---

## 6. What's Next

Ordered by what unblocks the most, building the happy path first:

1. ~~**Font rasterization**~~ — **Done.** TrueType rasterizer in the drawing library. Zero-copy parser, scanline rasterizer with 4× oversampling, coverage map output. Simple interface: `TrueTypeFont::rasterize(codepoint, size, buffer, scratch) → GlyphMetrics`. 21 tests. Running on bare metal in the compositor.
2. ~~**Syscall error types**~~ — **Done.** `SyscallError` enum (13 variants) + `SyscallResult<T>` on all 25 syscalls + `print()` convenience. All 6 userspace binaries migrated. Eliminated raw `i64` returns and ad-hoc `< 0` checks.
3. ~~**Userspace memory allocation**~~ (§3.1) — **Done.** `memory_alloc`/`memory_free` (#25/#26) + `GlobalAlloc` in `sys` library. `Vec`/`String`/`Box` available to all userspace programs.
4. ~~**Structured IPC**~~ (§3.5) — **Done.** Ring buffer library implemented (`libraries/ipc/`). Kernel allocates two pages per channel. All services migrated to ring buffer messages.
5. ~~**Input driver**~~ (§3.3) — **Done.** virtio-input keyboard driver with IPC forwarding to compositor. Cross-process channels for direct driver↔compositor communication.
6. ~~**Event loop**~~ (§3.4) — **Done.** Compositor, GPU driver, and input driver all run continuous event loops. Init stays alive.
7. **Text layout** — connective tissue between fonts, drawing, and the compositor. This is an _interface_ question (gets the design treatment), not just an implementation. How does text flow? How does the editor specify what to render? Must be simple to reason about.
8. **Filesystem service** (§3.2) — blocked on Decision #16. Unblocks runtime resource loading, documents, everything the OS is about.
9. ~~**Wait timeout**~~ — **Done.** For finite timeouts (0 < timeout < u64::MAX), `sys_wait` creates an internal timer, adds it to the wait set with a sentinel index. If the timer fires first, returns `WouldBlock`. Timer cleanup: immediate on non-blocked paths; deferred to next `wait` call for the blocked→woken path (stored on thread struct).
