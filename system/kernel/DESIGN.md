# Kernel Design Notes

Architectural decision record for every kernel subsystem. Captures the "why" behind each choice: the goal, the approach taken, alternatives considered and rejected, and the rationale. Sections 0.x cover the foundational architecture (greenfield decisions). Sections 1.x–5.x cover improvements made during the roadmap phase.

The finished code's module-level docs are the authoritative reference for _what_ the code does; these notes capture _why_ it does it that way.

---

## 0.1 Boot Sequence & Exception Level

**Goal:** Cold boot on QEMU `virt`, transition from EL2 (hypervisor) to EL1 (kernel), enable MMU, enter Rust.

**Approach:** `boot.S` assembly trampoline. Core 0 only (parks others via MPIDR check). Creates coarse 2 MiB block mappings for both identity (TTBR0, needed until MMU is live) and kernel upper VA (TTBR1). Enables MMU, drops to EL1h via `ERET`, jumps to `kernel_main` at upper VA.

**Why EL1 (not EL2):** Hypervisor mode provides no benefit for a single-OS kernel. EL2 adds complexity (stage-2 translation, HCR_EL2 configuration) with no upside. Decision #16 explicitly rejected hypervisor design.

**Why coarse then refine:** `boot.S` must be minimal — just enough to enable the MMU and reach Rust. Fine-grained W^X refinement happens in Rust (`memory::init()`) once the kernel is running at upper VA with a heap available.

---

## 0.2 Address Space Layout (Split TTBR)

**Goal:** Isolate kernel and user address spaces using hardware support.

**Approach:** ARMv8 split TTBR — `TTBR1_EL1` maps the kernel (upper VA, `0xFFFF_...`), `TTBR0_EL1` maps user processes (lower VA, `0x0000_...`). Kernel VA = PA + `0xFFFF_0000_0000_0000` (simple offset). User VA layout: code at 4 MiB, channel shared memory at 1 GiB, stack at 2 GiB.

**Alternatives considered:**

- **Single address space** (TTBR0 only, kernel/user separated by range): Rejected. Split TTBR is the intended ARM mechanism. Gives free kernel isolation and makes context switch cheaper — only TTBR0 is swapped, TTBR1 stays.

**Why this VA layout:** Code at 4 MiB avoids null-pointer dereference landing in valid code. Channel shared memory and stack at fixed VAs keep the memory map predictable. Guard page below the stack catches overflow (no VMA → fault → kill).

---

## 0.3 Exception Handling & Context Save/Restore

**Goal:** Handle IRQs (timer), SVCs (syscalls), and synchronous faults (page faults) from EL0.

**Approach:** `exception.S` installs `VBAR_EL1` with the standard ARM exception vector table. On any exception: save full register state (x0–x30, SP, ELR, SPSR, SP_EL0, TPIDR_EL0, NEON q0–q31, FPCR/FPSR) to the current Thread's `Context` at offset 0 (located via `TPIDR_EL1`). Call the appropriate Rust handler. Handler returns a `Context` pointer — possibly a different thread. Restore from that pointer and `ERET`.

**Why save NEON eagerly:** User code can use SIMD at any time. Lazy save tracks "dirty" state per thread, adding complexity for marginal benefit. Full save is ~512 bytes per context switch but predictable and simple.

**Why Context at offset 0:** `TPIDR_EL1` → `Thread` → `Context` with zero offset computation. Simple, fast. A compile-time assertion (`offset_of!(Thread, context) == 0`) enforces this invariant.

---

## 0.4 Page Tables & W^X Enforcement

**Goal:** Fine-grained page permissions for both kernel and user code.

**Approach:** Boot starts with 2 MiB blocks. `memory::init()` refines the kernel's block into 4 KiB L3 pages with per-section permissions: `.text` RX, `.rodata` RO, `.data`/`.bss` RW. User pages are always 4 KiB with per-page W^X (enforced by `segment_attrs`: X wins over W if both set).

**Why W^X:** Any page that is both writable and executable is an injection vector. W^X is the minimum viable security invariant for page permissions. ARM provides the bits (`PXN`, `UXN`, `AP_RO`) — using them costs nothing.

---

## 0.5 Heap Allocator

**Goal:** Dynamic allocation for kernel data structures (threads, page tables, vectors, etc.).

**Approach:** Three-tier allocation:

1. **Slab caches** for common sizes (64–2048 bytes, O(1) alloc/free).
2. **Linked-list allocator** for variable sizes (first-fit, address-sorted free list, coalescing on free).
3. **Buddy allocator** for page frames (contiguous 2^n, 4 KiB – 4 MiB).

`GlobalAlloc` routes by size: ≤2 KiB → slab, else → linked-list. Page frames are requested separately via `page_alloc::alloc_frame()`.

**Dealloc routing:** Address-based, not size-based. `dealloc()` checks whether the pointer falls within the linked-list heap region `[region_start, region_end)`. If so, it goes to the linked-list. Pointers outside that range (in slab pages allocated from the buddy allocator) go to slab. This prevents cross-allocator contamination: during early boot (before `page_allocator::init()`), slab can't grow, so the linked-list serves all allocations. If dealloc routed by size class, those frees would go to slab — corrupting its free list with linked-list addresses.

**PA validation:** `free_frames()` validates that the PA is page-aligned and within the physical RAM range before writing to it. A corrupted PA would cause a data abort (writing to an unmapped address). This catches allocator bugs early instead of propagating silent corruption.

**Alternatives considered:**

- **Bump allocator:** Can't free individual allocations. A kernel needs to free thread stacks, page table nodes, etc. Ruled out.
- **Single linked-list for everything:** Works but O(n) per alloc. The kernel allocates many small, fixed-size objects (threads, VecDeque nodes). Slab is O(1) for these.

**Why three tiers:** Each tier handles what it's best at. Slab eliminates fragmentation for hot-path small objects. Linked-list handles rare variable-size allocations. Buddy handles physically contiguous multi-page allocations (needed for DMA, page tables).

---

## 0.6 Process Model & ELF Loading

**Goal:** Run user code at EL0 with isolated address spaces.

**Approach:** Pure functional ELF64 parser (`no_std`, no allocation) extracts PT_LOAD segments. Process creation: allocate ASID + L0 table, create VMAs for each segment + stack, eagerly map first code page + top stack page, demand-page the rest. Each process owns: address space, handle table, kernel stack (16 KiB).

**Why ELF:** Standard, well-documented, what the Rust toolchain produces. No custom format needed.

**Why demand paging:** Avoids allocating all pages upfront. Process startup is fast (2 pages mapped vs. potentially dozens). Foundation for future memory-mapped files.

**Why `include_bytes!` (no filesystem):** No filesystem exists yet. `build.rs` compiles user programs and embeds the ELF binaries in `.rodata`. Bootstrap solution — will be replaced when the filesystem is implemented.

**Cleanup:** On process exit, the kernel drains all handles (closing channel endpoints), invalidates TLB entries for the ASID, frees all owned page frames + page table frames, and releases the ASID. Full resource reclamation — no leaks.

---

## 0.7 IPC Channels & Shared Memory

**Goal:** Inter-process communication between user processes.

**Approach:** Shared-memory ring buffers. Kernel allocates a physical page, maps it into both processes at a fixed VA (`CHANNEL_SHM_BASE + index * PAGE_SIZE`). Signal/wait notification primitives with a persistent flag (lost-wakeup safe). Lock ordering invariant: channel lock always released before acquiring the scheduler lock.

**Alternatives considered and rejected:**

- **L4-style synchronous messages:** Blocks the sender until the receiver is ready. Can't deliver input to a busy editor — incompatible with the OS design where editors may be computation-bound.
- **Mach-style async queued messages:** Double copy (sender → kernel → receiver). Extra complexity and overhead for no benefit over direct shared memory.
- **Star topology (kernel in every data path):** Kernel becomes a bottleneck. Data should flow directly between processes; kernel mediates only the control plane.

**Why this design:** Shared memory gives zero-copy data transfer. Kernel handles only creation and handle management (control plane), not data flow. Documents will be memory-mapped separately — ring buffers carry only control messages (edit protocol, input events, overlays). One mechanism for all IPC.

**Prior art:** io_uring (ring buffers in shared memory), Fuchsia channels (handle-based IPC).

---

## 0.8 Handle-Based Access Control

**Goal:** OS-mediated access control for kernel objects.

**Approach:** Per-process fixed-size array (256 slots). Each slot holds a kernel object reference + rights bitfield (read/write). Kernel validates handle + rights on every syscall. Handles are indices (not pointers) — user code can't forge access.

**Alternatives considered:**

- **Full capability system** (Fuchsia-style): More powerful (capability transfer, rights attenuation) but significantly more complex. Over-engineered for current needs.
- **Centralized ACLs:** Separate permissions database. Extra indirection, harder to reason about per-process state.
- **No access control:** Rejected — even a personal OS needs isolation between untrusted editors and trusted OS services.

**Why 256 fixed slots:** Simple, no allocation needed. A user process with >256 kernel objects would be doing something unusual. Can grow if needed — the handle API doesn't expose the limit.

---

## 0.9 Syscall Interface

**Goal:** Controlled entry from EL0 to EL1 for kernel services.

**Approach:** `SVC #0` from user code. Register ABI: `x8` = syscall number, `x0`–`x5` = arguments, `x0` = return value (≥0 success, <0 error). Twelve syscalls in three families: core (`exit`, `write`, `yield`), handle/IPC (`handle_close`, `channel_signal`, `channel_wait`), scheduling (`scheduling_context_create`, `scheduling_context_bind`, `scheduling_context_borrow`, `scheduling_context_return`), synchronization (`futex_wait`, `futex_wake`).

**Why SVC:** Standard EL0→EL1 trap instruction on ARM. HVC is EL1→EL2 (hypervisor), SMC is for the secure monitor.

**Why register ABI (not memory):** Registers are faster than memory reads, natural for the ARM calling convention, and require no validation of a memory-based argument block.

**Write syscall validation:** User buffer address checked (`< USER_VA_END`), length capped (4 KiB), and every page in the range verified readable via `AT S1E0R` hardware translation instruction. Prevents the kernel from reading unmapped memory on behalf of user code.

---

## 1.1 Sync Primitives

**Current:** `IrqMutex` masks IRQs to prevent reentry on a single core. No actual locking — works only because there's one core. `heap.rs` also masks IRQs directly rather than using `IrqMutex`.

**Target:** Ticket spinlock + IRQ masking. FIFO-fair, no starvation, standard for short kernel critical sections.

**Approach:**

- Replace `IrqMutex` internals: mask IRQs (save DAIF), acquire ticket lock (atomic fetch-add on `next_ticket`, spin on `now_serving`), release = increment `now_serving` + restore DAIF.
- Use `ldaxr`/`stlxr` (LL/SC) for ticket operations. More portable across aarch64 implementations than LSE atomics (`ldadd`). Start with LL/SC.
- Migrate `heap.rs` to use `IrqMutex` instead of raw DAIF manipulation.
- External API (`IrqMutex::lock()` -> `IrqGuard`) unchanged — callers don't need to change.

**Why ticket locks:**

- **Test-and-set:** cache-line bouncing (every core writes the same line).
- **Ticket:** separates "take a number" (write) from "check counter" (read), reducing contention. FIFO ordering prevents starvation.
- **MCS:** better under very high contention but adds per-lock queue nodes. Unjustified complexity for <= 8 cores.

**Depends on:** Nothing. Foundation for all SMP work.

---

## 1.2 Multi-Core Boot

**Current:** `boot.S` parks all cores except core 0 (`mpidr_el1` check). Only core 0 runs `kernel_main`.

**Target:** All cores boot, initialize own state, enter scheduler. Per-core kernel stack, exception vectors, timer, GIC CPU interface.

**Approach:**

- PSCI (Power State Coordination Interface) via `hvc` to bring up secondary cores. QEMU `virt` supports PSCI. Core 0 calls `CPU_ON` for each secondary with a boot address and context ID.
- Secondary core trampoline: enable MMU (shared TTBR1, empty TTBR0), set own kernel stack, configure `TPIDR_EL1`, init GIC CPU interface + timer, enter scheduler idle loop.
- Per-CPU data: `[PerCpu; MAX_CORES]` array (`MAX_CORES` = 8, QEMU `virt` max). Holds current thread pointer, core ID, idle thread, local scheduling state. Indexed by MPIDR affinity.
- `TPIDR_EL1` per-core points to that core's current thread context.

**Why PSCI over spin-table:** PSCI is the ARM-standard firmware interface. Works on QEMU, real hardware with UEFI/ATF, and most hypervisors. Spin-table is QEMU-specific.

**Depends on:** 1.1 (sync primitives must handle multi-core before secondary cores enter the scheduler).

---

## 1.3 SMP-Aware Scheduler

**Superseded by section 6.1 (EEVDF + scheduling contexts).** The original priority-based FIFO scheduler (three levels: Idle/Normal/High, round-robin within each) has been replaced. The global queue + per-core current-thread structure (stage A) was retained; per-core queues with work stealing (stage B) remain a future option if the global lock bottlenecks.

**Depends on:** 1.1, 1.2.

---

## 2.1 Slab Allocator

**Current:** One linked-list allocator for all kernel heap allocations. First-fit walk for every `Box<Thread>`, `Vec`, page table metadata alloc.

**Target:** Slab caches for fixed-size kernel objects. Linked-list allocator handles variable/rare allocations. Hot path through slabs.

**Approach:**

- Slab cache = pool of pre-allocated objects of one size. Alloc = pop free list O(1). Free = push free list O(1). No fragmentation within a cache.
- Each cache owns one or more 4 KiB pages ("slabs"), divided into N objects of the cache's size with embedded free list.
- Caches for: `Thread` (~700 bytes), `HandleTable` entries, channel structs.
- `GlobalAlloc` routes by size to slab cache, falls back to general allocator.

**Why slab:** Kernel allocates few object types very frequently. Slab exploits this — zero fragmentation, O(1), cache-friendly. Proven in Linux (SLUB), FreeBSD (UMA).

**Depends on:** None (benefits from 1.1 for SMP safety).

---

## 2.2 Buddy Allocator

**Current:** Free-list page frame allocator. One 4 KiB frame at a time. No contiguous multi-page allocation.

**Target:** Buddy system for contiguous 2^n page allocation. Single-page alloc stays O(1). Automatic coalescing.

**Approach:**

- Free lists per order: 0 = 4 KiB, 1 = 8 KiB, ..., max order 10 = 4 MiB.
- Alloc order-n: pop from order-n list, or split order-(n+1) block.
- Free order-n: check buddy (`buddy_pa = block_pa XOR (PAGE_SIZE << order)`). If buddy free, merge and recurse upward. Otherwise add to order-n list.
- Replace `page_allocator.rs`. New API: `alloc_frames(order) -> Option<usize>`, `free_frames(pa, order)`. Single page = `alloc_frames(0)`.

**Why buddy:** O(log n) contiguous allocation with automatic coalescing. Best tradeoff for single pages (address spaces) + multi-page (DMA, large maps). Bitmap allocators coalesce in O(n). XOR trick makes buddy ID trivial.

**Depends on:** None.

---

## 2.3 ASID Generation Recycling

**Current:** 8-bit ASIDs (1-255) with free stack for recycling. Panics if all 255 concurrent ASIDs in use. Functional but not robust.

**Target:** Generation-based. Exhaustion triggers: increment generation, flush TLB, start over. Lazy ASID re-acquire on context switch. No hard limit.

**Approach:**

- Global generation counter (`u64`).
- Each `AddressSpace` stores ASID + generation.
- Context switch: if thread's generation != global, allocate new ASID (may trigger rollover).
- Rollover: mark all ASIDs free, `tlbi vmalle1is`, increment generation.
- Standard approach — see Linux `arch/arm64/mm/context.c`.

**Depends on:** 1.1 (SMP synchronization for generation rollover).

---

## 3.1 Timer Resolution

**Current:** 10 Hz (100 ms tick).

**Target:** 250 Hz (4 ms tick).

**Approach:** Change timer reload in `timer.rs` from `freq / 10` to `freq / 250`.

**Depends on:** None.

---

## 3.2 Boot Identity Map Cleanup

**Current:** `boot.S` TTBR0 identity map tables linger after kernel transitions to upper VA. Wasted memory.

**Target:** Reclaim those pages into the frame allocator after `page_alloc::init()`.

**Approach:** Boot tables are at known symbols from `boot.S`. Free those frames after all cores have booted (secondary cores need the identity map during their trampoline).

**Depends on:** 1.2 (must wait for all secondary cores to finish using the identity map).

---

## 4.1 Demand Paging

**Current:** All pages eagerly allocated/mapped at process creation. Stacks fully allocated (4 pages = 16 KiB). ELF segments copied before process runs.

**Target:** Lazy allocation (map on fault). Foundation for memory-mapped I/O.

**Approach:**

- Extend `user_fault_handler`: distinguish invalid access (kill) from valid-but-unmapped (allocate + resume).
- Per-process VMA list: describes intended memory layout (code, data, stack, channels). Replaces pre-mapping.
- Data abort from EL0: read `FAR_EL1`, look up VMA, alloc frame, zero/copy, map into address space, return to user.
- Stack VMA extends downward from `USER_STACK_TOP`. Guard page = gap in VMA list (no VMA covers it -> kill).

**Why:** Foundation for memory efficiency, memory-mapped files, shared memory. Makes process creation faster even without a filesystem.

**Depends on:** Benefits from 2.2 (buddy) but not required.

---

## 5.1 Virtio Framework

**Current:** Hardcoded GIC + PL011 UART. No device discovery, no DMA.

**Target:** Minimal virtio-mmio driver framework. virtio-console + virtio-blk.

**Approach:**

- **Discovery:** QEMU `virt` places virtio-mmio at `0x0a000000 + 0x200 * n` (n = 0..31). Probe magic number register. (A more general kernel would use a device tree.)
- **Transport:** virtio-mmio (spec section 4.2). Virtqueue setup (descriptor table, available ring, used ring), feature negotiation, interrupt handling. One module reused by all device drivers.
- **virtio-console:** replaces raw PL011, bidirectional, interrupt-driven RX.
- **virtio-blk:** read/write sectors. Foundation for future filesystem.
- **DMA:** physically contiguous buffers (buddy allocator), pass PAs to device.

**Why virtio:** Standard paravirtualized device interface. QEMU, KVM, Firecracker. Public spec. Same drivers work under real hypervisors.

**Depends on:** 2.2 (buddy allocator for contiguous DMA buffers).

---

## 6.1 Scheduling Algorithm: EEVDF + Scheduling Contexts

**Current:** Priority-based FIFO with three levels (High, Normal, Idle). Global run queue behind a single lock. No fairness guarantees — High threads starve Normal indefinitely. Functional for two demo processes but not for a real OS.

**Target:** Two-layer scheduling. Scheduling contexts provide per-workload temporal isolation and server billing. EEVDF provides proportional-fair selection with latency differentiation among eligible threads.

**Layer 1 — Scheduling Contexts (budget management):**

A scheduling context is a kernel object: `{ budget, period, remaining, replenish_at }`. Accessed via handles in the per-process handle table — same mechanism as IPC channels. A thread can only run when its scheduling context has remaining budget. An editor's threads share one context (the document's budget pool). The kernel charges elapsed time to the running thread's context on every timer tick and on deschedule.

Budget replenishment: periodic (every `period` ns, reset `remaining = budget`). Simpler than sporadic server initially; can upgrade later.

Context donation: OS service explicitly borrows an editor's scheduling context via syscall when processing that editor's IPC messages. Returns to its own context when done. Bills the rendering/compositing work to the editor that requested it. Prevents noisy-neighbor — a greedy editor exhausts only its own budget.

The OS service self-budgets from the total system allocation. It creates scheduling contexts for each editor process based on document mimetype and state (content-type-aware policy).

Best-effort admission: all contexts admitted. Under overload, EEVDF handles fairness. No hard rejection of new documents.

**Layer 2 — EEVDF (thread selection):**

Among threads whose scheduling context has remaining budget:

1. **Virtual runtime (vruntime):** Tracks accumulated CPU time, weighted by 1/weight. Higher weight = vruntime grows slower = more CPU share.
2. **Eligibility:** A thread is eligible if its lag ≥ 0 (lag = entitled time − actual time). Threads that overconsumed are ineligible until the math catches up.
3. **Virtual deadline:** For each eligible thread, deadline = became-eligible time + requested time slice. Thread with earliest deadline runs.
4. **Latency differentiation:** Threads requesting shorter time slices get earlier deadlines — interactive work runs sooner without consuming more than its fair share. No heuristics.

Data structure: replace the three `VecDeque`s (high/normal priority) with a single sorted structure (BinaryHeap or augmented tree) keyed by virtual deadline, filtered by eligibility and budget.

**New kernel objects and syscalls:**

- `SchedulingContext` kernel object: budget, period, remaining, replenish_at
- `SchedulingContextSlot` wrapper with reference counting (freed when last handle closed)
- `HandleObject::SchedulingContext(SchedulingContextId)` variant in handle table
- Thread holds `SchedulingInfo { eevdf, context_id, saved_context_id, last_started }`
- Syscalls: `scheduling_context_create(budget, period)`, `scheduling_context_bind(handle)`, `scheduling_context_borrow(handle)`, `scheduling_context_return()`
- Create and bind are separate operations — OS service creates contexts for editors, editors bind themselves
- Timer interrupt: charge runtime, check budget exhaustion, trigger replenishment
- Free list for scheduling context ID reuse

**Content-type-aware policy (OS service, not kernel):**

| Content type / state        | Example budget | Example period | Rationale                     |
| --------------------------- | -------------- | -------------- | ----------------------------- |
| `audio/*` (playing)         | 1–2 ms         | 5 ms           | Low-latency buffer fills      |
| `video/*` (playing)         | 4 ms           | 16 ms          | 60fps frame delivery          |
| `text/*` (actively editing) | 2 ms           | 16 ms          | Responsive keystrokes         |
| `image/*` (editing)         | 6 ms           | 33 ms          | Heavier compute, 30fps OK     |
| `video/*` (paused)          | 2 ms           | 16 ms          | Just showing a still, demoted |
| Background indexing         | 2 ms           | 100 ms         | Trickle, stay out of the way  |

The OS service adjusts contexts dynamically as document state changes. The kernel is policy-agnostic — it enforces budgets without understanding why they were chosen.

**Alternatives considered:**

- **CFS:** No latency differentiation without heuristics. EEVDF subsumes it.
- **Stride scheduling:** No latency differentiation. Clean but EEVDF is strictly better.
- **EEVDF alone:** No temporal isolation, no server billing. Noisy-neighbor problem.
- **Contexts alone (priority selection):** No fairness within priority levels.
- **SCHED_DEADLINE (EDF+CBS):** No proportional fairness for non-deadline tasks.
- **Lottery scheduling:** High short-term variance. Deterministic preferred.

**Prior art:** Linux EEVDF (kernel 6.6+, Stoica & Abdel-Wahab 1995 paper), seL4 MCS scheduling contexts (Lyons et al.), QNX adaptive partitioning (partition inheritance = context donation), Apple Clutch (thread groups = content-type grouping).

**Depends on:** 1.1 (sync primitives), 1.3 (SMP scheduler infrastructure), 0.8 (handle table for scheduling context handles).

---

## 7.1 Unsafe Minimization Discipline

**Goal:** Formalize an invariant that all `unsafe` code is concentrated in low-level primitives, with safe interfaces exported to higher layers. Inspired by Asterinas's framekernel architecture (safe Rust services atop an unsafe framework layer) and Theseus's compiler-as-isolation approach.

**Invariant:** `unsafe` blocks are permitted only for operations that cannot be expressed in safe Rust:

1. **Inline assembly** — CPU register access, system instructions (`msr`, `mrs`, `dsb`, `tlbi`, `wfe`, `hvc`, `svc`, `dc`), memory barriers.
2. **Volatile MMIO** — `read_volatile`/`write_volatile` for memory-mapped device registers and page table entries.
3. **Linker symbol access** — Reading addresses of symbols defined in `boot.S` or the linker script (`__text_start`, `__kernel_end`, etc.).
4. **Raw pointer arithmetic in page table walks** — Navigating hardware-defined 4-level page table structures where pointer offsets are derived from VA bit extraction.
5. **`GlobalAlloc` trait implementation** — Required by the Rust allocator interface.
6. **`Send`/`Sync` trait implementations** — Where the safety argument is documented and rests on specific synchronization guarantees (e.g., `IrqMutex` provides exclusive access, write-once statics are immutable after init).
7. **Stack/context allocation** — `alloc_zeroed`, `dealloc`, `core::mem::zeroed()` for thread contexts and stacks where layout is known.

**Prohibited patterns:**

- No `static mut`. Use `AtomicU64`, `IrqMutex`, or write-once `UnsafeCell` with documented safety argument.
- No `unsafe` in the OS service layer (EL0 trusted process). If the OS service needs `unsafe`, that's a design smell — the kernel API is missing an abstraction.
- No `transmute` except for zero-cost conversions between repr-compatible types (none currently needed).
- No raw pointer dereference outside the categories above. If you're dereferencing a raw pointer in business logic, extract a safe abstraction.

**Audit baseline (2026-03-08):** ~99 `unsafe` blocks across kernel + userspace. All justified. Zero `static mut`. Categories: assembly/hardware (33), raw pointer walks (30+), system register reads (15), unsafe trait impls (4), static/global state (6), memory/stack ops (8), syscall wrappers (3). No vulnerabilities found.

**Enforcement:** Code review discipline (no tooling enforcement yet). Every new `unsafe` block must include a `// SAFETY:` comment explaining which category it falls under and why the invariants hold. Existing blocks should be annotated incrementally.

**Rationale:** The kernel already follows this discipline emergently. Formalizing it prevents drift as the codebase grows. The key insight from Asterinas: if you can draw a clear line between "unsafe foundation" and "safe services," the trusted computing base for memory safety is just the foundation — everything above it gets compiler-verified safety for free. Our line: kernel primitives (categories 1-7 above) are the foundation; everything else — scheduler logic, handle table operations, process management, IPC channel logic — is safe Rust built on those primitives.

---

## 8.1 Kernel Identity: Microkernel by Convergence

**Settled 2026-03-08.** The kernel is a microkernel. Not by ideology — each sub-decision independently pushed complexity outward (drivers, filesystem, rendering, editors all in userspace). What remains is the microkernel set: address spaces, threads, IPC (channels), scheduling (EEVDF + scheduling contexts), interrupt forwarding, and handle-based access control.

**The kernel's job:** Multiplex hardware resources behind handles. Provide a single event-driven wait mechanism (`wait_any`). The kernel doesn't understand what any resource is _for_ — it just manages access.

**Handle types (current and planned):**

| Handle type       | Status      | Resource                              |
| ----------------- | ----------- | ------------------------------------- |
| Channel           | Implemented | Shared-memory IPC ring buffer         |
| SchedulingContext | Implemented | Budget/period time allocation         |
| Timer             | Implemented | One-shot deadline notification        |
| Device (planned)  | —           | MMIO mapping + interrupt notification |

**Guiding rule:** Every new kernel feature should be expressible as "a new handle type that can be waited on."

---

## 8.2 Event Multiplexing: `wait`

**Goal:** A single syscall for blocking on multiple event sources. The foundational primitive for event-driven userspace — without it, processes need one thread per event source.

**Syscall:** `wait(handles_ptr, count, timeout_ns) → index`. Syscall #12. Block until any handle has a pending event or timeout expires. Returns the 0-based index of the first ready handle in x0. Timeout of `0` = poll (non-blocking check, returns `WouldBlock` if none ready). Timeout of `u64::MAX` = wait forever. Supports Channel and Timer handles; Device handles will be added when implemented. Timeout support beyond poll/forever deferred (userspace can create a timer handle for custom timeouts).

**Replaced:** `channel_wait` (syscall #5, removed). `wait` with a single channel handle is equivalent but uses the general mechanism.

**Unifies:** Channel notifications, timer expiry, interrupt delivery (planned) — all through one mechanism.

**Lost-wakeup prevention:** The wait set is stored on the thread _before_ checking handle readiness. If a signal arrives during the readiness scan, `channel::signal` calls `set_wake_pending_for_handle` which finds the wait set and sets `wake_pending` + `wake_result` on the thread. `block_current_unless_woken` checks the flag and returns immediately with the correct index. This is the same pattern used by futex — the `wake_pending` flag is now shared infrastructure for all blocking paths.

**Lock ordering:** Channel lock → scheduler lock (unchanged). The readiness check acquires the channel lock (one channel at a time). Blocking acquires the scheduler lock. The wait set stored on the thread (under scheduler lock) bridges the two: signalers under the scheduler lock can read the wait set to compute the return index without needing the channel lock.

**Implementation:** `syscall.rs` (sys_wait), `scheduler.rs` (try_wake_for_handle, set_wake_pending_for_handle, store_wait_set, clear_wait_state, updated block_current_unless_woken), `channel.rs` (signal uses try_wake_for_handle), `thread.rs` (WaitEntry, complete_wait_for, wake_pending/wake_result fields).

**Prior art:** Linux epoll, FreeBSD kqueue, Fuchsia `zx_object_wait_many`, seL4 notification objects.

---

## 8.3 Device Handles & Interrupt Forwarding (planned)

**Goal:** Support userspace drivers. The kernel provides hardware access through handles; drivers run at EL0.

**Planned approach:**

- **Device discovery:** Parse DTB (device tree blob) at boot to enumerate devices. QEMU `virt` passes DTB in `x0` at entry (currently ignored by `boot.S`).
- **MMIO mapping:** Syscall to map a device's MMIO region into the calling process's address space. Returns a device handle. The driver reads/writes registers as normal memory operations — zero overhead.
- **Interrupt forwarding:** Driver registers for a device's interrupt via syscall. The kernel's IRQ handler masks the interrupt and signals the driver's handle. Driver calls `interrupt_ack` when done (unmasks). One context switch per interrupt.
- **DMA buffers:** Syscall to allocate physically contiguous pages (buddy allocator) and map into driver's address space. Returns the physical address for programming device descriptors.

**New syscalls (planned):**

| Syscall            | Args                    | Returns    |
| ------------------ | ----------------------- | ---------- |
| device_map         | device_id, offset, size | handle     |
| interrupt_register | device_id, irq_nr       | handle     |
| interrupt_ack      | handle                  | 0          |
| dma_alloc          | order                   | handle, PA |

**Depends on:** DTB parser (device discovery), `wait` (interrupt notification).

---

## 8.4 Timers

**Goal:** Userspace-visible timer kernel objects. Processes need timeouts for I/O, animation, network protocols.

**Syscall:** `timer_create(timeout_ns) → handle`. Syscall #13. Creates a one-shot timer that fires after `timeout_ns` nanoseconds. The returned handle becomes permanently "ready" when the deadline passes (level-triggered — unlike channels which consume the signal). Waited on via `wait` alongside channels. Fits "handles all the way down" — timers are kernel objects with the same lifecycle as channels and scheduling contexts.

**Deadline representation:** Internally stored as absolute counter ticks (not nanoseconds) to avoid repeated ns↔ticks conversion on every 250 Hz tick check. Computed at creation: `deadline = counter() + timeout_ns * freq / 1e9`.

**Firing mechanism:** The hardware timer IRQ handler calls `check_expired()` on every tick. Two-phase design: scan all 32 timer slots under the timer lock, collect fired (timer_id, waiter_thread_id) pairs, release lock, then wake each waiter via `try_wake_for_handle` / `set_wake_pending_for_handle`. Maintains lock ordering: timer → scheduler.

**Waiter tracking:** Each timer stores `waiter: Option<ThreadId>`. Set by `sys_wait` before checking readiness (same store-before-check pattern as the wait set). Cleared by the fire path (`.take()`) or by explicit unregister when `wait` returns for any other reason. Prevents stale registrations.

**Level-triggered:** Once fired, `check_fired()` returns true on every call until the timer is destroyed. One-shot timers don't need signal consumption — the user should close the handle after use.

**Implementation:** `timer.rs` (timer objects + hardware timer), `syscall.rs` (sys_timer_create, sys_wait timer integration), `handle.rs` (Timer variant). 32-slot table under `IrqMutex`. Process exit cleanup via handle drain.

**Depends on:** `wait` (event multiplexing).

---

## 8.5 Futex

**Goal:** Fast userspace synchronization primitive. Building block for mutexes, condition variables, semaphores — all without creating kernel objects per lock.

**Approach:** Two syscalls (`futex_wait` #10, `futex_wake` #11):

- `futex_wait(addr, expected)` — Translate user VA → PA via `AT S1E0R`, check `*addr == expected`, record thread in hash table, block via `block_current_unless_woken`. Returns `WouldBlock (-8)` if value changed.
- `futex_wake(addr, count)` — Translate VA → PA, collect matching waiters (under futex lock), wake them (under scheduler lock). Two-phase design maintains lock ordering (futex → scheduler, same direction as channel → scheduler).

**Fast path:** Userspace does an atomic CAS to acquire a lock. No syscall. Slow path (contention): `futex_wait` to sleep, `futex_wake` to wake. Kernel involvement only on contention.

**Implementation:** 64-bucket hash table keyed by physical address (word-aligned, `(pa >> 2) % 64`). Physical address keying ensures processes sharing the same physical page (IPC shared memory) see the same futex.

**Lost-wakeup prevention:** Race window between recording waiter in futex table and actually blocking in the scheduler. Solution: `set_wake_pending(tid)` flag on Thread. If a waker runs during this window, `try_wake` fails (thread still Running), so waker sets the pending flag. When the waiter enters `block_current_unless_woken`, it checks the flag and returns immediately instead of blocking.

**Cleanup:** `remove_thread(id)` purges a thread from all futex wait queues. Called during `exit_current_from_syscall` (Phase 2b, acquires futex lock, not scheduler lock — safe ordering).

**Prior art:** Linux futex(2) — the standard answer. Every userspace mutex library (pthreads, parking_lot, std::sync::Mutex) is built on futex.

---

## 8.6 DTB Parser

**Goal:** Replace hardcoded device addresses with device tree discovery. Makes the kernel portable across hardware configurations.

**Approach:** Parse the Flattened Device Tree (FDT) blob passed by firmware. `boot.S` saves `x0` (DTB PA from firmware) in `x24` and passes it to `kernel_main`. Parsing happens between `heap::init()` (needs `Vec`) and `page_allocator::init()` (may reclaim DTB memory).

**Discovery strategy:** Try firmware-provided PA first; if that fails (QEMU on macOS doesn't set x0 for bare-metal ELF), scan the first 512KB of RAM (pre-kernel area) for the FDT magic. Graceful fallback to "not found" if DTB isn't available.

**Parser:** Pure function on `&[u8]`, ~260 lines. Extracts compatible strings, reg entries (#address-cells=2, #size-cells=2), and GIC interrupts (SPI offset +32, PPI offset +16). Returns a `DeviceTable` with `find_first()` and `find_all()` lookups. 14 host tests verify parsing.

**L3 mapping fix:** `memory::init()` replaces the kernel's 2MB block with L3 4KB pages. Originally, pre-kernel pages (0x40000000-0x40080000) had empty L3 entries. Fixed to map the entire 2MB block — pre-kernel pages are RW NX (data), kernel sections get W^X refinement.

**Currently hardcoded (DTB not yet wired to device init):**

| Device | Address      | Source    |
| ------ | ------------ | --------- |
| GIC    | 0x0800_0000  | QEMU virt |
| UART   | 0x0900_0000  | QEMU virt |
| virtio | 0x0A00_0000+ | QEMU virt |

**Next:** Wire DTB-discovered addresses into device initialization (GIC, UART, virtio). Requires driver model to be settled.
