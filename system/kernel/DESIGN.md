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

| Handle type       | Status      | Resource                           |
| ----------------- | ----------- | ---------------------------------- |
| Channel           | Implemented | Shared-memory IPC ring buffer      |
| Interrupt         | Implemented | IRQ forwarding to userspace driver |
| SchedulingContext | Implemented | Budget/period time allocation      |
| Timer             | Implemented | One-shot deadline notification     |

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

## 8.3 Device Handles & Interrupt Forwarding

**Goal:** Support userspace drivers. The kernel provides hardware access through handles; drivers run at EL0.

**Approach:**

- **Device discovery:** Parse DTB (device tree blob) at boot to enumerate devices. **Done** (§8.6). DTB parser discovers ~40 devices; GIC and virtio-mmio addresses initialized from DTB data.
- **MMIO mapping:** `device_map(pa, size)` syscall (#16) maps a device's MMIO region into the calling process's address space with Device-nGnRE memory attributes (MAIR index 1). Returns the user VA. VA is bump-allocated from a dedicated region (`DEVICE_MMIO_BASE` at 512 MiB, up to `DEVICE_MMIO_END` at 1 GiB). Validates that the PA is outside RAM (device space only). Zero overhead for register access — the driver reads/writes device memory directly.
- **Interrupt forwarding:** `interrupt_register(irq)` syscall (#14) enables the IRQ in the GIC and returns a waitable handle (`HandleObject::Interrupt`). When the IRQ fires, the kernel's IRQ handler masks it at the GIC distributor, marks the handle pending, and wakes the driver thread. `interrupt_ack(handle)` syscall (#15) clears pending and re-enables the IRQ. One context switch per interrupt.
- **DMA buffers:** `dma_alloc(order, pa_out_ptr)` syscall (#17) allocates 2^order contiguous pages from the buddy allocator, maps them into the caller's DMA VA region (`DMA_BUFFER_BASE` at 256 MiB, up to `DMA_BUFFER_END` at 512 MiB), writes the PA to a user-provided pointer, and returns the user VA. `dma_free(va, order)` syscall (#18) unmaps and frees. Per-process DMA allocation tracking; all DMA buffers freed on process exit. Order 0–4 (4 KiB – 64 KiB).

**Edge-triggered semantics:** Interrupt handles differ from timer handles (which are level-triggered/permanent). Each `pending` flag is set on IRQ fire and consumed by `interrupt_ack`. Missing an ack just means pending stays true on the next `wait` check.

**IRQ handler flow:** Acknowledge → identify IRQ → if timer: `timer::handle_irq()`, else: `interrupt::handle_irq(id)` → reschedule → EOI. The reschedule runs after every IRQ (not just timer) so woken driver threads get scheduled promptly.

**Two-phase wake:** Same pattern as timers. Collect (InterruptId, ThreadId) pairs under the interrupt table lock, then wake via `try_wake_for_handle` / `set_wake_pending_for_handle` after releasing it. Lock ordering: interrupt → scheduler.

**Lost-wakeup prevention:** Same as `wait` syscall. `sys_wait` calls `register_waiter` on each interrupt handle BEFORE storing the wait set and checking readiness. If the IRQ fires in the gap, `set_wake_pending_for_handle` catches it.

**New syscalls:**

| Nr  | Syscall            | Args           | Returns |
| --- | ------------------ | -------------- | ------- |
| 14  | interrupt_register | x0=irq_nr      | handle  |
| 15  | interrupt_ack      | x0=handle      | 0       |
| 16  | device_map         | x0=pa, x1=size | user VA |

**Implementation:** `interrupt.rs` (registration table, 32 slots under `IrqMutex`), `handle.rs` (`Interrupt(InterruptId)` variant), `interrupt_controller.rs` (`disable_irq` via ICENABLER), `address_space.rs` (`map_device_mmio` + `PageAttrs::user_device_rw`), `paging.rs` (`ATTRIDX1`, `DEVICE_MMIO_BASE/END`), `syscall.rs` (3 new syscalls + `wait` integration), `main.rs` (IRQ dispatch). Process exit cleanup via handle drain. `sys` wrappers: `interrupt_register`, `interrupt_ack`, `device_map`.

**Depends on:** DTB parser (§8.6, **done**), `wait` (§8.2, **done**), GIC (§0.3, **done**).

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

**Update (2026-03-09):** DTB now wired into device init. GIC bases from `"arm,cortex-a15-gic"`, virtio-mmio from `"virtio,mmio"` entries. Falls back to hardcoded QEMU `virt` defaults if no DTB.

---

## 9.0 Kernel Completion Roadmap

Everything in sections 0.x–8.x is implemented. This section captures what remains to make the kernel fully usable by the OS service and userspace drivers.

**Dependency graph:**

```text
Phase 1 (DMA) ──────────────────────────┐
                                        ├─→ Phase 5 (Virtio Migration)
Phase 2a (Process struct) ──┬─→ Phase 2b (Thread create)
                            ├─→ Phase 3 (Process create) ─┤
                            ├─→ Phase 6 (Process kill)    │
                            └─→ Phase 7 (Memory sharing)  │
                                                          │
Phase 4 (Handle transfer) ←───────────────────────────────┘

Phase 7 (Memory sharing) ← partially blocked on filesystem design
Phase 8 (COW mechanics) ← blocked on filesystem design
```

**Execution order:** 2a → 1 (parallel OK) → 2b → 3 → 4 → 6 → 5 → 7 → 8.

---

### 9.1 DMA Buffer Allocation (Phase 1) — Done

**Goal:** Expose physically contiguous allocation to userspace. Virtio drivers need DMA-capable buffers for descriptor rings and data.

**Syscalls:**

| Nr  | Syscall   | Args                    | Returns |
| --- | --------- | ----------------------- | ------- |
| 17  | dma_alloc | x0=order, x1=pa_out_ptr | user VA |
| 18  | dma_free  | x0=user_va, x1=order    | 0       |

`dma_alloc` allocates 2^order contiguous pages (order 0–4, 4 KiB – 64 KiB), maps into the caller's address space with normal memory attributes, writes the PA to the user-provided pointer (for programming device DMA registers). Returns the user VA. `dma_free` unmaps and frees.

**VA region:** `DMA_BUFFER_BASE` (256 MiB) to `DMA_BUFFER_END` (512 MiB). Bump-allocated per process.

**Implementation:** `address_space.rs` (`DmaAllocation`, `next_dma_va`, `map_dma_buffer`, `unmap_dma_buffer`, `unmap_page_inner`), `paging.rs` (`DMA_BUFFER_BASE`, `DMA_BUFFER_END`), `syscall.rs` (`sys_dma_alloc`, `sys_dma_free`, `is_user_page_writable`, `OutOfMemory` error), `sys` (`dma_alloc`, `dma_free`). `free_all()` drains DMA allocations on process exit.

**Depends on:** Nothing. Buddy allocator already supports `alloc_frames(order)`.

---

### 9.2 Process Struct Extraction (Phase 2a) — Done

**Goal:** Introduce a `Process` kernel object that owns the address space and handle table. Foundation for multi-threaded processes, process creation from userspace, and process handles.

**Current state:** `Thread` owns `Option<Box<AddressSpace>>` and `HandleTable` directly. Single-threaded processes only. Process identity is implicit (a thread IS a process).

**Target:** `Process { id, address_space, handles, threads }`. Threads hold `process_id: Option<ProcessId>`. Syscall handlers resolve process via thread's `process_id`. Global process table (fixed-size array under `IrqMutex`, same pattern as interrupt/timer tables).

**Key design choices:**

- Handle table is per-process (shared across threads). Matches the OS design where handles represent process-level resources.
- Address space is per-process (shared TTBR0 across threads).
- Kernel threads have `process_id: None` (no process association).
- Process cleanup: last thread exit triggers full cleanup (handles, address space, ASID).

**Refactoring scope:** `thread.rs` (remove address_space and handles), `process.rs` (new Process struct + table), `scheduler.rs` (process-level cleanup), `syscall.rs` (resolve process for handle/address_space access), `channel.rs` (process-level channels), `main.rs` (boot sequence).

**Depends on:** Nothing. All existing behavior preserved.

---

### 9.3 Thread Creation (Phase 2b) — Done

**Goal:** Allow processes to create additional threads.

**Syscall:**

| Nr  | Syscall       | Args                      | Returns |
| --- | ------------- | ------------------------- | ------- |
| 19  | thread_create | x0=entry_va, x1=stack_top | handle  |

Creates a new thread in the calling process. Shares address space and handle table. Returns a `HandleObject::Thread(ThreadId)` handle — waitable, becomes ready on thread exit. New thread starts unbound (no scheduling context); caller can bind one via existing `scheduling_context_bind`.

**Process exit semantics:** Process is alive while any thread is alive. Last thread exit triggers full process cleanup. Non-last thread exit just marks the thread exited (kernel stack reclaimed on reap). Last thread exit does full cleanup: drain handles, close channels/timers/interrupts/thread handles, release scheduling contexts, free address space.

**Implementation:** `thread_exit.rs` — dedicated module for thread exit notification (own `IrqMutex<State>`, same two-phase wake pattern as timer/interrupt). `HandleObject::Thread(ThreadId)` variant. `Process.thread_count` incremented in `spawn_user`, decremented on exit. `exit_current_from_syscall` restructured with `ExitInfo` enum (Last vs NonLast).

**Depends on:** Phase 2a (Process struct).

---

### 9.4 Process Creation from Userspace (Phase 3) — Done

**Goal:** Allow the OS service to spawn new processes.

**Syscalls:**

| Nr  | Syscall        | Args                   | Returns |
| --- | -------------- | ---------------------- | ------- |
| 20  | process_create | x0=elf_ptr, x1=elf_len | handle  |
| 21  | process_start  | x0=handle              | 0       |

`process_create` parses the ELF from the caller's memory, creates a new process with address space + handle table + one suspended thread. Returns `HandleObject::Process(ProcessId)`. Waitable — becomes ready when all threads exit. `process_start` moves the initial thread to Ready.

**Two-phase create/start:** Gives the parent time to set up the child (transfer handles, bind scheduling context) before it runs.

**Implementation:** `process.rs` (`create_from_user_elf` — eagerly maps ALL segment pages with `Backing::Anonymous` since user ELF data is temporary). `process_exit.rs` — dedicated module for process exit notification (own `IrqMutex<State>`, same two-phase wake pattern as thread_exit/timer/interrupt). `HandleObject::Process(ProcessId)` variant. Scheduler gains `suspended: Vec<Box<Thread>>` to hold threads until `start_suspended_threads(pid)` moves them to the ready queue. Exit path extended: last-thread-exit calls `process_exit::notify_exit` and drains Process handles from the exiting process's handle table.

**Key design choice — eager mapping:** User-provided ELF bytes live in the caller's address space and can't be referenced later (unlike `&'static [u8]` for embedded ELFs). Solution: copy ELF data to a kernel buffer, eagerly map all pages, use `Backing::Anonymous` on VMAs, drop the buffer. Max ELF size: 1 MiB.

**Depends on:** Phase 2a (Process struct).

---

### 9.5 Handle Transfer (Phase 4) — DONE

**Goal:** Allow a parent to give handles to a child process before starting it.

**Syscalls:**

| Nr  | Syscall        | Args                                     | Returns                     |
| --- | -------------- | ---------------------------------------- | --------------------------- |
| 5   | channel_create | —                                        | handle_a \| (handle_b << 8) |
| 22  | handle_send    | x0=target_proc_handle, x1=handle_to_send | 0                           |

`handle_send` copies a handle from the caller's table into the target process's table. Only works on suspended processes (Process.started == false). Caller retains its copy. For Channel handles, also maps the shared page into the target's address space.

`channel_create` allocates a new channel (shared page + two endpoints), maps the shared page into the caller, and inserts both endpoint handles. Returns packed `handle_a | (handle_b << 8)`.

**Channel refactoring (complete):** Rewrote `channel.rs` from ThreadId-based to encoded ChannelId-based design. `ChannelId(channel_index * 2 + endpoint_index)` — endpoint identity embedded in the ID. Explicit `register_waiter`/`unregister_waiter` pattern (same as timer/interrupt/thread_exit/process_exit). Two-phase wake: channel lock → release → scheduler lock. `signal()` computes peer ChannelId from encoding, passes as wake reason.

**Depends on:** Phase 3 (process handles exist).

---

### 9.6 Process Kill (Phase 6) — DONE

**Goal:** Allow the OS service to terminate misbehaving processes.

**Syscall:**

| Nr  | Syscall      | Args      | Returns |
| --- | ------------ | --------- | ------- |
| 23  | process_kill | x0=handle | 0       |

Terminates all threads in the target process. Runs full cleanup. Process handle becomes ready (waitable notification). Self-kill prevented (returns InvalidArgument).

**Implementation:** Multi-phase cleanup, same pattern as `exit_current_from_syscall`:

1. **Phase 1 (scheduler lock):** Remove target threads from ready/blocked/suspended lists. Mark threads running on other cores as Exited. Drain handle table and categorize resources. If no threads are running on other cores, take the process for immediate address space cleanup. If threads are still running, set `process.killed = true` and `thread_count = running_count` for deferred cleanup.
2. **Phase 2 (outside lock):** Notify `thread_exit` and `process_exit` for all killed threads. Remove threads from futex wait queues.
3. **Phase 3 (outside lock):** Close channels, destroy interrupts/timers/thread handles/process handles.
4. **Phase 4 (outside lock):** Free address space (TLB invalidation + page deallocation + ASID release). Immediate if no running threads; deferred via `maybe_cleanup_killed_process` in `schedule_inner` otherwise.

**Deferred cleanup:** When `schedule_inner` parks an exited thread from a killed process, it decrements `thread_count`. When it reaches zero, the address space is freed inline (rare path, acceptable under scheduler lock since the process is typically small).

**Depends on:** Phase 2a (Process struct).

---

### 9.7 Userspace Virtio Migration (Phase 5) — DONE

**Goal:** Move virtio-blk and virtio-console from in-kernel to userspace drivers. Validates the entire microkernel driver model.

**Approach:** Each driver becomes a separate ELF binary (now in `system/platform/drivers/`). At boot, kernel probes virtio-mmio slots (minimal MMIO reads for magic/version/device_id), spawns the appropriate driver process, writes device info (MMIO PA, IRQ) to a channel shared page, and starts the driver. Each driver: `device_map` for MMIO, `dma_alloc` for virtqueue buffers. In-kernel `virtio/` module removed entirely. Shared `virtio` rlib (in `system/library/`) provides userspace virtio transport and split virtqueue implementation.

**Implementation notes:**

- Kernel retains minimal probe logic inline in `main.rs` (~80 lines) for device discovery. Drivers handle all device initialization (negotiate, queue setup, I/O).
- Sub-page MMIO alignment: QEMU virt's virtio-mmio slots have 0x200 stride within 4K pages. Drivers page-align the PA for `device_map` and add the sub-page offset to the returned VA.
- Channel shared page mapped at fixed `CHANNEL_SHM_BASE` in each driver's address space (bypasses channel-index-derived VA) so drivers read device info from a known address.
- Console driver not exercised yet (QEMU virt doesn't add a virtio-console by default; only blk device present).
- Drivers use polling (spin-loop) for completion, matching the previous in-kernel behavior. Interrupt-driven I/O is a straightforward enhancement via `interrupt_register` + `wait` + `interrupt_ack`.

**Validation:** `cargo run --release` boots, virtio-blk driver reads sector 0 and prints "HELLO VIRTIO BLK" — same functionality as the former in-kernel driver, entirely through syscalls (`device_map`, `dma_alloc`, `dma_free`, `write`, `channel_signal`, `exit`). Init/echo IPC unaffected.

**Depends on:** Phases 1 (DMA), 3 (process create), 4 (handle transfer).

---

### 9.8 Memory Sharing (Phase 7)

**Goal:** Allow the OS service to map shared memory into editor processes. Foundation for the document memory model.

**Partially blocked on filesystem design.** The kernel primitive is straightforward: "map a physical page into another process's address space." Policy (which pages, COW semantics) lives in the OS service. Can implement the primitive before the filesystem design settles.

**Depends on:** Phase 2a. Full design depends on filesystem COW decisions.

---

### 9.9 Filesystem COW Kernel Mechanics (Phase 8)

**Goal:** Kernel-level copy-on-write for memory-mapped documents. Editor writes trigger page faults, kernel allocates new pages, filesystem manages on-disk snapshots.

**Blocked on filesystem on-disk design.** Research complete (`design/research-cow-filesystems.md`). Requires settling the last sub-decision of Decision #16.

---

### Syscall Number Map (complete)

| Nr  | Syscall                   | Status      |
| --- | ------------------------- | ----------- |
| 0   | exit                      | Implemented |
| 1   | write                     | Implemented |
| 2   | yield                     | Implemented |
| 3   | handle_close              | Implemented |
| 4   | channel_signal            | Implemented |
| 5   | channel_create            | Implemented |
| 6   | scheduling_context_create | Implemented |
| 7   | scheduling_context_borrow | Implemented |
| 8   | scheduling_context_return | Implemented |
| 9   | scheduling_context_bind   | Implemented |
| 10  | futex_wait                | Implemented |
| 11  | futex_wake                | Implemented |
| 12  | wait                      | Implemented |
| 13  | timer_create              | Implemented |
| 14  | interrupt_register        | Implemented |
| 15  | interrupt_ack             | Implemented |
| 16  | device_map                | Implemented |
| 17  | dma_alloc                 | Implemented |
| 18  | dma_free                  | Implemented |
| 19  | thread_create             | Implemented |
| 20  | process_create            | Implemented |
| 21  | process_start             | Implemented |
| 22  | handle_send               | Implemented |
| 23  | process_kill              | Implemented |

---

## 10.0 Hardening & Code Quality

Expert review of the post-roadmap codebase (Phases 1–6 + virtio migration) identified 11 issues: 4 correctness bugs, 4 code quality improvements, and 3 missing infrastructure items. This section documents each issue, the fix plan, and execution order.

**Dependency graph:**

```text
10.1 (handle_send) ─────────────────┐
10.2 (sched ctx refcount) ──────────┼─→ 10.3 (interrupt-driven virtio)
10.4 (channel leak) ────────────────┘         │
                                              ↓
10.5 (WaitableRegistry) ──→ 10.6 (O(1) lookup) ──→ 10.7 (dispatch macro)
                                                          │
10.8 (metrics) ─────────────────────────────────────┐     │
10.9 (watchdog budgets) ────────────────────────────┼─→ all done
10.10 (kernel stack guards) ────────────────────────┘
```

**Execution order:** 10.1 → 10.2 → 10.4 → 10.3 → 10.5 → 10.6 → 10.7 → 10.8 → 10.9 → 10.10. First four are correctness fixes (do first). Next three are code quality (reduce surface area before adding new code). Last three are infrastructure.

---

### 10.1 Fix handle_send: Move Semantics — DONE

**Bug:** `sys_handle_send` copies the source handle into the target process but does not remove it from the caller. For channel handles, this duplicates an endpoint — two processes hold handles to the same endpoint. Channel's `closed_count` expects exactly two endpoint closes (one per endpoint). A duplicated endpoint leads to three closes: the second triggers page free, the third accesses freed memory.

**Current code (`syscall.rs:451–503`):** Phase 1 reads source via `get_entry` (non-destructive). Phase 2 inserts a copy into target. Source handle survives.

**Fix:** Change `handle_send` to **move** the handle. Phase 1: `close` the source handle (removes from table, returns object + rights). Phase 2: insert into target. If Phase 2 fails, re-insert into source (rollback). This matches the intended use case: parent creates a channel (gets both endpoints), sends one to child, keeps the other.

**Scope:** `syscall.rs` (sys_handle_send). ~15 lines changed. No new modules.

**Depends on:** Nothing.

---

### 10.2 Fix Scheduling Context Ref Counting — DONE

**Bug:** `bind_scheduling_context` stores `context_id` on the thread but does not increment the context's `ref_count`. If all handles to the context are closed, `release_context_inner` decrements ref_count to 0 and frees the slot. The thread's `context_id` now points to a freed slot. `has_budget` treats `None` slots as unlimited budget — the thread silently escapes its allocation.

**Current code:** `scheduler.rs:488–506` (bind), `scheduler.rs:233–244` (release). ref_count tracks handle references only, not bind references.

**Fix:** Increment `ref_count` on bind. Decrement on:

- Thread exit (`exit_current_from_syscall`, after auto-returning borrowed context).
- Process kill (`kill_process`, when draining threads that have bound contexts).

Add `scheduling.context_id` cleanup to both exit paths: read the bound context_id (and saved_context_id if borrowing), call `release_context_inner` for each.

**Scope:** `scheduler.rs` (bind, exit, kill paths). ~20 lines changed. No new modules.

**Depends on:** Nothing.

---

### 10.3 Interrupt-Driven Virtio Drivers — DONE

**Bug:** `virtio::Virtqueue::wait_used()` busy-waits in a `spin_loop()`. Drivers read the IRQ number from shared memory and ignore it. A spinning driver burns an entire core. This contradicts the kernel's event-driven design (handles, `wait`, interrupt forwarding — all the machinery exists and is unused).

**Fix applied:** Drivers now register for their device interrupt and block via `wait` instead of polling:

1. Driver reads IRQ from channel shared memory.
2. `sys::interrupt_register(irq)` → gets waitable interrupt handle.
3. Submit request to virtqueue, `device.notify()`.
4. `sys::wait(&[irq_handle], u64::MAX)` → blocks until device signals completion.
5. `device.ack_interrupt()` → clear virtio interrupt status (must precede GIC unmask).
6. `vq.pop_used()` → process completed request.
7. `sys::interrupt_ack(irq_handle)` → re-enable the IRQ in the GIC.

Removed `wait_used` from virtio library. Kept virtio as a pure library (no syscall dependency) — the interrupt-driven flow lives in each driver (~5 lines).

**Additional fix:** `interrupt_controller::enable_irq` now sets `ITARGETSR` to route SPIs to CPU 0. Without this, SPIs were enabled but had no delivery target — the GIC silently dropped them. PPIs (like the timer) don't need ITARGETSR (per-CPU by definition), which is why this never surfaced before.

**Files changed:** `virtio/lib.rs` (removed `wait_used`), `virtio-blk/main.rs`, `virtio-console/main.rs`, `interrupt_controller.rs` (ITARGETSR fix).

---

### 10.4 Fix Channel Leak in spawn_virtio_driver — DONE

**Bug:** `spawn_virtio_driver` creates a channel (`ch_a`, `_ch_b`) and gives only `ch_a` to the driver. `_ch_b` is never inserted into any handle table and never closed. The channel's `closed_count` never reaches 2. The shared page is never freed. One page leaked per virtio device.

**Current code:** `main.rs:309` — `let (ch_a, _ch_b) = channel::create()`.

**Fix:** Two options:

- **(A) Close ch_b immediately.** The kernel doesn't need the peer endpoint for virtio drivers (it writes device info to the shared page before driver start, then never communicates). `channel::close_endpoint(ch_b)` after setup. Shared page freed when driver closes ch_a on exit. Simple, correct.
- **(B) Give ch_b to the kernel "init" or OS service process.** More future-proof (OS service could signal the driver). But no kernel process exists to hold it yet.

**Decision:** Option A for now. When the OS service process exists, virtio driver management moves to userspace entirely (kernel just probes and spawns, OS service does the rest).

**Scope:** `main.rs` (spawn_virtio_driver). 1 line added.

**Depends on:** Nothing.

---

### 10.5 Extract WaitableRegistry Generic — DONE

**Problem:** Four modules implemented the same waiter pattern — `thread_exit.rs`, `process_exit.rs`, `timer.rs`, `interrupt.rs`. Each had: `create`, `destroy`, `register_waiter`, `unregister_waiter`, `check_ready`, `notify` (two-phase wake). ~100 lines each, nearly identical. ~300 lines of pure duplication.

**Fix:** Created `waitable.rs` with a generic `WaitableRegistry<Id>` — a plain data structure (no lock) that callers embed inside their existing `IrqMutex`-protected state. API: `create`, `destroy`, `register_waiter`, `unregister_waiter`, `check_ready` (non-consuming), `notify` (set ready + return waiter for two-phase wake), `clear_ready` (for edge-triggered semantics like interrupts). Vec + linear search — adequate for small counts (≤32), 10.6 will upgrade to O(1).

**Refactored modules:**

- `thread_exit.rs` — fully replaced: `IrqMutex<WaitableRegistry<ThreadId>>` + thin wrappers. 104 → 54 lines.
- `process_exit.rs` — fully replaced: same pattern. 102 → 56 lines.
- `timer.rs` — embedded `WaitableRegistry<TimerId>` in `TimerTable` alongside `slots: [Option<u64>; 32]` (deadline_ticks only). Domain-specific code (hardware timer, deadlines) untouched. 241 → 220 lines.
- `interrupt.rs` — embedded `WaitableRegistry<InterruptId>` in `InterruptTable` alongside `slots: [Option<u32>; 32]` (IRQ number only). Domain-specific code (GIC, IRQ handling) untouched. 186 → 158 lines.
- `channel.rs` — kept as-is (two endpoints per channel, consume-on-check semantics, shared pages — genuinely different pattern).

**Result:** New `waitable.rs` (~97 lines). Net: ~100 lines removed. 20 host tests in `test/tests/waitable.rs`.

**Depends on:** Nothing, but doing 10.6 immediately after makes sense (the registry's internal data structure benefits from O(1) lookup).

---

### 10.6 O(1) Notification Lookup — DONE

**Problem:** All notification modules use `Vec` + linear search by ID. `check_exited` is O(n), `register_waiter` is O(n), `notify_exit` is O(n). `sys_wait` checks every handle against every module — O(handles × entities). With 100 tracked entities, this is measurably slow under the scheduler lock.

**Fix:** Added `WaitableId` trait with `fn index(self) -> usize`. All kernel ID types implement it (trivially — `self.0 as usize`). `WaitableRegistry` storage changed from `Vec<Entry<Id>>` with linear scan to `Vec<Option<Entry>>` indexed directly by ID. `Entry` no longer stores the ID (position is identity). Every operation — `check_ready`, `notify`, `register_waiter`, `clear_ready`, `destroy` — is now O(1) via `entries.get(id.index())`. `create` grows the Vec as needed with `resize_with`. Freed slots become `None`.

**Scope:** `waitable.rs` (core change), `thread.rs`, `process.rs`, `timer.rs`, `interrupt.rs` (trait impls, 3 lines each). All 20 waitable host tests pass unchanged. Kernel boots and runs with all modules exercised.

---

### 10.7 Syscall Dispatch Macro — DONE

**Problem:** `dispatch()` had 24 match arms, ~20 of which were identical boilerplate: call handler returning `Result<u64, E>`, store Ok/Err in `c.x[0]`, return the same context pointer.

**Fix:** A `dispatch_syscall!` macro collapses each boilerplate arm into a single line. Works for both `Error` and `HandleError` (both `#[repr(i64)]`, so `e as i64 as u64` is uniform). The four special cases that manipulate `ctx` directly (exit, yield, futex_wait, wait) remain hand-written — they may block or switch threads, returning a different context pointer.

**Scope:** `syscall.rs`. ~80 lines of match arms reduced to ~30. No behavioral change. All host tests pass, QEMU smoke test passes.

---

### 10.8 Kernel Metrics — DONE

**Problem:** Zero instrumentation. No context switch count, page fault count, syscall count, or lock contention measurement. When debugging gets hard (and it will — SMP timing issues are non-reproducible), there's no data.

**Fix:** Per-core `AtomicU64` counters (`Relaxed` ordering — monotonic diagnostics, not synchronization). New `metrics.rs` module with `CoreMetrics` struct (5 counters) indexed by `core_id()`. One-line `#[inline(always)]` increment functions at 5 call sites:

- `schedule_inner` → `inc_context_switches()` at both `swap_ttbr0` calls (actual thread switches only, not re-runs)
- `syscall::dispatch` → `inc_syscalls()` at entry (counts all syscalls including blocking ones)
- `user_fault_handler` → `inc_page_faults()` on EL0 data/instruction aborts (demand paging attempts)
- `irq_handler` → `inc_timer_ticks()` on timer PPI (250 Hz per core)
- `IrqMutex::lock` spin loop → `inc_lock_spins()` per spin iteration (contention indicator)

Panic handler calls `metrics::panic_dump()` which prints per-core summaries using panic-safe serial output (no lock acquisition). Metrics are atomics, not behind `IrqMutex`, so no deadlock risk during panic.

**Scope:** `metrics.rs` (~100 lines), 5 one-line increments, 1 line in panic handler. 3 import additions (syscall.rs, scheduler.rs, sync.rs).

---

### 10.9 Watchdog via Scheduling Context Budgets — DONE

**Problem:** If a userspace driver enters a spin loop (e.g., `wait_used` before 10.3 is done, or a bug), that core is permanently burned. No scheduling context means unlimited budget — the thread runs forever until the next timer tick yields, but it immediately wins the next selection too (EEVDF gives it low vruntime from not running during the brief yield).

**Fix applied:** Two parts:

1. **Default scheduling context for all kernel-spawned user threads.** A shared default context (10ms/50ms — 20% of one core) is created during `scheduler::init()`. Both `spawn_user` and `spawn_user_suspended` bind it to new threads via `bind_default_context`, incrementing the ref_count. The OS service (future) can override with content-type-aware budgets via the existing `create_scheduling_context`/`bind_scheduling_context` syscalls.

2. **Budget exhaustion → idle.** `schedule_inner`'s "re-run old thread" branch now checks `has_budget`. If the current thread exhausted its budget and no other thread has budget either (`select_best` returns None), the scheduler runs idle instead of the exhausted thread. The thread resumes on the next replenishment.

**Scope:** `scheduler.rs` only. ~25 lines added. Default context constants (`DEFAULT_BUDGET_NS`, `DEFAULT_PERIOD_NS`), `bind_default_context` helper, `default_context_id` field in State, budget check in `schedule_inner`'s re-run branch.

**Depends on:** 10.2 (ref counting fix — budget contexts must survive handle close while bound).

---

### 10.10 Kernel Stack Guard Pages — DONE

**Problem:** Kernel thread stacks are heap-allocated (`alloc_zeroed` inside Thread). No guard page below the stack. A deep call chain in scheduler or IPC code silently corrupts the heap. User stacks have a guard page (gap below USER_STACK_VA), but kernel stacks don't.

**Fix:** Allocate kernel stacks from the buddy allocator with a guard page at the bottom.

Implementation:

1. `alloc_guarded_stack()` in `thread.rs` computes the order needed for `(stack_pages + 1)` pages, allocates from the buddy allocator, and calls `memory::set_kernel_guard_page()` on the bottom page.
2. `set_kernel_guard_page()` in `memory.rs` handles the TTBR1 page table: if the containing 2MB block is still a coarse L2 block descriptor, it "breaks" it into an L3 page table (allocates a frame, populates 512 entries replicating the block, replaces the L2 entry with a table descriptor). Then clears the guard page's L3 entry to 0 (invalid). Protected by `KERNEL_PT_LOCK`.
3. Thread's `stack_alloc_pa` and `stack_alloc_order` replace the old `stack_bottom`/`stack_size` fields.
4. On drop, `clear_kernel_guard_page()` restores the L3 entry (reading attributes from a neighbor entry), then `free_frames()` returns all pages to the buddy allocator.
5. EL1 faults (`exc_sync` in exception.S) now switch to per-core emergency stacks (4 KiB × 4 cores in `.bss`) before calling `kernel_fault_handler()` in Rust. EC=0x25 (data abort at EL1) identifies likely stack overflow.
6. `boot_tt1_l2_1` symbol promoted to `.global` in boot.S (required for cross-CGU references).

**Stack sizes after change:**

- User kernel stacks: order 3 (8 pages), 1 guard + 7 usable = 28 KiB (was 16 KiB).
- Kernel thread stacks: order 5 (32 pages), 1 guard + 31 usable = 124 KiB (was 64 KiB).
- Boot/idle threads: unchanged (no allocation, use static boot stacks).

**Files modified:** `thread.rs`, `memory.rs`, `exception.S`, `main.rs`, `boot.S`.

---

## 11.0 Final Review Findings

**Status: All 41 issues resolved (2026-03-10).** Kernel builds cleanly, 216 host tests pass, QEMU smoke test boots with 4 SMP cores + IPC + virtio-blk.

Full-codebase review (35 kernel source files, 2 assembly files, 2 linker scripts, 6 userspace programs, 13 test files) identified 3 critical, 12 high, 15 medium, and 11+ low issues across correctness, consistency, code quality, documentation, testing, and idiomatic Rust. This section documents every finding for tracking.

**Priority tiers (all resolved):**

- **11.1–11.3** — Critical. Assembly/linker/virtio correctness.
- **11.4–11.15** — High. Resource leaks, missing checks, test gaps.
- **11.16–11.30** — Medium. Consistency, deduplication, documentation.
- **11.31–11.41** — Low. Polish, naming, style.

---

### 11.1 Emergency stacks too small for MAX_CORES

**Severity:** Critical.

**Bug:** `exception.S:371` allocates `__exc_stacks` as `.space 4096 * 4` but `per_core::MAX_CORES` is 8. A kernel fault on core 4–7 computes SP past the allocation (`(core_id + 1) * 4096` for core 7 = 32 KiB, allocation is 16 KiB), clobbering adjacent `.bss` data. The fault handler then pushes a frame onto corrupted memory, producing a cascading fault with no diagnostic output.

**Fix:** `.space 4096 * 8` to match MAX_CORES. Add a comment cross-referencing `per_core::MAX_CORES`.

**Scope:** `exception.S`. 1 line.

---

### 11.2 Emergency stacks alignment not guaranteed by linker

**Severity:** Critical.

**Bug:** The `.align 12` in `exception.S` aligns the input section fragment, but `link.ld` has no explicit placement for `__exc_stacks`. The MPIDR-indexed SP arithmetic assumes 4096-byte base alignment. The linker may or may not honour the input section's alignment within the output `.bss`.

**Fix:** Give the emergency stacks an explicit named section (`.bss.exc_stacks`) with `ALIGN(4096)` in `link.ld`, matching the treatment of `.bss.stack` and `.bss.stacks`.

**Scope:** `exception.S` (section tag), `link.ld` (new section entry).

---

### 11.3 virtio `push_chain` doesn't write `desc.next` for intermediate descriptors

**Severity:** Critical.

**Bug:** `virtio/lib.rs:333–350` — intermediate descriptors get `DESC_F_NEXT` in flags but `desc.next` is never set to the next descriptor in the chain. It retains the free-list link. On a freshly initialized queue the free list happens to be `0 → 1 → 2 → ...`, so the first multi-buffer submission appears correct. After `free_descriptor_chain` rebuilds the free list in reverse order, the second request sends malformed chains — data corruption or device hang.

Masked today because both drivers issue one request then exit.

**Fix:** Add `desc.next = next_free;` on the intermediate path:

```rust
if i + 1 < bufs.len() {
    desc.flags |= DESC_F_NEXT;
    desc.next = next_free;   // ADD THIS
    current = next_free;
}
```

**Scope:** `virtio/lib.rs`. 1 line.

---

### 11.4 `sys_channel_create` leaks shared page on handle insert failure

**Severity:** High.

**Bug:** `syscall.rs` — if the second handle insert fails, the channel shared page is already mapped into the caller's address space but never unmapped. Also returns `InvalidArgument` instead of the appropriate error code.

**Fix:** On insert failure, unmap the shared page and close the channel. Return correct error.

**Scope:** `syscall.rs` (sys_channel_create). ~10 lines.

---

### 11.5 `channel::shared_info` returns stale PA after full close

**Severity:** High.

**Bug:** `channel.rs:153–161` — after `closed_count == 2` frees the shared page, the `Channel` struct remains in the Vec with the old `shared_pa`. A call to `shared_info` on a closed channel returns a PA that belongs to a different allocation (use-after-free of the PA field).

**Fix:** Zero `shared_pa` on full close. Check for zero at read sites (or check `closed_count < 2`).

**Scope:** `channel.rs`. ~5 lines.

---

### 11.6 Duplicate ELF loading logic in `process.rs`

**Severity:** High.

**Bug:** `create_from_user_elf` and `spawn_from_elf` are ~80% identical — segment loop, VMA creation, page mapping, stack setup. The `copy_nonoverlapping` in `create_from_user_elf` has no SAFETY comment while the identical call in `spawn_from_elf` does. The two paths will diverge silently.

**Fix:** Extract a shared helper `load_elf_into_address_space(elf_bytes, addr_space, eager_all: bool)` that handles the segment-loading loop. Each public function becomes a thin wrapper.

**Scope:** `process.rs`. ~80 lines refactored, net reduction ~60 lines.

---

### 11.7 `with_process` / `with_process_of_thread` panic on stale process

**Severity:** High.

**Bug:** `scheduler.rs:1314–1334` — both call `.expect("process not found")` and are called from syscall paths. A stale process handle (e.g., process killed between handle lookup and this call) causes a kernel panic rather than returning an error.

**Fix:** Change return type to `Option<R>` or `Result<R, E>` and propagate to syscall layer.

**Scope:** `scheduler.rs`, `syscall.rs`, `channel.rs`. ~20 lines across files.

---

### 11.8 ASID search formula diverges from host test

**Severity:** High.

**Bug:** `address_space_id.rs:44` uses `(start + offset) % 255 + 1` while the host test (`asid.rs:34`) uses `(start - 1 + offset) % 255 + 1`. With `start = 1`, the kernel formula yields ASID 2 on the first allocation, skipping ASID 1. The host test correctly starts at ASID 1.

**Fix:** Align the kernel formula with the host test: `(start as u16 - 1 + offset) % 255 + 1`.

**Scope:** `address_space_id.rs`. 1 line.

---

### 11.9 `sys_process_create` leaks process on handle insert failure

**Severity:** High.

**Bug:** `syscall.rs:548–559` — if `handles.insert` fails after `create_from_user_elf`, the rollback calls `process_exit::destroy` but never calls `scheduler::kill_process` to clean up the process and its suspended thread. Process and thread leak.

**Fix:** Add `scheduler::kill_process(process_id)` to the rollback path, with full Phase 2 cleanup (same pattern as `sys_process_kill`).

**Scope:** `syscall.rs` (sys_process_create). ~15 lines.

---

### 11.10 Test coverage gap: `channel.rs` has no host tests

**Severity:** High.

**Bug:** The IPC backbone — encoded ChannelId scheme, two-phase wake, lost-wakeup prevention, endpoint close/refcount — has zero host-level testing. The channel encoding math and state machine are pure algorithms with no hardware dependencies.

**Fix:** Create `test/tests/channel.rs`. Test: encoding/decoding (`channel_index`, `endpoint_index`), signal/pending flag logic, close_endpoint refcounting, double-close behavior.

**Scope:** New test file. ~100–150 lines.

---

### 11.11 Test coverage gap: `futex.rs` has no host tests

**Severity:** High.

**Bug:** The futex hash function, bucket lookup, and PA-keyed wait/wake logic are pure algorithms. The cross-process PA-keyed synchronization semantics are a subtle invariant not validated anywhere.

**Fix:** Create `test/tests/futex.rs`. Test: bucket index computation, hash distribution, registration/deregistration.

**Scope:** New test file. ~80–100 lines.

---

### 11.12 Test drift risk: `slab.rs` and `asid.rs` tests duplicate kernel logic

**Severity:** High.

**Bug:** Both test files reimplement the kernel algorithm instead of using `#[path = "…"] mod`. Changes to the real module won't be caught by tests. All 11 other test files use the include pattern.

**Fix:** Refactor to `#[path = "…"] mod slab;` and `#[path = "…"] mod address_space_id;` with the same stubs used by other tests.

**Scope:** `test/tests/slab.rs`, `test/tests/asid.rs`. ~40 lines each.

---

### 11.13 `device_tree.rs` parser aborts on unknown FDT token

**Severity:** High.

**Bug:** Line 263 returns `None` on unrecognized tokens. FDT spec reserves token values and the Linux parser skips them. Any DTB with firmware extension tokens silently fails to parse.

**Fix:** Replace `_ => return None` with `_ => { /* skip unknown token */ }` and advance past it.

**Scope:** `device_tree.rs`. ~3 lines.

---

### 11.14 `per_core.rs` dead `id` field

**Severity:** High.

**Bug:** `PerCpu.id` is `u32` (not atomic), initialized to 0, never written by `init_core`. `core_id()` reads MPIDR directly and is always authoritative. The field is dead weight that could mislead a reader.

**Fix:** Remove `PerCpu.id` and the `INIT` that sets it. `core_id()` is the single source of truth.

**Scope:** `per_core.rs`. ~5 lines.

---

### 11.15 `interrupt_controller::acknowledge()` doc misleading

**Severity:** High.

**Bug:** Doc says "returns the IRQ id" but actually returns the full IAR register (includes CPUID in bits [12:10]). Callers must pass it intact to `end_of_interrupt`. A caller extracting just bits [9:0] as an IRQ number would silently break EOI.

**Fix:** Change doc to: "Returns the full IAR register value (not just the IRQ ID) — pass it intact to `end_of_interrupt`."

**Scope:** `interrupt_controller.rs`. Doc comment only.

---

### 11.16 Scheduling context release duplicated between exit and kill paths

**Severity:** Medium.

**Problem:** `exit_current_from_syscall` (lines 754–768) releases `context_id` and `saved_context_id` inline. `kill_process` (lines 991–998) has a local closure `release_thread_contexts` that does the same thing. The two paths are structurally different for the same semantic operation.

**Fix:** Extract a shared free function `release_thread_context_ids(s: &mut State, thread: &mut Thread)` and use it in both places.

---

### 11.17 bind/borrow scheduling context: increment-then-undo pattern

**Severity:** Medium.

**Problem:** `bind_scheduling_context` and `borrow_scheduling_context` optimistically increment `ref_count`, then check a condition, and undo on failure. Safe under the lock, but fragile — a future early-return that forgets to undo leaks a ref.

**Fix:** Restructure to check-first, increment-only-on-success, using the split-borrow pattern already established in `current_thread_and_process_do`.

---

### 11.18 `find_thread_pid` and `with_thread_mut` don't search suspended list

**Severity:** Medium.

**Problem:** Both functions search ready, blocked, and cores but skip `suspended`. A caller looking up a suspended thread panics. Inconsistent with `kill_process` which handles suspended threads.

**Fix:** Either add `suspended` to the search, or add a comment/debug_assert documenting the constraint.

---

### 11.19 Handle categorization duplicated in exit and kill paths

**Severity:** Medium.

**Problem:** The `for obj in handle_objects { match obj { ... } }` pattern that sorts handles into channel/interrupt/timer/thread/process buckets appears identically in `exit_current_from_syscall` and `kill_process`.

**Fix:** Extract `categorize_handles(objects: Vec<HandleObject>, s: &mut State) -> HandleCategories`.

---

### 11.20 Thread constructors repeat all fields 4 times

**Severity:** Medium.

**Problem:** `Thread::new`, `new_boot`, `new_idle`, `new_user` all manually set every field. Adding a new field requires updating all four. ~100 lines of boilerplate.

**Fix:** Extract `Thread::base_fields(id, state, trust_level)` that captures the common initialization. Each public constructor mutates only what differs.

---

### 11.21 `is_user_page_readable` duplicates `user_va_to_pa`

**Severity:** Medium.

**Problem:** Both functions perform the identical `AT S1E0R` + `MRS PAR_EL1` sequence. `user_va_to_pa` is the strict superset. Any future change to PAR parsing must be made in two places.

**Fix:** `fn is_user_page_readable(va: u64) -> bool { user_va_to_pa(va).is_some() }`.

---

### 11.22 `set_kernel_guard_page` SAFETY comment misleading

**Severity:** Medium.

**Problem:** `memory.rs:228–230` comment says "single 64-bit write is atomic on AArch64. The L3 table maps identical pages, so any concurrent access resolves correctly regardless of TLB state." The truth is: it's the `tlb_invalidate_all()` call that makes the break-before-make safe, not write atomicity.

**Fix:** Remove "regardless of TLB state" and note that the subsequent TLB flush is what provides safety.

---

### 11.23 `PAGE_SIZE` defined in three places

**Severity:** Medium.

**Problem:** `paging.rs:7` (canonical, `u64`), `page_allocator.rs:18` (imports and casts), `slab.rs:17` (hardcodes `4096` independently). If page size changes, `slab.rs` is silently missed.

**Fix:** `slab.rs`: `use super::paging; const PAGE_SIZE: usize = paging::PAGE_SIZE as usize;`.

---

### 11.24 Serial formatting duplicated between locked and panic variants

**Severity:** Medium.

**Problem:** `serial.rs` has duplicated logic for `put_hex`/`panic_put_hex` and `put_u32`/`panic_put_u32`. Same decimal/hex formatting written twice.

**Fix:** Extract a shared `format_hex`/`format_decimal` helper that takes a write function as parameter. Or have the locked variant call the panic variant after acquiring the lock.

---

### 11.25 `free_all` TLB precondition undocumented

**Severity:** Medium.

**Problem:** `address_space.rs:102` — `free_all` does not call `invalidate_tlb` itself. The caller must do it first. This is not documented. A future caller that skips TLB invalidation would free frames that are still reachable via stale TLB entries.

**Fix:** Add to the doc comment: "Caller must call `invalidate_tlb()` before this."

---

### 11.26 `order_for_pages` reimplements stdlib

**Severity:** Medium.

**Problem:** `thread.rs:339–347` — manual loop equivalent to `pages.next_power_of_two().trailing_zeros() as usize`. The stdlib version handles edge cases and communicates intent more clearly.

**Fix:** Replace with `pages.next_power_of_two().trailing_zeros() as usize`.

---

### 11.27 `set_wake_pending_inner` doesn't search blocked list — undocumented

**Severity:** Medium.

**Problem:** `scheduler.rs:413–433` searches `cores` and `ready` but not `blocked`. This is intentionally correct (called only when `try_wake` returned false, meaning the thread is not blocked), but the reasoning is non-obvious.

**Fix:** Add comment: "Thread is guaranteed not blocked — `set_wake_pending` is only called when `try_wake` already returned false."

---

### 11.28 `init_secondary` dead code

**Severity:** Medium.

**Problem:** `scheduler.rs:959` — `let _ = ctx_ptr;` with comment "Keep ctx*ptr used so idle isn't optimized away." The idle thread is moved into `s.cores[idx].idle` — it won't be optimized away. The `let * =` is a no-op. Raw pointers don't extend lifetimes.

**Fix:** Remove the dead line and misleading comment.

---

### 11.29 `sys_write` double `\n` → `\r\n` translation

**Severity:** Medium.

**Problem:** `sys_write` does `\n` → `\r\n` translation at the syscall layer. `serial::raw_puts` also does it. A userspace `\n` becomes `\r\n` → `\r\r\n`. A userspace `\r\n` becomes `\r\n` → `\r\r\n`.

**Fix:** Remove the translation from one layer. Prefer keeping it in `serial.rs` (closest to hardware) and removing it from `syscall.rs`.

---

### 11.30 Userspace programs hardcode `SHM = 0x4000_0000`

**Severity:** Medium.

**Problem:** `init/main.rs`, `echo/main.rs`, `virtio-blk/main.rs`, `virtio-console/main.rs` all hardcode `0x4000_0000` with no cross-reference to `paging::CHANNEL_SHM_BASE`. If the kernel constant changes, four programs silently break.

**Fix:** Add comment `// must match kernel paging::CHANNEL_SHM_BASE` to each. Long-term: expose as a shared constant via a header crate.

---

### 11.31 `address_space.rs` double-own risk on `map_page`

**Severity:** Low.

**Problem:** `map_page` unconditionally pushes PA into `owned_frames`. Calling it twice with the same PA (e.g., re-mapping) produces a double-free in `free_all`.

**Fix:** Add `debug_assert!(!self.owned_frames.contains(&Pa(pa as usize)))`.

---

### 11.32 `Rights` lacks `PartialEq`/`Eq` derives

**Severity:** Low.

**Problem:** `handle.rs` — `Rights(u32)` has no equality derives. Other newtypes (`Pa`, `ChannelId`, etc.) do. Adding them costs nothing and enables `rights == Rights::READ` comparisons.

**Fix:** `#[derive(Clone, Copy, Debug, PartialEq, Eq)]`.

---

### 11.33 `0xFF00` idle thread marker is a magic constant

**Severity:** Low.

**Problem:** `thread.rs:139,270` — `0xFF00` appears as a bare hex literal. Used in `is_idle()` check and `new_idle()` constructor.

**Fix:** `const IDLE_THREAD_ID_MARKER: u64 = 0xFF00;`.

---

### 11.34 `page_offset` in `memory_region.rs` misnamed

**Severity:** Low.

**Problem:** `VmaList::page_offset(va)` computes the page-aligned _base_ address (`va & !0xFFF`), not the offset within a page. Name is misleading. Also appears to be dead code (handle_fault recomputes inline).

**Fix:** Rename to `page_base` or `align_down_to_page`. Remove if unused.

---

### 11.35 `Vma.readable` field always true, never checked

**Severity:** Low.

**Problem:** `memory_region.rs` — `readable: bool` is set in ELF loading and tests but never read in production code. All pages are at least readable. Dead weight.

**Fix:** Either add a `// TODO: enforce readable=false` comment for future no-access mappings, or remove the field.

---

### 11.36 `WaitableRegistry::create` silently ignores duplicates

**Severity:** Low.

**Problem:** `waitable.rs:66–81` — a duplicate `create` call for an existing ID is a no-op. If a thread ID were ever reused without destroying the old entry, the new thread would inherit the old `ready` flag.

**Fix:** Add `debug_assert!(self.entries[idx].is_none(), "duplicate waitable ID")`.

---

### 11.37 `DrainHandles` inconsistent with `close` in return type ordering

**Severity:** Low.

**Problem:** `DrainHandles::next()` yields `(Handle, HandleObject)` while `close()` returns `(HandleObject, Rights)`. `drain` discards rights; `close` discards the handle index. The argument ordering conventions are reversed.

**Fix:** Align to `(HandleObject, Rights)` everywhere, or document the intentional difference.

---

### 11.38 `insert_at` reuses `HandleError::TableFull` for "slot occupied"

**Severity:** Low.

**Problem:** `handle.rs:120–135` — `insert_at` returns `Err(HandleError::TableFull)` when the specific slot is occupied. Semantically different from "no free slots." Could add `HandleError::SlotOccupied`, or document the reuse.

---

### 11.39 `scheduling_context::maybe_replenish` overflow risk

**Severity:** Low.

**Problem:** `(periods_skipped + 1) * period` can overflow `u64` if a system runs long enough. `charge` uses `saturating_sub` but `maybe_replenish` uses bare arithmetic.

**Fix:** Use `saturating_add` and `saturating_mul` in the replenish calculation.

---

### 11.40 `fill_tables` in boot.S clobber list incomplete

**Severity:** Low.

**Problem:** Comment says "Clobbers: x13, x14, x15" but the function also writes x16, x17. Not a bug today (callers don't use x16/x17) but documentation is wrong.

**Fix:** Update comment: "Clobbers: x13–x17."

---

### 11.41 `KERNEL_VA_OFFSET` no cross-reference between link.ld and memory.rs

**Severity:** Low.

**Problem:** Appears in both `link.ld` (`0xFFFF000000000000`) and `memory.rs` (`0xFFFF_0000_0000_0000`). A mismatch produces a non-booting kernel with no diagnostic. No cross-reference comment.

**Fix:** Add `/* must match memory::KERNEL_VA_OFFSET */` in `link.ld` and vice versa.
