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
**Status:** Active — research spike in progress, partial sub-decisions made
**Context:** From-scratch kernel (tentative) with Rust (tentative) on aarch64. Settled: soft RT, no hypervisor (EL1 not EL2), preemptive + cooperative yield. Open: privilege model, address space model, IPC, binary format. L4 cautionary tale still relevant. Need to hit real obstacles (scheduling, memory management, driver complexity) before evaluating alternatives (seL4, Zircon, Linux-as-runtime).

### Privilege model (EL1 / EL0 boundary)
**Informs:** Decision #16 (Technical Foundation)
**Status:** Three options identified, not yet decided
**Context:** Kernel runs at EL1 (settled by ruling out hypervisor). What runs at EL0? Options: (A) traditional — all non-kernel code at EL0, (B) language-safety — everything at EL1, rely on Rust, (C) hybrid — kernel + viewers at EL1, editors at EL0. Key tension: editor immediate-writes want low overhead (favors B/C), but extensibility and third-party editors want hardware isolation (favors A/C). Connects to editor model (Decision #8) and complexity philosophy (Decision #4).

### Address space model (TTBR0 / TTBR1)
**Informs:** Decision #16 (Technical Foundation)
**Status:** Leaning single (TTBR0 only), not committed
**Context:** Current boot code uses only TTBR0 with EPD1=1 (TTBR1 walks disabled). Traditional split uses TTBR0 for user pages, TTBR1 for kernel pages. Single address space is simpler but relies on other isolation mechanisms. Decision depends on privilege model — if everything runs at EL1, split address space is less necessary.

---

## Discussion Backlog

Topics to explore, roughly prioritized by which unsettled decisions they'd inform. Not a task list — a menu of interesting conversations to have when the urge strikes.

### High leverage (unblocks multiple decisions)

1. **Rendering technology deep dive** (Decision #11) — The next most consequential unsettled decision. Constrains layout, tech foundation, and interaction model. Should explore: what does Servo look like embedded? What does "programs talk to engine through a protocol" actually mean in practice? What are the real costs of a web engine dependency?

2. **What does the IPC look like?** (Decision #16) — The edit protocol describes beginOperation/endOperation, but what's the actual mechanism? Message passing, shared memory, something else? This is where the abstract design meets reality. Closely tied to rendering technology choice.

3. **The interaction model** (Decision #17) — What does using this OS actually feel like? Mercury OS and Xerox Star are reference points. How do you find documents? What does "opening" something look like? How do queries surface in the GUI?

### Medium leverage (deepens settled decisions)

4. **Compound document authoring workflow** — We know the structure (manifests + references + layout), but how does a user actually *create* a compound document? Do they start with a layout and add content? Does it emerge from combining simple documents?

5. **Content-type rebase handlers in practice** — We know the theory (git merge generalized). What would a text rebase handler actually look like as an API? What about images? This would validate the edit protocol's upgrade path.

6. **The metadata query API** — Decision #7 settled on "simple query API backed by embedded DB." What does this API actually look like? What are the verbs? How does it feel to use from both GUI and CLI?

### Exploratory (interesting but less urgent)

7. **Historical OS deep dives** — Plan 9's /proc and per-process namespaces. BeOS's BFS attributes in practice. OpenDoc's component model and why it failed. Xerox Star's property sheets. Each could inform current design.

8. **The "no save" UX** — We committed to immediate writes + COW. What does this feel like for content that's expensive to re-render? What about "I was just experimenting, throw this away"? Is there a need for explicit "draft mode" or does undo cover it?

9. **Editor plugin API design** — What's the actual interface between an editor plugin and the OS? How does an editor register, receive input, draw overlays? This is where the abstract editor model becomes concrete.

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

---

## Research Spikes

Active or planned coding explorations. These are learning exercises, not commitments. Code may be thrown away.

### Bare metal boot on arm64 (QEMU)
**Status:** Active — boots and prints to UART
**Goal:** Build a minimal kernel on aarch64/QEMU. Learn what's involved in boot, exception handling, context switching, memory management.
**Informs:** Decision #16 (Technical Foundation) — whether writing our own kernel is tractable and worthwhile vs. building on existing.
**What exists:** `system/kernel/` directory — boot.S (boot sequence, EL2→EL1 drop, exception vectors, full context save/restore for IRQ, UART output helpers, fatal exception handler, MMU setup), main.rs (Context struct with compile-time layout assertions, kernel_main, irq_handler stub), uart.rs (PL011 MMIO driver). Builds with `cargo build` targeting `aarch64-unknown-none` on stable Rust.
**Original success criteria:** ~~Something boots and prints to serial console.~~ Done.
**Next steps (in order):**
1. **Timer interrupt** — Set up ARM generic timer to fire periodically. Exercises the IRQ path end-to-end, prints tick count to UART. Prerequisite for scheduler. Quick win.
2. **Page tables + enable MMU** — Build identity-mapped page tables, call `enable_mmu`. This is where bare-metal gets real and the spike starts answering its question (is from-scratch worth it?).
3. **Heap allocator** — Carve out a region for dynamic allocation. Bump allocator (advance pointer, never free) is enough to start. Unlocks dynamic data structures.
4. **Threads + scheduler** — Create thread contexts, implement round-robin in `irq_handler`, launch with `enter_first_thread`. The payoff: actual preemptive multitasking. Two threads printing alternating messages.

Dependencies: Timer and page tables are independent. Scheduler needs both timer (for preemption) and heap (for thread contexts).

**Risk:** If we decide to build on an existing kernel, this code is throwaway. That's fine — the knowledge isn't throwaway.
