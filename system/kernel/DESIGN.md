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

**Approach:** `SVC #0` from user code. Register ABI: `x8` = syscall number, `x0`–`x5` = arguments, `x0` = return value (≥0 success, <0 error). Six syscalls: `exit`, `write`, `yield`, `handle_close`, `channel_signal`, `channel_wait`.

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
