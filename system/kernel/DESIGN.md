# Kernel Design Notes

Architectural decision record for every kernel subsystem. Captures the "why" behind each choice: the goal, the approach taken, alternatives considered and rejected, and the rationale. Sections 0.x cover the foundational architecture. Sections 6.x+ cover scheduler evolution, hardening, and code quality.

The finished code's module-level docs are the authoritative reference for _what_ the code does; these notes capture _why_ it does it that way.

---

## 0.1 Boot Sequence & Exception Level

**Goal:** Cold boot on QEMU `virt`, transition from EL2 (hypervisor) to EL1 (kernel), enable MMU, enter Rust.

**Approach:** `boot.S` assembly trampoline. Core 0 boots first (parks others via MPIDR check). Creates coarse 2 MiB block mappings for both identity (TTBR0, needed until MMU is live) and kernel upper VA (TTBR1). Enables MMU, drops to EL1h via `ERET`, jumps to `kernel_main` at upper VA. Secondary cores (1–3) are brought online via PSCI `CPU_ON` from `kernel_main` after core 0 finishes initialization (see §0.10).

**Why EL1 (not EL2):** Hypervisor mode provides no benefit for a single-OS kernel. EL2 adds complexity (stage-2 translation, HCR_EL2 configuration) with no upside. Decision #16 explicitly rejected hypervisor design.

**Why coarse then refine:** `boot.S` must be minimal — just enough to enable the MMU and reach Rust. Fine-grained W^X refinement happens in Rust (`memory::init()`) once the kernel is running at upper VA with a heap available.

**Identity map reclamation:** After all cores have booted and transitioned to upper VA, `reclaim_boot_ttbr0()` frees the boot identity map tables (known symbols from `boot.S`) back into the frame allocator. Must wait until secondary cores complete their trampolines.

---

## 0.2 Address Space Layout (Split TTBR)

**Goal:** Isolate kernel and user address spaces using hardware support.

**Approach:** ARMv8 split TTBR — `TTBR1_EL1` maps the kernel (upper VA, `0xFFFF_...`), `TTBR0_EL1` maps user processes (lower VA, `0x0000_...`). Kernel VA = PA + `0xFFFF_FFF0_0000_0000` (T1SZ=28, 64 GiB kernel VA range). User VA layout: code at 4 MiB, channel shared memory at 1 GiB, stack at 2 GiB.

**Alternatives considered:**

- **Single address space** (TTBR0 only, kernel/user separated by range): Rejected. Split TTBR is the intended ARM mechanism. Gives free kernel isolation and makes context switch cheaper — only TTBR0 is swapped, TTBR1 stays.

**Why this VA layout:** Code at 4 MiB avoids null-pointer dereference landing in valid code. Channel shared memory and stack at fixed VAs keep the memory map predictable. Guard page below the stack catches overflow (no VMA → fault → kill).

---

## 0.3 Exception Handling & Context Save/Restore

**Goal:** Handle IRQs (timer), SVCs (syscalls), and synchronous faults (page faults) from EL0.

**Approach:** `exception.S` installs `VBAR_EL1` with the standard ARM exception vector table. On any exception: save full register state (x0–x30, SP, ELR, SPSR, SP_EL0, TPIDR_EL0, NEON q0–q31, FPCR/FPSR) to the current Thread's `Context` at offset 0 (located via `TPIDR_EL1`). Call the appropriate Rust handler. Handler returns a `Context` pointer — possibly a different thread. Restore from that pointer and `ERET`.

**Why save NEON eagerly:** User code can use SIMD at any time. Lazy save tracks "dirty" state per thread, adding complexity for marginal benefit. Full save is ~512 bytes per context switch but predictable and simple.

**Why Context at offset 0:** `TPIDR_EL1` → `Thread` → `Context` with zero offset computation. Simple, fast. A compile-time assertion (`offset_of!(Thread, context) == 0`) enforces this invariant.

**TPIDR_EL1 update timing (Fix 17, 2026-03-14):** `TPIDR_EL1` must be updated to the new thread's Context _before_ the scheduler lock drops. Originally, exception.S set `TPIDR_EL1` after the Rust handler returned (`msr tpidr_el1, x0`). But the Rust handler drops the `IrqMutex` guard on return, which re-enables IRQs. If a pending timer IRQ fires in the ~3-instruction window between lock release and the `msr tpidr_el1`, `save_context` reads the stale TPIDR and overwrites the old thread's Context (now in the ready queue) with kernel-mode state, corrupting it. Fix: `schedule_inner` writes `msr tpidr_el1` while the lock is held. The exception.S write is now redundant but kept for defense-in-depth.

**Eret validation (2026-03-14):** `validate_context_before_eret` in `main.rs` checks ELR/SPSR/SP consistency before every handler return: (1) EL1 return with user-range ELR (would crash with EC=0x21), (2) EL0 return with kernel-range ELR (privilege escalation), (3) EL1 return with non-kernel SP (stack corruption). Panics with detailed diagnostics instead of an opaque exception.

---

## 0.4 Page Tables & W^X Enforcement

**Goal:** Fine-grained page permissions for both kernel and user code.

**Approach:** Boot starts with 2 MiB blocks (L2 entries). `memory::init()` refines the kernel's block into 16 KiB L3 pages with per-section permissions: `.text` RX, `.rodata` RO, `.data`/`.bss` RW. User pages are always 16 KiB with per-page W^X (enforced by `segment_attrs`: X wins over W if both set). 2-level page tables (L2+L3) with T0SZ/T1SZ=28 (64 GiB VA per half).

**Why W^X:** Any page that is both writable and executable is an injection vector. W^X is the minimum viable security invariant for page permissions. ARM provides the bits (`PXN`, `UXN`, `AP_RO`) — using them costs nothing.

---

## 0.5 Heap Allocator

**Goal:** Dynamic allocation for kernel data structures (threads, page tables, vectors, etc.).

**Approach:** Three-tier allocation:

1. **Slab caches** for common sizes (64–2048 bytes, O(1) alloc/free).
2. **Linked-list allocator** for variable sizes (first-fit, address-sorted free list, coalescing on free).
3. **Buddy allocator** for page frames (contiguous 2^n, 16 KiB – 4 MiB).

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

**Service loading:** The kernel embeds only init via `include_bytes!`. All other service ELFs are in a flat archive (service pack) linked as a `.services` section. The kernel maps this region read-only into init's address space at `SERVICE_PACK_BASE`. Init looks up ELFs by role ID and passes them to `process_create`.

**ASID allocation:** 8-bit ASIDs (1–255) with generation-based recycling. Each `AddressSpace` stores an ASID + generation number. On context switch, if the thread's generation doesn't match the global generation, the ASID is lazily revalidated. When all 255 ASIDs are exhausted: flush TLB (`tlbi vmalle1is`), increment generation counter, restart allocation from ASID 1. No hard limit — the system never panics on ASID exhaustion. Standard approach (cf. Linux `arch/arm64/mm/context.c`).

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

**Goal:** OS-mediated access control for kernel objects. Capability-grade handle system that supports rights attenuation, badging, and scalable handle counts.

**Approach:** Per-process two-level handle table. Base array of 256 slots (fast path, no allocation) + overflow pages of 256 slots each allocated on demand from the heap. Handle is `u16` (65,536 address space), capped at 4,096 entries per process. Kernel validates handle + rights on every syscall. Handles are indices (not pointers) — user code can't forge access.

**Named rights (u32 backing):**

| Right    | Bit | Gates                                                                          |
| -------- | --- | ------------------------------------------------------------------------------ |
| READ     | 0   | `vmo_read`, `vmo_map` readable, channel read                                   |
| WRITE    | 1   | `vmo_write`, `vmo_map` writable, `vmo_snapshot`, `vmo_restore`, `vmo_op_range` |
| SIGNAL   | 2   | `channel_signal`                                                               |
| WAIT     | 3   | `wait` on handle                                                               |
| MAP      | 4   | `vmo_map` (required for any mapping)                                           |
| TRANSFER | 5   | `handle_send` (transfer handle to another process)                             |
| CREATE   | 6   | Reserved for future factory handles                                            |
| KILL     | 7   | `process_kill`                                                                 |
| APPEND   | 8   | `vmo_write` at offset >= committed_size only (no overwrite of existing data)   |
| SEAL     | 9   | `vmo_seal` (consumed on use — irreversible freeze)                             |

`Rights::ALL` = `0x3FF` (bits 0-9). `Rights::from_raw(u32)` masks to defined bits, silently dropping undefined bits. Future rights consume bits 10+ without changing existing code.

**Monotonic attenuation:** `handle_send(target, handle, rights_mask)` creates a new handle in the target process with `original & mask`. Rights can only be removed, never added. A handle with READ+MAP but not WRITE allows read-only mapping — cannot write, cannot snapshot. The `attenuate` method is `const fn`, enabling compile-time rights computation.

**Badges:** Each handle carries an opaque `u64` badge (default 0). `handle_set_badge(handle, badge)` / `handle_get_badge(handle)` syscalls. Badges are preserved through `handle_send` and survive rights attenuation. Use case: init sets badges on handles before sending them to services, so shared servers identify which client a handle was assigned to without needing a global PID namespace. Inspired by seL4's badge mechanism — well-understood, minimal kernel surface.

**Alternatives considered:**

- **Centralized ACLs:** Separate permissions database. Extra indirection, harder to reason about per-process state.
- **No access control:** Rejected — even a personal OS needs isolation between untrusted editors and trusted OS services.
- **Fixed 256-slot table (original design):** Simple but insufficient for compound documents with many channels. The two-level scheme preserves O(1) access for the common case (first 256 handles) while growing on demand for handle-heavy processes.

**Why two-level, not a hash table or tree:** The base array covers 99% of processes with zero allocation and zero indirection. Overflow pages are rare and amortize across 256 handles each. A hash table would add per-lookup overhead to every syscall (hash + probe) for a growth property that's rarely needed. A BTreeMap would be O(log n) per lookup — worse than the O(1) direct-index scheme.

**Prior art:** Fuchsia/Zircon (handle-based with rights attenuation, badges for server identification), seL4 (capability-based, badges on endpoints), L4Re (capability IPC with badges).

---

## 0.9 Syscall Interface

**Goal:** Controlled entry from EL0 to EL1 for kernel services.

**Approach:** `SVC #0` from user code. Register ABI: `x8` = syscall number, `x0`–`x5` = arguments, `x0` = return value (≥0 success, <0 error). Twelve syscalls in three families: core (`exit`, `write`, `yield`), handle/IPC (`handle_close`, `channel_signal`, `channel_wait`), scheduling (`scheduling_context_create`, `scheduling_context_bind`, `scheduling_context_borrow`, `scheduling_context_return`), synchronization (`futex_wait`, `futex_wake`).

**Why SVC:** Standard EL0→EL1 trap instruction on ARM. HVC is EL1→EL2 (hypervisor), SMC is for the secure monitor.

**Why register ABI (not memory):** Registers are faster than memory reads, natural for the ARM calling convention, and require no validation of a memory-based argument block.

**Write syscall validation:** User buffer address checked (`< USER_VA_END`), length capped (4 KiB), and every page in the range verified readable via `AT S1E0R` hardware translation instruction. Prevents the kernel from reading unmapped memory on behalf of user code.

---

## 0.10 Synchronization Primitives

**Goal:** Mutual exclusion for kernel data structures on SMP.

**Approach:** `IrqMutex<T>` — ticket spinlock + IRQ masking. Mask IRQs (save DAIF), acquire ticket (`atomic fetch-add` on `next_ticket`, spin on `now_serving`), release = increment `now_serving` + restore DAIF. `IrqMutex::lock()` returns `IrqGuard` — drop restores DAIF and releases the ticket. Uses `ldaxr`/`stlxr` (LL/SC) for ticket operations. External API unchanged from the original single-core version.

**Why ticket locks:**

- **Test-and-set:** cache-line bouncing (every core writes the same line).
- **Ticket:** separates "take a number" (write) from "check counter" (read), reducing contention. FIFO ordering prevents starvation.
- **MCS:** better under very high contention but adds per-lock queue nodes. Unjustified complexity for ≤8 cores. Revisit if scaling beyond that.

Lock ordering is documented in `LOCK-ORDERING.md`. Cross-cutting safety invariants (TPIDR chain, handle lifecycle, ASID lifecycle) in `SAFETY-MAP.md`.

---

## 0.11 Multi-Core Boot (SMP)

**Goal:** All cores boot, initialize own state, enter the scheduler.

**Approach:** PSCI (Power State Coordination Interface) via `hvc` to bring up secondary cores. Core 0 calls `CPU_ON` for each secondary with a boot address and context ID. Secondary core trampoline: enable MMU (shared TTBR1, empty TTBR0), set own kernel stack, configure `TPIDR_EL1`, init GIC CPU interface + timer, enter scheduler idle loop.

**Per-core data:** `[PerCpu; MAX_CORES]` array (`MAX_CORES` = 8, QEMU `virt` max). Holds current thread pointer, core ID. Indexed by MPIDR affinity. `core_id()` reads MPIDR directly — single source of truth.

**Boot/idle thread:** Each core has a single boot/idle thread (`new_boot_idle(core_id)`) that serves dual purpose: represents the initial execution context (kernel_main on core 0, secondary_main on cores 1+), and acts as the idle fallback when no user threads are runnable. Marked with `IDLE_THREAD_ID_MARKER` so the scheduler returns it to the per-core idle slot, never the global ready queue. This prevents cross-core migration — each boot thread stays on its originating core's boot stack.

**Why PSCI over spin-table:** PSCI is the ARM-standard firmware interface. Works on QEMU, real hardware with UEFI/ATF, and most hypervisors. Spin-table is QEMU-specific.

---

## 0.12 Architecture Abstraction (the `arch` Module Contract)

**Goal:** A clean boundary between architecture-specific and generic kernel code. THE reference for anyone porting this kernel to a new ISA. After this extraction, the generic kernel never constructs page table descriptors, never names a CPU register, and never emits inline assembly. All 14 files under `arch/aarch64/` — zero asm outside arch.

**Design principle:** The arch module is a driver. It translates hardware specifics into kernel-internal abstractions — same pattern as a virtio driver translating device registers into OS primitives. The interface is the simplest version that works. Complexity lives inside each arch implementation (leaf node). Compile-time module selection via `#[cfg(target_arch)]`, not trait objects. Zero-overhead: all calls monomorphized/inlined.

**Three settled design decisions:**

**Device discovery -> SEPARATE.** DTB/ACPI is consumed by init (userspace) to find device addresses. On x86 it would be ACPI tables — completely different mechanism. Putting either inside the arch boundary would abstract _platform_, not _architecture_. `device_tree.rs` stays generic. Arch provides minimal boot info (RAM base/size, DTB pointer). Informed by the NT HAL lesson: the HAL's device-enumeration scope was eaten by firmware standardization (ACPI, UEFI). Abstract what genuinely varies between ISAs, nothing more.

**Page tables -> VA/PA/permissions interface.** Walk logic, descriptor format, TLB invalidation are all arch-internal. The generic kernel never constructs descriptors. Arch owns the walk and calls the page allocator directly for intermediate table pages — same pattern as Linux (`arch/arm64/mm/mmu.c` calls `alloc_pages()`), Zircon (`ArmArchVmAspace::MapPages()` calls `AllocPage()`), and Redox. seL4 is the only kernel that externalizes table provisioning to userspace, but that's driven by formal verification constraints (eliminating implicit allocation from the proof), not applicable here.

**Context -> fully arch-defined, generic accessors.** The register set IS the architecture. ARM64 Context has x[31], sp, elr, spsr, q[32]; x86_64 would have rax-r15, rip, rflags, xmm. Zero overlap. The abstraction is method-based: `pc()`, `set_sp()`, `arg(n)`, `set_user_mode()`. On aarch64, `pc()` returns `self.elr`; on x86_64 it would return `self.rip`.

**Additional decisions:**

- **IrqState -> opaque newtype.** Zero-cost `#[repr(transparent)]` wrapper, consistent with the project's existing `Pa` newtype pattern. No production C kernel does this (C lacks zero-cost newtypes), but Rust enables it. `interrupts::mask_all() -> IrqState`, `interrupts::restore(IrqState)`.
- **Serial -> arch for now, platform later.** PL011 is technically a device (board-specific), not architecture. Linux, Zircon, and seL4 all separate arch from platform. But a platform layer serves zero boards today. Serial (~60 lines) lives in arch with an explicit marker: "platform-specific, extract to `platform::` when a second board target arrives (v0.14)."

**Settled interface:**

```rust
mod arch {
    // Boot
    fn init_boot_cpu(dtb_ptr: *const u8) -> BootInfo;
    fn init_secondary_cpu(core_id: usize);

    // Context (fully arch-defined, generic accessors)
    struct Context { /* arch-specific */ }
    // Methods: new, pc, set_pc, sp, set_sp, arg, set_arg,
    //          set_user_mode, set_user_tls, user_tls

    // Core identity
    fn core_id() -> u32;
    fn set_current_thread(ctx: *mut Context);

    // MMU
    mod mmu {
        create, map, unmap, switch, invalidate, destroy,
        set_kernel_guard, clear_kernel_guard, is_user_accessible
    }

    // Interrupts
    mod interrupts {
        init, enable_irq, disable_irq, acknowledge,
        end_of_interrupt, send_ipi, mask_all -> IrqState, restore(IrqState)
    }

    // Timer
    mod timer { init, set_deadline_ns, now_ns, frequency }

    // Serial (platform-specific; extract to platform:: at v0.14)
    mod serial { init, put_byte }

    // Power
    mod power { cpu_on, system_off }
}
```

**What stays generic (no arch dependency):** `scheduler.rs` (algorithm + state machine), `scheduling_algorithm.rs` (pure EEVDF math), `scheduling_context.rs` (budget/period accounting), `channel.rs`/`handle.rs`/`waitable.rs` (IPC + capabilities), `process.rs`/`thread.rs` (process model), `executable.rs` (ELF loading), `futex.rs` (PA-keyed wait/wake), `heap.rs`/`slab.rs`/`page_allocator.rs` (allocators), `device_tree.rs` (DTB parsing), `sync.rs` (IrqMutex — generic lock logic, arch for IRQ masking), `metrics.rs`/`syscall.rs` (dispatch logic). Roughly 60-65% of the kernel is architecture-independent.

**Verification:** Full test suite passes. Clippy clean. No `asm!` outside `arch/aarch64/`. No ARM64 register names outside `arch/aarch64/`. Grep audit for leakage.

---

## 6.1 Scheduling Algorithm: EEVDF + Scheduling Contexts

**Goal:** Two-layer scheduling. Scheduling contexts provide per-workload temporal isolation and server billing. EEVDF provides proportional-fair selection with latency differentiation among eligible threads.

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

**Depends on:** 0.10 (sync primitives), 0.11 (SMP boot), 0.8 (handle table for scheduling context handles).

### Accepted limitation: `reprogram_next_deadline` inside scheduler lock

`reprogram_next_deadline` is called from `schedule_inner` while holding the global scheduler `IrqMutex`. This means timer reprogramming (reading the timer heap, computing the minimum deadline, writing `CNTV_TVAL`) extends the lock hold time by tens of nanoseconds per context switch.

**Why this is acceptable:** The target is a personal workstation (≤32 cores). Scheduler lock contention scales roughly O(n²) with core count, but the constant factor for a ~50ns hold-time extension is negligible at this scale — worst case ~1.5μs total stall across all cores, invisible in practice.

**What a production multi-core kernel would do:** Per-CPU timer management. Compute the deadline under the lock, store it in `PerCoreState`, release the lock, then program the timer register. This is what Linux does — each core manages its own timer wheel independently, and `__schedule()` drops its per-CPU runqueue lock before the timer path runs. The kernel already has `PerCoreState` infrastructure to support this if needed.

**When to revisit:** If this kernel ever targets server-class hardware (64+ cores) or NUMA topologies where cache-line bouncing on the scheduler lock adds microseconds per access. Not applicable for the personal workstation target.

### Known issue: ready queue reuse race

`schedule_inner` moves the old thread to the global ready queue while the originating core is still on its kernel stack (between lock drop and `restore_context_and_eret`). Another core can pick up the thread and restore it, causing two cores to execute on the same stack. See §12.1 for full analysis and fix plan (per-core deferred ready list).

---

## 6.2 GICv3 Migration, Tickless Idle & IPI Wakeup

**Goal:** Replace the fixed-rate 250 Hz timer tick with demand-driven scheduling. Idle cores sleep in WFI and consume near-zero power. Woken cores are kicked by IPI when work arrives. Requires GICv3 for software-generated interrupts (SGIs) via system registers.

### GICv3 migration

**Approach:** GICv3 with system register CPU interface (ICC\_\*\_EL1) and MMIO distributor (GICD) + redistributor (GICR). (Replaced GICv2 which used MMIO for the CPU interface.)

**Why GICv3:** GICv2's SGI mechanism writes to GICD_SGIR (a distributor MMIO register), which is shared and requires synchronization. GICv3 uses ICC_SGI1R_EL1 — a per-core system register write, no lock, no MMIO. This is essential for sending IPIs from inside the scheduler lock without adding a lock dependency.

**Implementation:** `interrupt_controller.rs` provides the `InterruptController` trait (7 methods: `init_distributor`, `init_per_core`, `acknowledge`, `end_of_interrupt`, `enable_irq`, `disable_irq`, `send_ipi`) with a concrete `GicV3` implementation. Static dispatch only — the global `GIC` instance is a concrete `GicV3`, no vtable. GICv2 code deleted entirely (no parallel implementations). `boot.S` updated: EL2→EL1 transition enables ICC_SRE_EL2 (system register interface) before dropping to EL1. All QEMU scripts use `-machine virt,gic-version=3`.

**Key hardware details:**

- CPU interface: ICC_IAR1_EL1 (acknowledge), ICC_EOIR1_EL1 (EOI), ICC_PMR_EL1 (priority mask), ICC_IGRPEN1_EL1 (group enable), ICC_SGI1R_EL1 (software-generated interrupt)
- Distributor (GICD): base address from DTB. GICD_CTLR, GICD_ISENABLER, GICD_ICENABLER, GICD_IPRIORITYR, GICD_IROUTER for SPI configuration
- Redistributor (GICR): base address from DTB (GIC node reg property, second range). Per-core GICR frame (64 KiB stride). GICR_WAKER for core wake, SGI/PPI configuration via GICR_ISENABLER0/GICR_IPRIORITYR

### Tickless idle

**Approach:** `reprogram_next_deadline()` computes the earliest deadline from three sources — timer objects (AtomicU64 cache for lock-free reads), current thread's scheduling context budget expiry, and scheduling context replenishment times — then programs CNTV_TVAL_EL0 for exactly that deadline. If no deadlines exist, programs u32::MAX ticks (~68 seconds at 62.5 MHz). No fixed tick rate — the hardware timer fires only when something needs attention. (Replaced the original 250 Hz fixed tick.)

**Deadline cache:** `EARLIEST_DEADLINE` is an `AtomicU64` updated on timer create/destroy/expire. `earliest_timer_deadline()` reads it with `Acquire` ordering — no lock needed on the hot path. The timer lock is only held when mutating the timer table.

### IPI wakeup

**Problem:** With tickless idle, a core sleeping in WFI won't wake until its own timer fires. If another core enqueues a thread, the sleeping core doesn't know.

**Solution:** `ipi_kick_idle_core()` is called from `try_wake` (under the scheduler lock) after enqueuing a thread. It scans `PerCoreState.is_idle` flags and sends SGI 0 via `GIC.send_ipi(core_id)` to the first idle core found. One IPI is sufficient — the woken core picks up the thread from the global ready queue.

**Race prevention:** Both the idle-entry path (sets `is_idle = true`, enters WFI) and `try_wake` (reads `is_idle`, pushes to queue) execute under the scheduler `STATE` lock. This prevents the race where core A reads `is_idle = false` for core B, then core B sets `is_idle = true` and sleeps, stranding the thread. See `LOCK-ORDERING.md` for the full analysis.

**WFI not WFE:** IPIs (SGI 0 via ICC_SGI1R_EL1) wake WFI but not WFE. WFE requires an explicit SEV event, which GICv3 IPIs do not generate.

**Depends on:** 6.1 (scheduling contexts for budget deadline computation), 0.10 (IrqMutex for scheduler lock), 0.11 (SMP boot, per-core state).

---

## 6.3 Per-Core Ready Queues & SMP Scalability

**Goal:** Replace the single global ready queue with per-core ready queues, enabling core-affine scheduling, work stealing, and provable Concurrent Work Conservation (CWC). Three novel properties that no other production microkernel combines: (1) per-core EEVDF with virtual lag preservation across migration, (2) workload-granularity migration via scheduling contexts, (3) property-tested CWC guarantee.

**Prior art and research basis:**

- Stoica & Abdel-Wahab 1995: original EEVDF paper, single-queue assumption.
- Linux 6.6+ EEVDF (Zijlstra 2023): per-CPU runqueues with `vlag` preservation on migration. Per-rq virtual time diverges unboundedly (Li et al. 2016, Journal of Systems and Software).
- Ipanema (Lepers et al., EuroSys 2020): defined Concurrent Work Conservation (CWC), proved Linux CFS and FreeBSD ULE violate it. No production kernel formally guarantees CWC.
- seL4 MCS: scheduling contexts are core-bound, no kernel migration. Big kernel lock on SMP.
- Zircon: per-CPU queues with weight-based fair + EDF deadline. Lock-per-CPU, thread briefly on no queue during migration.
- BWoS (Wang et al., OSDI 2023): block-based work-stealing deque, formally verified on weak memory.
- Caladan (Fried et al., OSDI 2020): delegation-based scheduling, centralized decision/distributed execution.
- Apple Clutch: thread groups for workload-level scheduling.

### 6.3.1 Architecture: Per-Core Queues Inside Single Lock

**Decision:** Per-core ready queues within the existing `IrqMutex<State>` (Option A). The lock boundary does not change — the data organization does.

**Rationale:** At ≤8 cores, a single lock with ~100ns hold time produces worst-case ~700ns stall across all cores — negligible against a 4ms scheduling quantum. seL4 SMP chose the same approach (big kernel lock, formally verified CLH). The per-core queue organization enables core affinity, work stealing, and CWC without the complexity of fine-grained locking (lock ordering, races during block/wake transitions, threads briefly on no queue).

```rust
struct State {
    // Per-core ready queues (replaces single RunQueue)
    local_queues: [LocalRunQueue; MAX_CORES],
    cores: [PerCoreState; MAX_CORES],
    deferred_drops: [Vec<Box<Thread>>; MAX_CORES],

    // Global (unchanged)
    blocked: Vec<Box<Thread>>,
    suspended: Vec<Box<Thread>>,
    processes: Vec<Option<Process>>,
    scheduling_contexts: Vec<Option<SchedulingContextSlot>>,
    // ... identity management
}

struct LocalRunQueue {
    ready: Vec<Box<Thread>>,    // EEVDF selection within this queue
    load: u32,                  // runnable count (steal heuristic)
}
```

**What changed:** The single `RunQueue { ready: Vec<Box<Thread>> }` becomes `[LocalRunQueue; MAX_CORES]`. Each core's `schedule_inner` selects from its own `local_queues[core]`. The `deferred_ready` mechanism is eliminated — a preempted thread goes directly into its own core's local queue, which is safe because the single lock prevents any other core from observing it until lock release, at which point `restore_context_and_eret` has already switched the SP.

**What didn't change:** The global lock, the blocked list, processes, scheduling contexts, identity management. The `deferred_drops` mechanism remains (thread drop still can't happen on the thread's own stack).

### 6.3.2 EEVDF Virtual Lag Preservation

**Problem:** With per-core queues, each queue has its own virtual time domain (avg_vruntime). When a thread migrates (work steal, wake placement), its vruntime is relative to the source queue's virtual time. Placing it raw on the destination queue would destroy its fairness position.

**Solution:** Virtual lag (`vlag`) preservation, adapted from Linux's approach but integrated with scheduling contexts.

```text
vlag = weight × (avg_vruntime_source − vruntime) / DEFAULT_WEIGHT
```

Positive vlag = thread is underserved (eligible, should run soon). Negative = overserved. On the destination queue:

```text
vruntime_dest = avg_vruntime_dest − (vlag × DEFAULT_WEIGHT / weight)
eligible_at_dest = vruntime_dest   // fresh deadline on destination
```

This preserves the thread's relative fairness position: a thread owed 2ms of CPU on core 0 is still owed 2ms on core 3.

**Scheduling context budget is absolute, not relative.** Unlike vruntime (which is per-queue), a scheduling context's `remaining` budget is in nanoseconds — migration does not change it. A thread with 5ms remaining has 5ms regardless of which core it runs on. This is a property seL4 gets for free (core-bound SCs) that we preserve across actual migration.

### 6.3.3 Core Affinity and Placement

**Thread tracks `last_core`:** Set in `schedule_inner` when a thread is activated. Stored in `Scheduling { last_core: u32 }`.

**Placement on wake (blocked → ready):**

1. If `last_core` is idle → place there (cache affinity, O(1) check)
2. Else: find the core with lowest `load` count → place there (spread)
3. IPI the target core if `is_idle` (SGI 0, existing mechanism)

**Placement on steal (see §6.3.4):** Stolen thread's `last_core` is updated to the stealing core.

**No migration cost heuristic (yet):** Linux uses `sched_migration_cost` (500μs) to suppress migration of cache-hot tasks. On Apple Silicon and QEMU virt, L2 is shared across all cores — migration cost is L1-only (~25μs for 64KB working set). A migration cost heuristic is deferred until workloads demonstrate measurable cache penalties (§13.2).

### 6.3.4 Work Stealing

When `schedule_inner(core)` finds its local queue empty after EEVDF selection returns None:

1. **Scan all cores** (round-robin starting from `core + 1`, wrapping)
2. **Find busiest:** the core with the highest `load` count (ties broken by lowest core index)
3. **Select steal victim:** Among the busiest core's ready threads that have budget (scheduling context check), pick the thread with the **highest vlag** — the most underserved thread. This is the EEVDF-fair steal criterion: steal the thread that most deserves to run, giving it a core where it can actually run.
4. **Normalize:** Compute vlag on source queue, apply on destination queue (§6.3.2)
5. **Update:** Decrement source `load`, increment dest `load`, set thread's `last_core = core`

**Budget-aware stealing:** A thread whose scheduling context has exhausted its budget is not stolen. It would just sit in the destination queue until replenishment — wasting a steal and polluting the new core's virtual time.

### 6.3.5 Workload-Granularity Migration (Novel)

**Insight:** Threads sharing a scheduling context are usually a _workload_ — they communicate, share caches, and benefit from co-location. seL4 binds SCs to cores rigidly (no migration). Linux migrates individual threads (no workload grouping). Neither considers that a scheduling context represents a workload unit.

**Design:** When stealing, prefer to steal **all threads from the same scheduling context** on the victim core, up to the imbalance amount. This keeps workload threads co-located on the stealing core.

**Algorithm:**

1. Select the steal victim thread as in §6.3.4 (highest vlag with budget)
2. If the victim has a scheduling context, collect all other ready threads on the same core with the same `context_id`
3. Steal the group (up to half the victim core's load — never drain a core)
4. Normalize all stolen threads' vruntimes via vlag preservation

**Constraint:** Workload co-location is a preference, not a hard affinity. If a core has threads from context C and is idle while another core with context C threads is overloaded, stealing proceeds normally — CWC takes priority over co-location.

### 6.3.6 Concurrent Work Conservation (CWC)

**Definition (Ipanema, Lepers et al. 2020):** A scheduler satisfies CWC if: whenever a core is idle and at least one other core has more than one runnable thread, the idle core will acquire work within bounded time.

**Guarantee:** The work-stealing mechanism (§6.3.4) fires on every `schedule_inner` invocation for an idle core. Since `schedule_inner` runs at least on every timer tick (~4ms worst case) and on every IPI wakeup (immediate), an idle core will steal within one scheduling quantum of work becoming available. The IPI mechanism (§6.2) ensures that newly enqueued work on a non-idle core triggers a reschedule — if no idle core exists, the new thread waits in the target core's local queue until preemption.

**Formal property (tested, not proved):** Property-based tests encode:

```text
∀ state: if ∃ core_a (is_idle) ∧ ∃ core_b (load > 1) →
    after schedule_inner(core_a): core_a.load > 0 ∨ core_b.load ≤ 1
```

This is tested over 50+ seeds × 500+ steps with randomized spawn/block/wake/exit operations.

**What this kernel guarantees that others don't:** CWC + EEVDF + scheduling contexts. Linux violates CWC (Ipanema proved it). seL4 doesn't steal (userspace decides). Zircon's migration is best-effort. Our property-based tests verify CWC holds after every scheduling step.

### 6.3.7 `deferred_ready` Elimination

The `deferred_ready` mechanism (§12.1) existed to prevent the cross-core stack reuse race: with a global queue, pushing a preempted thread to `ready` immediately let another core pick it up while the originating core was still on the thread's kernel stack.

With per-core queues, a preempted thread goes into `local_queues[core].ready` — the _same_ core's queue. No other core accesses this queue during `schedule_inner` (single lock prevents it). By the time the lock drops and another core could steal the thread, `restore_context_and_eret` has already switched the SP. The race window is closed by the lock.

**`deferred_drops` remains:** An exited thread still can't be dropped during `schedule_inner` (we're on its stack). Same mechanism as before — push to `deferred_drops[core]`, drain next invocation.

### 6.3.8 Arch Interface Additions

One new arch operation:

| Operation                   | File                    | Purpose                                                                                                                                       |
| --------------------------- | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `send_reschedule_ipi(core)` | interrupt_controller.rs | SGI #1 (distinct from idle wakeup SGI #0). Used when a thread is enqueued on a non-idle core that should re-evaluate its scheduling decision. |

**Not needed (documented for completeness):**

- **I-cache flush on migration:** Not required. The kernel doesn't JIT or dynamically generate code. Userspace code is mapped via page tables with I-cache coherency maintained by the MMU. ARM64 hardware maintains I-cache coherency across cores for normal memory mappings.
- **L1 data cache flush on migration:** Not required. ARM64 L1 D-cache is coherent across cores via snooping (inner-shareable domain). DSB ISH ensures visibility.
- **TLB shootdown on migration:** Not required. TLB invalidation is already broadcast (ASIDE1IS). Thread keeps its ASID across migration.

### 6.3.9 Option B: Lock Decomposition (For Porters)

The current design uses a single `IrqMutex<State>` with per-core data organized inside it (Option A). This is correct and performant for ≤8 cores. A porter targeting server-class hardware (64+ cores) or NUMA topologies should migrate to fine-grained locking (Option B):

**Lock structure:**

```text
LOCAL[N]: IrqMutex<LocalState>    // per-core ready queue, current, idle
GLOBAL:   IrqMutex<GlobalState>   // blocked, suspended, processes, contexts
```

**Lock ordering:** `GLOBAL` before `LOCAL`. Never acquire `GLOBAL` while holding `LOCAL`.

**Key operation changes:**

- `schedule_inner`: `LOCAL[core]` only (fast path). `GLOBAL` not needed.
- `try_wake`: `GLOBAL` (find in blocked) → release → `LOCAL[target]` (enqueue). Thread is briefly on no queue between locks (Zircon does this).
- `block_current`: `GLOBAL` (add to blocked) → `LOCAL[core]` (schedule next). Ordering is safe (GLOBAL first).
- Work steal: `LOCAL[core]` → release → `LOCAL[victim]`. Never hold two LOCAL locks simultaneously.

**Migration path:** The per-core data organization is identical between Option A and B. The change is mechanical: split `State` into `LocalState` + `GlobalState`, replace lock acquisition sites. All scheduling logic, EEVDF selection, work stealing, vlag normalization, and CWC guarantees transfer unchanged.

**Why not now:** Fine-grained locking adds complexity (lock ordering constraints, brief no-queue windows) without measurable benefit at ≤8 cores. The seL4 team made the same decision and maintains it even in their verified SMP kernel.

**Depends on:** 6.1 (EEVDF algorithm), 6.2 (IPI wakeup), 0.10 (IrqMutex), 0.11 (SMP boot).

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

**Implementation:** `syscall.rs` (sys_wait), `scheduler.rs` (try_wake_for_handle, set_wake_pending_for_handle, push_wait_entry, clear_wait_state, updated block_current_unless_woken), `channel.rs` (signal uses try_wake_for_handle), `thread.rs` (WaitEntry, complete_wait_for, wake_pending/wake_result fields).

**Prior art:** Linux epoll, FreeBSD kqueue, Fuchsia `zx_object_wait_many`, seL4 notification objects.

---

## 8.3 Device Handles & Interrupt Forwarding

**Goal:** Support userspace drivers. The kernel provides hardware access through handles; drivers run at EL0.

**Approach:**

- **Device discovery:** Parse DTB (device tree blob) at boot to enumerate devices. **Done** (§8.6). DTB parser discovers ~40 devices; GIC and virtio-mmio addresses initialized from DTB data.
- **MMIO mapping:** `device_map(pa, size)` syscall (#16) maps a device's MMIO region into the calling process's address space with Device-nGnRE memory attributes (MAIR index 1). Returns the user VA. VA is bump-allocated from a dedicated region (`DEVICE_MMIO_BASE` at 512 MiB, up to `DEVICE_MMIO_END` at 1 GiB). Validates that the PA is outside RAM (device space only). Zero overhead for register access — the driver reads/writes device memory directly.
- **Interrupt forwarding:** `interrupt_register(irq)` syscall (#14) enables the IRQ in the GIC and returns a waitable handle (`HandleObject::Interrupt`). When the IRQ fires, the kernel's IRQ handler masks it at the GIC distributor, marks the handle pending, and wakes the driver thread. `interrupt_ack(handle)` syscall (#15) clears pending and re-enables the IRQ. One context switch per interrupt.
- **DMA buffers:** ~~`dma_alloc`/`dma_free` syscalls removed~~ — replaced by VMOs. The `sys` library provides the same `dma_alloc`/`dma_free` API signatures backed by `vmo_create(CONTIGUOUS)` + `vmo_map` + `vmo_op_range(LOOKUP)` for PA retrieval. Per-process tracking via VMO handle table; cleanup on process exit via VMO Drop.

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

**Device init:** DTB wired into device initialization. GIC bases from `"arm,cortex-a15-gic"`, virtio-mmio from `"virtio,mmio"` entries. Falls back to hardcoded QEMU `virt` defaults (GIC 0x0800_0000, UART 0x0900_0000, virtio 0x0A00_0000+) if no DTB.

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

### 9.1 DMA Buffer Allocation (Phase 1) — Superseded by VMOs

**Original goal:** Expose physically contiguous allocation to userspace for virtio drivers.

**Superseded:** `dma_alloc`/`dma_free` kernel syscalls removed. The `sys` library provides the same API backed by `vmo_create(CONTIGUOUS)` + `vmo_map` + `vmo_op_range(LOOKUP)`. DMA buffers are now VMOs — they get all VMO features (handle-based lifecycle, cross-process sharing, Drop cleanup) for free.

---

### 9.2 Process Struct Extraction (Phase 2a) — Done

**Goal:** Introduce a `Process` kernel object that owns the address space and handle table. Foundation for multi-threaded processes, process creation from userspace, and process handles.

**Approach:** `Process { id, address_space, handles, threads }`. Threads hold `process_id: Option<ProcessId>`. Syscall handlers resolve process via thread's `process_id`. Global process table (fixed-size array under `IrqMutex`, same pattern as interrupt/timer tables).

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

| Nr  | Syscall          | Args                                                     | Returns                     |
| --- | ---------------- | -------------------------------------------------------- | --------------------------- |
| 5   | channel_create   | —                                                        | handle_a \| (handle_b << 8) |
| 22  | handle_send      | x0=target_proc_handle, x1=handle_to_send, x2=rights_mask | 0                           |
| 28  | handle_set_badge | x0=handle, x1=badge                                      | 0                           |
| 29  | handle_get_badge | x0=handle                                                | badge                       |

`handle_send` moves a handle from the caller's table into the target process's table with optional rights attenuation. The target handle receives `source_rights & rights_mask` (rights can only be removed, never added). `rights_mask=0` means preserve all rights from the source. The handle's badge is preserved through the transfer. Only works on suspended processes (Process.started == false). For Channel handles, also maps the shared page into the target's address space.

`handle_set_badge` / `handle_get_badge`: each handle carries an opaque u64 badge (default 0). Init sets badges on handles before sending them to services, so services can identify which client a handle was assigned to. Badges are preserved through `handle_send` and survive rights attenuation.

`channel_create` allocates a new channel (shared page + two endpoints), maps the shared page into the caller, and inserts both endpoint handles. Returns packed `handle_a | (handle_b << 16)`.

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

**Approach:** Each driver becomes a separate ELF binary (now in `system/services/drivers/`). At boot, kernel probes virtio-mmio slots (minimal MMIO reads for magic/version/device_id), spawns the appropriate driver process, writes device info (MMIO PA, IRQ) to a channel shared page, and starts the driver. Each driver: `device_map` for MMIO, `dma_alloc` (now VMO-backed) for virtqueue buffers. In-kernel `virtio/` module removed entirely. Shared `virtio` rlib (in `system/libraries/`) provides userspace virtio transport and split virtqueue implementation.

**Implementation notes:**

- Kernel retains minimal probe logic inline in `main.rs` (~80 lines) for device discovery. Drivers handle all device initialization (negotiate, queue setup, I/O).
- Sub-page MMIO alignment: QEMU virt's virtio-mmio slots have 0x200 stride within 4K pages. Drivers page-align the PA for `device_map` and add the sub-page offset to the returned VA.
- Channel shared page mapped at fixed `CHANNEL_SHM_BASE` in each driver's address space (bypasses channel-index-derived VA) so drivers read device info from a known address.
- Console driver not exercised yet (QEMU virt doesn't add a virtio-console by default; only blk device present).
- Drivers use polling (spin-loop) for completion, matching the previous in-kernel behavior. Interrupt-driven I/O is a straightforward enhancement via `interrupt_register` + `wait` + `interrupt_ack`.

**Validation:** `cargo run --release` boots, virtio-blk driver reads sector 0 and prints "HELLO VIRTIO BLK" — same functionality as the former in-kernel driver, entirely through syscalls (`device_map`, `vmo_create`, `vmo_map`, `write`, `channel_signal`, `exit`). Init/echo IPC unaffected.

**Depends on:** Phases 1 (DMA), 3 (process create), 4 (handle transfer).

---

### 9.8 Memory Sharing (Phase 7) — Superseded by VMOs

**Original goal:** Allow processes to share physical memory for the display pipeline.

**Superseded:** `memory_share` kernel syscall removed. The `sys` library provides the same API backed by cross-process `vmo_map(handle, flags, target_process)`. Init creates a contiguous VMO, maps it locally, then maps it into target processes via VMO cross-process mapping. Handle-based sharing replaces PA-based sharing.

**Per-process channel SHM bump allocator:** (Unchanged.) Each process tracks `next_channel_shm_va`. Channel shared pages are mapped at sequential VAs starting from `CHANNEL_SHM_BASE`.

---

### 9.8.1 Userspace Heap Allocation ✅

**Goal:** Allow userspace processes to dynamically allocate anonymous memory. Foundation for `GlobalAlloc` (unlocks `Vec`, `String`, `Box`) and any non-trivial data structure.

**Syscalls:**

| Nr  | Syscall      | Args                 | Returns |
| --- | ------------ | -------------------- | ------- |
| 13  | memory_alloc | x0=page_count        | user VA |
| 14  | memory_free  | x0=va, x1=page_count | 0       |

`memory_alloc` creates an anonymous VMA and bump-allocates VA from the heap region. Pages are NOT eagerly mapped — they are demand-paged on first touch via the existing fault handler (anonymous backing, zero-filled). `memory_free` removes the VMA, walks the page table to find and free any demand-paged physical frames, and invalidates TLB.

**VA region:** `HEAP_BASE` (16 MiB) to `HEAP_END` (256 MiB). Bump-allocated per process (no VA reclamation on free). 240 MiB of heap VA space.

**Per-process budget:** `DEFAULT_HEAP_PAGE_LIMIT` = 8192 pages (32 MiB physical). Prevents a single process from exhausting RAM.

**Why demand paging (not eager):** Unlike DMA buffers (which need PAs for device programming), heap pages have no reason to be physically allocated before use. Demand paging means a process that allocates a 1 MiB buffer but only touches the first page only costs one physical frame. Reuses the existing `handle_fault` → anonymous VMA → zero-fill path.

**Process exit cleanup:** Demand-paged heap frames are tracked in `owned_frames` (via `map_page` in the fault handler). `free_all()` frees all of them. `heap_allocations` Vec is cleared separately.

**Implementation:** `paging.rs` (HEAP_BASE/END), `memory_region.rs` (VmaList::remove), `address_space.rs` (HeapAllocation, next_heap_va, map_heap, unmap_heap, read_and_unmap_page), `syscall.rs` (sys_memory_alloc #25, sys_memory_free #26), `sys` library (memory_alloc, memory_free wrappers).

**Depends on:** Nothing. Builds on existing demand paging infrastructure.

---

### 9.9 Filesystem COW Kernel Mechanics (Phase 8)

**Goal:** Kernel-level copy-on-write for memory-mapped documents. Editor writes trigger page faults, kernel allocates new pages, filesystem manages on-disk snapshots.

**Blocked on filesystem on-disk design.** Research complete (`design/research/cow-filesystems.md`). Requires settling the last sub-decision of Decision #16.

---

### Syscall Number Map (complete)

Grouped by abstraction layer, dense 0–36. Pre-v1.0: renumber freely on add/remove. At v1.0: freeze.

| Nr  | Syscall                    | Group          |
| --- | -------------------------- | -------------- |
| 0   | exit                       | Runtime        |
| 1   | write                      | Runtime        |
| 2   | yield                      | Runtime        |
| 3   | handle_close               | Capability     |
| 4   | handle_send                | Capability     |
| 5   | handle_set_badge           | Capability     |
| 6   | handle_get_badge           | Capability     |
| 7   | channel_create             | IPC            |
| 8   | channel_signal             | IPC            |
| 9   | wait                       | Event loop     |
| 10  | futex_wait                 | Sync           |
| 11  | futex_wake                 | Sync           |
| 12  | timer_create               | Time           |
| 13  | memory_alloc               | Heap           |
| 14  | memory_free                | Heap           |
| 15  | vmo_create                 | VMO            |
| 16  | vmo_map                    | VMO            |
| 17  | vmo_unmap                  | VMO            |
| 18  | vmo_read                   | VMO            |
| 19  | vmo_write                  | VMO            |
| 20  | vmo_get_info               | VMO            |
| 21  | vmo_snapshot               | VMO            |
| 22  | vmo_restore                | VMO            |
| 23  | vmo_seal                   | VMO            |
| 24  | vmo_op_range               | VMO            |
| 25  | process_create             | Process/thread |
| 26  | process_start              | Process/thread |
| 27  | process_kill               | Process/thread |
| 28  | process_set_syscall_filter | Process/thread |
| 29  | thread_create              | Process/thread |
| 30  | scheduling_context_create  | Scheduling     |
| 31  | scheduling_context_borrow  | Scheduling     |
| 32  | scheduling_context_return  | Scheduling     |
| 33  | scheduling_context_bind    | Scheduling     |
| 34  | device_map                 | Device         |
| 35  | interrupt_register         | Device         |
| 36  | interrupt_ack              | Device         |

`dma_alloc`, `dma_free`, and `memory_share` have been removed from the kernel. The `sys` library provides the same API signatures backed by VMOs internally (contiguous VMO + cross-process `vmo_map` + `vmo_op_range(LOOKUP)` for PA retrieval). Syscall filter mask widened from u32 to u64 to cover the full table.

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

**Result:** New `waitable.rs` (~97 lines). Net: ~100 lines removed. 20 host tests in `test/tests/kernel_waitable.rs`.

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

**Fix:** Create `test/tests/ipc_channel.rs`. Test: encoding/decoding (`channel_index`, `endpoint_index`), signal/pending flag logic, close_endpoint refcounting, double-close behavior.

**Scope:** New test file. ~100–150 lines.

---

### 11.11 Test coverage gap: `futex.rs` has no host tests

**Severity:** High.

**Bug:** The futex hash function, bucket lookup, and PA-keyed wait/wake logic are pure algorithms. The cross-process PA-keyed synchronization semantics are a subtle invariant not validated anywhere.

**Fix:** Create `test/tests/kernel_futex.rs`. Test: bucket index computation, hash distribution, registration/deregistration.

**Scope:** New test file. ~80–100 lines.

---

### 11.12 Test drift risk: `slab.rs` and `asid.rs` tests duplicate kernel logic

**Severity:** High.

**Bug:** Both test files reimplement the kernel algorithm instead of using `#[path = "…"] mod`. Changes to the real module won't be caught by tests. All 11 other test files use the include pattern.

**Fix:** Refactor to `#[path = "…"] mod slab;` and `#[path = "…"] mod address_space_id;` with the same stubs used by other tests.

**Scope:** `test/tests/mem_slab.rs`, `test/tests/mem_asid.rs`. ~40 lines each.

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

**Resolution:** Superseded by `system_config.rs` SSOT (2026-03-25). All crates now `include!(env!("SYSTEM_CONFIG"))` — PAGE_SIZE is defined once, consumed everywhere via build.rs env var plumbing.

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

---

## 12.0 Known Issues

### 12.1 Ready Queue Reuse Race (SMP)

**Status: FIXED (2026-04-01).** Root cause identified 2026-03-31. Fix: per-core `deferred_ready` list, same pattern as `deferred_drops`.

**Bug:** When `schedule_inner` moves the old thread to the ready queue (via `park_old`), another core can immediately pick it up and restore it — while the originating core is still executing on the old thread's kernel stack. The originating core hasn't yet reached `restore_context_and_eret` (which switches SP to the new thread's stack). If the other core restores the thread and user code does an SVC, `svc_handler` runs on the same kernel stack as the originating core's `irq_handler` — stack corruption.

**Evidence:** Stack canary `0xCAFEBABE12400001` (irq_handler) overwritten with `0xCAFEBABE5BC00002` (svc_handler). Two exception handlers on the same stack simultaneously. Crash logs in `system/test/crashes/`.

**This is the same fundamental issue as the deferred_drops cross-core free (fixed in d00d92b), but applied to thread READY transitions instead of thread EXIT transitions.** Both stem from the window between scheduler lock drop and SP switch in `restore_context_and_eret`.

**Fix:** Per-core `deferred_ready[MAX_CORES]` list in `State`. `park_old` pushes preempted (Ready, non-idle) threads to `deferred_ready[core]` instead of the global ready queue. At the start of the NEXT `schedule_inner` on the same core, `deferred_ready[core]` is drained into the global ready queue — by which time the core has switched to a different stack via `restore_context_and_eret`. Only `park_old` needs deferral; `try_wake_impl`, `spawn_user`, and `start_suspended_threads` add threads that are not the caller's current stack. Trade-off: a preempted thread is invisible to other cores for one scheduling tick (sub-millisecond delay).

**Defense-in-depth mitigations (retained):**

- Stack canaries in `irq_handler` and `svc_handler` (detect corruption, produce diagnosable crash instead of mysterious instruction abort)
- `PANIC_GATE` atomic CAS for serialized multi-core panic output
- Context SP validation before `restore_context_and_eret` (catch zeroed/user-range SP)
- DSB SY before GICD ISENABLER (handler table globally visible before IRQ enabled)
- Per-core `deferred_drops` (prevents the EXIT variant of this race)
- 20 property-based SMP scheduler model tests (6 new for `deferred_ready`)
- Thread churn stress workers (50k create/exit cycles under SMP load)

---

## 13.0 Forward-Looking Concerns

Architectural concerns that don't affect current milestones but will need kernel-level design decisions before the relevant milestone begins. Extracted from a 2026-04-01 deep research review — most of the review's findings were wrong (assumed monolithic kernel, graphics in kernel, flat address space), but these two survived as genuine future concerns.

### 13.1 Security Model Before Network (v0.11)

**Context:** All userspace processes are currently trusted equally. No capability enforcement, no per-channel access control beyond what init happens to wire up. Correct for a personal project running only its own code.

**When it breaks:** v0.11 (network) and v0.12 (web browser as translator) introduce untrusted content. A compromised decoder or network service has the same kernel-level access as the document service.

**Design space:**

- **Capability-based** (seL4, Fuchsia): Processes hold unforgeable tokens granting access to specific resources. Handle-based access (§0.8) is already halfway there — handles are per-process, unforgeable, typed. The gap is that init creates all channels and hands out handles by convention, not by enforcement. A capability model would make handle grants explicit and auditable.
- **Sandboxed by default:** All services start with zero capabilities; init grants exactly what's needed. The decoder sandbox pattern (PNG decoder: RO file store, RW content region, nothing else) already works this way _informally_. Formalizing it means the kernel rejects syscalls on handles a process doesn't hold.
- **Per-document permissions:** Documents carry access metadata; the document service enforces read/write/execute. Orthogonal to process capabilities.

**Kernel implications:** Handle creation, channel setup, and VMO cross-process mapping would need to carry and enforce capability metadata. This is a deep interface change — easier to design in before v0.11 than retrofit after.

**When to resolve:** Before v0.11 planning. Candidate for design decision #18 during v0.7's design decisions milestone.

### 13.2 EEVDF Tuning for Interactive + Media Workloads (v0.6–v0.9)

**Context:** The EEVDF scheduler (§6.1) is correct — property-based tests, SMP stress testing, 2,313+ tests. But its tuning parameters (slice length, virtual time decay for sleeping tasks, lag bounds) have only been validated under current workloads: text editing, layout computation, rendering.

**What changes:** v0.6 (media) adds audio/video decoding with real-time deadlines. v0.9 (realtime/streaming) adds latency-sensitive concurrent flows.

**Specific questions:**

1. Does a CPU-bound layout recompute starve an audio decoder's deadline? EEVDF is fair, not priority-aware — a thread that uses its full slice delays all others by one slice.
2. Do sleeping tasks (idle editor) accumulate negative lag that causes latency spikes when they wake? Linux's EEVDF caps lag to prevent this, but the cap value matters.
3. Is the current slice length appropriate for media-sensitive workloads, or should it be content-class-aware (shorter slices for audio decoders)?

**Kernel implications:** If the answer to (1) or (3) is "no," the syscall interface may need priority hints or deadline annotations (e.g., `thread_set_deadline(period, runtime)` à la SCHED_DEADLINE). This is a syscall ABI addition — easier to add before services depend on default behavior.

**When to resolve:** v0.6 should include scheduler stress tests with media-like workload patterns (periodic short bursts at fixed intervals). Priority/deadline syscall design before v0.9.

---

## 14. Platform Assumptions & Porting Notes

The kernel currently targets two platforms: QEMU `virt` machine and a custom macOS ARM64 hypervisor. This section documents every platform-specific assumption, so a porter knows exactly what to change for real hardware (Raspberry Pi, ARM server, custom SoC, etc.).

### 14.1 Hardcoded Physical Addresses

These constants assume the QEMU `virt` memory map. Real hardware has different addresses.

| Constant        | Value        | File                     | What it is                                | Porting                                                             |
| --------------- | ------------ | ------------------------ | ----------------------------------------- | ------------------------------------------------------------------- |
| RAM_START       | 0x4000_0000  | boot.S, system_config.rs | Physical RAM base                         | DTB `/memory` node; RPi=0x0, server=0x8000_0000                     |
| RAM_END         | 0x5000_0000  | boot.S                   | Identity map ceiling (256 MiB)            | DTB `/memory` size; increase for >256 MiB                           |
| PHYS_BASE       | 0x4008_0000  | link.ld.in               | Kernel load address (RAM_START + 512 KiB) | Adjust with RAM_START                                               |
| UART0_PA        | 0x0900_0000  | serial.rs                | PL011 console                             | DTB serial/UART node                                                |
| DEFAULT_GICD_PA | 0x0800_0000  | interrupt_controller.rs  | GIC distributor                           | DTB `interrupt-controller` node                                     |
| DEFAULT_GICR_PA | 0x080A_0000  | interrupt_controller.rs  | GIC redistributor                         | DTB `interrupt-controller` node                                     |
| pvpanic         | 0x0902_0000  | exception.S, main.rs     | QEMU panic device                         | Not present on real hardware; skip if DTB lacks `qemu,pvpanic-mmio` |
| Virtio MMIO     | 0x0A00_0000  | main.rs                  | 32 virtio slots, stride 0x200             | DTB `virtio,mmio` nodes; real hardware may not have virtio          |
| Device L2       | indices 4, 5 | boot.S fill_table        | Boot page table device blocks             | Derived from device PAs; adjust if MMIO range differs               |

### 14.2 Memory & Page Table Layout

| Assumption              | Value                   | File                                 | Porting                                                                                                     |
| ----------------------- | ----------------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------- |
| Page granule            | 16 KiB                  | system_config.rs, boot.S, link.ld.in | Some SoCs prefer 4 KiB; requires coordinated changes to page tables, linker script, and all PAGE_SIZE users |
| T0SZ / T1SZ             | 28 (36-bit VA = 64 GiB) | boot.S                               | Sufficient for ≤64 GiB RAM; adjust for larger systems                                                       |
| KERNEL_VA_OFFSET        | 0xFFFF_FFF0_0000_0000   | system_config.rs                     | Derived from T1SZ=28; changes if T1SZ changes                                                               |
| L2 block size           | 32 MiB                  | boot.S                               | Consequence of 16 KiB granule; 4 KiB granule → 2 MiB blocks                                                 |
| KASLR slide granularity | 32 MiB                  | boot.S                               | Must match L2 block size                                                                                    |

### 14.3 Boot Protocol

| Assumption                  | File              | Porting                                                                                                                                |
| --------------------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Entry at EL2 (optional)     | boot.S            | Code detects EL via `CurrentEL` and handles EL1 or EL2. Works on both firmware (EL2) and bootloader (EL1) entry.                       |
| DTB PA in x0                | boot.S, main.rs   | AArch64 boot protocol standard. Fallback: scan first 256 KiB of RAM for FDT magic. Real bootloaders (U-Boot, UEFI) pass DTB correctly. |
| PSCI for SMP                | main.rs, power.rs | HVC #0 (assumes EL2 or KVM). Bare-metal with ATF uses SMC instead. `power.rs` already abstracts the conduit — change HVC→SMC.          |
| ICC_SRE_EL2 for GICv3       | boot.S            | Configures system register interface at EL2. Safe to skip at EL1 (firmware already configured).                                        |
| Core parking via MPIDR[7:0] | boot.S            | Assumes flat affinity (core_id = MPIDR Aff0). Hierarchical affinity (clusters) requires `Aff1:Aff0` decoding.                          |

### 14.4 Interrupt Controller

| Assumption                 | File                    | Porting                                                                                                                                                          |
| -------------------------- | ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| GICv3                      | interrupt_controller.rs | GICv2 has different register layout (no redistributors, 1-byte ITARGETSR instead of 8-byte IROUTER). Needs a separate driver or version-dispatching abstraction. |
| GICR stride = 128 KiB      | interrupt_controller.rs | GICv3 standard. GICv4 may differ; query DTB `#redistributor-regions`.                                                                                            |
| Flat affinity for IPI      | interrupt_controller.rs | `ICC_SGI1R_EL1` target list assumes Aff3=Aff2=Aff1=0. Multi-cluster SoCs need hierarchical affinity routing.                                                     |
| Virtual timer PPI = IRQ 27 | timer.rs                | ARM architectural standard for CNTV. Universal across all ARMv8.                                                                                                 |

### 14.5 Timer & Entropy

| Assumption                       | File       | Porting                                                                                                                                                                                      |
| -------------------------------- | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| CNTVCT_EL0 for KASLR entropy     | boot.S     | Generic timer present on all ARMv8. Entropy quality depends on boot-time counter value (host uptime on hypervisor, time-since-reset on bare metal). Acceptable for KASLR; not cryptographic. |
| RNDR (FEAT_RNG) for PRNG seeding | entropy.rs | Probed at runtime via `ID_AA64ISAR0_EL1[63:60]`. Falls back to jitter entropy if absent. Apple M1+ and ARMv8.5+ support RNDR.                                                                |
| Jitter entropy from CNTVCT       | entropy.rs | Works on all CPUs. Quality varies: higher on bare metal (real cache effects), lower on VMs (deterministic scheduling).                                                                       |
| CNTFRQ varies                    | timer.rs   | Read dynamically via `mrs cntfrq_el0`. 24 MHz (Apple Silicon), 62.5 MHz (QEMU), 50–125 MHz (server SoCs). No hardcoded assumption.                                                           |

### 14.6 KASLR

| Assumption                             | File              | Porting                                                                                                                                      |
| -------------------------------------- | ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| PIE binary (--pie -z notext)           | build.rs          | Linker generates allocated R_AARCH64_RELATIVE entries. Self-contained: works on any ELF loader.                                              |
| 8-bit entropy (256 × 32 MiB positions) | boot.S            | Bits [32:25] of CNTVCT_EL0. On bare metal with short uptime, lower bits may be predictable. Mix with RNDR if available for stronger entropy. |
| All TTBR1 entries shift uniformly      | boot.S fill_table | Device + RAM L2 entries shift by the same offset. Ensures `phys_to_virt` works uniformly.                                                    |
| RELATIVE addend check                  | boot.S            | Physical address addends (< KERNEL_VA_OFFSET) are not slid. Kernel VA addends (≥ KERNEL_VA_OFFSET) are slid.                                 |

### 14.7 Hypervisor-Specific Code (Remove for Bare Metal)

| Feature                      | Files                | What to do                                                                                                                |
| ---------------------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| pvpanic                      | exception.S, main.rs | DTB-conditional. Already skipped gracefully if `qemu,pvpanic-mmio` absent. No changes needed.                             |
| HVF MMIO workarounds         | memory_mapped_io.rs  | Avoids writeback/pair addressing modes. Harmless on real hardware (generates valid but slightly slower MMIO).             |
| Virtual timer (CNTV vs CNTP) | timer.rs             | Uses CNTV (virtual timer) due to HVF restrictions. CNTV works on bare metal too (CNTVOFF=0 by default). No change needed. |
| DTB RAM scan                 | main.rs              | Fallback for hypervisors that don't pass DTB in x0. Harmless — real bootloaders pass DTB correctly, scan never triggers.  |

### 14.8 Migration Checklist

For a new platform, in order:

1. **DTB required.** Ensure the bootloader passes a valid DTB in x0 (or place one in the first 256 KiB of RAM).
2. **Adjust RAM_START** in `system_config.rs` and `boot.S` to match the platform's physical memory base.
3. **Adjust PHYS_BASE** in `link.ld.in` (RAM_START + 512 KiB).
4. **Verify UART.** Serial output is the first sign of life. If the platform UART isn't PL011 at 0x0900_0000, update `serial.rs` UART0_PA. Long-term: probe from DTB.
5. **Verify GIC version.** If GICv2 (e.g., Raspberry Pi 4 uses a BCM interrupt controller, not GIC), a new driver is needed. If GICv3, DTB discovery already works.
6. **PSCI conduit.** Change HVC→SMC in `power.rs` if running at EL1 with ARM Trusted Firmware.
7. **Page granule.** If the platform requires 4 KiB pages instead of 16 KiB, this is a significant change: `system_config.rs`, `boot.S` (TCR, block sizes), `link.ld.in`, and all code that uses PAGE_SIZE.
8. **Test entropy.** Boot twice, verify different KASLR slides in serial output. If slides are identical, the timer counter isn't providing entropy — add RNDR or another source to boot.S.
9. **Remove nothing.** pvpanic, DTB scan fallback, and HVF MMIO workarounds are all harmless on real hardware. Don't remove code that works everywhere.

---

## 15.0 Virtual Memory Objects (VMOs)

**Goal:** A single memory abstraction for all shared memory. VMO subsumes `memory_share` (syscall #24), `dma_alloc`/`dma_free` (syscalls #17/#18). When you build a new way, kill the old way — no parallel memory abstractions.

**Design principles:**

1. **VMO is THE memory object.** One abstraction for all shared memory. Channels carry messages; VMOs carry data.
2. **Capability-native.** VMO handles participate fully in the rights system: attenuation (READ, WRITE, MAP, APPEND, SEAL), transfer via `handle_send`, badges. Same model as channels, timers, interrupts.
3. **Ownership-typed (Theseus-inspired).** The kernel-internal `Vmo` type uses Rust ownership to prevent use-after-free and double-unmap at compile time. `Drop` unmaps all mappings and frees all pages. No manual `freed` flag. No other microkernel can do this — it's a Rust-specific advantage.
4. **Designed for the general case.** Not "what our OS needs" — "what any consumer of this microkernel needs." Decisions evaluated against the full landscape of microkernel use cases.

### 15.1 Six Settled Decisions

**1. Size: Fixed at creation.** Resize is architecturally wrong in a capability system. Zircon added `ZX_VMO_RESIZABLE` and immediately needed `ZX_VM_ALLOW_FAULTS` and `ZX_VM_REQUIRE_NON_RESIZABLE` — defensive flags that exist solely because resize creates a class of bugs where one process shrinks a VMO mapped in another, causing unexpected faults. seL4, NOVA, L4Re — the kernels optimizing for correctness — all chose fixed.

**2. Backing: Lazy by default (demand-paged, zero-fill on fault).** Pages allocated on first touch, not at creation. A database allocating 1 GiB shouldn't pay for pages it hasn't touched. Explicit commit available via `vmo_op_range(COMMIT, offset, len)` for processes that need deterministic allocation (no faults on hot path).

**3. Contiguity: Non-contiguous default. `VMO_CONTIGUOUS` flag for DMA.** Contiguous VMOs use the buddy allocator for 2^n contiguous frames (eager allocation — contiguity requires all pages allocated together). Restrictions: cannot snapshot (COW copy wouldn't be contiguous), always eager, always pinned. Every OS special-cases contiguous allocation (Zircon `zx_vmo_create_contiguous`, Linux CMA, QNX `MAP_PHYS`).

**4. VA placement: Kernel-picks.** `vmo_map` maps into the process's shared memory region. Kernel picks the next available VA using the existing `VmaList` (sorted list, gap search). Optional `VMO_MAP_FIXED` for specific-VA mapping (fails on overlap, never silently replaces). VMAR extension point documented for future (see §15.4).

**5. Channels remain separate.** Channels and VMOs are distinct IPC primitives: channels are message pipes (ordered, small), VMOs are shared memory (unordered, large). Every microkernel keeps them separate (Zircon channels + VMOs, seL4 endpoints + frames, L4 IPC + dataspaces).

**6. Capability-native.** VMO handles participate fully in the capability system (§0.8). Rights attenuation, transfer, badges — all work.

### 15.2 Four Novel Features (Beyond Existing Microkernels)

**N1. Built-in generation numbers (versioned memory).** Every VMO has a generation counter (u64). `vmo_snapshot()` increments the generation and COW-forks the page list. `vmo_restore(generation)` reverts to a previous snapshot. Bounded snapshot ring (configurable depth, default 64). No other production microkernel offers versioned memory objects. COW is typically a filesystem concern (ZFS, Btrfs) or process-fork mechanism. Making COW a VMO primitive means any consumer gets point-in-time snapshots, undo, and concurrent-read-while-write for free.

Implementation: per-page reference counting (refcount stored alongside Pa in the page list). Write to a page with refcount > 1 triggers COW (allocate new page, copy, update current generation's page list, decrement old page's refcount). Snapshot ring eviction: when the ring wraps, walk the oldest snapshot's page list decrementing refcounts and freeing pages that hit zero.

Interactions: sealed VMOs reject `snapshot` and `restore` (content frozen, existing snapshots remain readable). Contiguous VMOs cannot snapshot (COW would break contiguity). Append-only VMOs snapshot normally (captures the append frontier).

**N2. Append-only permission.** New right: APPEND (bit 8). A handle with APPEND but not WRITE can write at `offset >= committed_size` but cannot overwrite existing data. Enforced in `vmo_write` syscall. Use cases: log-structured stores, audit trails, append-only document history. The document service can hand an APPEND-only VMO to an editor — the editor can add content but never modify or delete previous entries.

**N3. Seal (immutable freeze).** `vmo_seal()` permanently freezes the VMO's content, permissions, and metadata. Irreversible. All subsequent mutating operations return `PermissionDenied`. Existing snapshots survive. Mappings remain valid (read-only — writable PTEs remapped as read-only on seal). Use case: init creates a VMO with font data, seals it, sends READ+MAP handles to services. Services know by construction that the content will never change — no TOCTOU, no races, tamper-proof. Linux has `memfd_create(MFD_ALLOW_SEALING)` + `fcntl(F_ADD_SEALS)` for the same reason (Android uses it to replace ashmem). Seal requires the SEAL right. Once sealed, the SEAL right is consumed (monotonic — can't unseal, can't re-seal).

**N4. Content-type tag.** Each VMO carries an optional `type_tag: u64` set at creation. The kernel doesn't interpret it — opaque metadata. Distinct from badges (which identify the sender). When VMO handles travel via IPC, the receiver checks `vmo_get_info().type_tag` to verify content type matches expectations. Catches version mismatches, corrupted handles, and protocol errors without a side-channel. Inspired by RedLeaf's `RRef<T>` (OSDI '20), reduced to minimum viable form. Type tag is immutable after creation.

**Key insight:** No existing microkernel combines all four novel features (ownership-typed, versioned, permission-rich, content-tagged). Each exists in isolation in research systems. This kernel composes them into a single coherent abstraction.

### 15.3 Page Commitment Tracking

Per-VMO page list: `BTreeMap<u64, (Pa, u32)>` where key = page offset, Pa = physical address, u32 = reference count (for COW snapshot sharing).

- **Uncommitted page:** absent from BTreeMap. Zero-fill on fault (or return zeros for `vmo_read` without allocating).
- **Committed page, refcount=1:** exclusively owned by current generation. Writes go directly to the page.
- **Committed page, refcount>1:** shared between current generation and N snapshots. Write triggers COW: allocate new page, copy content, insert at refcount=1, decrement old page's refcount.
- **Contiguous VMO:** BTreeMap pre-populated at creation with all pages at refcount=1. No faulting.

**Why BTreeMap (not global hash table, not PTE-is-truth):** VMO must be self-contained because it can be mapped into multiple processes. Its page state can't live in any one process's page tables. BTreeMap gives O(log n) lookup, sparse storage (uncommitted ranges cost nothing), and iteration for COW snapshots. Matches Zircon's `VmPageList` architecture. Mach's global hash table creates lock contention under SMP.

### 15.4 VMAR Extension Point (Future)

**What's missing:** `vmo_map` maps into a single flat shared region per process. Any process that can call `vmo_map` can map anywhere in that region. There's no way to confine a component to a sub-region of the VA space.

**What VMARs would add:** `Vmar` kernel object (handle-based, capability-controlled). Every process gets a root VMAR at creation. `vmar_allocate(parent, size, flags)` carves sub-regions. `vmar_map(vmar, vmo, ...)` maps into a specific VMAR. Composable sandboxing — hand a library or plugin a sub-VMAR, and it can only map VMOs within its designated region. The VA-space equivalent of capability confinement. Zircon uses this for component framework isolation.

**Compatibility:** `vmo_map(handle, flags)` continues to work — it maps into the root VMAR. When VMARs arrive, `vmo_map` becomes sugar for `vmar_map(root_vmar, ...)`. No API break.

**Why deferred:** Pure additive change. The VMO API works without VMARs. ~400-600 lines for a feature no consumer needs until in-process sandboxing.

**Research references:** Zircon VMOs (zx_vmo_create, zx_vmar_map), seL4 Untyped/frames, Mach/XNU vm_object, QNX shm_ctl, L4Re dataspaces, Linux mmap/memfd, Redox schemes. Research: Theseus OS (ownership-typed MappedPages — OSDI '20), Twizzler (object-relative pointers — USENIX ATC '20), RedLeaf (RRef<T> typed cross-domain memory — OSDI '20), TreeSLS (capability tree checkpointing — SOSP '23), Asterinas (framework/service safety split — USENIX ATC '25).

---

## 16.0 Pager Interface

**Goal:** VMO-level pagers — a channel attached to a VMO that receives page fault notifications. When an uncommitted page is accessed, the kernel forwards the fault to the pager instead of zero-filling. The pager resolves the fault (reads from disk, decompresses, generates content), commits the page, and tells the kernel to wake blocked threads.

**Design decision: Pager as VMO attribute (Zircon-inspired, not seL4's thread-level model).** Different VMOs can have different pagers — the document service pages document VMOs, the filesystem service pages file VMOs. seL4 attaches fault endpoints to threads, which conflates "what memory should I page" with "which thread faulted." VMO-level attachment keeps concerns separate.

**Exception dispatch priority chain** (designed for future extensibility):

1. Translation fault on pager-backed VMO -> dispatch to VMO pager (this phase)
2. Any exception + process has exception handler -> dispatch to process handler (future: debuggers, breakpoints, illegal instructions)
3. Kill the process

The extension point for process-level exception handling is a one-line addition to the fault handler. No refactoring needed.

**Fault deduplication:** `pending_faults: BTreeSet<u64>` in VMO. First fault on page N adds to set + sends to pager. Subsequent faults on page N just block (no duplicate message to pager). `pager_supply` removes from set.

**Pager death:** Channel close -> wake all pager waiters -> re-fault -> no pager + uncommitted -> kill process. Conservative: wrong data is worse than no data. This matches Zircon's behavior — a dead pager means the VMO is irrecoverable.

**Syscalls:**

| Nr  | Syscall       | Args                                     | Returns | Rights       |
| --- | ------------- | ---------------------------------------- | ------- | ------------ |
| 25  | vmo_set_pager | x0=vmo_handle, x1=channel_handle         | 0       | WRITE on VMO |
| 26  | pager_supply  | x0=vmo_handle, x1=offset_pages, x2=count | 0       | WRITE on VMO |

**Thread blocking:** New field `pager_wait: Option<(VmoId, u64)>` on Thread. `block_current_for_pager` marks the thread as blocked. `pager_supply` scans the blocked list for matching (vmo_id, page_offset) and wakes them. Woken threads re-enter the fault handler and find committed pages.

**What this unlocks:** Demand-paged filesystems, memory-mapped files, sandboxed decoders that lazily decode on access.

**Prior art:** Zircon pager (zx_pager_create, zx_pager_supply_pages), seL4 fault endpoints, Mach external memory managers.

---

## 17.0 Security Hardening

Implemented security features and their design rationale. Each feature was chosen because it provides meaningful protection at minimal cost on ARMv8, and because the architecture abstraction (§0.12) concentrates the implementation in a single arch module.

### 17.1 Kernel PRNG

ChaCha20 with fast key erasure (Bernstein 2017). **Novel: type-state seeding** — `EntropyPool -> Prng` transition enforced at compile time via Rust's type system. An uninitialized PRNG cannot be used; a seeded PRNG cannot be re-seeded. No production kernel does this (C lacks the type machinery). The PRNG is the foundation for all randomization: ASLR, PAC keys, future stack cookies.

**Entropy sources:** RNDR instruction (if FEAT_RNG available, probed at runtime via `ID_AA64ISAR0_EL1[63:60]`) + CPU jitter extraction (memory-access timing variation measured via CNTVCT). RNDR provides hardware-quality entropy on Apple M1+ and ARMv8.5+. Jitter provides a universal fallback — quality varies (higher on bare metal, lower on VMs).

**Per-CPU instances, per-process fork:** Each core gets its own PRNG (no lock contention). `fork()` derives a per-process seed, ensuring layout isolation between processes. 28 tests including RFC 8439 test vectors and statistical quality checks. Zero `unsafe` in the PRNG core.

### 17.2 User Address Space Layout Randomization (ASLR)

Per-process randomized bases for heap, DMA, device MMIO, and stack regions. ~14 bits entropy for each region. Per-process PRNG fork (§17.1) ensures layout isolation between processes — compromising one process's layout reveals nothing about another's.

**What's randomized:** Heap base, DMA region base, device MMIO base, stack base. **What's not (yet):** Channel SHM and shared memory remain at fixed VAs (userspace addresses them directly — full ASLR requires a bootstrap protocol). Deterministic fallback when PRNG unavailable (boot proceeds, just without randomization).

### 17.3 Pointer Authentication (PAC)

Per-process PAC keys: 5 x 128-bit keys (APIA, APDA, APIB, APDB, APG) generated from the process's PRNG fork, stored in Process struct, loaded on context switch alongside the TTBR0 swap. `arch::security` module with feature detection (`pac_supported()`, `bti_supported()`). Raw system register encodings for key writes.

**Why PAC replaces stack canaries:** PAC is strictly superior on ARM64. Stack canaries protect one value (return address) with one secret (canary). PAC protects every authenticated pointer with per-process keys, cannot be bypassed by reading the canary from an adjacent stack frame, and costs one instruction per sign/verify (hardware accelerated, ~1 cycle). The only reason to use canaries on ARM64 is if PAC isn't available — and all Apple Silicon and ARMv8.3+ support it.

### 17.4 Branch Target Identification (BTI)

BTI enforcement prevents jumping into the middle of a function (JOP attacks). Enabled alongside PAC in the `arch::security` module. Requires compiler support (`-Zbranch-protection=bti`).

### 17.5 Execute-Only User Code Pages

New `PageAttrs::user_xo()` maps code segments as execute-only (AP=RO, no AP_EL0, UXN=0). EL0 can fetch instructions but load/store on code pages faults. Prevents code disclosure attacks that leak ASLR layout. ~5 lines of page table configuration. A process cannot read its own code section — the only way to discover code addresses is by executing them.

### 17.6 Kernel Address Space Layout Randomization (KASLR)

8-bit entropy (256 possible positions), 32 MiB slide granularity (matches L2 block size with 16 KiB pages). The kernel loads at a random physical offset on each boot.

**Implementation:** PIE binary (`--pie -z notext`) with position-independent code. `boot.S` reads CNTVCT_EL0 before MMU enable, extracts bits [32:25] as the slide index, shifts all TTBR1 L2 entries uniformly (device + RAM). Post-link fixup tool processes `R_AARCH64_RELATIVE` relocations: physical address addends (< KERNEL_VA_OFFSET) are not slid, kernel VA addends (>= KERNEL_VA_OFFSET) are slid. Self-contained: works on any ELF loader.

**Why 8 bits, not more:** 256 positions x 32 MiB = 8 GiB slide range, which fits comfortably in the 64 GiB kernel VA space (T1SZ=28). More bits would require smaller slide granularity (increasing TLB pressure from partial block mappings) or a larger VA range (increasing T1SZ, reducing user VA). 8 bits is the sweet spot for a microkernel where the kernel image is small and the attacker must guess remotely.

---

## 18.0 Spectre/Meltdown Design Story

Not implemented, but the architecture explicitly supports it. Someone building a multi-tenant system on this kernel needs a clear path to adding mitigations. This section documents that path.

### 18.1 Meltdown-Class (Mitigated by Default)

Split TTBR (§0.2) gives kernel/user page table isolation for free. Userspace TTBR0 literally cannot address kernel memory — this is the KPTI mitigation that Linux had to retrofit painfully, and ARM microkernels get it by default. No work needed. No performance cost.

### 18.2 Spectre-Class (Injection Points Documented)

After the architecture abstraction (§0.12), every mitigation injection point is a single function in a single arch module. Adding a speculation barrier to `context_switch` is literally one instruction in one file. Without the arch abstraction, these same barriers would need to be sprinkled across `exception.S`, `scheduler.rs`, `main.rs`, and `syscall.rs` — fragile, easy to miss a path.

| Mitigation                              | Injection point          | ARM64 instruction     | x86_64 equivalent | Notes                                                    |
| --------------------------------------- | ------------------------ | --------------------- | ----------------- | -------------------------------------------------------- |
| Speculation barrier on syscall entry    | `arch::syscall_entry()`  | `sb` or `csdb`        | `lfence`          | Prevents speculative execution past privilege transition |
| Speculation barrier before eret         | `arch::context_switch()` | `sb` or `csdb`        | `lfence`          | Prevents speculative access to new process's data        |
| Indirect branch prediction invalidation | `arch::context_switch()` | BTI (already enabled) | IBPB              | Per-process branch predictor isolation                   |
| Speculative store bypass disable        | `arch::init_boot_cpu()`  | SSBS                  | SPEC_CTRL MSR     | One-time configuration at boot                           |
| Retpoline for indirect calls            | Compiler flag            | N/A (BTI sufficient)  | `-mretpoline`     | Toolchain concern, not kernel code                       |

### 18.3 Porting Guide Implications

The architecture porting guide (future packaging phase) should include a "security hardening" section listing these injection points and what each architecture needs. This turns Spectre/Meltdown from "unsupported" into "supported by design, implement per your threat model." A porter targeting a multi-tenant use case adds five instructions across three functions. A porter targeting a single-user embedded system skips them all — zero cost.
