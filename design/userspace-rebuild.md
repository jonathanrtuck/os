# Userspace Rebuild Plan

Rebuild the userspace on top of the verified kernel (30 syscalls, 5 object
types, sync IPC). Target: the same UI/UX as the v0.6-pre-rewrite prototype,
rebuilt from first principles on the new foundation.

The old prototype (tagged `v0.6-pre-rewrite`) validated the design through a
working system: init, presenter, document, layout, metal render, text editor,
store, virtio drivers, PNG decoder. This plan rebuilds that system using the new
kernel's sync call/reply IPC, capability-based access control, and data/control
plane split.

Reference: `design/architecture.md` for the pipeline, `STATUS.md` for kernel
state, `design/research/kernel-userspace-interface.md` for the kernel ABI.

---

## Layering

Six layers, built bottom-up. Each layer depends only on layers below it.

### Layer 0: Foundation (complete)

Already built during kernel development:

- `userspace/abi` — raw syscall wrappers for all 30 syscalls
- `userspace/ipc` — SPSC ring buffers, seqlock state registers, typed messages
- `userspace/init` — skeleton: parses device manifest, spawns processes

### Layer 1: Service infrastructure

- **Protocol crate** (`userspace/protocol`) — message type definitions for every
  IPC boundary. Single source of truth for message layouts. One module per
  boundary.
- **Name service** (`userspace/servers/name`) — first service init spawns. Holds
  a table of (name → endpoint capability). Services register on boot, discover
  dependencies by name.
- **Init completion** — init as service manager: creates endpoints, spawns
  services in dependency order, passes capability handles.
- **Service pack tooling** (`tools/mkservices`) — packs service ELFs into a flat
  binary archive. Kernel maps it into init's address space. Bootstrap mechanism
  only — runtime service loading comes later via the filesystem.

### Layer 2: Drivers

- **virtio infrastructure** — virtqueue setup, MMIO register access, DMA buffer
  management. Thin library used by all virtio drivers.
- **virtio-console** — debug output. First driver, enables printf-debugging for
  everything above.
- **virtio-input** — keyboard and tablet. Produces input events on an event ring
  (SPSC ring buffer in VMO + event signal).
- **virtio-blk** — block device. Sync IPC interface: read/write sector requests.
- **Metal render driver** — compositor. Reads scene graph from shared VMO,
  renders via Metal commands over virtio device 22. The hypervisor deserializes
  and replays via the Metal API.

### Layer 3: Core libraries

Adapted from the old prototype (`git show v0.6-pre-rewrite:<path>`). These are
kernel-agnostic — pure algorithms and data structures. Update imports to use the
new `abi`/`ipc` crates, run old tests against new code.

- **Scene graph** (`userspace/libraries/scene`) — shared-memory node tree.
  First-child / next-sibling encoding. Double-buffered with generation counter.
- **Fonts** (`userspace/libraries/fonts`) — font table parsing, glyph metrics,
  shaping, rasterization. Used by layout (metrics only) and compositor
  (rasterization).
- **Drawing** (`userspace/libraries/drawing`) — 2D primitives: blending, blur,
  fill, line, gradient, NEON SIMD paths.
- **Render** (`userspace/libraries/render`) — scene graph rendering, clip masks,
  damage tracking, surface pool, frame scheduling.
- **Layout lib** (`userspace/libraries/layout`) — line breaking, text wrapping,
  glyph positioning.
- **Piece table** (`userspace/libraries/piecetable`) — text buffer with
  efficient insert/delete.
- **Animation** (`userspace/libraries/animation`) — timing curves, easing
  functions.
- **Filesystem** (`userspace/libraries/fs`) — COW block filesystem with
  snapshots.
- **Icons** (`userspace/libraries/icons`) — build-time SVG → vector path data.
- **Store** (`userspace/libraries/store`) — catalog serialization format.

### Layer 4: Core services

- **Document service** (`userspace/servers/document`) — sole writer to the
  document buffer. Applies edit requests from editors. Manages undo ring via COW
  snapshots. Communicates with store service for persistence.
- **Layout service** (`userspace/servers/layout`) — pure function: (document
  content + viewport state + font metrics) → positioned text runs. Reads
  document buffer (RO). Reads viewport state via seqlock register. Writes layout
  results to a dedicated VMO.
- **Presenter** (`userspace/servers/presenter`) — the OS service from
  `architecture.md`. Event loop, input routing, view state (cursor, selection,
  scroll, blink, animation), scene graph builder. Sole writer to the scene graph
  VMO.

### Layer 5: Leaf nodes

- **Text editor** (`userspace/editors/text`) — content-type leaf node. Receives
  editing key events, translates into write requests via sync IPC to the
  document service. Read-only shared memory mapping of the document buffer.
- **Store service** (`userspace/servers/store`) — COW filesystem over the block
  device. Provides persistence for the document service.
- **PNG decoder** (`userspace/servers/png-decoder`) — content-type decoder. Sync
  IPC: receives decode request, returns decoded pixel buffer.

### Layer 6: Integration

Full boot to a working text editor with the same UX as v0.6-pre-rewrite. Visual
baseline comparison via hypervisor screenshots.

---

## IPC Protocol Design

### Three transport patterns

Every IPC boundary in the system uses one of three patterns. These map directly
to the primitives in `userspace/ipc`:

1. **Sync call/reply** (via kernel endpoints) — for transactional operations
   where the caller needs a result. Client calls, blocks until server replies.
   Handles can be transferred in either direction.

2. **Event ring** (SPSC ring buffer in VMO + event signal) — for continuous
   unidirectional streams where the producer must not block on the consumer.
   Producer writes to ring, signals event. Consumer drains at its own pace.

3. **State register** (seqlock in VMO + event signal) — for continuously-updated
   state where the reader only needs the latest value. Writer updates atomically
   via seqlock. Reader retries on torn read. Event signal wakes the reader when
   new data is available.

### Transport mapping

| Boundary                 | Transport              | Direction              | Rationale                                                                                                                                                                                                                    |
| ------------------------ | ---------------------- | ---------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| init → service           | Sync call              | init → service         | One-shot config delivery. Init calls the service's bootstrap endpoint with config VMO + capability handles.                                                                                                                  |
| name service             | Sync call              | any → name service     | Register, Lookup, Unregister. Lookup replies include the target's endpoint capability via handle transfer.                                                                                                                   |
| input driver → presenter | Event ring             | driver → presenter     | Input is a continuous stream. Driver must not block waiting for presenter to process events.                                                                                                                                 |
| editor ↔ document        | Sync call              | editor → document      | Write requests are transactional. Editor sends insert/delete, blocks until document confirms.                                                                                                                                |
| presenter → editor       | Sync call              | presenter → editor     | Presenter dispatches input events to editor. Editor processes and replies.                                                                                                                                                   |
| document ↔ store         | Sync call              | document → store       | Persistence operations: commit, snapshot, restore, query. All request/response.                                                                                                                                              |
| document → presenter     | Event signal           | document → presenter   | Pure notification: "document changed, re-read the buffer." No payload.                                                                                                                                                       |
| presenter → layout       | State register + event | presenter → layout     | Viewport state (scroll, dimensions, font size) via seqlock. Event signals "recompute needed."                                                                                                                                |
| layout → presenter       | State register + event | layout → presenter     | Layout results (line info, visible runs) via seqlock in a dedicated VMO. Event signals "layout ready."                                                                                                                       |
| presenter → compositor   | State register + event | presenter → compositor | Scene graph in shared VMO. Double-buffered with generation counter. Presenter writes back buffer, bumps generation (release fence), signals event. Compositor reads front buffer (acquire fence). Zero syscalls on hot path. |
| compositor → GPU         | Virtqueue DMA          | compositor → GPU       | Hardware interface. Unchanged from old prototype.                                                                                                                                                                            |
| document ↔ decoder       | Sync call              | document → decoder     | Decode requests are request/response. Decoded pixels returned via VMO handle transfer.                                                                                                                                       |

### Name service protocol

Flat namespace. Names are short ASCII strings (max 32 bytes). Three operations:

```text
Register(name: [u8; 32], endpoint: Handle) → Ok | AlreadyExists
Lookup(name: [u8; 32]) → Ok(endpoint: Handle) | NotFound
Unregister(name: [u8; 32]) → Ok | NotFound
```

Handle transfer uses the kernel's native capability transfer (up to 4 handles
per IPC message). When the name service replies to Lookup, the target service's
endpoint capability is attached as a transferred handle.

Well-known names:

| Name            | Service                                                 |
| --------------- | ------------------------------------------------------- |
| `"name"`        | Name service (implicit — init passes this cap directly) |
| `"console"`     | virtio-console driver                                   |
| `"input"`       | Input event multiplexer                                 |
| `"blk"`         | virtio-blk driver                                       |
| `"render"`      | Metal render driver (compositor)                        |
| `"store"`       | Store service (filesystem)                              |
| `"document"`    | Document service                                        |
| `"layout"`      | Layout service                                          |
| `"presenter"`   | Presenter (OS service)                                  |
| `"editor.text"` | Text editor                                             |

### Bootstrap sequence

```text
1. Kernel creates init (the only process the kernel creates directly).
   Init receives: device manifest in a bootstrap VMO.

2. Init creates the name service endpoint.
   Init spawns the name service, passes it the endpoint to recv on.

3. Init spawns each service in dependency order, passing:
   - A capability to the name service endpoint (for register + discover)
   - A bootstrap VMO with service-specific config
   - Hardware-specific capabilities where needed (MMIO regions for drivers)

4. Each service boots:
   a. Reads config from bootstrap VMO
   b. Creates its own endpoint(s)
   c. Registers with name service: call(ns_ep, Register("myname", my_ep))
   d. Looks up dependencies: call(ns_ep, Lookup("dep")) → endpoint cap
   e. Sets up shared memory regions (VMOs) with dependencies
   f. Enters main loop

5. Init monitors children for crashes after all services are running.
```

Spawn order (respects dependency chains):

```text
1. name service       (no deps)
2. virtio-console     (no deps — enables debug output for everything after)
3. virtio-blk         (no deps)
4. virtio-input       (no deps)
5. metal-render       (no deps — reads scene graph VMO once wired)
6. store              (depends on: blk)
7. document           (depends on: store)
8. layout             (depends on: document — reads shared doc buffer)
9. presenter          (depends on: layout, document, input, render)
10. text-editor       (depends on: document, presenter)
```

### Protocol crate structure

```text
userspace/protocol/
  src/
    lib.rs            — re-exports, common types, message type range constants
    name_service.rs   — Register, Lookup, Unregister payloads
    bootstrap.rs      — ServiceConfig, bootstrap VMO layout
    input.rs          — KeyEvent, PointerEvent, PointerButton, modifier flags
    edit.rs           — WriteInsert, WriteDelete, WriteDeleteRange,
                        CursorMove, SelectionUpdate, StyleApply
    store.rs          — Commit, Snapshot, Restore, Query, Read payloads
    view.rs           — DocChanged, DocLoaded, ImageDecoded
    decode.rs         — DecodeRequest, DecodeResult
```

No modules for layout↔presenter or presenter↔compositor — those boundaries use
shared memory with layouts defined in their respective library crates (layout
lib, scene crate).

---

## Shared Memory Regions

Six VMOs carry bulk data between services. All created by init (or the dependent
service) and shared via capability transfer through the name service or
bootstrap.

| VMO             | Writer           | Reader(s)                          | Content                                                          |
| --------------- | ---------------- | ---------------------------------- | ---------------------------------------------------------------- |
| Document buffer | Document service | Layout, Presenter, Editor (all RO) | 64-byte header (content_len, cursor_pos, format) + content bytes |
| Layout results  | Layout service   | Presenter (RO)                     | Header + LineInfo array + VisibleRun array. Seqlock-protected.   |
| Viewport state  | Presenter        | Layout (RO)                        | Scroll offset, viewport dimensions, font size. Seqlock register. |
| Scene graph     | Presenter        | Compositor (RO)                    | Node tree + data buffer. Double-buffered, generation counter.    |
| Content region  | Init (bootstrap) | Layout, Compositor (RO)            | Font data, decoded image pixels. Append-only with content IDs.   |
| Pixel buffer    | Compositor       | GPU driver (RO)                    | Rendered frame. Double-buffered for vsync.                       |

---

## Reuse vs. Rewrite

| Component             | Decision                         | Rationale                                                                                                                 |
| --------------------- | -------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `sys` (syscalls)      | **Rewritten** as `userspace/abi` | New kernel ABI. Complete.                                                                                                 |
| `ipc` (ring buffers)  | **Rewritten** as `userspace/ipc` | New kernel primitives. Complete.                                                                                          |
| `protocol` (messages) | **Rewrite**                      | Transport model changed (sync call/reply + shared memory, no async channels). Message payloads similar but not identical. |
| `virtio`              | **Rewrite**                      | Thin, tightly coupled to old MMIO abstractions and old sys crate.                                                         |
| `scene`               | **Adapt**                        | Data structures are kernel-agnostic. Adjust buffer management for new VMO model.                                          |
| `fonts`               | **Adapt**                        | Pure font parsing, metrics, rasterization. No kernel dependency. Solid code.                                              |
| `drawing`             | **Adapt**                        | Pure 2D math, NEON SIMD. Zero kernel dependency.                                                                          |
| `render`              | **Adapt**                        | Scene rendering, clip masks, damage tracking. Adjust for new scene graph format.                                          |
| `layout` (lib)        | **Adapt**                        | Line breaking, shaping. Pure computation.                                                                                 |
| `piecetable`          | **Adapt**                        | Text buffer data structure. No external dependencies.                                                                     |
| `animation`           | **Adapt**                        | Timing/easing. Trivial, no kernel dependency.                                                                             |
| `fs`                  | **Adapt**                        | COW filesystem. Block-level, kernel-agnostic.                                                                             |
| `icons`               | **Keep**                         | Build-time SVG → path data. Pure data generation.                                                                         |
| `store`               | **Adapt**                        | Catalog serialization. May improve but algorithmically sound.                                                             |

"Adapt" means: copy from `v0.6-pre-rewrite` tag, update imports to use new
`abi`/`ipc` crates, fix interface mismatches, verify with old tests ported to
new infrastructure.

---

## Build Order

Optimized for verification and autonomous execution. Each step produces a
testable artifact.

### Phase 1: Protocol + Service Infrastructure

| Step | Deliverable                                 | Verification                                                                                                                       |
| ---- | ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| 1.1  | Protocol crate — all message types          | `cargo test`: serialization round-trips, size assertions                                                                           |
| 1.2  | Service pack tool (`mkservices`)            | Build tool, produce a valid pack from test ELFs                                                                                    |
| 1.3  | Init completion — spawn services, pass caps | Integration test under hypervisor: init spawns child, child calls syscall, exits cleanly                                           |
| 1.4  | Name service                                | Integration test: init spawns name service + two children. Child A registers, child B looks up A's endpoint, calls it, gets reply. |

### Phase 2: Drivers

| Step | Deliverable                      | Verification                                                                  |
| ---- | -------------------------------- | ----------------------------------------------------------------------------- |
| 2.1  | virtio-console                   | Integration test: child process prints to console, hypervisor captures output |
| 2.2  | virtio-input                     | Event script sends synthetic keystrokes, verify receipt via console output    |
| 2.3  | virtio-blk                       | Read/write sector round-trips, verified via console output                    |
| 2.4  | Metal render driver (compositor) | Render a solid-color scene graph, compare screenshot against reference        |

### Phase 3: Core Libraries

| Step | Deliverable                         | Verification                                                                  |
| ---- | ----------------------------------- | ----------------------------------------------------------------------------- |
| 3.1  | Scene graph crate                   | Host-target `cargo test`: serialization, tree operations, generation counter  |
| 3.2  | Fonts crate                         | Host-target tests: metric extraction, glyph ID lookup, reference values       |
| 3.3  | Drawing crate                       | Host-target tests: pixel-exact blending, blur, fill against reference images  |
| 3.4  | Render crate                        | Host-target tests: scene walk, clip masks, damage rects                       |
| 3.5  | Piece table                         | Host-target tests: insert, delete, iteration, stress                          |
| 3.6  | Layout lib                          | Host-target tests: line breaking, wrapping, positioned runs against reference |
| 3.7  | Animation, filesystem, icons, store | Host-target tests: timing curves, COW round-trips, icon path data             |

### Phase 4: Core Services + Leaf Nodes

| Step | Deliverable      | Verification                                                          |
| ---- | ---------------- | --------------------------------------------------------------------- |
| 4.1  | Store service    | Integration: write document to filesystem, read back, verify contents |
| 4.2  | Document service | Integration: send edit requests, verify buffer state, test undo/redo  |
| 4.3  | Layout service   | Integration: provide document + viewport, verify positioned runs      |
| 4.4  | Presenter        | Integration: wire to layout + compositor, verify scene graph output   |
| 4.5  | Text editor      | Integration: send keystrokes, verify edits appear in document buffer  |

### Phase 5: Integration

| Step | Deliverable     | Verification                                                                 |
| ---- | --------------- | ---------------------------------------------------------------------------- |
| 5.1  | Full boot       | All services start, presenter builds initial scene graph, compositor renders |
| 5.2  | Visual baseline | Screenshot comparison against old prototype baselines                        |
| 5.3  | Input-to-pixels | Type text → see it rendered. Click → cursor moves. Full pipeline exercised.  |

---

## Build System

Cargo workspace with cross-compilation. Userspace crates target
`aarch64-unknown-none` (bare-metal, no_std). Host-target test crates target
`aarch64-apple-darwin` for libraries with platform-independent logic.

```text
Cargo.toml (workspace root)
  members:
    kernel/
    userspace/abi/
    userspace/ipc/
    userspace/protocol/
    userspace/init/
    userspace/servers/name/
    userspace/servers/document/
    userspace/servers/layout/
    userspace/servers/presenter/
    userspace/servers/store/
    userspace/servers/png-decoder/
    userspace/drivers/console/
    userspace/drivers/input/
    userspace/drivers/blk/
    userspace/drivers/metal-render/
    userspace/editors/text/
    userspace/libraries/scene/
    userspace/libraries/fonts/
    userspace/libraries/drawing/
    userspace/libraries/render/
    userspace/libraries/layout/
    userspace/libraries/piecetable/
    userspace/libraries/animation/
    userspace/libraries/fs/
    userspace/libraries/icons/
    userspace/libraries/store/
    tools/mkservices/
```

The Makefile gets new targets:

- `make userspace` — build all userspace crates for aarch64-unknown-none
- `make pack` — build mkservices, produce service pack
- `make boot` — build kernel + service pack, run under hypervisor
- `make integration-userspace` — run userspace integration tests under
  hypervisor

---

## Open Questions (deferred, not blocking)

1. **Content Region lifecycle.** The old prototype loaded fonts in init and
   populated the Content Region there. The design says the document service
   should own this. Defer to when the document service is functional.

2. **Multiple editors.** The old prototype had text-editor and rich-editor.
   Start with text-editor only. Rich editor is a second leaf node behind the
   same interface.

3. **9p host share.** The old prototype supported loading assets from the host
   filesystem via virtio-9p. Defer until the native block device path works.

4. **Service restart.** The name service enables dynamic re-registration after a
   crash. Implementing restart policy is deferred — init monitors children but
   doesn't restart them yet.
