# Rendering Pipeline: Production-Grade Plan

Current state: the rendering pipeline works end-to-end (scene graph ->
compositor -> virtio-metal -> host GPU -> display) but has several architectural
gaps relative to production compositors. This plan addresses each gap in
dependency order.

Reviewed by three independent agents: Apple Silicon TBDR architecture, paravirt
GPU prior art, and codebase-grounded audit. Corrections from all three are
incorporated.

## Target Hardware

Apple Silicon M4 Pro. The GPU uses **Tile-Based Deferred Rendering (TBDR)** —
the screen is divided into small tiles (~32x32 pixels), geometry is binned per
tile, fragments are shaded per tile in fast on-chip SRAM, and the final result
is written to system memory once per tile. This has profound implications for
load actions, render pass structure, and overdraw:

- `MTLLoadAction.clear` is nearly free (tile SRAM starts zeroed, no memory read)
- `MTLLoadAction.load` is expensive (reads every tile from system memory)
- `MTLLoadAction.dontCare` is free (undefined tile contents, no memory read)
- Scissor rects cull entire tiles during rasterization, but vertex processing
  still runs for all submitted geometry
- Each render pass boundary forces a tile flush (store to memory + load for next
  pass)
- Hidden Surface Removal (HSR) discards occluded fragments before the fragment
  shader runs, but only for opaque geometry with depth testing

The compositor runs as a guest OS under a hypervisor. Metal commands are
serialized into a flat command buffer and sent over virtio. The host
deserializes and replays them via the Metal API. All TBDR costs are paid at the
host's Metal layer.

## Dependency Graph

```text
Phase 1: Vsync alignment ───────┐
                                ├──> Phase 3: Async frame submission
Phase 2: Damage tracking ───────┤
                                ├──> Phase 5: Vertex buffer caching
Phase 4: Atlas improvements ────┘
                                     Phase 6: Multi-threaded compositor
                                     Phase 7: GPU path rendering (future)
```

Phases 1, 2, 4 are independent of each other. Phases 3 and 5 benefit from
earlier phases but are not strictly blocked.

---

## Phase 1 — Vsync Alignment

**Goal:** The compositor renders in response to actual display vsync events, not
clock estimates.

**Current state:** Host-side infrastructure is done (uncommitted in hypervisor
repo): `CADisplayLink` replaces `DispatchSourceTimer`, `vsyncCounter` exposed at
config space offset `0x10`. Guest compositor ignores it — estimates vsync timing
from `clock_read() + frame_interval`.

**Problem:** Clock drift between guest estimate and host display causes missed
frames (compositor renders too late for the vsync it was targeting) or wasted
frames (renders too early, then idles).

**Context:** Standard virtio-gpu does not provide vblank interrupts at all.
Linux's virtio-gpu driver added a software vblank timer (hrtimer-based) in late
2025 to synthesize vsync from the guest side. Our custom virtio-metal device can
do better because we control both sides.

### Design

**Vsync counter polling with improved deadlines.** The current `recv_timed` loop
(IPC receive with timeout) is the correct structure — it naturally multiplexes
IPC messages with timer-driven rendering. The change: replace the estimated
`next_vsync` with a vsync-counter-derived deadline.

Each render loop iteration reads `device.config_read32(0x10)` (host vsync
counter). When the counter advances, the compositor knows a real vsync just
occurred. The next deadline is set to `now + frame_interval` anchored to the
observed vsync, not to an accumulating estimate.

**Why not a vsync interrupt?** The kernel ABI has separate wait primitives for
Events (`event_wait`) and Endpoints (`recv_timed`). There is no unified
multi-type wait. The compositor must wait on its IPC endpoint for service
requests (SETUP, UPLOAD_IMAGE, BIND_HOST_TEXTURE, POINTER, etc.) AND wake on
vsync timing. `recv_timed` with a vsync-derived deadline achieves both without a
kernel ABI extension.

A dedicated vsync SPI interrupt remains a future option if a unified wait
primitive (`wait_any`) is added to the kernel. The polling approach is
sufficient: reading a config-space register is a single MMIO load (~1-2us VM
exit), and it happens at most once per frame.

### Changes

**Hypervisor (already done, uncommitted):**

- `displayLinkFired` increments `vsyncCounter` on each real vsync.
- Config space offset `0x10` returns `vsyncCounter`.

**Guest render driver:**

- Add `last_vsync_counter: u32` state.
- Each loop iteration: read `config_read32(0x10)`. If counter advanced, record
  `vsync_time = now` and compute
  `next_deadline = vsync_time + frame_interval - render_budget` (render_budget =
  estimated CPU time to build the frame, so submission lands just before the
  next vsync).
- Replace the fixed `next_vsync = now + frame_interval` with counter-anchored
  timing.
- Missed vsync handling: if the counter advanced by more than 1, skip directly
  to the latest — never queue up stale frames.

### Verification

- Visual regression test passes.
- Log render timestamps and vsync counter values. Verify render_frame calls
  track within 1ms of actual vsync ticks.
- No judder at 120Hz: capture 240 frames, verify inter-frame timing variance <
  0.5ms.

---

## Phase 2 — Damage Tracking

**Goal:** Only re-render regions of the screen that actually changed.

**Current state:** Every frame clears the entire framebuffer (`LOAD_CLEAR`) and
re-renders all scene nodes from scratch. A cursor blink redraws the entire A4
page, shadow, title bar, and all text.

### TBDR-Aware Damage Strategy

On Apple Silicon TBDR, `LOAD_LOAD` is expensive (reads every tile from system
memory). `LOAD_CLEAR` is nearly free. `CAMetalLayer` rotates drawables, so the
previous frame's pixels are not in the current drawable — they must be blitted
from `retainedFrame`.

The correct approach has two paths selected by damage area:

**Small damage (< 20% of screen):**

1. Blit full `retainedFrame` → current drawable (blit encoder, ~40MB bandwidth)
2. Begin render pass with `LOAD_DONT_CARE` (free — blit already filled pixels)
3. Set scissor rect to damage region (TBDR skips tiles outside scissor)
4. Walk only scene nodes that intersect the damage rect
5. End render pass, `STORE_STORE`
6. Copy drawable → `retainedFrame` (for next frame)

Using `LOAD_DONT_CARE` instead of `LOAD_LOAD` avoids reading the drawable back
into tile memory after the blit already wrote it. This saves ~20MB/frame of
redundant read bandwidth.

**Large damage (>= 20% of screen):** Full repaint as today: `LOAD_CLEAR`, walk
all nodes, `STORE_STORE`. No retained frame blit. The overhead of the blit +
partial redraw exceeds the savings when damage is large.

The 20% threshold is an initial estimate. Tune empirically by measuring frame
time at various damage fractions.

### Scene Graph VMO Extension

The `SceneHeader` is currently 80 bytes (generation, node_count, root,
data_used, dirty_bits[8]). The `dirty_bits` array occupies offsets 12-75.

Append damage rect fields after the existing header:

```text
offset 76: damage_x       (i32, millipoints)
offset 80: damage_y       (i32, millipoints)
offset 84: damage_width   (u32, millipoints)
offset 88: damage_height  (u32, millipoints)
```

New header size: 92 bytes. Update the compile-time assertion
(`size_of::<SceneHeader>() == 92`), `SCENE_SIZE`, `NODES_OFFSET`, and
`DATA_OFFSET` accordingly.

A damage rect of (0, 0, 0, 0) means "full repaint" (backwards compatible for the
first frame, mode changes, space switches, etc.).

### Damage Rect List

A single damage rect cannot represent disjoint damage (cursor blink + clock
update in different corners). Use a small rect list (up to 4 rects) instead:

```text
offset 76:  damage_count   (u8, 0-4. 0 = full repaint)
offset 77:  _pad           (3 bytes)
offset 80:  damage_rects   ([DamageRect; 4], 16 bytes each = 64 bytes)
```

New header size: 144 bytes. Each `DamageRect` is
`(x: i32, y: i32, w: u32, h: u32)` in millipoints. If `damage_count` is 0 or
exceeds 4, full repaint.

When the compositor receives N damage rects, it computes the bounding box for
the scissor rect and walks only nodes that intersect any of the rects. If the
bounding box exceeds 20% of the screen, switch to full repaint.

### Cumulative Damage Across Skipped Generations

The presenter writes the scene graph under a seqlock. The compositor may miss
intermediate generations (e.g., reads gen 4, misses gen 5's full repaint, reads
gen 6's small damage). If damage is not cumulative, gen 5's changes outside gen
6's rect are permanently missed.

Solution: **the presenter accumulates damage.** The damage rects in the header
represent ALL changes since the compositor's last acknowledged generation, not
just the latest write. The presenter tracks `compositor_ack_gen` (set by the
compositor writing to a shared field, or inferred from the seqlock read
pattern). On each scene write, the presenter unions the new damage with the
accumulated damage since `compositor_ack_gen`.

Implementation: add a `reader_gen` field to the header (offset 144, u32). The
compositor writes its last successfully read generation here after `end_read`
succeeds. The presenter reads `reader_gen` and resets accumulated damage when it
sees the compositor has caught up.

### Buffer Age

Once double-buffering (Phase 3) is active, the render target alternates between
two DMA buffers. The `retainedFrame` on the host always contains the most recent
completed frame, so the blit approach already handles buffer age correctly — the
compositor always blits the latest retained content regardless of which buffer
it's rendering into.

If the host ever moves away from `retainedFrame` to direct drawable reuse,
buffer age accumulation (union of N frames of damage, where N = buffer age)
would be required. For now, the retained frame model handles this.

### Backdrop Blur Interaction

A backdrop-blur node depends on ALL content beneath it, not just its own pixels.
If any node behind a blur changes, the blur must be re-rendered.

Rule: when computing damage, if ANY damage rect intersects a region that is
visually behind a backdrop-blur node, expand the damage to include the blur
node's full bounds plus blur kernel padding (3 \* sigma pixels on each side).

The presenter knows which nodes have `backdrop_blur_radius > 0` and their
bounds. During damage calculation, it checks whether any damaged node is an
ancestor or sibling-before a blur node. If so, the blur node's bounds (padded)
are added to the damage rect list.

If this expansion pushes total damage above 20%, fall back to full repaint.

### Live Image (Video Frame) Damage

Live image updates (`refresh_live_images` detecting a generation counter change)
do NOT modify the scene graph generation. The compositor must synthesize damage
rects for live image changes.

When `refresh_live_images` returns `true`, the compositor looks up each changed
image's `content_id` in the scene graph, finds the corresponding
`Content:: Image` node's screen-space bounding box, and adds it to the damage
rect list for the current frame.

### Presenter Damage Reporting

The presenter knows the cause of each rebuild:

| Cause                    | Damage rect                                               |
| ------------------------ | --------------------------------------------------------- |
| Cursor blink             | cursor bounding box (single glyph cell)                   |
| Character insert/delete  | current line + lines below (reflow)                       |
| Scroll                   | full viewport (full repaint)                              |
| Ctrl+Tab space switch    | full screen (full repaint)                                |
| Clock update             | clock node bounding box                                   |
| Pointer move             | old cursor rect union new cursor rect                     |
| Backdrop blur behind any | blur node bounds + 3\*sigma padding (full repaint likely) |

### Wire Protocol Changes

Add `LOAD_BLIT_RETAINED` as a new load action value for `CMD_BEGIN_RENDER_PASS`.
When the host sees this action:

1. Blit `retainedFrame` → current drawable via blit encoder
2. Begin render pass with `MTLLoadAction.dontCare` on the drawable
3. Apply scissor from the guest's `CMD_SET_SCISSOR`

After the frame completes, copy drawable → `retainedFrame` (the host already
does this for cursor-only presents).

### Verification

- Visual regression: screenshot must match full-repaint path pixel-for-pixel.
  Run both paths and compare.
- Performance: instrument `render_frame` with vertex count. Cursor blink should
  generate < 20 quads (cursor rect + clock if ticking), not hundreds.
- Corruption test: force damage to be deliberately wrong (too small), verify the
  visual corruption is detected. This ensures the system actually relies on
  damage, not accidental full repaints.
- Blur test: change content behind a blurred node, verify the blur updates.
- Skipped-gen test: simulate a missed generation (artificial delay in
  compositor), verify accumulated damage covers all changes.

---

## Phase 3 — Async Frame Submission

**Goal:** CPU work on frame N+1 overlaps with GPU execution of frame N.

**Current state:** `submit_and_wait` blocks the guest CPU until the host GPU
finishes: push descriptor, notify, wait on IRQ, pop used. The CPU is idle during
GPU render time.

**Depends on:** Phase 1 (correct vsync timing prevents rendering ahead of
display).

### Design

**Guest-side: double-buffered DMA, async final submission.**

A single frame may require multiple `submit_and_wait` calls: the backdrop blur
path submits 2 separate command buffers (blit + blur passes), and the draw-op
loop may split across multiple submissions if the DMA buffer overflows.

The double-buffer scheme applies only to the **final** submission of each frame
(the one containing `present_and_commit`). All intermediate submissions within a
frame (blur passes, overflow splits) remain synchronous because they reuse the
same DMA buffer and must complete before the next pass begins.

Two render DMA buffers: `render_dma[0]` and `render_dma[1]`. Track which is "in
flight" (submitted but not yet completed by the GPU):

```text
Frame N:
  [intermediate blur/overflow submissions — synchronous, reuse render_dma[0]]
  [final submission into render_dma[0] — submit_async, mark in-flight]

Frame N+1:
  [wait for render_dma[0] completion IF still in-flight]
  [intermediate submissions — synchronous, reuse render_dma[1]]
  [final submission into render_dma[1] — submit_async, mark in-flight]
```

The overlap: while the GPU processes frame N's final command buffer in
`render_dma[0]`, the CPU immediately begins scene walk and vertex generation for
frame N+1 using `render_dma[1]`. The CPU only blocks if the previous frame's
final submission hasn't completed by the time the CPU needs that buffer again (2
frames later).

### Guest Changes

Replace `submit_and_wait` with two functions:

```rust
fn submit_async(device, vq, queue_index, dma_pa, cmd_len) {
    vq.push(dma_pa, cmd_len as u32, false);
    device.notify(queue_index);
    // Return immediately — don't wait for GPU.
}

fn ensure_completed(vq, irq_event, device) {
    // Check if previous submission finished.
    if vq.pop_used().is_none() {
        // Still in flight — wait for GPU completion IRQ.
        let _ = abi::event::wait(&[(irq_event, 0x1)]);
        device.ack_interrupt();
        let _ = abi::event::clear(irq_event, 0x1);
        vq.pop_used();
    }
}
```

The render loop becomes:

1. `ensure_completed` on the in-flight buffer (blocks only if GPU is slow).
2. Build frame into the now-available buffer.
3. Intermediate blur submissions: `submit_and_wait` (synchronous, same buffer).
4. Final submission: `submit_async` (non-blocking).
5. Swap active buffer index.
6. Continue to next vsync.

### Host Changes (Critical)

The host currently calls `cb.commit()` then `cb.waitUntilCompleted()`, which
blocks the host GPU dispatch thread and prevents GPU frame pipelining. The
guest-side double buffering achieves nothing without this host change.

Replace synchronous wait with triple-buffer semaphore:

```swift
private let frameSemaphore = DispatchSemaphore(value: 2)

// In presentAndCommit:
frameSemaphore.wait()  // blocks if 2 frames already in-flight
cb.addCompletedHandler { [weak self] _ in
    self?.frameSemaphore.signal()
    // Trigger virtio completion: write used ring, inject IRQ
    self?.completeVirtioRequest(state: state, vm: vm)
}
cb.commit()
// Return immediately — don't wait for GPU.
```

The virtio used-ring write and guest IRQ injection move into the
`addCompletedHandler` callback, firing only when the GPU actually finishes. This
lets the host return from `handleNotify` immediately after commit, freeing the
dispatch queue for the next guest submission.

`CAMetalLayer` provides 3 drawables by default. With max 2 in-flight frames
(semaphore value 2), `nextDrawable()` never blocks — there's always at least 1
drawable available. If the guest runs faster than display refresh,
`frameSemaphore.wait()` provides natural backpressure.

### Verification

- All visual regression tests pass.
- Instrument: measure time from `submit_async` to next CPU work starting. Should
  be < 0.1ms (just the virtio notify overhead).
- Stress test: rapid scene changes (fast typing) at 120Hz. No tearing, no buffer
  corruption, no use-after-submit.
- Verify IRQ timing: guest completion IRQ fires only after GPU finishes (from
  `addCompletedHandler`), not after host commit.

---

## Phase 4 — Atlas Improvements

**Goal:** Eliminate full-atlas resets and reduce upload bandwidth.

**Current state:** When the glyph atlas fills, `GlyphAtlas::reset()` wipes
everything. Every glyph must be re-rasterized and re-uploaded. Atlas uploads
send entire dirty rows (2048 pixels wide) even when only one glyph was added.

### 4a: LRU Shelf Eviction

The atlas uses a shelf (row-pack) allocator: `row_y`, `row_x`, `row_h` track the
current allocation cursor. Glyphs are placed left-to-right; when a glyph doesn't
fit horizontally, a new shelf starts below. Historical shelves have no metadata
— they only exist implicitly via hash-table entries.

Replace full reset with shelf-based LRU:

**New data structure:** `ShelfInfo` array alongside the existing allocator:

```rust
struct ShelfInfo {
    y: u16,
    height: u16,
    last_used_frame: u32,
}
const MAX_SHELVES: usize = 128;  // 2048px / ~16px avg = ~128 shelves
shelves: [ShelfInfo; MAX_SHELVES],
shelf_count: usize,
```

When a shelf is started (in `pack`, when `row_x` resets to 0), record it in the
`shelves` array. When a glyph from a shelf is used during rendering, update that
shelf's `last_used_frame`.

**Eviction:** When `pack` fails (no vertical space), find the shelf with the
smallest `last_used_frame`. Invalidate all hash-table entries whose atlas Y
coordinate falls within that shelf's `[y, y+height)` range. This requires a full
scan of the hash table (16384 slots) — acceptable because eviction is rare (once
per atlas fill, typically every few minutes of typing).

**Hash-table invalidation:** The atlas uses open-addressed hashing with linear
probing and no tombstones. Deleting entries from this scheme requires rehashing.
Two options:

1. **Tombstone-based deletion:** Change the empty sentinel from `glyph_id == 0`
   to a separate state. Deleted entries become tombstones (skipped during
   lookup, reusable during insert). Simpler but degrades probe performance over
   time.

2. **Batch rebuild:** On eviction, collect all non-evicted entries, clear the
   table, and re-insert them. O(n) but runs only on eviction. Preserves
   tombstone-free performance for normal lookups.

Option 2 is preferred: eviction is rare, and the normal-case lookup performance
matters more.

After eviction, the freed shelf's Y range is available for new allocations. The
allocator cursor can jump to the freed range rather than always appending at the
bottom.

### 4b: Partial Atlas Upload

Track a dirty rectangle within the atlas instead of "dirty from Y onwards":

```rust
atlas_dirty: Option<AtlasDirtyRect>,

struct AtlasDirtyRect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}
```

When a glyph is rasterized into the atlas, expand the dirty rect to include its
bounding box. `upload_atlas_dirty` uploads only the dirty rect region via
`CMD_UPLOAD_TEXTURE` with the exact `(x, y, w, h)`.

On Apple Silicon's unified memory, `tex.replace(region:)` is a cheap memcpy with
negligible per-call overhead. Uploading a 20x30 glyph (600 bytes) instead of a
2048x30 row (61,440 bytes) saves ~100x bandwidth in the virtio command stream.

### Future: Shared-Memory Atlas (Blob Resource)

The atlas pixel data is rasterized in the guest and uploaded through the virtio
command stream. An alternative: the host allocates an IOSurface-backed texture
and maps its memory into the guest's address space (via a shared VMO). The guest
rasterizes directly into shared memory. The host's texture reads the same memory
— no upload command needed.

This mechanism already exists for video frames (IOSurface zero-copy path). For
the atlas, it would eliminate the `CMD_UPLOAD_TEXTURE` overhead entirely. This
is a natural extension but not urgent — partial upload (4b) already reduces
bandwidth to negligible levels for the atlas use case.

### Verification

- Render a document with > 200 unique glyphs (fills atlas). Verify no visual
  glitches after eviction.
- Instrument atlas upload bytes per frame. Single-character edits should upload
  < 5KB.
- Eviction correctness: after eviction, re-request an evicted glyph and verify
  it is re-rasterized correctly (not stale pixels from the previous shelf
  occupant).

---

## Phase 5 — Vertex Buffer Caching

**Goal:** Avoid regenerating the full draw list on every frame.

**Current state:** `render_frame` walks the scene graph and builds a new
`DrawList` (with `Vec<u8>` vertex data and `Vec<DrawOp>` ops) from scratch on
every frame. The draw list is discarded after submission.

**Depends on:** Phase 2 (damage tracking tells us which frames need a rebuild at
all).

### Design: Generation-Based Invalidation

The simpler of two approaches (full partial-update patching is Phase 5b):

- Retain the `DrawList` across frames.
- On each frame, compare scene generation to the last-built generation.
- If unchanged AND no live image changes AND no animation deadlines: skip vertex
  generation entirely, resubmit the cached draw list.
- If changed: rebuild from scratch (but with damage tracking from Phase 2, only
  damaged nodes are walked, so the rebuild is already cheap).

This gives the primary benefit (zero vertex gen cost on idle frames) without the
complexity of partial vertex buffer patching.

### Resubmission Without Rebuild

On idle frames, the compositor still needs to submit a frame to the GPU (because
`retainedFrame` blit may be needed, or a live image texture may have changed
requiring a re-render). But the vertex data and draw-op list are identical. The
compositor can skip `walk_node` entirely and go straight to the DMA encoding
loop.

### Phase 5b (Future): Partial Vertex Patching

For complex scenes where even damage-limited rebuilds are expensive:

- Each `DrawOp` tracks which scene `NodeId` produced it.
- On damage, identify which ops overlap the damage rect.
- Re-walk only the damaged nodes, generating replacement vertices.
- Patch the `DrawOp` entries in place (swap vertex data, keep surrounding ops).

This requires a reverse mapping from node to draw-op, which adds complexity.
Defer until profiling shows the damage-limited rebuild is a bottleneck.

### Verification

- Visual correctness: identical output whether draw list is cached or rebuilt.
  Regression screenshots should be bit-identical.
- Performance: on idle frames (no scene change), `render_frame` should take <
  0.1ms (no vertex generation, just DMA encoding and submission).

---

## Phase 6 — Multi-Threaded Compositor

**Goal:** Separate CPU-bound scene walk from GPU-bound command submission.

**Current state:** Single-threaded: scene walk, vertex gen, atlas raster, atlas
upload, and GPU submission all happen sequentially in the render loop.

**Depends on:** Phases 1-5 (the single-threaded path should be fully optimized
before adding threading complexity).

### Design

Two threads within the render driver's address space:

- **Build thread:** Reads scene graph, generates draw list, rasterizes glyphs
  into atlas. Produces a completed `DrawList` + atlas dirty region.
- **Submit thread:** Takes completed draw list, uploads atlas changes, encodes
  into DMA command buffer, submits to virtio. Handles IPC (SETUP, UPLOAD_IMAGE,
  etc.). Waits for GPU completion.

Communication: double-buffered `DrawList`. Build thread writes to list A while
submit thread consumes list B. Swap on vsync boundary using an Event object as a
semaphore.

### Thread Safety Audit (Required Before Implementation)

The render driver has several pieces of shared mutable state that must be
addressed:

1. **`static mut FONT_VA`** (main.rs:49) — Set once at startup, read on every
   glyph rasterization. Must become `static AtomicUsize` or `OnceLock<usize>`.

2. **`GlyphAtlas`** — 4MB+ struct, mutated during rasterization. Must be
   exclusively owned by the build thread. The submit thread reads only the atlas
   pixel buffer (for upload), so the atlas dirty rect can be communicated
   through the `DrawList` handoff.

3. **`images` array** — Copied into `WalkContext` at the start of each
   `render_frame`. The IPC handler (on the submit thread) can receive
   UPLOAD_IMAGE or BIND_HOST_TEXTURE that modifies `self.images`. The copy must
   happen under a lock, or image slot updates must be queued and applied at the
   frame boundary.

4. **`Compositor` struct** — Not `Send` or `Sync`. Must be split: build-thread
   state (atlas, scene reader, walk context) vs. submit-thread state (device,
   virtqueues, DMA buffers, IPC handler, image slots).

### Kernel Requirement

The render driver currently runs as a single-threaded process. Multi-threading
requires `thread_create` within the same address space. The kernel supports this
(`THREAD_CREATE` syscall). Shared memory is natural (same address space).
Synchronization uses Event objects (signal from one thread, wait from the
other).

### Host-Side Consideration

The host processes setup and render queues on the same serial dispatch queue
(`gpuThread.runSync`). A long setup command (shader compilation) blocks render
queue processing. Consider processing the two queues on separate dispatch
queues, or ensuring setup commands never overlap with render-critical paths.

### Verification

- All visual regression tests pass.
- Frame time under load: complex scene + rapid edits. Should improve by the
  lesser of {build time, submit time} since they now overlap.
- Data race testing: stress test with rapid IPC + rapid scene changes. No
  crashes, no visual corruption.

---

## Phase 7 — GPU Path Rendering (Future)

**Goal:** Move vector path rasterization from CPU to GPU.

**Current state:** The CPU scanline rasterizer in `path.rs` handles icons,
cursor shapes, and Content::Path nodes. Results are cached in the glyph atlas,
so each unique path is rasterized only once.

**Why future:** The current CPU approach is correct and cached. It's only a
problem if the set of unique paths grows large or paths need to be rendered at
many sizes (zoom). The virtio-metal protocol would need compute shader support.

### Possible Approaches

1. **SDF rendering (most practical near-term).** Precompute signed distance
   fields for icons at a reference size. Store SDF textures. Render at any size
   with a fragment shader. No compute shaders needed — only a new fragment
   shader and precomputed textures. This is how Flutter/Impeller renders vector
   icons.

2. **Vello-style compute tiling (most capable long-term).** Translate path
   commands into a compute shader workgroup dispatch. Requires
   `CMD_DISPATCH_COMPUTE` in the virtio-metal protocol and compute pipeline
   state creation.

3. **Tessellation to triangles.** CPU tessellates paths into triangle meshes
   (lyon crate). GPU renders triangles with the existing solid pipeline. No new
   GPU features needed. Quality depends on tessellation resolution.

### Virtio-Metal Protocol Extensions (for option 2)

- `CMD_CREATE_COMPUTE_PIPELINE` (setup queue)
- `CMD_DISPATCH_COMPUTE` (render queue)
- `CMD_SET_COMPUTE_BUFFER` / `CMD_SET_COMPUTE_TEXTURE` (render queue)

### TBDR Note

On Apple Silicon, compute shaders run on the same GPU cores as vertex/fragment
shaders. Compute dispatches between render passes force a tile flush.
Interleaving compute and render work should be minimized. The Vello approach
would need to complete all compute work before the render pass begins.

---

## Additional TBDR Optimizations (Not Phased)

These are smaller wins that can be applied opportunistically:

### Combine Blur Passes Into Single Virtio Submission

Currently, the backdrop blur path submits 2-3 separate virtio command buffers
per blur node (blit + horizontal blur + vertical blur), each requiring a full
virtio round-trip. The guest could instead encode all blur passes as sequential
begin/end render pass sequences in a single DMA buffer submission. The host
already processes commands sequentially and creates encoders on the fly —
multiple render passes within one submission is natural.

This avoids multiple virtio round-trips per blur and lets the Metal driver
optimize pass boundaries within a single `MTLCommandBuffer`.

### Memoryless Storage for Intermediate Textures

`TEX_BLUR` is used as an intermediate render target — produced and consumed
within a single frame, never read back. On Apple Silicon,
`MTLStorageMode.memoryless` lets a texture exist only in tile SRAM, never backed
by system memory. This eliminates the store/load cost for intermediate textures.
However, the blur spans multiple render passes, so memoryless may not be
applicable without restructuring the blur as a single-pass operation with
programmable blending.

### Front-to-Back Opaque Pre-Pass

TBDR's Hidden Surface Removal discards occluded fragments before the fragment
shader runs — but only for opaque geometry with depth testing. The current
pipeline renders back-to-front (for alpha blending) without a depth buffer. For
opaque UI chrome (title bars, solid backgrounds), a depth-pre-pass rendered
front-to-back could let HSR eliminate all fragments behind opaque surfaces.

Practical benefit is limited for this compositor (most content requires alpha
blending for text, shadows), but worth considering if opaque content grows.

---

## Implementation Order

```text
 Week 1    Phase 1 (vsync counter polling)
           Phase 4 (atlas LRU + partial upload)
 Week 2    Phase 2 (damage tracking — scene header, presenter damage,
                     TBDR-aware load strategy, blur interaction)
 Week 3    Phase 3 (async submission — guest double-buffer + host
                     addCompletedHandler)
 Week 4    Phase 5 (vertex buffer caching)
 Later     Phase 6 (multi-threaded compositor)
 Future    Phase 7 (GPU path rendering)
```

Phases 1 and 4 are quick wins that immediately improve the pipeline. Phase 2 is
the most impactful single change (eliminating redundant work) but also the most
complex due to backdrop blur interaction, cumulative damage, and the TBDR load
strategy. Phase 3 unlocks CPU/GPU parallelism but requires coordinated changes
in both the guest and host. Phase 5 is the refinement layer on top of damage
tracking. Phases 6 and 7 are for when the simpler optimizations have been
exhausted.
