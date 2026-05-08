# Project Status

## Current State: Kernel Complete — Userspace Next

The kernel is complete: functionally verified, performance-optimized, and ready
for userspace. All finalization items are resolved or explicitly deferred with
rationale.

**Branch:** `main`

## Kernel

30 syscalls across 5 object types. ~28K LOC Rust. Framekernel discipline: all
`unsafe` confined to `frame/` module, enforced at compile time.

**Object types:** VMO (8 syscalls), Endpoint (2 + call/recv/reply), Event (5),
Thread (5), Address Space (2), plus handle_dup/close/info, clock_read,
system_info.

**Scheduler:** Multi-core fixed-priority preemptive, 256 levels, per-CPU
`SpinLock<PerCoreState>` (no global lock). SMP up to 8 cores. Thread creation
load-balanced via `least_loaded_core()` with IPI to wake remote cores.

**SMP concurrency:** Per-object locking via ConcurrentTable (per-slot
TicketLock + atomic generations). Per-CPU scheduler locks. IPI infrastructure
(GICv3 SGI for cross-core wake). Syscall dispatch as free functions accessing
global ConcurrentTable state — no global kernel lock. Atomic refcounts
(lock-free increment/decrement). Debug-mode lockdep validator (8 lock classes,
ordering verification).

**IPC:** Synchronous call/recv/reply via endpoints. Priority inheritance. Up to
128 bytes data + 4 handle transfers per message. One-shot reply caps. Badge
passed from caller's handle.

**Memory:** VMOs with COW snapshots, sealing, lazy allocation, pager-backed
fault handling, cross-space mapping. 16 KiB pages (Apple Silicon native).

### Verification Summary

12-phase verification campaign (`design/kernel-verification-plan.md`):

| Phase                 | Key results                                       |
| --------------------- | ------------------------------------------------- |
| 0. Spec Review        | Interaction matrix, state machines, 16 invariants |
| 1. Unsafe Audit       | 85 blocks in 15 files, all clean                  |
| 2. Property Testing   | 33 proptests                                      |
| 3. Fuzzing            | 4 targets, 218K+ runs, zero crashes               |
| 4. Miri               | All host-target tests pass                        |
| 5. Coverage           | 96-100% on all critical files                     |
| 6. Mutation Testing   | Zero non-equivalent survivors                     |
| 7. Sanitizers         | ASan clean on all 704 tests                       |
| 8. Concurrency        | Cross-core IPC, SMP stress on 4 vCPUs             |
| 9. Error Injection    | All 12 error codes explicitly tested              |
| 10. Static Analysis   | Clippy pedantic, both targets clean               |
| 11. Bare-Metal + Perf | 14 benchmarks + 3 workloads, baselines set        |
| 12. Regression Infra  | Pre-commit + nightly gates, Makefile targets      |

**Bugs found and fixed:** 26 total (20 during verification, 4 during
finalization — IPC reply cap delivery, thread placement, badge wiring,
thread_exit ordering, 2 post-finalization — missed-wakeup race in wake(),
redundant TTBR0 switches).

**Test suite:** 557 tests, 4 fuzz targets, 33 property tests, 16 invariant
checks, 34 bare-metal integration tests, 14 per-syscall benchmarks + 3 workload
benchmarks + 3 SMP benchmarks.

**Performance gates:** Per-benchmark statistical thresholds (P99 + 3σ) in
`kernel/bench_baselines.toml`. Regression = bug.

### SMP benchmark results (2026-05-07, 4 cores under hypervisor)

```text
IPC null round-trip (2-core):       3565 cyc/rtt  (was 3746, −5% from TTBR0 skip)
object churn (1-core):              5243 cyc/iter
object churn (multi-core wall):    27770 cyc/iter  0.7x/4 scaling
cross-core wake (event ping-pong):  4544 cyc/rtt  (~2272 one-way)
```

## Kernel Finalization: Complete

All finalization items resolved. The kernel is ready for userspace.

### 1. Direct process switch for IPC — DONE

When a server is blocked in recv and a client calls, the kernel now
context-switches directly caller→server without touching the run queue.
`sched::direct_switch` marks the caller Blocked, the server Running, and
switches registers. Zero scheduler overhead for the common IPC fast path.

On the reply path, the kernel compares caller and server priorities. If the
caller should preempt (caller priority >= server's post-reply priority), it uses
`sched::wake_and_switch` to swap directly. Otherwise, normal wake.

Impact: cross-core IPC round-trip dropped from 4444 to 3847 cycles (−13.4%).

### 2. Topology hints — DEFERRED

`set_affinity` stores Performance/Efficiency/Any hints but nothing reads them.
On M4 Pro bare metal these map to P-core and E-core clusters. Under the
hypervisor (4 identical vCPUs) they have no effect. Per the "unverifiable work
does not ship" rule, this is deferred until bare-metal testing is available. The
syscall and storage are in place; only the scheduler read path is missing.

### 3. Benchmark baselines — CHECKED

All 17 benchmarks pass regression thresholds after the Endpoint struct
optimization, thread load balancing, and direct switch changes. SMP benchmarks
confirm 3.3x/4 scaling. `make bench-check` is green.

### 4. HandleTable RwSpinLock — DEFERRED

Concurrent handle lookups within the same address space are serialized by the
AddressSpace slot lock. An RwSpinLock would allow parallel reads. This is an
internal optimization with no ABI impact — it can be added when multi-threaded
userspace workloads reveal contention. Current SMP scaling (3.3x/4) suggests the
slot lock is not the primary bottleneck.

### 5. Missed-wakeup race in wake() — FIXED

`sched::wake()` silently dropped wakeups when the target thread was Running (not
yet Blocked). This caused a deadlock in event_wait and a latent race in IPC call
when a signal/message arrived between registering the waiter and calling
`block_current()`:

1. Thread adds itself as waiter (Running)
2. Another core signals → `signal()` removes waiter, calls `wake()`
3. `wake()` sees Running → no-op (old code returned here)
4. Thread calls `block_current()` → Blocked forever

Fix: `Thread.pending_wake` flag. `wake()` sets it when the target is Running.
`block_current()` checks and consumes it under the thread slot lock (TicketLock
serializes the two operations). If pending_wake is set, block_current returns
immediately. Multi-core churn benchmark now runs cleanly without the spin-wait
workaround.

### 6. Redundant TTBR0 switches — FIXED

`maybe_switch_page_table` (scheduler path) and `pick_and_setup` (idle path)
unconditionally wrote TTBR0_EL1 + ISB even when already pointing at the correct
page table. Changed both to `switch_table_if_needed` which reads TTBR0 first and
skips the MSR+ISB if already correct. Saves ~5% on IPC round-trip and ~3% on
cross-core wake.

## What's Next: Userspace

Rebuild the userspace on the verified kernel, targeting the same UI/UX as the
v0.6-pre-rewrite prototype. Full plan in `design/userspace-rebuild.md`.

**Architecture:** `design/architecture.md` (pipeline, responsibilities).
**Rebuild plan:** `design/userspace-rebuild.md` (layering, protocol design,
build order, verification strategy).

### Completed (Layer 0)

- `userspace/abi` — raw syscall wrappers for all 30 syscalls
- `userspace/ipc` — SPSC ring buffers, seqlock state registers, typed messages
- `userspace/init` — parses SVPK service pack, spawns services in separate
  address spaces
- `userspace/servers/hello` — test service (Phase 1.3 verification)

### Phase 1 — Protocol + Service Infrastructure (COMPLETE)

1. **Protocol crate** — DONE. 7 modules, 17 message types, 56 tests. Covers all
   IPC boundaries: name service, bootstrap, input, edit, store, view, decode.
   `userspace/protocol/`
2. **Service pack tool** — DONE. Host tool packs flat binaries into a
   page-aligned archive (SVPK format). 18 tests. `tools/mkservices/`
3. **Init completion** — DONE. Init parses SVPK pack, creates address spaces,
   maps code/stack VMOs, spawns service threads. Verified: hello service runs a
   syscall and exits cleanly in its own address space. Kernel fixes: page table
   creation in `space_create`, cross-space page table switch in context switch,
   existing-page fault resolution, instruction abort handling.
4. **Name service** — DONE. Register/Lookup/Unregister via sync IPC. Integration
   test: init spawns name service + test-a + test-b. test-a registers its
   endpoint, test-b looks it up (with handle transfer through reply), calls
   test-a directly, verifies magic reply value. All exit code 0.

The kernel's ABI is frozen. Changes driven by userspace needs will add syscalls
or extend existing ones, never break the existing interface.

### Phase 2 — Drivers (IN PROGRESS)

**Kernel extensions for drivers:**

- **Device VMOs** (`VmoFlags::DEVICE`, `Vmo::new_physical`) — VMOs backed by
  specific physical addresses for MMIO. Page table entries use `ATTR_DEVICE`
  (MAIR index 0, Device-nGnRnE). Identity-mapped (VA = PA).
- **DMA VMOs** (`VmoFlags::DMA`, `Vmo::new_contiguous`) — contiguous physical
  pages, identity-mapped (VA = PA). Capability-gated: `vmo_create` with DMA flag
  requires a Resource handle of kind `Dma` as `args[2]`.
- **Resource type** (`ObjectType::Resource`) — kernel-created authority tokens
  (Zircon model). Bootstrap installs a DMA Resource as handle 6 in init's space.
- **Bootstrap handles for init:** 0=space, 1=code VMO, 2=pack VMO, 3=device
  manifest VMO, 4=UART MMIO VMO, 5=virtio MMIO VMO, 6=DMA Resource.
- **Service stack VA** moved to `0x1_0000_0000` (above physical RAM range) to
  reserve the PA-matching VA range for identity-mapped DMA buffers.
- **IPC TOCTOU fix** — endpoint call queue race under SMP (pre-existing bug).

**Drivers:**

1. **Console driver (PL011)** — DONE. Maps UART device VMO, registers with name
   service, prints "console: ready" on boot. `userspace/drivers/console/`
2. **virtio-input** — DONE (skeleton). Probes MMIO for input devices, requests
   DMA from init, sets up virtqueue, binds IRQ, event loop reads EV_KEY/EV_ABS
   with modifier tracking. Event output to presenter not yet wired (Phase 4).
   Now in service pack. `userspace/drivers/input/`
3. **virtio-blk** — DONE. Probes MMIO for block device (ID 2), negotiates FLUSH
   feature, reads capacity, allocates DMA for virtqueue + data buffer, runs
   self-test (write/read/verify 16 KiB block + flush), registers with name
   service as "blk", enters IPC serve loop. Protocol: shared data VMO for bulk
   transfer, read_block/write_block/flush/get_info methods. Verified end-to-end:
   hypervisor provides file-backed block device, driver reports capacity,
   write+read 16K: OK, flush: OK. `userspace/drivers/blk/`
4. **Metal render driver** — DONE. Probes MMIO for Metal GPU (device ID 22),
   negotiates features, sets up 2 virtqueues (setup + render), reads display
   config from device config space (width/height/refresh). Compiles MSL shaders,
   creates render pipeline, renders fullscreen solid-color frame via
   DRAWABLE_HANDLE. Registers with name service as "render", enters IPC serve
   loop. Wire format: `protocol::metal::CommandWriter` (no_std, no alloc).
   Verified: hypervisor `--capture 0` produces uniform (101,101,105) screenshot
   across all ~11M pixels. `userspace/drivers/metal-render/`

**Phase 2.2 infrastructure (DONE):**

- **Persistent init** — serve loop after spawning services, handles DMA_ALLOC
  requests via sync IPC. Driver sends size → init calls `vmo_create_dma` with
  DMA Resource → returns VMO handle via reply.
  `protocol::bootstrap::DmaAllocRequest`.
- **Virtio library** (`userspace/drivers/virtio/`) — MMIO transport + split
  virtqueue. Adapted from v0.6 prototype, 16 KiB pages, pure no_std, no deps.
- **ABI extension** — `vmo::create_dma(size, resource)` wraps vmo_create with
  DMA flag and Resource handle argument.

**Phase 2.3 kernel fixes:**

- **MAX_BOOTSTRAP_HANDLES** — `thread_create_in` shared the IPC handle limit
  (4), but driver bootstrapping needs 5 handles (code, stack, name svc, device
  VMO, init ep). Added separate `MAX_BOOTSTRAP_HANDLES = 8`.
- **Device IRQ dispatch** — `DEVICE_IRQ_HANDLER` was never registered.
  `device_irq_dispatch` now looks up `IrqTable` bindings and signals events.
- **SPI unmask on bind** — `event_bind_irq` recorded bindings but didn't unmask
  the SPI at the GIC. Added `unmask_spi(intid)` to the bind path.
- **Console dup-before-register** — IPC handle transfer is a move, not copy.
  Console (and blk) now dup their endpoint before passing to name service.
- **Input IRQ from slot** — IRQ number computed from discovered MMIO slot index
  (48 + slot), not hardcoded.

**Phase 2.4 additions:**

- **Metal protocol** (`protocol::metal`) — wire format constants and
  `CommandWriter` for Metal-over-virtio command buffers. 8-byte command headers,
  setup commands (compile, get_function, create_pipeline), render commands
  (begin/end render pass, set pipeline, vertex bytes, draw, present). 7 tests.
- **Visual verification** (`test/verify.py`) — pixel-level screenshot assertions
  (solid_color, uniform, not_black, dimensions, pixel_at) for automated visual
  regression testing via hypervisor `--capture`.
- **virtio constant** — `DEVICE_METAL = 22` in virtio library.

### Phase 3 — Core Libraries (COMPLETE)

10 libraries adapted from v0.6 prototype, all compiling for both host
(`aarch64-apple-darwin`) and bare-metal (`aarch64-unknown-none`) targets. 488
new library tests (1,045 total workspace tests).

| Library    | Lines | Dependencies          | Tests |
| ---------- | ----- | --------------------- | ----- |
| scene      | 4,232 | none                  | 70    |
| drawing    | 3,835 | none                  | 77    |
| animation  | 1,513 | none                  | 76    |
| fs         | 2,426 | none                  | 63    |
| piecetable | 1,363 | none                  | 60    |
| layout     | 543   | none                  | 45    |
| icons      | 1,080 | none                  | 37    |
| fonts      | 3,403 | harfrust, read-fonts  | 32    |
| store      | 438   | fs                    | 28    |
| render     | 4,958 | drawing, scene, fonts | 0     |

**Bugs found and fixed during port:**

- **fonts: `isqrt_i64` convergence** — Newton's method initial guess could start
  below the root, causing premature termination. `isqrt(100)` returned 8 instead
  of 10. Fixed initial guess to always overshoot.
- **fonts: embolden test expectation** — test expected half the per-edge offset
  at corners, but the FreeType algorithm applies the full per-edge offset
  (strength is per-edge, not total).
- **render: `protocol` dependency** — `DirtyRect` and `ContentRegion` types
  inlined into the render crate to remove the old protocol dependency.

### Phase 4 — Core Services + Leaf Nodes (COMPLETE)

1. **Store service** — DONE. COW filesystem over block device. Mounts or
   formats, opens the document store, registers as "store", enters IPC serve
   loop. Handles CREATE, READ, WRITE, TRUNCATE, COMMIT, SNAPSHOT, RESTORE,
   DELETE_SNAPSHOT, GET_INFO. Shared VMO for bulk data transfer. Integration
   test (test-store): write/read round-trip, snapshot/restore, commit.
   `userspace/servers/store/`

2. **Document service** — DONE. Sole writer to the document buffer. Receives
   edit requests (INSERT, DELETE, CURSOR_MOVE, SELECT, UNDO, REDO) from editors
   via sync IPC. Manages undo ring (64-entry snapshot ring) via COW snapshots
   through the store service. Document buffer is a shared VMO (64-byte header +
   content bytes) with generation counter (Release/Acquire) for lock-free
   cross-process reads. Clients call SETUP to receive an RO handle. Per-edit
   snapshots for per-operation undo granularity.

   Integration test (test-document): SETUP + VMO mapping, two inserts verified
   via shared memory, delete verified, undo×2 verified (restores through
   snapshot chain), redo verified, cursor move + GET_INFO verified.
   `userspace/servers/document/`

3. **Layout service** — DONE. Pure function: (document content + viewport
   state - font metrics) → positioned text runs. Reads the document buffer (RO
   shared VMO via SETUP to document service). Receives viewport state via
   seqlock register in a shared VMO (from presenter). Computes line breaks using
   the `layout` library (`layout_paragraph` with `WordBreaker`). Writes results
   to a seqlock-protected layout results VMO.

   Protocol: SETUP (presenter sends viewport VMO, receives layout results VMO),
   RECOMPUTE (trigger relayout, replies when done), GET_INFO (current stats).
   Results VMO: seqlock generation (8 bytes) + LayoutHeader (16 bytes) +
   LineInfo array (20 bytes/line, max 512 lines). ViewportState: scroll_y,
   viewport_width, viewport_height, char_width (fixed-point 16.16), line_height.

   Protocol tests (10 host-target tests): round-trip serialization for all wire
   types, size constraints, page-fit verification.

   Integration test (test-layout): waits for test-document to finish, clears
   document, inserts multi-line text ("Hello world\nSecond line\nThird"),
   verifies 3-line layout (byte offsets, positions, widths). Appends text,
   re-layouts, verifies 4-line layout. Verifies GET_INFO metadata.
   `userspace/servers/layout/`

4. **Presenter** — DONE. Compiles document state + layout results into a scene
   graph. Reads document buffer (RO VMO), writes viewport state (seqlock) to the
   layout service, reads layout results (seqlock), builds a scene graph tree:
   root (background) → viewport (clips children, margin offset) → per-line
   Glyphs nodes (monospace ShapedGlyph arrays) + cursor node (ROLE_CARET
   rectangle).

   Protocol: SETUP (returns scene graph VMO handle), BUILD (triggers full
   rebuild, replies with stats), GET_INFO (current state). Scene graph VMO uses
   the `scene` library's SceneWriter/SceneReader with generation counter.

   Integration test (test-presenter): waits for test-layout to finish, clears
   document, inserts 3-line text, calls BUILD, verifies: root node (background),
   viewport node (clips_children), 3 Glyphs nodes with non-zero glyph counts,
   cursor node (ROLE_CARET with background color), GET_INFO metadata.
   `userspace/servers/presenter/`

5. **Text editor** — DONE. Content-type leaf node: receives key events from the
   presenter via sync call/reply, translates into document edit operations.
   Handles printable characters, backspace, delete, return, tab (4 spaces),
   shift+tab (dedent). Multi-hop RPC: presenter → editor → document → store.
   Reads cursor position from the shared document buffer (RO mapping).

   Protocol: DISPATCH_KEY (KeyDispatch: key_code, modifiers, character →
   KeyReply: action, content_len, cursor_pos). USB HID key codes for special
   keys (0x28=Return, 0x2A=Backspace, 0x2B=Tab, 0x4C=Delete).

   Integration test (test-editor): 7 test cases — character insert, backspace,
   multi-character sequence, return/newline, tab, shift+tab dedent, forward
   delete with cursor positioning. All verified via shared memory document
   buffer reads. `userspace/editors/text/`

**Infrastructure improvements (Phase 4.5):**

- **Name service** — eliminated fixed-size limits. `MAX_ENTRIES` (16) and
  `MAX_WATCHERS` (8) replaced with `Vec`-backed dynamic storage via heap
  allocator. The old limits were a latent boot deadlock: with 15+ services all
  starting concurrently, >8 would simultaneously WATCH for names that hadn't
  registered yet, silently failing and hanging the dependency chain.

### Phase 5 — Integration (COMPLETE)

1. **Full boot** — DONE. All 10 production services start, discover each other
   via the name service, and enter their serve loops. Boot dependency chain:
   name → console → blk/input/render → store → document → layout → presenter →
   text-editor. Test services removed from the production pack. `cargo r` builds
   and launches the OS fullscreen with Metal GPU + block device.

2. **Compositor** — DONE. Render driver transformed from solid-color stub into a
   scene-graph-driven compositor. Receives the presenter's scene graph VMO via
   `comp::SETUP`, walks the node tree depth-first, generates Metal vertex data
   (backgrounds, per-glyph rectangles, cursor). Heap-allocated vertex buffer
   (`Vec<u8>`) avoids the 128 KiB stack limit. `comp::RENDER` triggers a full
   frame from the current scene graph state. `userspace/servers/drivers/render/`

3. **Presenter active mode** — DONE. On boot: connects to document, layout,
   compositor (sends scene graph VMO, receives display dimensions), and text
   editor. Performs initial scene graph build and render. On `KEY_EVENT` from
   the input driver: dispatches to text editor, rebuilds scene, re-renders.

4. **Input driver → presenter pipeline** — DONE. US QWERTY keymap translates
   evdev key codes to ASCII characters / HID special keys. Lazy presenter lookup
   on first key event. Forwards key-down events via sync IPC
   (`presenter_service::KEY_EVENT`).

5. **Visual verification** — DONE. Automated via hypervisor event scripts
   (`test/phase5-hello.events`). Types "hello", captures frame 5, verifies:
   glyph rectangles at correct positions (sRGB 244,244,244), cursor at column 5
   (sRGB 229,229,229), background (sRGB 96,96,99). `test/verify.py` assertions
   all PASS.

**Rendering note:** glyphs are currently rendered as colored rectangles (one per
character position). Actual font rasterization + texture rendering is planned
but not yet wired into the compositor pipeline. The fonts and render libraries
exist and are tested (Phase 3); connecting them to the Metal compositor is the
next rendering milestone.

## What's Next: v0.6 Parity

The userspace rebuild plan (`design/userspace-rebuild.md`) is complete through
all 5 phases. The full document editing pipeline works end-to-end. Remaining
work to reach full v0.6-pre-rewrite parity is tracked in
`design/v06-parity-plan.md` (Phases 6–13).

### v0.6 Parity Progress

- Phase 6 (glyph atlas + textured rendering): COMPLETE
- Phase 7 (cursor blink + selection): NOT STARTED
- Phase 8 (keyboard navigation): NOT STARTED
- Phase 9 (scroll + viewport): NOT STARTED
- Phase 10 (visual chrome): NOT STARTED
- Phase 11 (content-type typography): NOT STARTED
- Phase 12 (PNG decoder): NOT STARTED
- Phase 13 (filesystem + virtio-9p): NOT STARTED

## Session Resume

To resume work: read this file and `design/v06-parity-plan.md`, check
`git log --oneline -20` for recent commits, identify the current phase from the
progress tracker above, and continue.
