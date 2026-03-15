# Exploration Journal

A research notebook for the OS design project. Tracks open threads, discussion backlog, and insights across sessions. This is the "pick up where you left off" document.

---

## Boot Display via ramfb (2026-03-15)

**Status:** Design settled. Ready to implement.

### Context

Currently the OS outputs diagnostic text to the host terminal (serial) and renders the UI in the QEMU window (virtio-gpu). The QEMU window is blank during boot because virtio-gpu requires userspace driver initialization. On real ARM hardware with UEFI, firmware provides an immediate framebuffer (EFI GOP) that the kernel can write to from its first instruction — boot text on the physical display is the natural default.

### Design Decisions

**1. Use QEMU's `ramfb` device for early boot display.**
Add `-device ramfb` to QEMU. The kernel writes a framebuffer config to QEMU's `fw_cfg` memory-mapped interface early in boot, then writes pixels directly to a memory region. No virtio negotiation required. This mirrors the real-hardware pattern of firmware framebuffer → GPU driver handoff.

**2. Serial and ramfb are independent, simultaneous channels.**
They serve different audiences. Both are active during boot; neither suppresses the other.

- **Serial (host terminal):** Full structured diagnostic log. Everything verbose — memory map, page table setup, SMP core bringup, interrupt controller init, subsystem lifecycle, timing data, warnings/errors with full context. The developer/operator channel. Survives display driver crashes.
- **ramfb (QEMU window / physical display):** Curated user-facing narrative. A small number of milestone messages conveying boot progress at a human-meaningful level. Not implementation details.

**3. Curated milestone messages (not wall-of-text, not just a logo).**
Each message corresponds to a phase where, if it hangs, the user knows _where_ it hung. That's the practical utility beyond aesthetics. Approximate milestones:

1. "Starting up" — kernel entry, MMU, memory
2. "Starting processors" — SMP bringup (perceptible latency on real multi-core hardware)
3. "Connecting devices" — virtio probing, storage, input (can stall on missing/slow hardware)
4. "Loading fonts" — font file I/O; signals "about to show you something"
5. "Preparing your workspace" — OS service, compositor init, document pipeline ready
6. UI appears (compositor takes over via virtio-gpu)

**4. Growing list with visual dimming.**
Messages accumulate as a list. The current (most recent) message renders bright; previous messages dim to grey. This provides:

- Forward-motion feel without explicit animation — contrast shift as each new line appears
- "Where did it stop" diagnostic value — the full list is visible if boot hangs
- Visual weight stays on the current phase, not the history

**5. Hard cut to compositor.**
When virtio-gpu takes over, the ramfb content is simply replaced. No fade, no transition. This matches what real hardware does (GPU driver takes over the display). Revisit later only if it feels wrong in practice.

### Implementation Notes

- The font rasterizer and drawing library already exist — rendering text to the ramfb is straightforward.
- ramfb initialization is a kernel-level concern (fw_cfg write). The milestone messages could be driven by kernel or early userspace — TBD during implementation.
- The milestone list should be easy to update as the boot sequence evolves.

---

## Resolution-Independent Rendering + Dirty-Rect Optimization (2026-03-15)

**Status:** Design session in progress. Breadboarding phase.

### Problem

The UI is noticeably laggy at host-native resolution (3456×2234 on Retina). Root cause: the compositor repaints the entire 31 MB framebuffer and the GPU driver transfers the entire 31 MB through virtio MMIO on every frame — even when only a cursor blink or single keystroke changed. The dirty rect infrastructure already exists end-to-end (DirtyRect type, DamageTracker, PresentPayload with 6 rects, GPU driver partial TRANSFER_TO_HOST_2D) but the compositor always sends `rect_count: 0` (full-screen transfer).

Additionally, the scene graph uses physical pixel coordinates. The OS service receives `fb_width`/`fb_height` from init and lays out directly to those values. There is no logical/physical coordinate distinction — layout is resolution-specific.

### Design Discussion

**Scene graph coordinate model — logical vs physical:**

Two options: (A) scene graph in logical coordinates (points), compositor applies scale factor; or (B) scene graph in physical pixels, OS service pre-applies scale.

Decision: **Logical coordinates (option A).** Rationale:

- Isolate uncertain decisions behind interfaces (founder claim). The OS service shouldn't know or care about the display's physical resolution. It thinks in points.
- The scene graph is rebuilt on every state change anyway (keystroke, scroll, cursor move), so "resolution change requires a scene rebuild" isn't an extra cost.
- Clean separation: OS service declares _what_ to render (in its coordinate space), compositor decides _how_ (at the physical resolution it knows).
- Font rendering: compositor rasterizes glyphs at physical pixel size (`logical_font_size × scale_factor`). Optical sizing uses physical DPI. Both are compositor concerns.
- Integer precision at i16 logical coordinates with 2× scale: every logical pixel maps to 2 physical pixels. Sub-pixel glyph positioning is a compositor concern (during rasterization), not a scene graph concern. macOS uses the same model.
- Pixel-snapping for borders/dividers: compositor rounds to nearest physical pixel. Not purely multiply-by-scale — it makes snapping decisions too.

The scale factor becomes compositor config (alongside DPI), received from init. The OS service never sees it.

**Dirty-rect optimization — scene diffing vs damage declaration:**

Two approaches: (A) compositor diffs old vs new scene graph, derives dirty rects automatically; or (B) OS service declares damage regions explicitly (Wayland model).

Decision: **Scene diffing (option A).** Rationale:

- The scene graph is a flat array of fixed-size repr(C) nodes — diffing is `memcmp` per slot. Cheap.
- As the OS service grows in complexity (compound documents, multiple editors, layout engine), requiring it to perfectly track damage is a maintenance burden that compounds. Diffing absorbs that — the OS service just rebuilds the scene graph however it wants, and the compositor figures out what changed.
- Same reasoning as React: developers stopped manually tracking DOM mutations and let the framework diff. The expensive operation is pixel rendering, not scene construction.
- The compositor already keeps the scene in shared memory. Double-buffering the scene (keeping the previous version) adds one memcpy of the node array per frame — negligible vs the rendering cost.

### Incremental Delivery

Pieces chain together, each independently shippable and testable:

**Piece 1 — Activate existing dirty-rect GPU transfer.**
The compositor already repaints everything. Add a scene diff step: compositor keeps a copy of the previous scene graph, compares per-node, computes bounding rects of changed nodes, passes dirty rects in PresentPayload. The GPU driver already handles `rect_count > 0`. Highest leverage (cuts 31 MB MMIO transfer to a few KB for a keystroke), lowest risk (no interface changes, no scene graph format changes).

But: the compositor also repaints the entire framebuffer from the scene graph. Even with partial GPU transfer, every frame still touches every pixel in the back buffer. Piece 2 addresses this.

**Piece 2 — Dirty-rect clipped rendering.**
Compositor clips its rendering to the dirty region. `render_scene` currently takes a full-framebuffer clip rect — narrow it to the dirty rect union. Nodes outside the dirty region are skipped. Overlapping nodes within the dirty region are repainted (back-to-front within the rect). Requires copying the previous framebuffer's clean regions to the back buffer (or using the same buffer with dirty-rect writes only).

**Piece 3 — Logical coordinate model.**
Change scene graph Node.x/y/width/height to logical coordinates. Add scale_factor to compositor config. Compositor multiplies all positions and sizes by scale_factor during rendering. OS service lays out in points. Font sizes in scene graph are logical; compositor computes `physical_px = logical_size × scale_factor` for rasterization. This is the interface/structural change.

### Breadboard — Piece 1: Scene Diff + Partial GPU Transfer

```text
compositor render loop (current):
  wait for scene update
  render_scene(back_fb, scene_graph)         ← repaints everything
  send MSG_PRESENT(buffer_index, rect_count=0) ← full transfer
  swap buffers

compositor render loop (piece 1):
  wait for scene update
  diff(prev_scene_nodes, curr_scene_nodes)   ← NEW: per-node memcmp
    → collect changed node indices
    → compute bounding rects of changed nodes (in abs coords)
    → union into dirty region (up to 6 DirtyRects)
  render_scene(back_fb, scene_graph)         ← still repaints everything (piece 2 fixes this)
  send MSG_PRESENT(buffer_index, rect_count=N, rects=[...])  ← partial transfer
  memcpy curr_scene_nodes → prev_scene_nodes ← save for next diff
  swap buffers
```

**Scene diff algorithm:**

```
fn diff_scenes(
    prev: &[Node; MAX_NODES],
    curr: &[Node; MAX_NODES],
    node_count: usize,
) -> DamageTracker {
    let mut damage = DamageTracker::new(fb_width, fb_height);
    for i in 0..node_count {
        // Fixed-size repr(C) — byte comparison is correct and fast.
        let prev_bytes = &prev[i] as *const Node as *const [u8; NODE_SIZE];
        let curr_bytes = &curr[i] as *const Node as *const [u8; NODE_SIZE];
        if prev_bytes != curr_bytes {
            // Node changed. Compute its absolute bounding rect
            // by walking parent chain (or: store abs coords in prev pass).
            let rect = abs_bounds(curr, i);
            damage.add(rect.x, rect.y, rect.w, rect.h);
            // Also damage the OLD position if the node moved.
            let old_rect = abs_bounds(prev, i);
            damage.add(old_rect.x, old_rect.y, old_rect.w, old_rect.h);
        }
    }
    damage
}
```

**Pressure point — computing absolute bounds:** Nodes store positions relative to parent. To get the absolute bounding rect for a changed node, you need to walk up the parent chain. But the scene graph uses left-child/right-sibling (no parent pointer). Options:

1. Pre-compute absolute positions during the render walk and cache them.
2. Add a `parent: NodeId` field to Node (4 bytes, increases node size).
3. Walk the tree once to build a parent map (array of NodeId, indexed by node index).

Option 3 is cheapest — one pass over the node array building `parent[i]` from `first_child`/`next_sibling`, then `abs_bounds` walks up via `parent[]`. The parent map is the same size as the node array (512 × 2 bytes = 1 KB) and rebuilt each frame.

**Pressure point — nodes added/removed:** If the node count changes between frames, new nodes have no prev entry and removed nodes leave stale prev entries. Handle by: if `curr_node_count != prev_node_count`, dirty the entire screen (fall back to full repaint). This is rare (only on document open/close, not on typing). Alternatively, diff up to `min(prev_count, curr_count)` and dirty any nodes beyond that range.

**Pressure point — data buffer changes:** A node's text content can change without the node struct changing (if the text is referenced by DataRef with same offset/length but different bytes). This means struct-level memcmp can miss text edits. Fix: include a content hash in the node, or always diff data buffer contents for Text/Path nodes. The simpler approach: the OS service already writes new text at new data buffer offsets (append-only within a frame), so the DataRef offset will differ, which the memcmp catches.

### What's Not Covered Yet

- Piece 2 implementation details (dirty-rect clipped rendering, overlapping node handling)
- Piece 3 implementation details (logical coordinate model, scale factor plumbing)
- Interaction between dirty-rect rendering and subpixel font rendering (dirty rect slicing through a glyph)
- Whether the DamageTracker's 6-rect limit is sufficient or needs a smarter merge strategy

---

## Scene Scroll Fix + Kernel TPIDR Race Fix (2026-03-14)

**Status:** Two bugs fixed. Both committed.

### Bug 1: Text overflow + cursor misalignment in scene builder

**Symptom:** Text runs extended past the viewport bottom (no scroll clipping). Cursor and selection rects positioned at absolute coordinates, misaligned when scrolled.

**Root cause:** `build_editor_scene` in core's `scene_state.rs` created text runs at absolute y positions (0, 20, 40...) without applying scroll. The compositor's `node.scroll_y` only offset children as a pixel value, but the runs were already at absolute positions. Cursor/selection used absolute `line * line_height` without subtracting scroll offset.

**Fix:** Extracted layout helpers from `scene_state.rs` to scene library as public functions: `layout_mono_lines`, `byte_to_line_col`, `line_bytes_for_run`, `scroll_runs`. Core now calls `scroll_runs(all_runs, scroll_lines, line_height, content_h)` to filter runs to the visible viewport and adjust y positions. Cursor y = `cursor_line * line_height - scroll_px` (viewport-relative). Selection rects clipped to viewport bounds. Compositor renders with `scroll_y = 0` and `CLIPS_CHILDREN` — core pre-applies all scrolling.

**Tests:** 11 new tests: monospace layout (basic, trailing newline, soft wrap, empty), byte-to-line-col (basic, soft wrap, cursor consistency with layout), scroll filtering (no scroll, filters above viewport, cursor at bottom, empty text). 943 total pass.

**Files:** `libraries/scene/lib.rs` (+125 lines), `services/core/scene_state.rs` (-19 net), `test/tests/scene.rs` (+140 lines).

### Bug 2: Kernel EC=0x21 instruction abort under SMP (TPIDR_EL1 race)

**Symptom:** Intermittent kernel crash — EC=0x21 (instruction abort at current EL) with ELR=FAR=0x0A003A00 (virtio MMIO physical address range, level 0 translation fault). Only manifested under concurrent load with 4 SMP cores and 5+ processes.

**Root cause:** `schedule_inner` parks the old thread in the ready queue and returns the new thread's context pointer. But `TPIDR_EL1` (used by `save_context` in exception.S to locate the write target) was only updated by exception.S _after_ the Rust handler returned — which is after the `IrqMutex` lock drops and re-enables IRQs. If a pending timer IRQ fires in that window (between lock release and the `msr tpidr_el1` in exception.S), `save_context` reads the stale TPIDR and overwrites the **old** thread's Context with kernel-mode state (SPSR=EL1h, ELR=kernel addr, SP from the wrong thread's stack). When the old thread is later scheduled from the ready queue, `eret` restores EL1 mode with a garbage low address as ELR, causing the instruction abort.

**Why the address was `0x0A003A00`:** This is a user-range VA (low address). With SPSR=EL1h, the eret returns to EL1 which uses TTBR0 for lower-half VA walks. If TTBR0 is the empty L0 table (kernel/idle thread) or any table without a mapping at that address, the walk fails at level 0 — exactly matching ESR=0x86000004 (IFSC=4, level 0 translation fault).

**Fix:** Set `TPIDR_EL1` to the new thread's Context pointer inside `schedule_inner`, while the scheduler lock is held and IRQs are masked. This ensures `save_context` always writes to the correct (current) thread's Context, even if an IRQ fires immediately after the lock drops. The `msr tpidr_el1` in exception.S is now redundant but kept as defense-in-depth. Also added `validate_context_before_eret` — checks ELR/SPSR/SP consistency before every eret return (catches EL1-to-user-VA, EL0-to-kernel-VA, and EL1-with-bad-SP).

This is the same class of bug as Fix 5 (aliasing UB) and Fix 6 (nomem on DAIF) — an SMP timing window that only manifests under concurrent scheduling pressure. The window was ~3-5 instructions wide (lock release → TPIDR write in asm), but with 4 cores running 250 Hz timers, it was hittable.

**Stress tested:** 3000 keys at 1ms intervals, 5 processes on 4 SMP cores. No crash. 943 tests pass.

**Files:** `kernel/scheduler.rs` (+21 lines), `kernel/main.rs` (+71 lines), `kernel/exception.S` (+6 lines comments).

---

## Compositor Split + Scene Graph Design (2026-03-13)

**Status:** Design conversation in progress. Key architectural decisions settling.

### Context

Reviewed the full userspace architecture above the kernel. The compositor (`services/compositor/`, 2260 lines) had accumulated two fundamentally different responsibilities: OS service work (document ownership, input routing, edit protocol) and rendering work (surface management, compositing, GPU presentation). These need to separate into distinct processes.

### Protocol Crate (completed)

Created `libraries/protocol/` as the single source of truth for all IPC message types and payload structs. 8 modules organized by protocol boundary (device, gpu, input, edit, compose, editor, present, fs). All 22 message type constants centralized; zero duplicates remain across the codebase. Net -333 lines.

Also fixed test infrastructure: libraries now have proper Cargo.toml files so the test crate uses normal Cargo dependencies instead of `#[path]` source includes. This eliminated a duplicate `DirtyRect` definition that existed solely to work around the test build limitation.

### Design Decisions Reached

**1. OS service and compositor are separate processes.**
Not a code-organization split — a process boundary with IPC. The OS service owns document semantics; the compositor owns pixels. This matches the design principle of simple connective tissue between components, and validates the IPC protocol at a real boundary.

**2. The interface between them is a scene graph.**
Evaluated three options:

- **Buffer-based (Wayland model):** OS service renders content into pixel buffers, compositor just composites. Simple but puts all rendering work in the OS service.
- **Scene-graph-based (Fuchsia Scenic, Core Animation, game engines):** OS service sends a tree of typed visual nodes, compositor renders and composites. More capable.
- **Command-based (X11, Plan 9 draw):** OS service sends drawing commands. Historical — every modern system moved away from this.

Chose scene graph because:

- "OS renders everything" means the OS _layer_ (everything in `/services` and `/libraries`), not specifically the OS service process. The compositor IS part of the OS.
- Layout and compositing are the same pipeline: document structure → positioned visual elements → pixels. The scene graph is the intermediate representation between those two stages.
- It naturally supports compound documents (Decision #14): the document's spatial/temporal/logical relationships compile to a scene tree.
- It doesn't artificially prevent game-engine-level rendering later (3D, animation, transforms) — you just add node types.

**3. The scene graph is NOT the document structure — it's a compiled output.**
The document model has semantic content (logical relationships, metadata, temporal sync) that the compositor doesn't need. The screen has visual elements (chrome, cursor, selection) that aren't in any document. The OS service _compiles_ document structure into a scene graph, like a compiler turns source into machine code.

**4. The scene graph lives in shared memory.**
Written by the OS service, read by the compositor. The compositor is a pure function from scene graph to pixels. No scene graph state inside the compositor — if it crashes, the graph is still there, just restart and re-render. Same pattern as the existing document buffer.

**5. The screen is the root compound document.**
The entire visual output can be thought of as a compound document with system chrome and document content as its parts. The compositor doesn't know it's rendering "the screen" — it renders a scene graph, and the screen is just the root node. No special case for "the desktop." Multi-document views are just different layout types for the root document.

### Research: Prior Art Surveyed

- **Fuchsia FIDL + Scenic:** Full IDL for IPC (FIDL), scene-graph compositor (Scenic). Validated the typed-channel approach and scene-graph-at-OS-level pattern.
- **Singularity OS contracts:** State-machine-typed channels. Too complex for single-language Rust OS, but the insight that channels should be typed per-protocol is applicable.
- **seL4 CAmkES:** Framework-generated typed interfaces. Userspace typing over untyped kernel transport — same pattern we're following.
- **Wayland:** Buffer-based compositor protocol. Good surface lifecycle model but doesn't match "OS renders everything."
- **Core Animation:** Property-based layer tree with animations. The hybrid model (pre-rendered backing stores in a scene tree) is close to the right answer.
- **Game engines (Unity, Godot, Bevy):** Scene tree of typed nodes with transforms and components. The compositor is essentially a document rendering engine — same structure.

### Additional Decisions (continued discussion)

**6. Interaction state (cursor, selection) belongs to the View, not to individual document nodes.**
The cursor navigates the compound document's flow layout, crossing content-type boundaries. The text editor owns "cursor within text" but the OS service owns "cursor within the compound document." When the cursor reaches the boundary of one content part (e.g., top of a paragraph), the OS service consults the layout to determine what's above/below and moves focus accordingly. Editors signal boundary-hit; the OS service navigates between content parts.

**7. Focus path model for compound document editing.**
The View tracks a focus path (stack) through the compound document tree. Each level has its own cursor state. "Zooming into" a sub-document pushes onto the stack; returning pops. Whether child cursor state is preserved on pop is a UX policy decision, not structural -- the architecture supports both.

**8. One node type with content variants (Core Animation model).**
Rejected separate types for Container, Text, Image, Path. Instead: one Node type with a rich visual base and an optional Content variant (None, Text, Image, Path). Inspired by CALayer.

Rationale:

- In compound documents, almost every container also needs visual properties (background, border, corner radius). Separate types force wrapper nodes everywhere.
- Fixed-size nodes in shared memory: one flat array, one allocation strategy, indices work uniformly.
- Core Animation proved this works at scale -- CALayer handles 95% of cases.
- Starting with four types, you'd gradually merge them anyway as each gains the others' properties.

Node carries: tree links (first_child, next_sibling as indices), geometry (x, y, width, height relative to parent), scroll_y, visual decoration (background, border, corner_radius, opacity), flags (clips_children, visible), and a content variant.

Content variants:

- None: pure container
- Text: string ref (offset+len into data buffer), font_size, color
- Image: pixel data ref (shm offset), source dimensions
- Path: command ref (offset+len into data buffer), fill, stroke

Variable-length data (text strings, path commands) lives in a separate data buffer region of shared memory, referenced by offset+length from the node.

**9. Relative positioning with scroll_offset solves scrolling immediately.**
Children are positioned relative to parent's content area. Scrolling = changing one scroll_y value on a clipping container. Compositor offsets all children during render. No OS service round-trip. Full declarative layout (flex, column, row) is the right end state but can be added later as a field on Node without structural change.

**10. The View is an OS service concept, not a scene graph concept.**
The View (focus path, cursor state, document binding) lives entirely in the OS service. The OS service translates View state into scene graph mutations (position cursor node, update text content, set scroll_y). The compositor never knows about Views, documents, or editing. It renders a tree of Nodes. Clean separation: scene graph = rendering interface, View = document interaction model.

**11. ~~Compositor owns text layout.~~ REVERSED (2026-03-14): OS service owns text layout.**
~~The compositor has the font rasterizer; it also owns line breaking, word wrapping, and glyph positioning.~~

**Revised (2026-03-14):** The OS service owns all text layout (line breaking, wrapping, glyph positioning, hit testing). The compositor is content-agnostic — it renders positioned visual elements without knowing what "text" is. This is consistent with the architecture: the OS service will house the layout engine for all content types (text, images, compound documents), so text is not a special case. The scene graph carries pre-laid-out content (positioned glyphs or pre-computed line breaks), not raw strings. The OS service needs font metrics (advance widths, line height — a few hundred bytes) but not the full rasterized glyph cache.

Prior art is unanimous: Core Animation, Wayland, Fuchsia Scenic, web browsers, game engines — all put text layout above the compositor, never inside it. Reason: text layout is content understanding, and the design axiom "OS natively understands content types" means that understanding belongs in the OS service, not the pixel pump.

Cursor and selection remain properties of the Text content variant for now. The OS service knows glyph positions (it did the layout) so it can position cursor and selection rects directly. The compositor renders them without understanding what they mean.

**TODO:** Redesign the Text content variant to carry positioned/pre-laid-out text instead of raw strings + width constraints. The cursor and selection may become positioned rects (Path nodes) rather than byte offsets, since the OS service can compute their pixel positions.

**TODO:** Better name for "View" (the thing that holds document + focus path + overlays).

### Scene Graph TODOs

- **Ctrl+Tab image viewer**: Scene graph only builds text editor view. Need Image content node support or hybrid approach for image mode. Medium effort (~1hr).
- **Text editor keystrokes**: Up/Down arrows (line navigation), Cmd+arrow (start/end of line/document), Shift+key (uppercase), Delete (forward delete). ~100-150 lines in text-editor/main.rs. All mechanical — patterns established.

### Open Questions

1. **How do typed channels work?** `Channel<P>` API design, multiplexing across different protocol types with `wait()`.
2. ~~**Shared memory double-buffering.**~~ **Done (2026-03-14).** `DoubleWriter`/`DoubleReader` in scene library. Two `SCENE_SIZE` regions, generation-counter-based swap with release/acquire fences. Compositor `SceneState` migrated.
3. ~~**Scene graph shared memory layout.**~~ **Done (2026-03-14).** `SceneWriter`/`SceneReader` in scene library. Header (64 B) + node array (512 × Node) + data buffer (64 KiB). 34 host-side tests.
4. **Text content variant redesign — settled on A' (positioned text runs).** The OS service sends runs of text with a starting (x, y) and pre-computed per-character advance widths. Each run is a contiguous sequence of characters on one line. The compositor walks the run, rasterizing each glyph at `x + sum(advances[0..i])`. The OS service has the font metrics (advance widths, kerning — small data). The compositor has the glyph cache (rasterized coverage maps — big data). Layout lives entirely in the OS service. Rasterization lives entirely in the compositor. Cursor and selection become positioned pixel-coordinate rects (regular scene graph nodes with backgrounds), not byte offsets.

   For monospace text (current prototype), per-character advances are uniform, so a single `char_advance: u16` value suffices. For proportional text (future), the OS service sends a `DataRef` to an array of per-glyph u16 advances in the data buffer.

   **Implementation sequence:** Redesign Content::Text first (with tests), then proceed to the process split. The current Content::Text with byte-offset cursor/selection bakes layout into the compositor — changing the representation first makes the split clean.

### Implications for Existing Decisions

- **Decision #11 (Rendering Technology):** The "existing web engine" leaning may shift. If the compositor is a scene-graph renderer, a web engine becomes a content-type translator that produces scene graph nodes, not the rendering substrate. The scene graph IS the rendering engine.
- **Decision #15 (Layout Engine):** The layout engine compiles document structure into the scene graph. The "CSS for spatial" option still works — CSS layout produces positioned nodes that become scene graph nodes. But the layout engine is now clearly upstream of the compositor, not inside it.
- **Decision #14 (Compound Documents):** The three-axis relationship model (spatial, temporal, logical) maps to the scene graph. Spatial relationships become positions/transforms. Temporal and logical relationships may need scene graph support beyond static trees (animation timelines, visibility state).

---

## Kernel Bug Investigation Follow-up (2026-03-13)

**Status:** Resolved. Root cause of the EC=0x21 crash identified and fixed in 2026-03-14 session (Fix 17: TPIDR_EL1 race in `schedule_inner`). Fixes 12-16 below addressed contributing factors and separate bugs found during the same investigation.

**Context:** User reported kernel crash (EC=0x21, ELR=0x0) during normal typing speed (not rapid input as in the 2026-03-11 crash). The crash signature was identical — instruction abort at EL1 with null ELR — but the trigger conditions differed. The scene graph compositor migration had changed the userspace event loop structure.

### Bugs Found and Fixed

**Fix 12: Stale waiter registrations (correctness bug).**
When `sys_wait` takes the `BlockResult::Blocked` path, the thread is context-switched away. Unfired handle registrations (channel, timer, interrupt, thread exit, process exit waiters) remained live even after the thread was woken. On subsequent signals to those handles, the stale registrations could cause: (a) spurious wakeups with incorrect return values, (b) redundant wake attempts. The spurious wakeup issue explains the "spurious wakeup from sys_wait" bug noted in the Virtio-9P section — init's `wait(&[channel])` returning before the response was the stale waiter bug in action.

Fix: Added `stale_waiters` field to Thread. `complete_wait_for` now copies unfired entries to `stale_waiters` before clearing `wait_set`. At the start of the next `sys_wait`, stale waiters are unregistered from all handle types. Added `scheduler::take_stale_waiters()` to support this deferred cleanup pattern.

**Fix 13: Per-ASID TLB invalidation (performance + correctness hardening).**
`swap_ttbr0` used `TLBI VMALLE1IS` which flushes ALL TLB entries (both TTBR0 and TTBR1) across ALL cores on every context switch between different address spaces. This unnecessarily flushed kernel I-TLB entries on all cores, causing every core to re-walk page tables for kernel code. Under high context-switch rates (~400/sec), this created sustained TLB pressure that could transiently affect instruction fetch timing. While not a correctness bug per se, the unnecessary full flush was wasteful and the per-ASID alternative (`TLBI ASIDE1IS`) was already used correctly elsewhere in `address_space.rs::invalidate_tlb()`.

Fix: Changed `swap_ttbr0` to use `TLBI ASIDE1IS, <old_asid>` — invalidates only the outgoing ASID's TLB entries. Kernel TTBR1 entries and other processes' TTBR0 entries are preserved.

**Fix 14: Diagnostic assertions in exception handlers.**
Added `debug_assert` checks for null context pointers in `irq_handler`, `svc_handler`, and `user_fault_handler`. These will catch the null pointer at its source rather than manifesting as EC=0x21 in the exception vector. Also added a null check on the `schedule_inner` return value.

### Analysis

The original crash (ELR=0x0 at EL1) and the later variant (ELR=0x0A003A00) share the same root cause: **Fix 17 (TPIDR_EL1 race in `schedule_inner`, 2026-03-14).** When the scheduler lock drops after `schedule_inner` returns, IRQs are re-enabled. If a timer IRQ fires before exception.S updates TPIDR_EL1, `save_context` overwrites the old (parked) thread's Context with kernel-mode state. Fixes 12-14 addressed contributing factors but not the root cause. Fixes 5/6 (aliasing UB, nomem on DAIF) changed the timing enough to suppress most occurrences but didn't close the window.

Stress tested: 14,484 keys over 120 seconds (pre-Fix 17), 3000 keys at 1ms (post-Fix 17). No crash after Fix 17.

### Proactive Bug Hunting (continued, same session)

**Fix 15: sys::counter() nomem removed.** `mrs cntvct_el0` had `nomem` — same class as Fix 6. The counter is monotonically increasing hardware state; with `nomem`, LLVM could CSE or hoist repeated reads, returning stale timestamps.

**Fix 16: Trampoline `ldr [sp, #8]` nomem removed.** 18 thread trampolines across fuzz/fuzz-helper/stress used `options(nostack, nomem)` on `ldr [sp, #8]` — a memory read falsely declared non-memory. LLVM could reorder the preceding `write_volatile` of the argument past the load.

**Coverage analysis** identified critical gaps: `interrupt_register` (zero coverage), `handle_send` success path (zero), `interrupt_ack` success path (zero), `device_map` success path (zero), 9 syscalls never tested under concurrency. Addressed with 5 new fuzz phases (32–36):

- Phase 32: interrupt register/ack full lifecycle (register, ack, duplicate rejection, re-register after close, multi-IRQ, poll)
- Phase 33: handle_send success path (create channel → create child → send endpoint → child signals back via received handle)
- Phase 34: device_map success path (map UART0 MMIO, read register, error cases)
- Phase 35: concurrent scheduling context create/bind/borrow/return (4 threads, 100 ops each)
- Phase 36: concurrent DMA alloc/free (4 threads, 50 ops each, varied orders)

**Unsafe audit** reviewed all kernel modules with `unsafe` blocks: scheduler, address_space, memory, paging, per_core, thread_exit, process_exit. No new bugs found. Lock ordering verified correct.

**Kernel Change Protocol** codified in CLAUDE.md: nomem default-deny, SAFETY comments required, stress test mandate, anomaly tracking.

All 36 fuzz phases pass. 896 host tests pass. Build clean.

---

## Virtio-9P Host Filesystem Passthrough (2026-03-11)

**Status:** Working. Font loading end-to-end via 9P2000.L protocol over virtio transport.

**What was built:** Userspace virtio-9p driver (~450 lines) that reads files from the host macOS filesystem via QEMU's `-fsdev` passthrough. Implements 6 9P operations (version, attach, walk, lopen, read, clunk). Init sends a read request via IPC, the driver reads the file through 9P, fills a shared DMA buffer, signals back. Compositor loads the font from the runtime buffer instead of `include_bytes!`.

**Bugs found and fixed:**

1. **`payload_as`/`from_payload` hangs for large structs on aarch64 bare metal.** Both init and the 9p driver hung when using these helpers with FsReadRequest (60 bytes — full payload). ~~Root cause unclear.~~ **Retrospective (2026-03-13):** Likely caused by kernel Fix 5 (aliasing UB) or Fix 6 (`nomem` on DAIF) — both produced timing-dependent miscompilation at opt-level 3 that could manifest as hangs in userspace syscall paths. Evidence: `payload_as` uses `read_unaligned` internally and works correctly for 56-byte `CompositorConfig` structs in the compositor (added after the kernel fixes). The manual field-by-field workaround remains in the 9P driver and init but is no longer believed necessary. **Status: resolved** (root cause was kernel UB, not `payload_as`).
2. **Spurious wakeup from `sys_wait`.** Init's `wait(&[channel])` returned before the 9p driver sent its response. Fix: loop on wait+try_recv until the expected response message type arrives. ~~Root cause of the spurious wakeup not fully diagnosed.~~ **Retrospective (2026-03-13):** Root cause identified as kernel Fix 12 (stale waiter registrations). When `sys_wait` takes the `BlockResult::Blocked` path, unfired handle registrations remained live. A subsequent signal to one of those stale handles would spuriously wake the thread. The retry loop remains correct as defense-in-depth. **Status: resolved.**

**Architecture notes:**

- Chose 9P over virtiofs (simpler protocol, no host daemon, confirmed in QEMU).
- Shared host directory: `system/share/` — contains source-code-pro.ttf (9 KB).
- QEMU flags added to all 4 scripts (run, test, integration, crash).
- This validates the prototype-on-host strategy from Decision #16: implement Files against the host filesystem during prototyping, defer the real COW filesystem.

**Open questions for later:**

- Should investigate the `payload_as` hang root cause — it affects all IPC with large payloads.
- Should investigate the spurious wakeup root cause — retry loop works but masks a potential kernel bug.
- Phase 2 (general file service with event loop, multiple files) deferred.

---

## Bug Report: Kernel Crash Under Rapid Keyboard Input (2026-03-11)

**Severity:** High — kernel panic (instruction abort at EL1)
**Reproducible:** Yes — type rapidly into the QEMU window for ~10 seconds
**Introduced:** Not by new code (all new code is userspace). Exposed by the first sustained high-frequency event processing workload.

### Crash Signature

```console
💥 kernel sync: EC=0x21 ESR=0x86000006 ELR=0x0 FAR=0x0
instruction abort at EL1
```

- **EC=0x21**: Instruction abort from current EL (EL1 = kernel)
- **ELR=0x0**: Kernel tried to execute at address 0 — null function pointer
- **Metrics at crash:** ~440 ctx_sw/sec, ~370 syscalls/sec, 2595 ticks (~10.4s uptime)

### What's Happening

Three userspace processes run continuous event loops:

1. **Input driver**: `wait(IRQ)` → read event → `channel_signal(compositor)` → loop
2. **Compositor**: `wait(input_channel)` → render char → `channel_signal(GPU)` → loop
3. **GPU driver**: `wait(compositor_channel)` → transfer+flush (2 virtio cmds) → loop

Each keystroke triggers: IRQ → input wake → channel signal → compositor wake → render → channel signal → GPU wake → 2 GPU commands → loop. Under rapid typing, this produces ~50+ full cycles/sec, each involving:

- Vec alloc/free for `wait_set` (one per `sys_wait` call)
- 3+ lock acquisitions (scheduler, channel, timer)
- Thread state transitions (Ready → Running → Blocked → Ready)

### Suspect Analysis

**Suspect 1: Kernel heap allocator stress (MOST LIKELY)**
Each `sys_wait` call allocates a `Vec<WaitEntry>` via `store_wait_set`, and the wake path frees it via `wait_set.clear()` + drop. At ~370 syscalls/sec, the kernel heap allocator (linked-list first-fit with coalescing) handles hundreds of small alloc/free cycles per second. A coalescing bug under rapid free/alloc patterns could corrupt the free list, leading to a subsequent allocation returning corrupted memory. The null function pointer (ELR=0x0) is consistent with using a corrupted Box<Thread> where a vtable or function pointer has been zeroed.

_Test:_ Pre-allocate the wait_set Vec and reuse it across `sys_wait` calls (eliminate the alloc/free hotpath). If the crash disappears, it's the allocator.

**Suspect 2: Scheduler two-phase wake race**
`channel::signal()` collects the waiter ThreadId under the channel lock, releases it, then calls `try_wake_for_handle` under the scheduler lock. Between releasing the channel lock and acquiring the scheduler lock, another signal could arrive for the same thread. Both callers try to wake the same thread. `try_wake_impl` searches `blocked` by ThreadId and uses `swap_remove`. The second caller wouldn't find it (returns false) and falls through to `set_wake_pending_for_handle`. This _should_ be safe, but the swap_remove changes the order of the blocked list, which could interact badly with concurrent operations on other cores.

_Test:_ Add a serial print in `try_wake_impl` when a thread isn't found in blocked/running/ready (the fall-through case). If this fires frequently, it's the wake race.

**Suspect 3: Slab/heap interaction**
The kernel heap routes allocs by size: ≤2 KiB → slab, else → linked-list. The `Vec<WaitEntry>` starts small (8 bytes per entry × 1-2 entries = 16-32 bytes) and goes through slab. Rapid alloc/free of slab objects could expose a slab bug (double-free or free-list corruption).

_Test:_ Force Vec<WaitEntry> to allocate from the linked-list allocator by reserving a minimum capacity (e.g., `Vec::with_capacity(256)`). If the crash disappears, it's the slab.

### Fixes Applied (2026-03-11)

**Crash signature:** `ELR=0x0`, instruction abort at EL1. `ret` to address 0 on a valid kernel stack. Under rapid typing, crashed in ~15 seconds at opt-level 3.

**Root causes found (multiple):**

**Fix 1: Idle thread park (category b — intent not implemented).** `park_old()` comment said idle threads "go back to `cores[].idle`" but the code didn't do it. Fix: `park_old()` takes `core` parameter, restores idle threads. 17 scheduler state machine tests (`test/tests/scheduler_state.rs`).

**Fix 2: wait_set Vec reuse (category a — hot path allocation).** Each `sys_wait` allocated a fresh `Vec<WaitEntry>` + clone (~740 slab ops/sec). Fix: clear and repopulate `thread.wait_set` in-place, stack-allocated `[Option<WaitEntry>; 17]`. `push_wait_entry()` replaces `store_wait_set()`.

**Fix 3: Enhanced fault handler (permanent).** `kernel_fault_handler` now receives SP, LR (x30), TPIDR_EL1, thread ID, and saved Context fields from the assembly. Dumps stack words.

**Fix 4: Deferred thread drop (category a — use-after-free).** `park_old` dropped exited threads immediately, freeing kernel stack pages while `schedule_inner` was still executing on them. Fix: `State::deferred_drops` list, drained at start of next `schedule_inner`.

**Fix 5: Aliasing UB in syscall dispatch (category a — `&mut *ctx` vs `&mut State`).** `dispatch()`, `sys_wait()`, `sys_futex_wait()`, and `block_current_unless_woken()` created `&mut *ctx` references that aliased with the scheduler lock's `&mut State` (both cover the same Thread Context). With inlining at opt-level 3, LLVM saw two `noalias` mutable references to overlapping memory → miscompilation. Fix: all Context access through `ctx` now uses `core::ptr::addr_of!` + raw pointer reads/writes. New `dispatch_ok()` + `result_to_u64!` replace the old `dispatch_syscall!` macro.

**Fix 6: `nomem` on IrqMutex DAIF asm (PRIMARY FIX — category a).** `options(nostack, nomem)` on `mrs daif` / `msr daifset` / `msr daif` in `sync.rs` told LLVM these instructions don't access memory. This allowed LLVM to reorder lock-protected memory operations past the interrupt masking boundary, creating a race where accesses occurred with interrupts enabled on SMP. Fix: removed `nomem` from all DAIF manipulation and system register writes (`msr tpidr_el1`, `msr daifclr`). **This was the main fix — crash time went from ~15s to ~100-188s.**

**Fix 7: `#[inline(never)]` on all scheduler public functions.** Prevents LLVM from inlining scheduler internals into syscall/IRQ handlers, reducing the optimization surface for aliasing exploitation. Cheap (one `bl` instruction per scheduler call, dominated by IrqMutex lock cost).

**Fix 8: Automated crash test (`crash-test.sh`).** Launches QEMU headless, sends rapid keyboard input via monitor socket (Python + Unix socket), monitors serial output for crash. Usage: `./crash-test.sh [seconds]`.

**Remaining issue (RESOLVED 2026-03-11, follow-up session):** The residual opt-level 2-3 crash was originally observed only via manual keyboard typing in the QEMU window. The headless stress test (50M iterations, 137s, 4 SMP cores, opt-level 3) passes consistently. The automated crash test via AppleScript was a flawed methodology — it depends on macOS display routing and QEMU window focus, which introduces timing variability unrelated to the kernel. The headless stress test saturates the exact same syscall paths (channel_signal/wait, timer create/destroy, scheduling context switches) at much higher rates than keyboard input ever could. **Opt-level 3 is safe for use.** All 11 fixes (especially Fix 5: aliasing UB and Fix 6: nomem on DAIF) resolved the underlying issues.

**Diagnostic investigation trail:** (1) schedule_inner elr=0 check → never fired. (2) SP capture → valid kernel stack. (3) LR=0 → confirmed `ret` to null. (4) x30=0 check → false positive for EL0 threads. (5) Thread Context dump → saved Context always valid. (6) ELR verification in assembly → didn't trigger (crash is NOT from eret). (7) opt-level bisection → opt-level 1 passes, 2-3 crash. (8) `#[inline(never)]` bisection → scheduler inlining contributes. (9) `nomem` removal → main fix.

### Additional Hardening (2026-03-11, continued)

**Fix 9: `nomem` removal across all inline asm.** Systematic audit of all 99 `unsafe` blocks. Removed `nomem` from:

- `timer.rs`: `msr cntp_tval_el0` (timer reprogram), `mrs cntpct_el0` (counter read), `msr cntp_ctl_el0` (timer enable), `msr daifclr` (IRQ unmask)
- `power.rs`: `hvc #0` (PSCI CPU_ON — boots secondary cores)
- `syscall.rs`: merged split AT+MRS asm blocks into single blocks (address translation + PAR_EL1 read must not be reordered)

**Fix 10: Headless stress test (`stress-test.sh` + `user/stress/main.rs`).** Userspace stress program exercises IPC ping-pong, timer churn, and allocator pressure without needing a display or keyboard. Integrated into build system (`build.rs`, `init/main.rs`). Usage: `./stress-test.sh [seconds]`.

**Fix 11: Property-based scheduler tests (`test/tests/scheduler_state.rs`).** 3 new tests added to existing 17:

- `randomized_scheduler_state_machine`: 500 random actions × 50 seeds, checks invariants after every action
- `rapid_block_wake_never_duplicates`: rapid block/wake cycles never create duplicate thread entries
- `all_threads_eventually_reaped`: exited threads are always cleaned up via deferred drops

Total: 20 scheduler state machine tests, all passing.

### How to Test

```sh
cd system
./crash-test.sh 120   # Automated: 120 seconds of rapid keyboard input
./stress-test.sh 30   # Headless stress test (no display needed)
cargo run --release    # Manual: type rapidly in QEMU window
cd test && cargo test scheduler_state  # Property-based scheduler tests
```

---

## Open Threads

Active questions we've started exploring but haven't resolved. Each thread links to the decisions it would inform.

### Is "compound" intrinsic or contextual?

**Informs:** Decision #14 (Compound Documents), Glossary
**Status:** Partially resolved by uniform manifest model (2026-03-09)
**Context:** With uniform manifests, every document has a manifest. A PDF becomes a document whose manifest references a single content file. "Compoundness" is a property of the manifest's structure (how many content references), not an intrinsic property of the content format. If the user decomposes a PDF for part-by-part editing, a new manifest with extracted parts could be created. Remaining question: is decomposition automatic, user-initiated, or editor-driven?

### ~~Referenced vs owned parts in documents~~ — SETTLED (copy semantics)

**Informs:** Decision #14 (Compound Documents)
**Status:** Settled (2026-03-11). Copy semantics with provenance metadata.
**Context:** Explored three approaches: reference (shared, changes propagate), copy (independent, self-contained), and copy-on-reference hybrid (start shared, diverge on first edit). References require tracking a global reference graph (delete checks, broken links, spooky action at a distance). The hybrid creates invisible state — whether editing affects the original depends on whether you've previously edited it in this context, which the user can't predict. Copy semantics won: each compound document gets its own copy of embedded content. Self-contained, no reference tracking, no broken links, no surprising behavior. COW at the filesystem level means "copies" share physical blocks until they diverge — logical independence with physical efficiency. The original document's ID is stored as provenance metadata, enabling an explicit "update to latest" action (pull, not push). One-directional: the compound knows about the original, the original doesn't know about the compound. Same parent→child / child-doesn't-know-parent isolation as the compound document model generally.

### OS service interface map

**Informs:** Decisions #9, #7, #14, #15, #17
**Status:** Preliminary mapping done (2026-03-09), no interfaces designed yet
**Context:** Mapped all inter-component interfaces by boundary. The OS service is where interface design effort concentrates — edit protocol, metadata queries, interaction model, translator interface. The kernel surface (12 syscalls) is small and stable. Internal OS service interfaces (renderer, layout engine, compositor, scheduling policy) matter for implementation but can evolve freely. Key finding: scheduling policy needs no separate interface (falls out of edit protocol + kernel syscalls). Web engine adapter is not separate from translator interface. See insights log for full table.

### Shell architecture and system gestures

**Informs:** Decision #17 (Interaction Model), OS service interface design
**Status:** Under active exploration (2026-03-10/11)
**Context:** The shell's architectural placement was explored across two sessions. Key findings:

1. **Blue-layer symmetry:** Trust (kernel/OS service/tools) and complexity (red/blue/black) are orthogonal axes. The blue adaptation layer wraps the core on all sides — drivers below (adapt hardware), translators at sides (adapt formats), editors + shell above (adapt users). Editors are "user drivers."

2. **Shell is blue-layer but not purely modal.** Initially proposed the shell as an untrusted tool identical to editors, active when no editor is (modal). But switching documents while in an editor requires the shell to intercept input — so the shell is ambient, not modal. Revised model: system gestures (switch, invoke search, close) baked into OS service input routing (always work, not pluggable); navigation UI (what search looks like, document list) provided by shell (pluggable, restartable).

3. **One-document-at-a-time leaning.** UI model closer to macOS fullscreen Spaces than windowed desktop. View one document at a time, switch through the shell. Not settled.

4. **Compound document editing tension.** "Editors bind to content types" + "one editor per document" conflict for compound documents. Initial instinct: editor nesting (same text editor used within presentations, standalone text docs, etc.). But nesting creates complexity. Unresolved — needs dedicated exploration.

Open questions: system gesture vs shell input boundary, compound editor nesting model, whether content-type interaction primitives (cursor/selection/playhead from OS service) need to become richer editing primitives for compound documents to work.

### View/edit in the CLI

**Informs:** Decision #17 (Interaction Model)
**Status:** Briefly mentioned, not explored
**Context:** The view/edit distinction is clear in GUI. How does it translate to CLI? Tools-as-subshells? Read-commands-always-safe? The CLI and GUI are equally fundamental interfaces (Belief #4), so the CLI can't be an afterthought.

### ~~Kernel architecture~~ — SETTLED (microkernel)

**Informs:** Decision #16 (Technical Foundation)
**Status:** All sub-decisions settled except filesystem COW on-disk design. Kernel is a microkernel by convergence.
**Context:** From-scratch Rust kernel on aarch64. Microkernel: address spaces, threads, IPC, scheduling, interrupt forwarding, handles. All semantic code in userspace. Settled: soft RT, no hypervisor (EL1), preemptive + cooperative yield, traditional privilege (EL0), split TTBR, handles, ELF, ring buffer IPC, three-layer process arch, SMP (4 cores), EEVDF + scheduling contexts, userspace drivers (MMIO mapping + interrupt forwarding), userspace filesystem (kernel owns COW/VM, filesystem manages on-disk layout). Remaining: filesystem COW on-disk design.
**Leaning — syscall API:** 12 syscalls in three families. Handle family: `wait(handles[])`, `close(handle)`, `signal(handle)`. Synchronization: `futex_wait`, `futex_wake`. Scheduling: `sched_create/bind/borrow/return`. Plus lifecycle: `exit`, `yield`, `write` (debug, temporary). Generic verbs on typed handles — handle type carries context. `wait` subsumes old `channel_wait` and gains multiplexing. OS service uses reactive/stream composition on top of `wait`. See insights log for full rationale.

### Display engine architecture

**Informs:** Decision #11 (Rendering Technology), Decision #15 (Layout Engine), Decision #17 (Interaction Model)
**Status:** Complete (2026-03-10). All three build steps done. Full display pipeline working end-to-end.
**Context:** Graphical output on QEMU virt. virtio-gpu (paravirtual, 2D protocol) reuses existing virtio infrastructure. Key architectural conclusions:

- **Surface-based trait, not framebuffer.** A raw framebuffer (`map() → &mut [u8]`) is specific to software rendering — GPU acceleration means the CPU never touches pixels. The universal abstraction is surfaces and operations: `create_surface`, `destroy_surface`, `fill_rect`, `blit`, `present`. The driver implements this trait; whether it uses CPU loops or GPU commands internally is the driver's business.
- **Display vs rendering are separate concerns in one device.** Display = get a buffer to the screen (last mile). Rendering = fill the buffer (compositing). Both always happen. GPU acceleration changes who fills the buffer (GPU vs CPU), not the display path. A GPU chip does both; one driver.
- **Three components, one interface.** Compositor (above) works with surfaces, calls trait methods. Driver (below) translates trait methods to hardware operations. The trait is the boundary — a contract, not a component. The compositor doesn't know if the driver uses CPU loops, GPU commands, or anything else. Software rendering is a fallback strategy inside the driver, not a separate thing the OS selects.
- **virtio-gpu overhead is inherent, not architectural.** Performance hit is the VM boundary (guest→host copy). With real hardware, the display controller reads directly from the buffer via DMA scanout — no copy. The abstraction doesn't add overhead; virtio does.
- **Build plan:** (a) virtio-gpu userspace driver ✅, (b) drawing primitives + bitmap font ✅, (c) toy compositor ✅. All done. Everything above the driver is portable to real hardware.
- **Step (a) done (2026-03-10):** `system/services/drivers/virtio-gpu/main.rs`. All 6 core 2D commands. Test pattern at 1280x800.
- **Step (b) done (2026-03-10):** `system/libraries/drawing/` — pure no_std drawing library. Surface abstraction with RGBA canonical format (encode/decode at pixel boundary). Primitives: fill_rect, draw_rect, draw_line (Bresenham), draw_hline/vline, set/get_pixel, blit. Embedded 8×16 VGA bitmap font with draw_glyph/draw_text. 41 host-side tests.
- **Step (c) done (2026-03-10):** `system/services/compositor/main.rs` — toy compositor draws demo scene (title bar, 3 colored panels with text, status bar) into shared framebuffer. `system/services/init/main.rs` — proto-OS-service that embeds all ELFs, reads device manifest, spawns all processes, orchestrates display pipeline. Kernel `memory_share` syscall (#24) enables zero-copy framebuffer sharing. Full pipeline: init → DMA alloc → share with compositor → compositor draws → signal → GPU driver presents → pixels on screen.
- **Alignment bug found (2026-03-10):** u64 `read_volatile` from 4-byte-aligned address is UB in Rust. Caused silent process death. Fixed by padding device manifest entries to 8-byte alignment. User fault handler didn't print diagnostic before killing process — known kernel bug.

Open questions: exact surface trait API, double buffering strategy, font choice for production (Spleen PSF2 over hand-rolled VGA font), trait naming.

### Compositor design

**Informs:** Decision #11 (Rendering Technology), Decision #14 (Compound Documents), Decision #15 (Layout Engine), Decision #17 (Interaction Model)
**Status:** Mental model established (2026-03-10), toy compositor implemented (2026-03-10)
**Context:** Explored the compositor's role, architecture, and how it differs from traditional desktop compositors. Key findings:

1. **Compositor = function from surface tree to pixel buffer.** Structurally identical to React's render pipeline: declarative tree (manifest = component tree) → damage calculation (= reconciliation/diff) → minimal pixel updates (= commit). The document manifest IS the scene graph.

2. **Scene graph is a tree shaped by document structure.** Not a flat list of overlapping windows. Compound documents create nested surfaces (chart within a slide within a presentation). The compositor embodies the document's structure. Tree is narrow and deep (one document, nested content parts) vs traditional desktop which is wide and shallow (dozens of top-level windows).

3. **Z-overlap is dramatically simpler.** Traditional desktops: 30+ arbitrary overlapping windows. Our OS: 1 document + maybe 1-2 floating elements + system UI = 3-4 z-layers total. No occlusion culling, no complex z-ordering. Structural constraints eliminate the problem rather than clever algorithms solving it.

4. **Two surface behaviors: contained and floating.** Most content is contained (clipped to parent, positioned by layout engine). Some needs to float: drag ghosts, popovers, tooltips, editor overlays, transitions. Floating surfaces are rendered above the normal tree, not clipped. Similar to Wayland subsurfaces vs popups, or CSS normal flow vs position:fixed.

5. **Compositor↔GPU driver data path matters.** Three options explored: (a) copy via IPC (too slow for framebuffer-sized data), (b) shared memory (correct — zero-copy, needs kernel Phase 7), (c) same process (simple but couples trust levels). Real systems use (b). For toy compositor, temporary coupling is OK with eyes open about what's scaffolding.

6. **"Informed" vs "blind" compositor.** Traditional compositors are blind — each app is an opaque pixel rectangle. Ours is informed — knows document structure, content types, document state. Enables: damage prediction (text cursor = known small rectangle), update cadence optimization (video at 24fps, static text at 0fps), content-type-aware rendering priority.

7. **Compound documents ARE nested windowing.** A presentation with an embedded chart is structurally identical to a window containing a sub-window. The differences: layout determines position (not user dragging), no chrome, nesting is content-driven. But the compositor's internal model needs the same tree structure. This connects directly to the unresolved compound document editing tension.

8. **Dragging in absolute layouts = window dragging.** Canvas/freeform layouts (Decision #14 spatial axis) let users drag content. During drag: compositor moves surface in real time. On drop: editor commits via beginOp/endOp. Same pattern as traditional window management.

9. **Pure containment is too rigid.** Pop-out editing (drag photo out to adjust), tooltips extending beyond parent, transitions between containers — all need floating surfaces. Don't commit to pure containment. Cost of floating support is low (extra render pass), UI cases are real.

**Implementation note (2026-03-11, commit 827bcc8):** The compositor now demonstrates the settled editor separation architecture. It receives input from the input driver, routes it to the text editor via IPC, receives write requests back, and applies them as sole writer to the document buffer. Four processes in the display pipeline (GPU driver, input driver, text editor, compositor). The compositor↔editor channel is bidirectional: input events out, write requests in. This validates the "OS service is sole writer" design in running code.

Open questions: exact scene graph API, how layout engine and compositor interface (does layout produce the tree that compositor renders?), React-style damage diffing (how much complexity is justified for 3-4 z-layers?), whether compositor is part of OS service or separate. Shared memory is no longer blocked — kernel Phase 7 (`memory_share` syscall #24) is done.

### COW Filesystem

**Informs:** Decision #16 (Technical Foundation — filesystem sub-decision), Decision #12 (Undo), Decision #14 (virtual manifest rewind)
**Status:** Interface designed (2026-03-11). Placement settled (userspace service). On-disk format deferred — prototype-on-host strategy adopted. See `design/research-cow-filesystems.md`.
**Context:** Studied RedoxFS (Rust, COW but no snapshots), ZFS (birth time + dead lists = gold standard for snapshots), Btrfs (refcounted subvolumes), Bcachefs (key-level versioning). Key findings: (1) birth time in block pointers is non-negotiable for efficient snapshots, (2) ZFS dead lists make deletion tractable, (3) per-document scoping needed (datasets/subvolumes, not whole-FS snapshots), (4) `beginOp`/`endOp` maps naturally to COW transaction boundaries. TFS (Redox's predecessor) attempted per-file revision history but didn't ship it — cautionary data point. Filesystem is a userspace service — kernel owns COW/VM mechanics (page fault handler), filesystem manages on-disk layout (B-trees, block allocation, snapshots). **New constraint (2026-03-09):** metadata DB must live on the COW filesystem so its historical state is preserved in snapshots — required for uniform rewind performance across static and virtual documents. Time-correlated vs per-document snapshots still open — per-document snapshots + a COW'd metadata DB might be sufficient for world-state queries without coordinated global snapshots. Needs further exploration.

**Files interface settled (2026-03-11).** 12 operations: create, clone, delete, size, resize, map_read, map_write, snapshot, restore, map_snapshot, snapshots (list), delete_snapshot, flush. Deliberately absent: paths/directories (files addressed by ID), permissions (OS service is sole consumer), extended attributes (metadata lives in DB), file locking (OS service serializes writes via event loop), links (copy semantics via clone), rename (metadata DB concern), batch operations (OS service sequences), file type info (metadata DB concern). The interface is a dumb file store — it knows nothing about documents, undo ordering, or compound structures. See journal insights for full design rationale.

**Prototype-on-host strategy (2026-03-11).** Implement the Files interface against macOS (regular files + file copies for snapshots + mmap). Build the real COW filesystem later once the interface is proven through actual editor/undo/document pipeline usage. The filesystem's on-disk format is a leaf node — complex inside, simple interface. Same pattern as the rendering engine decision (settle the architecture, defer the implementation).

**Prototype validated (2026-03-11).** `prototype/files/` — HostFiles implementation backed by macOS filesystem, 21 tests covering all 12 operations (create, clone, delete, size, resize, map_read, map_write, snapshot, restore, map_snapshot, snapshots, delete_snapshot, flush). Tests include: basic CRUD, snapshot chains, restore, clone independence, write-at-offset, resize, error cases, undo workflow simulation. Interface is proven through concrete implementation; ready for integration when the OS service pipeline reaches persistent document storage.

Open questions: on-disk format (deferred), snapshot naming, pruning policy, page cache placement, snapshot scope (global vs per-document vs time-correlated — punted, doesn't block interface).

### Virtual manifests, retention, and the OS-as-document

**Informs:** Decision #14 (Compound Documents), Decision #17 (Interaction Model), Decision #16 (COW filesystem)
**Status:** Core concepts settled (2026-03-09): static/virtual manifests, retention policies replacing transient concept, streaming as virtual. OS-as-document not yet committed.
**Context:** Manifests can be static (disk-backed, COW'd) or virtual (content generated on demand from internal state OR external sources). Virtual manifests enable: system-derived documents (inbox, search results, dashboard — internal state), streaming content (YouTube — external source). All documents are persistent — no "transient" concept. Retention policies handle cleanup (webpages 30 days, user content permanent). COW pruning system manages both edit history and document lifecycle. The OS itself could be presented as a document or query (shell/GUI as editors/viewers) — potentially informs Decision #17. Virtual documents inherit time-travel from underlying static documents' COW history. Design constraint: rewind performance must be uniform (metadata DB on COW filesystem). "Transient documents" concept explored and rejected — it's a retention policy, not a document type.

### ~~Privilege model (EL1 / EL0 boundary)~~ — SETTLED

**Resolved:** Traditional — all non-kernel code at EL0. One simple boundary, one programming model. Consistent with Decision #4 (simple connective tissue) and Decision #3 (arm64-standard interface). Language-safety (B) rejected as unsolved research problem for extensibility. Hybrid (C) rejected as two-ways-to-do-the-same-thing. See Decision #16 in decisions.md.

### ~~Address space model (TTBR0 / TTBR1)~~ — SETTLED

**Resolved:** Split TTBR — TTBR1 for kernel (upper VA), TTBR0 per-process (lower VA). Follows directly from the traditional privilege model. See Decision #16 in decisions.md.

---

## Discussion Backlog

Topics to explore, roughly prioritized by which unsettled decisions they'd inform. Not a task list — a menu of interesting conversations to have when the urge strikes.

### High leverage (unblocks multiple decisions)

1. ~~**Rendering technology deep dive** (Decision #11)~~ — **SETTLED.** Existing web engine integrated via adaptation layer. Key insight: a webpage IS a compound document (HTML=manifest, CSS=layout, media=referenced content) — can be handled through the same translator pattern as .docx. Rendering direction open (web engine renders everything vs. native renderer with web content translated inward). Engine complexity pushed into the blue adaptation layer. Prototype on macOS. See Decision #11 in decisions.md.

2. ~~**What does the IPC look like?**~~ (Decision #16) — **SETTLED.** Shared memory ring buffers with handle-based access control. One mechanism for all IPC. Kernel creates channels and validates messages at trust boundaries, but is not in the data path. Documents are memory-mapped separately. Editor ↔ OS service ring buffers carry control messages only: edit protocol (beginOp/endOp), input events, overlay descriptions. Metadata queries use a separate interface (not the editor channel — different cadence, potentially large results). Three-layer process architecture: kernel (EL1) + OS service (EL0, trusted, one process for rendering + metadata + input + compositing) + editors (EL0, untrusted). See Decision #16 in decisions.md.

3. **The interaction model** (Decision #17) — What does using this OS actually feel like? Mercury OS and Xerox Star are reference points. How do you find documents? What does "opening" something look like? How do queries surface in the GUI?

### Medium leverage (deepens settled decisions)

4. **Compound document authoring workflow** — We know the structure (manifests + references + layout), but how does a user actually _create_ a compound document? Do they start with a layout and add content? Does it emerge from combining simple documents?

5. **Content-type rebase handlers in practice** — We know the theory (git merge generalized). What would a text rebase handler actually look like as an API? What about images? This would validate the edit protocol's upgrade path.

6. **The metadata query API** — Decision #7 settled on "simple query API backed by embedded DB." What does this API actually look like? What are the verbs? How does it feel to use from both GUI and CLI?

6b. **IANA mimetype → OS document type mapping** — Systematic exercise: map common IANA mimetypes to OS document types, relationship axes, and editor bindings. Which mimetypes map to single-content documents (image/png → image document)? Which suggest compound documents (text/html → compound with flow layout)? What are the OS-native mimetypes for compound document types (presentation, project, album)? This would validate the three-axis model against real content types and surface edge cases. Connects to the mimetype-of-the-whole question (partially resolved) and content-type registration via editor metadata.

### Exploratory (interesting but less urgent)

7. **Historical OS deep dives** — Plan 9's /proc and per-process namespaces. BeOS's BFS attributes in practice. OpenDoc's component model and why it failed. Xerox Star's property sheets. Each could inform current design.

8. ~~**Scheduling algorithm**~~ — **SETTLED.** EEVDF + scheduling contexts (combined model). EEVDF provides proportional fairness with latency differentiation (shorter time slice = earlier virtual deadline). Scheduling contexts are handle-based kernel objects (budget/period) providing temporal isolation between workloads. Context donation: OS service borrows editor's context when processing its messages (explicit syscall). Content-type-aware budgeting: OS service sets budgets based on document mimetype and state. Best-effort admission. Shared contexts across an editor's threads. See Decision #16 in decisions.md.

9. **The "no save" UX** — We committed to immediate writes + COW. What does this feel like for content that's expensive to re-render? What about "I was just experimenting, throw this away"? Is there a need for explicit "draft mode" or does undo cover it?

10. **Editor plugin API design** — What's the actual interface between an editor plugin and the OS? How does an editor register, receive input, draw overlays? This is where the abstract editor model becomes concrete. The IPC ring buffer between editor ↔ OS service is essentially an RPC transport (msg_type = function name, payload = arguments). The API question is: what are the RPCs?

### Overlay protocol

**Informs:** Editor plugin API (#10), Rendering technology (#11)
**Status:** Three options identified, not yet committed
**Context:** Editors need to show tool-specific visual feedback (crop handles, selection highlights, brush preview, text cursor) without owning any rendering surface. Options:

- **A. Semantic overlays:** OS defines ~10-15 meaningful types (cursor, selection, bounding-box, guide-line, tool-preview). Editor says "selection is offsets 10-50," OS decides how to render. Scalable set, consistent styling, but limits editors to predefined vocabulary.
- **B. Overlay as mini-document:** Overlay is a small scene graph / SVG-like document in shared memory. Editor writes to it, OS renders. Ring buffer carries only "overlay updated" notifications. Most document-centric option.
- **C. Pixel buffer:** Editor gets a shared-memory pixel buffer, renders its own overlay, OS composites. Most flexible, but conflicts with "OS renders everything."
- **Hybrid A+B:** Semantic overlays for 90% case + custom overlay document escape hatch for exotic tool UI. Seems promising.

### Metadata query routing

**Informs:** File organization (#7), Interaction model (#17)
**Status:** Clarified — metadata queries don't belong in editor ↔ OS service ring buffer
**Context:** Metadata queries (search by tags, attributes, etc.) are request/response, potentially large results, not real-time. They're primarily a shell/GUI → OS service concern, not an editor concern. Should use a separate interface — possibly a separate channel type, or results as memory-mapped documents. The editor ↔ OS service channel carries only: input events, edit protocol, overlays.

---

## Insights Log

Non-obvious realizations worth preserving. These are the "aha moments" that should inform future design thinking.

### Decomposition is a spectrum, not a binary (2026-03-05)

Any content decomposes further — video into frames, text into codepoints, codepoints into bytes. Taken to its conclusion, everything is Unix. The OS draws its line at the mimetype level (anchored to IANA registry), same way Unix draws at the byte level (anchored to hardware). This isn't arbitrary — it's pragmatic and externally anchored.

### Selective undo and collaboration are the same problem (2026-03-05)

Both require rebaseable operations. Building content-type rebase handlers unlocks both. This means collaboration isn't a separate feature to "add later" — it's a natural consequence of investing in selective undo.

### Total complexity is conserved (2026-03-05)

External complexity is fixed. Making the core simpler by pushing everything into adapters doesn't reduce complexity — it displaces it. L4 microkernel is the cautionary tale. The design metric is minimizing total irregularity across core + adaptation layer jointly. This should directly inform the kernel architecture decision.

### Modal tools eliminate an entire problem class (2026-03-05)

One editor at a time means no concurrent composition, no operation merging, no coordination protocol. The "pen on desk" metaphor isn't just UX — it's an architectural simplification that removes the hardest part of the edit protocol.

### application/octet-stream is self-penalizing (2026-03-05)

The escape hatch back to Unix-level agnosticism exists, but using it means losing everything the OS provides. The system doesn't need to forbid bypassing the type system, because bypassing it is its own punishment.

### Hard RT costs are user-visible, not just developer-visible (2026-03-06)

Hard realtime doesn't just make the OS harder to build — it makes it worse for desktop use. Throughput drops (scheduler constantly servicing RT deadlines), low-priority tasks starve under high-priority load, and dynamic plugin loading fights provable timing bounds (can't admit code without timing analysis). Critically, soft RT is perceptually indistinguishable from hard RT for audio/video on modern hardware (sub-1ms scheduling latency vs ~5-10ms human perceptual threshold). Hard RT is for physical-consequence domains (medical, automotive, aerospace), not desktops.

### Preemptive and cooperative are complementary, not a binary (2026-03-06)

The edit protocol's beginOperation/endOperation boundaries are natural cooperative yield points. Preemptive scheduling is the safety net (buggy editor can't freeze system). Both work together: preemptive as the ceiling, cooperative as the efficient path. The full context save/restore infrastructure supports preemption; cooperative yield is purely additive — no rework needed.

### Hypervisor IPC works against "editors attach to content" (2026-03-06)

A hypervisor-based isolation model (editors in separate VMs) requires VM-exit/enter for every cross-boundary call. This directly conflicts with the immediate-write editor model — every `beginOperation`/write/`endOperation` would cross a VM boundary. The thin edit protocol's value comes from low overhead; VM transitions are the opposite of low overhead. Hardware isolation at the EL1/EL0 boundary (syscalls) is a much lighter mechanism for the same goal.

### Centralized authority simplifies access control (2026-03-06)

Full capability systems (seL4, Fuchsia) solve distributed authority — many actors granting, delegating, and revoking access to each other. This OS is architecturally centralized: the OS mediates all document access, renders everything, manages editor attachment. In a centralized-authority model, OS-mediated handles (per-process table, integer index, rights check) provide the same security guarantees as capabilities with far less machinery. Handles enforce view/edit and the edit protocol at the kernel level. The query/discovery tension that plagues capabilities (how do you search for documents you don't have capabilities to?) doesn't arise because the query system is OS-internal. Handles can extend to IPC endpoints and devices incrementally — growing toward capabilities only if distributed authority is ever needed.

### "OS renders everything" produces three-layer architecture (2026-03-07)

"The OS renders everything" is a design principle. "Rendering code should not be in the kernel" is an engineering constraint. Together they force a three-layer architecture: kernel (EL1, hardware/memory/scheduling/IPC), OS service (EL0, rendering/metadata/input/compositing), editors (EL0, untrusted tools). The primary IPC relationship is editor ↔ OS service — not "everything through the kernel." The kernel's IPC role is control plane (setup, access control, message validation), not data plane (actual byte transfer).

### Top-down design explains why content-type awareness is load-bearing (2026-03-08)

Most OSes are designed bottom-up: start from hardware, build abstractions upward. Unix asked "what does the PDP-11 give us?" → bytes → files → processes → pipes. The user-facing model is whatever the hardware abstractions naturally produce. This OS is designed top-down: start from the user experience ("what should working with documents feel like?") and work down toward hardware. Content-type awareness isn't an independent axiom — it's what you discover when user-level requirements (viewing is default, editors bind to content types, undo is global) flow down to the system level. It shows up in rendering, editing, undo, scheduling, file organization, and compound documents because every subsystem was designed to serve the user-level model, not the hardware-level model. Previous document-centric OSes (Xerox Star, OpenDoc) stopped at the UX — "documents first" but the kernel, scheduler, and filesystem remained content-agnostic. This OS takes document-centricity seriously at the system level, which is why content-type awareness permeates everywhere. The methodology (top-down) produced the principle (content-type awareness) as a natural consequence.

### Content-type awareness is a scheduling advantage (2026-03-08)

A traditional OS has no idea what a process is doing. Firefox playing video and Firefox rendering a spreadsheet look identical to the scheduler. Application developers manually request RT priority (and often get it wrong). This OS knows the mimetype of every open document. The OS service creates scheduling contexts for editors and sets budgets based on content type: tight period for `audio/*` playback, relaxed for `text/*` editing, trickle for background indexing. More importantly, the OS service knows document _state_ — video being played gets RT budget, video paused on a frame drops to background levels. The scheduling context isn't set once; the OS service adjusts it dynamically. This is the document-centric axiom paying dividends in an unexpected place: "OS understands content types" was a decision about file organization and viewer selection, but it turns out to be a scheduling decision too.

### Handles all the way down: memory, IPC, time (2026-03-08)

With scheduling contexts as handle-based kernel objects, three fundamental resources use the same access-control model: memory (address space), communication (channel), and time (scheduling context). This consistency makes the design feel inevitable rather than assembled. Each resource is created by the kernel, held via integer handle, rights-checked on use, and revocable. The pattern was adopted for IPC (forced by the access-control decision), then extended to scheduling because the domains were similar enough — the adoption heuristic in action.

### Ring buffers only carry control messages because documents are memory-mapped (2026-03-07)

The highest-bandwidth data in a typical OS (rendering surfaces, file contents) doesn't flow through IPC in this design. The OS service renders internally (no cross-process rendering surfaces). Documents are memory-mapped by the kernel into both OS service and editor address spaces (no file data in IPC). What remains for IPC is all small: edit protocol calls, input events, overlay descriptions, metadata queries. This is why one IPC mechanism (shared memory ring buffers) works for everything — the use cases that would break a simple mechanism are handled by memory mapping instead.

### IPC ring buffers are an RPC transport (2026-03-07)

The ring buffer between editor ↔ OS service is essentially remote procedure calls. `msg_type` is a function name, payload is arguments. OS service → editor: `deliverKeyPress(keycode, modifiers, codepoint)`, `deliverMouseMove(x, y)`. Editor → OS service: `beginOperation(document, description)`, `endOperation(document)`, overlay updates. This framing means the IPC message types ARE the editor plugin API — designing one designs the other.

### Metadata queries are a separate concern from editor IPC (2026-03-07)

The editor ↔ OS service channel carries real-time control messages: input events, edit protocol (beginOp/endOp), overlays. Metadata queries (search by tags, find documents by attribute) are request/response, potentially large results, not real-time — a fundamentally different interaction pattern. They're primarily a shell/GUI concern, not an editor concern. Mixing them into the same ring buffer conflates two different cadences. Separate interface, design later.

### Scheduling contexts are the policy/mechanism boundary (2026-03-08)

Scheduling is both policy and mechanism, and the two are separable. Mechanism (context switching, timer interrupts, register save/restore) and algorithm (EEVDF selection, budget enforcement) must live in the kernel — they require EL1 privileges and run on the critical path (250Hz × 4 cores = 1,000 decisions/sec). Policy (which threads deserve what budgets, when to adjust) belongs in the OS service — it has the semantic knowledge (content types, document state, user focus). Scheduling contexts are the interface between the two layers: the kernel says "I enforce whatever budget you give me," the OS service says "this editor needs 1ms/5ms because it's playing audio." Moving the algorithm to userspace would require an IPC round-trip on every timer tick — untenable. This is the same separation Linux uses (kernel EEVDF + cgroup budgets), arrived at independently from first principles.

### A webpage is a compound document (2026-03-08)

The OS's compound document model (manifests + referenced content + layout model) maps structurally to web content. HTML is the manifest with layout rules. CSS provides layout (flow, grid, fixed positioning — covering 4 of 5 fundamental layouts natively). Images, video, and fonts are referenced content. This structural equivalence means web content could be handled through the same translator pattern as .docx or .pptx — translated into the internal compound document representation at the boundary. "Browsing" becomes "viewing HTML documents through the same rendering path as any other compound document." The rendering direction (web engine renders everything vs. native renderer with web-to-compound-doc translation) is an open sub-question, but the structural mapping holds regardless.

### Rendering and drivers face opposite constraints (2026-03-08)

The "rethink everything" stance (Decision #3) helps with drivers and hurts with rendering. Drivers need narrow scope (just your hardware), each is a bounded problem, and first-principles design is an advantage. Rendering needs broad scope (reasonable coverage of common web features — you'd notice gaps in normal browsing), can't be built from scratch (web engines are millions of lines of code), and must accommodate external reality. The adaptation layer (foundations.md) resolves this asymmetry: push engine complexity into the blue layer, keep the OS core clean. This is exactly the kind of external/internal tension the adaptation layer was designed for. The driver model can be explored through building a small set of real drivers; the rendering model must be explored through integration with an existing engine.

### Native renderer preserves the direction of power (2026-03-08)

With a web engine as renderer (Approach A), the OS can only do what the engine supports. Custom rendering behavior means patching the engine or hoping for extension points — the OS is downstream of someone else's architectural decisions. With a native renderer (Approach B), the OS defines what's possible. The renderer can express layout behaviors, compositing effects, and content-type-specific rendering that CSS can't describe. Web content is a lossy import (translated inward to compound doc format, same as .docx), not the rendering model itself. The Safari analogy: Apple controls WebKit _and_ the platform, so they can add proprietary CSS extensions — but they're still constrained by the engine's architecture. A native renderer removes that constraint entirely. The compound document model is the internal truth; external formats (.docx, .pptx, .html) are all translations inward at the boundary. The OS doesn't think in HTML any more than it thinks in .docx.

### Settling the approach, not the technology (2026-03-08)

Decision #11 was settled by choosing the architectural approach (web engine as substrate, adaptation layer between engine and OS service) without committing to a specific engine. The interesting design work is in the interface between engine and OS service — the "blue layer" — not in the engine choice itself. The engine is a leaf node: complex inside, simple interface. Any engine that can be adapted to speak the OS's protocol works. This mirrors how Decision #16 settled IPC (shared memory ring buffers) without specifying message formats. The pattern: settle the architecture, defer the implementation.

### Files are a feature, not a limitation (2026-03-08)

Phantom OS tried to eliminate files entirely via orthogonal persistence (memory IS storage). The problems it encountered — ratchet (bugs persist forever, no clean restart), schema evolution (code updates vs persistent object structures), blast radius (one corrupted object graph poisons everything), GC at scale (unsolved) — are all consequences of removing the boundaries that files provide. Files give you: isolation (corrupt one document, not the system), format boundaries (schema evolution via format versioning), natural undo points (COW snapshots per file), and interoperability (external formats). Our "no save" approach preserves the same UX ("I never lose work") by writing immediately to a COW filesystem — getting the benefit without the systemic fragility. The lesson: the boundary between "document" and "storage" is load-bearing, not incidental.

### BeOS independently validated three of our decisions (2026-03-08)

BeOS/Haiku has been running with: MIME as OS-managed filesystem metadata (our Decision #5), typed indexed queryable attributes replacing folder navigation (our Decision #7), and a system-level Translation Kit with interchange formats (our Decision #14) — for 25+ years. We arrived at the same designs from first principles. This is strong validation. The differences that matter: BeOS attributes are lost on non-BFS volumes (portability problem), BFS indexes aren't retroactive (our system should be), translators don't chain automatically (open question for us), and BeOS is still app-centric at runtime (our OS-owns-rendering model is more radical).

### Typed IPC contracts formalize the edit protocol (2026-03-08)

Singularity's channel contracts are state machines defining valid message sequences with typed payloads. Compiler proves endpoints agree on protocol state. Our edit protocol (beginOp/endOp) is already a state machine. Formalizing IPC messages as contracts — even without compiler enforcement — would prevent editors from deadlocking the OS service, document the editor plugin API precisely (since "IPC message types ARE the editor plugin API"), and enable runtime validation at the trust boundary. This should inform the IPC message format design when we get there.

### Oberon's text-as-command eliminates the CLI/GUI distinction (2026-03-08)

In Oberon, any text on screen is potentially a command. Middle-click on `Module.Procedure` in any document and it executes. "Tool texts" are editable documents containing commands — user-configurable menus that are just text files. The insight: there IS no CLI/GUI split. Text is both content and command. Every document is simultaneously a workspace. This directly addresses our open thread on CLI/GUI parity (Decision #17). Our content-type awareness could recognize "command references" within text — a tool text becomes a compound document where some content is executable.

### The kernel is a handle multiplexer with one wait primitive (2026-03-08)

A pattern emerged from settling drivers and filesystem: the kernel's job is multiplexing hardware resources behind handles + providing a single event-driven wait mechanism (`wait`). Memory (address spaces), communication (channels), time (scheduling contexts), devices (MMIO mappings + interrupt handles), timers — all accessed via handles, all waited on via one syscall. The kernel doesn't understand what any of these are _for_. It just manages them. This is a concrete identity statement for the kernel: it's the handle multiplexer. Everything semantic (content types, document state, filesystem layout, driver protocols, rendering) lives in userspace. The consequence: every new kernel feature should be expressible as "a new handle type that can be waited on." See also "Syscall API: composable verbs on typed handles" for the full API shape.

### Syscall API: composable verbs on typed handles (2026-03-08)

The syscall surface should be a small set of composable verbs, not per-type specialized calls. Three families emerged from the design discussion:

**Handle family (generic verbs, any handle type):** `wait(handles[])` blocks until any handle is ready (multiplexer — subsumes the old `channel_wait`). `close(handle)` releases any handle. `signal(handle)` notifies a channel peer. New handle types (timers, interrupts) get `wait` support for free — "every new kernel feature should be expressible as a new handle type that can be waited on."

**Synchronization family (address-based, no handles):** `futex_wait(addr, expected)` and `futex_wake(addr, count)`. Separate from handles because futexes are synchronization primitives, not event sources — you never multiplex across locks. PA-keyed for cross-process shared memory.

**Scheduling family (domain-specific verbs):** `sched_create`, `sched_bind`, `sched_borrow`, `sched_return`. Prefixed because `borrow`/`return` are too generic alone, and these operations are genuinely type-specific.

Design principles: (1) handle type carries context — `signal(channel_handle)` not `channel_signal`; (2) `wait` takes multiple handles because multiplexing IS its purpose — other syscalls take single handles; (3) streams/reactive composition lives in the OS service (userspace), not the kernel — the kernel provides the event primitive (`wait`), the OS service composes it.

The OS service architecture is naturally reactive/stream-based: merge input events, edit protocol events, and timer ticks → fold into document state → render. This maps cleanly to reactive stream combinators (most.js, RxJS pattern). The kernel doesn't need to understand streams — it just needs to be a good event source.

### Virtual manifests: documents as interfaces, not necessarily files on disk (2026-03-09)

A manifest can be static (stored on disk, COW-snapshotted) or virtual (content generated by the OS service on read, like Plan 9's `/proc`). Static manifests back user-created content. Virtual manifests back system-derived views: inbox (query over messages), search results, "recent documents," system dashboard. Both are files in the filesystem namespace. Both are documents to the user. The distinction is an implementation detail — same interface, different backing.

Virtual documents don't need their own COW history. Their "state at time T" is recoverable by re-evaluating the query against the snapshot of the world at time T. The underlying static documents have COW history; virtual documents inherit time-travel for free. Same reason database views don't need their own transaction log.

Key analogy: a video file is static on disk, but the user sees content that changes over time (temporal axis). An inbox is computed from live state, and the user sees content that changes as messages arrive. From the user's perspective, both are "things that show changing content." The mechanism differs; the experience doesn't. Virtual vs static is like table vs view in a database.

### All documents are persistent — "transient" is a retention policy, not a concept (2026-03-09)

Initially proposed "transient documents" (in-memory only, discarded on close) for things like viewed webpages. But this creates two persistence types the user must understand — a leaky abstraction. Instead: all documents are persistent by default. Webpages, imports, everything is written to the COW filesystem. Retention policies handle cleanup — viewed webpages might be kept for 30 days, user-created content kept permanently. The COW pruning system (needed anyway for edit history) handles document lifecycle too. One mechanism, not two.

This gives significant benefits for free: rewindable browsing (COW history of page views), offline access (previously viewed pages are on disk), full-text search across browsed content. Browsers already cache page assets to disk — this model structures that same data as first-class documents instead of an opaque cache blob.

Streaming content (YouTube video) is a virtual document: the manifest is persistent (metadata about what you're watching), but content is generated on demand from an external source. Same pattern as inbox (generated from internal state) — virtual manifests can derive content from internal OR external sources.

### Document mimetype resolution: imports vs OS-native (2026-03-09)

Imported documents retain their original external mimetype as manifest metadata (e.g., `application/vnd.openxmlformats-officedocument.presentationml.presentation` for .pptx). OS-native documents get custom mimetypes (e.g., `application/x-os-presentation`). The document-level mimetype drives editor binding. On export, the user selects a target format; the OS pre-selects the original mimetype where available (re-export imported .pptx defaults to .pptx). For OS-native documents, the user chooses from available export translators (like png vs jpg vs webp for images). Original mimetype is an optional metadata field — present for imports, absent for OS-native. This partially resolves the "mimetype of the whole" open question.

### Uniform rewind performance is a design constraint (2026-03-09)

If virtual document rewind is noticeably slower than static document rewind, users must know whether a document is static or virtual to set expectations — the abstraction leaks. This makes the metadata DB's placement a non-negotiable: it must live on the COW filesystem so its historical state is preserved in snapshots. Querying "inbox last Tuesday" then reads from the metadata DB at Tuesday's snapshot — same cost as a current query. This constraint flows from the virtual manifest model down into the filesystem COW design (Decision #16).

### Three-axis layout model unifies compositional and organizational documents (2026-03-09)

The original five layout types (flow, fixed canvas, timeline, grid, freeform canvas) were four spatial sub-types plus one temporal sub-type. They covered compositional documents (slides, articles, video projects) but not organizational ones (source code projects, albums, playlists). The missing piece: the **logical** axis (hierarchical, sequential, flat, graph). Adding it as a third composable axis alongside spatial and temporal unifies all compound documents under one model. Every document is a point in a three-dimensional space (spatial × temporal × logical). Most use one or two axes. The model was stress-tested against spreadsheets, chat threads, musical scores, comics, mind maps, calendars, and dashboards — everything fits. No convincing fourth axis was found. Spatial, temporal, and logical correspond to the fundamental ways humans organize anything: where, when, and how-related.

### Compositor is a React render pipeline (2026-03-10)

The compositor maps 1:1 to React's architecture. Component tree = document manifest (surface tree). Virtual DOM = scene graph. Reconciliation/diff = damage calculation. Minimal DOM patches = minimal pixel updates. Render = pure function of state. Even "commit phase" is the same term. This isn't a loose analogy — it's structural identity. Both solve the same problem: given a tree of visual content that changes incrementally, efficiently update the output. The difference: React operates on semantic elements (DOM nodes), compositor operates on pixel buffers (opaque rectangles). But the orchestration pattern — declarative tree → diff → minimal update — is identical.

### Structural constraints beat clever algorithms (2026-03-10)

Traditional compositors need sophisticated occlusion culling and z-ordering because they manage 30+ arbitrary overlapping windows. Our compositor needs none of that — one-document-at-a-time + manifest-driven layout means 3-4 z-layers total. The compositor's simplicity comes from the document model (Decision #2) and interaction model (one-doc-at-a-time leaning), not from algorithmic cleverness. This is an instance of the "simple connective tissue" principle (Decision #4): structural constraints at the design level eliminate runtime complexity.

### Compound documents are nested compositing (2026-03-10)

A compound document with embedded content parts (chart in a slide, image in a text doc) creates a surface tree structurally identical to nested windows — minus chrome, minus user-driven positioning. The compositor must handle this tree. This means "no windows" doesn't mean "flat compositor" — it means "compositor shaped by document structure instead of user window management." The compositor is the mechanism that makes compound document rendering work. This connects the unresolved compound editing tension to a concrete architectural requirement.

### Uniform manifest model eliminates the simple/compound distinction (2026-03-09)

Every document is backed by a manifest — even "simple" ones (single text file). The simple/compound distinction becomes an internal property (how many content references) rather than a user-facing concept. Users see documents, never files. Manifests are the only thing the metadata query system needs to index. Content files are the source of truth for content (indexed separately for full-text search). This makes concrete the principle already stated in CLAUDE.md: "Everything-is-files is architectural, not UX. Users see abstractions, not files."

### Content-type registration via metadata eliminates a separate registry (2026-03-09)

Editors are files too. If their metadata includes which content types they handle, then the metadata query system IS the content-type registry. One system for "find me things by their properties," whether those things are documents or tools. No separate mutable registry that can get out of sync.

### Version history is orthogonal to the layout model (2026-03-09)

COW snapshots are an OS-level mechanism, not a layout axis. An audio file has content temporality (the waveform) AND version history (the edits). Conflating them would mean "this audio track starts at 0:30" and "this file was edited yesterday" live on the same axis. They don't. Content temporality is part of what the document IS. Version history is how the document has CHANGED. The COW/undo system operates on a dimension outside the layout model entirely — which is why undo is an OS feature, not an editor feature.

### Scheduling policy needs no separate interface (2026-03-09)

The OS service already knows mimetype (fundamental metadata), editor lifecycle (manages it), and document state (renders it). When an editor sends "play" through the edit protocol, the OS service both starts rendering frames AND adjusts the scheduling context via existing kernel syscalls. Content-type-aware scheduling is internal policy logic driven by information already flowing through the edit protocol. No dedicated scheduling interface needed.

### The kernel boundary has exactly two clients (2026-03-09)

Editors don't talk to the kernel directly — they talk to the OS service via IPC (channels underneath, but the editor's interface is the edit protocol). Users don't touch the kernel. The syscall API serves exactly two kinds of clients: the OS service and userspace drivers.

### Red/blue/black is a complexity principle, not an architecture diagram (2026-03-09)

The red/blue/black model (external reality → adapters → core) serves as a complexity management principle: total complexity is conserved, blue absorbs external messiness, black stays clean. The architecture has additional structure within "black" — the kernel (clean through semantic ignorance, mechanism only) and the OS service (clean through design, policy through principled interfaces). These are two different kinds of cleanness. The architecture diagram (architecture.mermaid) captures this structural detail; the red/blue/black model stays as a principle.

### OS service interfaces are where the personality lives (2026-03-09)

Interface map by boundary:

| Boundary                   | Interface                                                     | Clients             | Status             |
| -------------------------- | ------------------------------------------------------------- | ------------------- | ------------------ |
| Kernel ↔ userland          | Syscall API (24 syscalls, typed handles)                      | OS service, drivers | Mostly designed    |
| OS service ↔ Editors       | Edit protocol (beginOp/endOp, state, input)                   | Editors             | Partially designed |
| OS service ↔ Shell         | Shell interface (navigation, document lifecycle, queries)     | Shell               | Partially scoped   |
| OS service ↔ Editors/Shell | Metadata query API (document discovery)                       | Editors, shell      | Sketched (#7)      |
| Blue ↔ Black               | Translator interface (format conversion, includes web engine) | All translators     | Blank              |
| Blue ↔ Black               | Driver interface (device access)                              | Device drivers      | Sketched           |
| OS service internal        | Renderer, layout engine, compositor, scheduling policy        | —                   | Blank              |

The kernel surface is small and stable. The blue-layer interfaces are about pluggability. The OS service boundary — edit protocol, metadata queries, interaction model — defines what it feels like to use this OS. The web engine adapter is not a separate interface from the translator interface (a webpage IS a compound document, handled through the same translator pattern as .docx).

### Full-codebase review resolved: cross-team API changes are the coordination cost (2026-03-10)

Resolved all 41 issues from DESIGN.md §11 using a 4-agent team partitioned by file ownership (assembly/linker/userspace, tests, scheduler/thread, remaining kernel src). The zero-overlap rule prevented all merge conflicts. The only coordination cost was cross-boundary API changes: when one agent changed a return type (`shared_info` → `Option`, `DrainHandles` tuple order, `KillInfo` → nested `HandleCategories`), callers in other agents' files broke. Three such ripples required lead intervention. Lesson for future multi-agent work: partition by API dependency boundary, not just file ownership. The borrow checker caught a real issue in the extracted `release_thread_context_ids` helper (split borrow needed for `s.cores[core].current` vs `s.scheduling_contexts`).

### Framebuffer is an implementation detail, surfaces are the abstraction (2026-03-09)

A raw framebuffer (`map() → &mut [u8]`) is specific to software rendering. With GPU acceleration, the CPU never touches pixel data — it submits commands and the GPU writes to VRAM. `map()` doesn't even make sense when the buffer isn't in CPU-accessible memory. The real abstraction is surfaces and operations on them: create, destroy, fill, blit, present. Every real display stack converged here (Wayland's `wl_surface`, macOS's `CALayer`, Windows' `DirectComposition`). A software implementation (surfaces as RAM buffers, CPU loops for operations, virtio-gpu for present) and a GPU implementation (surfaces as VRAM textures, GPU commands for operations, page flip for present) implement the same interface — the compositor doesn't know which is behind it.

Display (get pixels to screen) and rendering (fill the buffer) are separate concerns that always happen sequentially. GPU acceleration changes who does the rendering (GPU vs CPU), not the display path. Both live in the same device and same driver because modern GPU chips have a rendering engine and a display controller on one die. This parallels the Linux DRM/KMS split: KMS handles display (mode setting, scanout), OpenGL/Vulkan handle rendering (drawing commands). Two concerns, one driver.

### Birth time is the key insight for efficient snapshots (2026-03-08)

ZFS's single most important design choice for snapshots: store the birth transaction group (TXG) in every block pointer. When freeing a block, compare its birth time to the previous snapshot's TXG — if born after, free it; if born before, it belongs to the snapshot. This gives O(1) snapshot creation, O(delta) deletion, and unlimited snapshots. The alternative (per-snapshot bitmaps) is O(N) per snapshot and limits snapshot count. RedoxFS stores only a seahash checksum in block pointers — no temporal information. Adding birth generation to block pointers would be the minimum viable change to enable proper snapshots. Dead lists (ZFS's sub-listed approach) make deletion near-optimal: O(sublists + blocks to free). For our "no save" model where `endOperation` creates a snapshot, efficient deletion is critical.

### Operation boundaries map naturally to COW transaction boundaries (2026-03-08)

`beginOperation` opens a COW transaction, editor writes are COW'd, `endOperation` commits the transaction and creates a snapshot. No impedance mismatch. The edit protocol and the filesystem protocol are structurally the same thing — this is the kind of accidental alignment that suggests the design is coherent.

### Unsafe minimization as stated invariant (2026-03-08)

Audit of all ~99 `unsafe` blocks in the kernel found zero unnecessary uses. All fall into 7 categories: inline assembly, volatile MMIO, linker symbols, page table walks, GlobalAlloc, Send/Sync impls, stack/context allocation. The kernel already follows the Asterinas pattern (unsafe foundation + safe services) emergently. Formalized as section 7.1 in kernel DESIGN.md to prevent drift as the codebase grows. Key rule: if the OS service (EL0) ever needs `unsafe`, the kernel API is missing an abstraction.

### Microkernel by convergence, not ideology (2026-03-08)

Each kernel sub-decision independently pushed complexity outward: drivers to userspace (fault isolation + unsafe minimization), filesystem to userspace (complex code outside TCB, hot path in kernel VM anyway), rendering to the OS service (not in-kernel), editors to separate processes (untrusted). What remains is exactly the microkernel set: address spaces, threads, IPC, scheduling, interrupt forwarding, handles. This wasn't a top-down decision to "build a microkernel" — it's what fell out of applying the project's principles (simple connective tissue, unsafe minimization, fault isolation, one model not two) to each sub-decision in turn. The kernel's identity emerged from its constraints: it multiplexes hardware resources behind handles and provides a single event-driven wait mechanism. Everything semantic lives in userspace. The L4 cautionary tale ("total complexity conserved") still applies — but the complexity displacement is justified at each boundary by specific architectural arguments, not by microkernel ideology.

### Trust and complexity are orthogonal axes (2026-03-10)

Red/blue/black (complexity: where does messiness live?) and kernel/OS service/tools (trust: what happens if it crashes?) are independently useful models. Conflating them creates apparent paradoxes — "where do editors go?" — because editors are messy (blue) but untrusted (not black), and those seem to point in different directions. Separating the axes reveals the architecture's symmetry: the core is both clean and trusted, adapters are both messy and untrusted, but for different reasons. The kernel is clean through ignorance. The OS service is clean through design. Drivers are messy because hardware is messy. Editors are messy because users are unpredictable.

### The blue layer wraps the core on all sides (2026-03-10)

The adaptation layer isn't just below (hardware drivers). The user is external reality too — unpredictable, shaped by expectations from other systems. Editors are "user drivers": they adapt human intent into the structured edit protocol, just as display drivers adapt device registers into the surface trait. `beginOperation/endOperation` is to editors what `create_surface/fill_rect/present` is to drivers. The OS core sits in the middle, semantically ignorant in both directions. This completes a symmetry: below (drivers adapt hardware), sides (translators adapt formats), above (editors and shell adapt users).

### The shell is a tool, not part of the OS (2026-03-10)

The shell (GUI/CLI) is architecturally identical to editors — an untrusted EL0 process in the blue layer. It binds to "system state" the same way a text editor binds to `text/*`. It translates navigational intent (find, open, switch) into OS service operations (metadata queries, document lifecycle). The OS service doesn't know or care what the interaction _feels like_ — the shell owns the UX, the OS owns the mechanism. If the shell crashes, the OS service provides a recovery fallback (same pattern as rendering a document with no editor attached). The shell is pluggable, though the OS will be tuned toward its primary shell's needs.

### User input always goes to a tool (2026-03-10)

There is always an active tool. The OS service routes input; it never interprets it. When an editor is active, it receives modification input. When no editor is active, the shell receives navigational input. This extends the editor model (one active per document) to the system level: one active tool, period. The OS service has no "bare" input handling mode. This makes the interaction model a shell design question, not an OS service design question — same separation as everywhere else (OS provides mechanism, tools bring semantics).

### Configuration is a protocol's opening sequence (2026-03-10)

Init passes device addresses and framebuffer info to drivers before starting them — fundamentally different from ongoing conversation. Initially leaned toward two mechanisms (config structs vs ring buffers). But Singularity showed the cleaner model: configuration is the opening messages in the channel's protocol. A GPU driver's "contract" starts with `state Init { receive ConfigMsg → Running }`. One mechanism, config is just the first message(s). Avoids the blurry boundary problem — what happens when a "config" channel later needs runtime updates? With one mechanism, it just sends more messages. No mechanism switch. Prior art: Singularity (contracts), QNX (MsgSend for everything). Counter-examples: Fuchsia (separate processargs), Unix (argv vs pipes). The temporal asymmetry (config is pre-start) is real but doesn't require a separate mechanism — the ring buffer is initialized before the child starts, just like the raw byte layout was.

### Fixed-size ring entries are the high-performance consensus (2026-03-10)

io_uring (64-byte SQE), LMAX Disruptor, L4 message registers, virtio descriptors (16 bytes) — all chose fixed-size entries in the ring, with variable-size data elsewhere. The arguments compound: no fragmentation, no wraparound complexity, predictable prefetching, one-cache-line-per-message on AArch64 (64 bytes = cache line). When you need large data, it goes in shared memory with a reference through the ring. This matches the OS design's existing principle (documents are memory-mapped, ring buffers carry control only) and makes it a design rule rather than a pressure point.

### Security as a side effect of good architecture (2026-03-07)

Handles enforce access (designed for edit protocol, not security). EL0/EL1 provides crash isolation (designed for clean programming model). Per-process address spaces provide memory isolation (designed for independent editors). Kernel message validation protects the OS service (designed for input correctness). Every security property falls out of design decisions made for other reasons. No security-specific machinery is needed because the architecture is naturally secure. This suggests a useful heuristic: if you're adding security features that don't serve the design, the architecture may be wrong.

### Editors as read-only consumers: "never make the wrong path the happy path" (2026-03-11)

The original edit protocol had editors calling beginOp/endOp around direct memory-mapped writes. This makes undo opt-in — a lazy editor that just writes without calling begin/end gets no undo points. The wrong path (skip the protocol) is the easy path. Inverted: editors get read-only mappings of documents. All writes go through the OS service via IPC. The OS service is the sole writer to document files, giving it full control over snapshots and undo with zero cooperation required from editors. Undo is automatic and non-circumventable. The lazy editor path (just send write requests) produces correct undo behavior. The diligent editor path (grouping writes into named operations) produces better undo granularity. The symmetry with the kernel is preserved: the kernel doesn't let processes write to other processes' memory (it goes through IPC). The OS doesn't let editors write to the OS's documents (it goes through IPC). Documents are shared resources — the OS renders them, versions them, indexes them — so mediated access follows the same principle as any shared resource. Performance is not a concern: the hot-path workloads (text editing, image adjustments) are low-bandwidth IPC; bulk data operations (rendering, audio capture) don't write to the document at those rates.

### OS service event loop eliminates file locking (2026-03-11)

Multiple data sources (editors, network services, audio drivers) may want to write to the same document concurrently (e.g., chat where user types and remote messages arrive). But all writes arrive at the OS service's event loop as IPC messages and are processed sequentially. Multiple sources of data arriving concurrently is not the same as multiple writers to a file. The serialization happens at the event loop, which already exists. No file locking needed because there's only one writer. This is the web server pattern: multiple clients, one server, sequential processing per resource.

### Copy semantics + COW = logical independence with physical efficiency (2026-03-11)

Compound documents use copy semantics — embedding a photo in a slide deck creates an independent copy. No reference tracking, no broken links, no cascading deletes. COW at the filesystem level means the "copy" shares physical blocks with the original until one diverges. The user gets clean mental model (each document is self-contained), the disk gets efficient storage (shared blocks). Original file ID stored as provenance metadata enables explicit "update to latest" (pull, not push). One-directional knowledge: compound knows about original, original doesn't know about compound.

### The filesystem is a dumb file store (2026-03-11)

By settling "OS service is the sole writer" and "compound documents use copy semantics," the filesystem's job became radically simple: store files by ID, provide memory-mapped access, take snapshots, restore snapshots. 12 operations. No paths, no permissions, no locking, no links, no file types, no extended attributes, no batch operations. Everything "interesting" (documents, undo ordering, metadata, queries, compound structures) lives above the filesystem in the OS service and metadata DB layers. Three-layer translation: user intent → metadata DB → document IDs → OS service → file IDs → filesystem. Each layer ignorant of the one above. The filesystem is the simplest possible foundation — its only job is correctness about COW, snapshots, and crash consistency.

### Compound document atomicity solved by sole-writer architecture (2026-03-11)

An edit on a compound document might touch multiple files (manifest + content files). With the OS service as sole writer, it simply does both writes sequentially and then takes a snapshot. No multi-file transaction mechanism needed in the filesystem. The atomicity problem that seemed to require filesystem support was actually solved by an architectural decision at a different layer. This is a recurring pattern: structural constraints at one level eliminate complexity at another.

---

## Research Spikes

Active or planned coding explorations. These are learning exercises, not commitments. Code may be thrown away.

### Bare metal boot on arm64 (QEMU)

**Status:** Complete — all 7 steps done
**Goal:** Build a minimal kernel on aarch64/QEMU. Learn what's involved in boot, exception handling, context switching, memory management.
**Informs:** Decision #16 (Technical Foundation) — whether writing our own kernel is tractable and worthwhile vs. building on existing.
**What exists:** `system/kernel/` — ~2,150 lines across 18 source files (at time of spike completion). boot.S (boot trampoline, coarse 2MB page tables, EL2→EL1 drop, early exception vectors), exception.S (upper-VA vectors, context save/restore, SVC routing), main.rs (Context struct, kernel_main, irq/svc dispatch, ELF loader + user thread spawn), elf.rs (pure functional ELF64 parser), build.rs (compiles user ELFs at build time), memory.rs (TTBR1 L3 refinement for W^X, PA/VA conversion, empty TTBR0 for kernel threads), heap.rs (bump allocator, 16 MiB), page_alloc.rs (free-list 4KB frame allocator), asid.rs (8-bit ASID allocator), addr_space.rs (per-process TTBR0 page tables, 4-level walk_or_create, W^X user page attrs with nG), scheduler.rs (round-robin preemptive, TTBR0 swap on context switch), thread.rs (kernel + user thread creation, separate kernel/user stacks), syscall.rs (exit/write/yield, user VA validation), timer.rs (ARM generic timer at 10 Hz), gic.rs (GICv2 driver), uart.rs (PL011 TX), mmio.rs (volatile helpers). Init later promoted to proto-OS-service at `system/services/init/`. Builds with `cargo run --release` targeting `aarch64-unknown-none` on nightly Rust.
**Original success criteria:** ~~Something boots and prints to serial console.~~ Done.
**Next steps (in order):**

1. ~~**Timer interrupt**~~ — Done. ARM generic timer fires at 10 Hz, IRQ path exercises full context save/restore, tick count prints to UART.
2. ~~**Page tables + enable MMU**~~ — Done. Identity-mapped L0→L1→L2 hierarchy with 2MB blocks, L3 4KB pages for kernel region with W^X permissions (.text RX, .rodata RO, .data/.bss/.stack RW NX).
3. ~~**Heap allocator**~~ — Done. Bump allocator (advance pointer, never free), 16 MiB starting at `__kernel_end`. Lock-free CAS loop. Unlocks `alloc` crate (Vec, Box, etc.).
4. ~~**Kernel threads + scheduler**~~ — Done. Thread struct with Context at offset 0 (compile-time assertion). Round-robin in `irq_handler` on each timer tick. Boot thread becomes idle thread (`wfe`). Box<Thread> for pointer stability (TPIDR_EL1 holds raw pointers into contexts). IRQ masking around scheduler state mutations.
5. ~~**Syscall interface**~~ — Done. SVC handler with ESR check, syscall table (exit/write/yield), user VA validation. EL0 test stub proves full EL0→SVC→EL1→eret path.
6. ~~**Per-process address spaces**~~ — Done. Kernel at upper VA (TTBR1), per-process TTBR0 with 8-bit ASID, 4-level page tables (walk_or_create), W^X user pages with nG bit, frame allocator for dynamic page table allocation, scheduler swaps TTBR0 on context switch, empty TTBR0 for kernel threads.
7. ~~**First real userspace process**~~ — Done. Standalone init binary compiled to ELF64 by build.rs, embedded in kernel via `include_bytes!`. Pure functional ELF parser extracts PT_LOAD segments. Loader allocates frames, copies data, maps with W^X permissions. Entry point from ELF header. Init later promoted to proto-OS-service at `system/services/init/`.

**Known simplifications (intentional, revisit later):** Single-core only (multi-core after userspace works). Bump allocator never frees (replace when threads are created/destroyed). No per-CPU IRQ stack (not needed — EL0→EL1 transitions use SP_EL1 automatically). 10 Hz timer (increase when scheduling granularity matters). No ASID recycling (255 max user address spaces). Coarse TTBR0 identity map from boot.S still loaded but unused after transition to upper VA.

Dependencies: All 7 steps complete. The spike validated the full stack: boot → MMU → heap → threads → syscalls → per-process address spaces → ELF loading. From-scratch kernel in Rust on aarch64 is tractable. Binary format settled as ELF.

**Risk:** If we decide to build on an existing kernel, this code is throwaway. That's fine — the knowledge isn't throwaway.
