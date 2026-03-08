# Exploration Journal

A research notebook for the OS design project. Tracks open threads, discussion backlog, and insights across sessions. This is the "pick up where you left off" document.

---

## Open Threads

Active questions we've started exploring but haven't resolved. Each thread links to the decisions it would inform.

### Is "compound" intrinsic or contextual?

**Informs:** Decision #14 (Compound Documents), Glossary
**Status:** Identified, not yet explored in depth
**Context:** A PDF has a single mimetype but contains text + images + vector graphics. Is it always compound (OS knows it has parts), or only compound when deliberately decomposed? Same question for ZIP, mp4. The answer must be consistent with the decomposition spectrum — we draw the line at mimetypes, so if something has one mimetype, is it by definition simple?

### Referenced vs owned parts in compound documents

**Informs:** Decision #14 (Compound Documents)
**Status:** Identified, not yet explored
**Context:** A slideshow referencing photos from your library (independent, survive deletion) vs. text blocks that only exist within the slideshow (owned, deleted with compound). Is this a property of the reference, a user choice, or two distinct relationship types?

### View/edit in the CLI

**Informs:** Decision #17 (Interaction Model)
**Status:** Briefly mentioned, not explored
**Context:** The view/edit distinction is clear in GUI. How does it translate to CLI? Tools-as-subshells? Read-commands-always-safe? The CLI and GUI are equally fundamental interfaces (Belief #4), so the CLI can't be an afterthought.

### Kernel architecture

**Informs:** Decision #16 (Technical Foundation)
**Status:** Research spike complete (all 7 steps). IPC mechanism settled. Scheduling algorithm settled. Most sub-decisions settled.
**Context:** From-scratch kernel (committed, promoted from tentative) with Rust (committed) on aarch64. Settled: soft RT, no hypervisor (EL1 not EL2), preemptive + cooperative yield, traditional privilege model (all userspace at EL0), split TTBR (TTBR1 kernel, TTBR0 per-process), OS-mediated handles for access control, ELF as binary format, IPC via shared memory ring buffers with handle-based access control, three-layer process architecture (kernel + OS service + editors), SMP (4 cores), EEVDF + scheduling contexts (combined). Remaining unknowns: driver model, filesystem.

### ~~Privilege model (EL1 / EL0 boundary)~~ — SETTLED

**Resolved:** Traditional — all non-kernel code at EL0. One simple boundary, one programming model. Consistent with Decision #4 (simple connective tissue) and Decision #3 (arm64-standard interface). Language-safety (B) rejected as unsolved research problem for extensibility. Hybrid (C) rejected as two-ways-to-do-the-same-thing. See Decision #16 in decisions.md.

### ~~Address space model (TTBR0 / TTBR1)~~ — SETTLED

**Resolved:** Split TTBR — TTBR1 for kernel (upper VA), TTBR0 per-process (lower VA). Follows directly from the traditional privilege model. See Decision #16 in decisions.md.

---

## Discussion Backlog

Topics to explore, roughly prioritized by which unsettled decisions they'd inform. Not a task list — a menu of interesting conversations to have when the urge strikes.

### High leverage (unblocks multiple decisions)

1. **Rendering technology deep dive** (Decision #11) — The next most consequential unsettled decision. Constrains layout, tech foundation, and interaction model. Should explore: what does Servo look like embedded? What does "programs talk to engine through a protocol" actually mean in practice? What are the real costs of a web engine dependency?

2. ~~**What does the IPC look like?**~~ (Decision #16) — **SETTLED.** Shared memory ring buffers with handle-based access control. One mechanism for all IPC. Kernel creates channels and validates messages at trust boundaries, but is not in the data path. Documents are memory-mapped separately. Editor ↔ OS service ring buffers carry control messages only: edit protocol (beginOp/endOp), input events, overlay descriptions. Metadata queries use a separate interface (not the editor channel — different cadence, potentially large results). Three-layer process architecture: kernel (EL1) + OS service (EL0, trusted, one process for rendering + metadata + input + compositing) + editors (EL0, untrusted). See Decision #16 in decisions.md.

3. **The interaction model** (Decision #17) — What does using this OS actually feel like? Mercury OS and Xerox Star are reference points. How do you find documents? What does "opening" something look like? How do queries surface in the GUI?

### Medium leverage (deepens settled decisions)

4. **Compound document authoring workflow** — We know the structure (manifests + references + layout), but how does a user actually _create_ a compound document? Do they start with a layout and add content? Does it emerge from combining simple documents?

5. **Content-type rebase handlers in practice** — We know the theory (git merge generalized). What would a text rebase handler actually look like as an API? What about images? This would validate the edit protocol's upgrade path.

6. **The metadata query API** — Decision #7 settled on "simple query API backed by embedded DB." What does this API actually look like? What are the verbs? How does it feel to use from both GUI and CLI?

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

### Security as a side effect of good architecture (2026-03-07)

Handles enforce access (designed for edit protocol, not security). EL0/EL1 provides crash isolation (designed for clean programming model). Per-process address spaces provide memory isolation (designed for independent editors). Kernel message validation protects the OS service (designed for input correctness). Every security property falls out of design decisions made for other reasons. No security-specific machinery is needed because the architecture is naturally secure. This suggests a useful heuristic: if you're adding security features that don't serve the design, the architecture may be wrong.

---

## Research Spikes

Active or planned coding explorations. These are learning exercises, not commitments. Code may be thrown away.

### Bare metal boot on arm64 (QEMU)

**Status:** Complete — all 7 steps done
**Goal:** Build a minimal kernel on aarch64/QEMU. Learn what's involved in boot, exception handling, context switching, memory management.
**Informs:** Decision #16 (Technical Foundation) — whether writing our own kernel is tractable and worthwhile vs. building on existing.
**What exists:** `system/kernel/` + `system/user/init/` — ~2,150 lines across 18 source files. boot.S (boot trampoline, coarse 2MB page tables, EL2→EL1 drop, early exception vectors), exception.S (upper-VA vectors, context save/restore, SVC routing), main.rs (Context struct, kernel_main, irq/svc dispatch, ELF loader + user thread spawn), elf.rs (pure functional ELF64 parser), build.rs (compiles init.S → init.elf at build time), memory.rs (TTBR1 L3 refinement for W^X, PA/VA conversion, empty TTBR0 for kernel threads), heap.rs (bump allocator, 16 MiB), page_alloc.rs (free-list 4KB frame allocator), asid.rs (8-bit ASID allocator), addr_space.rs (per-process TTBR0 page tables, 4-level walk_or_create, W^X user page attrs with nG), scheduler.rs (round-robin preemptive, TTBR0 swap on context switch), thread.rs (kernel + user thread creation, separate kernel/user stacks), syscall.rs (exit/write/yield, user VA validation), timer.rs (ARM generic timer at 10 Hz), gic.rs (GICv2 driver), uart.rs (PL011 TX), mmio.rs (volatile helpers). User program: system/user/init/ (init.S + link.ld). Builds with `cargo run --release` targeting `aarch64-unknown-none` on nightly Rust.
**Original success criteria:** ~~Something boots and prints to serial console.~~ Done.
**Next steps (in order):**

1. ~~**Timer interrupt**~~ — Done. ARM generic timer fires at 10 Hz, IRQ path exercises full context save/restore, tick count prints to UART.
2. ~~**Page tables + enable MMU**~~ — Done. Identity-mapped L0→L1→L2 hierarchy with 2MB blocks, L3 4KB pages for kernel region with W^X permissions (.text RX, .rodata RO, .data/.bss/.stack RW NX).
3. ~~**Heap allocator**~~ — Done. Bump allocator (advance pointer, never free), 16 MiB starting at `__kernel_end`. Lock-free CAS loop. Unlocks `alloc` crate (Vec, Box, etc.).
4. ~~**Kernel threads + scheduler**~~ — Done. Thread struct with Context at offset 0 (compile-time assertion). Round-robin in `irq_handler` on each timer tick. Boot thread becomes idle thread (`wfe`). Box<Thread> for pointer stability (TPIDR_EL1 holds raw pointers into contexts). IRQ masking around scheduler state mutations.
5. ~~**Syscall interface**~~ — Done. SVC handler with ESR check, syscall table (exit/write/yield), user VA validation. EL0 test stub proves full EL0→SVC→EL1→eret path.
6. ~~**Per-process address spaces**~~ — Done. Kernel at upper VA (TTBR1), per-process TTBR0 with 8-bit ASID, 4-level page tables (walk_or_create), W^X user pages with nG bit, frame allocator for dynamic page table allocation, scheduler swaps TTBR0 on context switch, empty TTBR0 for kernel threads.
7. ~~**First real userspace process**~~ — Done. Standalone init binary (system/user/init/) compiled to ELF64 by build.rs, embedded in kernel via `include_bytes!`. Pure functional ELF parser extracts PT_LOAD segments. Loader allocates frames, copies data, maps with W^X permissions. Entry point from ELF header. Replaces the old embedded .user_code hack.

**Known simplifications (intentional, revisit later):** Single-core only (multi-core after userspace works). Bump allocator never frees (replace when threads are created/destroyed). No per-CPU IRQ stack (not needed — EL0→EL1 transitions use SP_EL1 automatically). 10 Hz timer (increase when scheduling granularity matters). No ASID recycling (255 max user address spaces). Coarse TTBR0 identity map from boot.S still loaded but unused after transition to upper VA.

Dependencies: All 7 steps complete. The spike validated the full stack: boot → MMU → heap → threads → syscalls → per-process address spaces → ELF loading. From-scratch kernel in Rust on aarch64 is tractable. Binary format settled as ELF.

**Risk:** If we decide to build on an existing kernel, this code is throwaway. That's fine — the knowledge isn't throwaway.
