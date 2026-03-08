# Kernel Design Notes

Working reference for kernel improvements. Each section covers the design rationale, approach, and key decisions for one roadmap item. Written before implementation, updated during. The finished code's module-level docs are the authoritative reference; these notes capture the "why" behind choices.

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

**Current:** Round-robin scan of `Vec<Box<Thread>>` behind `IrqMutex`. O(n) per decision. No priorities. Single run queue.

**Target:** Priority support + SMP awareness. O(1) common case.

**Approach (two stages):**

**A. Global queue with per-core current-thread (start here):**

- One global run queue with ticket spinlock.
- Each core picks next ready thread on timer tick.
- Fine for 2-8 cores.

**B. Per-core run queues with work stealing (if needed):**

- Per-queue lock or lock-free local access.
- Empty queues steal from other cores.
- Thread affinity for cache warmth.
- Only implement if global queue bottlenecks.

**Priority:** Three levels via priority bitmap + per-priority queue:

- **Idle:** boot/WFE threads
- **Normal:** user processes
- **High:** OS service process (once it exists)

`schedule()` picks highest-priority non-empty queue, round-robins within it.

Public API unchanged: `schedule()`, `spawn()`, `block_current_and_schedule()`.

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
- Replace `page_alloc.rs`. New API: `alloc_frames(order) -> Option<usize>`, `free_frames(pa, order)`. Single page = `alloc_frames(0)`.

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

**Approach:** Boot tables are at known symbols from `boot.S`. Free those frames.

**Depends on:** None.

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
