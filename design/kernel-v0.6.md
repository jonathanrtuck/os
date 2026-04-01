# v0.6 Kernel: General-Purpose Microkernel

**Goal:** Transform the kernel from "the foundation of a document-centric OS" into "the best practical Rust microkernel in the world." A kernel so clean, well-documented, and easy to build on that there's no reason to reach for anything else.

**Thesis:** seL4 wins on formal verification. Linux wins on hardware support. This kernel wins on _clarity_ — Rust's type system + exhaustive testing + documented rationale for every decision. For the 99% of projects that aren't building avionics, "easy to understand, easy to verify, easy to build on" beats "formally proven but requires a PhD."

**Scope:** Kernel only. No changes to userspace services, document pipeline, or display stack. The kernel should emerge from this milestone as a standalone artifact that anyone can build on.

**Current state:** 11,375 lines of Rust/asm, 35 source files, 28 syscalls, 4-core SMP, EEVDF + scheduling contexts + donation, handle-based access, demand paging, interrupt forwarding, userspace drivers. 15,557 lines of tests (2,313+ tests). Zero architecture-specific code in the scheduler, handle table, channel logic, process model, waitable registry, futex, ELF loader, or allocators — roughly 60-65% of the kernel is already architecture-independent.

---

## Differentiation Strategy

| Kernel | Strength                          | Weakness (our opportunity)                                                   |
| ------ | --------------------------------- | ---------------------------------------------------------------------------- |
| seL4   | Formally verified                 | C, steep learning curve, limited SMP in verified config, painful to build on |
| Zircon | Production-grade, good primitives | C++, 200K+ lines, Google-coupled, not extractable                            |
| Redox  | Rust, active community            | Unix-shaped (POSIX baggage), less documented rationale                       |
| Linux  | Runs everywhere                   | Monolithic, 30M+ lines, not a microkernel                                    |

**Our position:** Rust + microkernel + small + documented + tested + modern scheduler. The kernel someone reaches for when they want to build something new without inheriting decades of design constraints.

---

## Phases

### Phase 1: Architecture Abstraction

**Design question:** What is the `arch` interface? This is THE critical design decision — it determines what "porting to a new architecture" means.

**Principle:** The arch module is a driver. It translates hardware specifics into kernel-internal abstractions. Same pattern as a virtio driver translating device registers into OS primitives. The interface should be the simplest version that works. Complexity lives inside each arch implementation (leaf node).

**What must be behind the arch boundary:**

| Concern                 | Current location                             | Why arch-specific                                                    |
| ----------------------- | -------------------------------------------- | -------------------------------------------------------------------- |
| Boot sequence           | `boot.S`                                     | Different entry conventions, MMU enable, exception level transitions |
| Context save/restore    | `exception.S`, `context.rs`                  | Register file differs (x86 has segments, flags; ARM has SPSR, ELR)   |
| Page tables             | `paging.rs`, `memory.rs`, `address_space.rs` | Descriptor format, levels, TLB invalidation                          |
| Interrupt controller    | `interrupt_controller.rs`                    | GICv3 (ARM) vs APIC (x86) vs PLIC (RISC-V)                           |
| Timer                   | `timer.rs` (partially)                       | Generic timer (ARM) vs HPET/TSC (x86)                                |
| Per-core state          | `per_core.rs`                                | MPIDR (ARM) vs APIC ID (x86)                                         |
| SMP boot                | `main.rs` (PSCI calls)                       | PSCI (ARM) vs SIPI (x86)                                             |
| Inline asm in scheduler | `scheduler.rs`                               | TTBR swap, TPIDR, DSB/ISB                                            |
| Serial console          | `serial.rs`                                  | PL011 (ARM/QEMU) vs 8250 (x86)                                       |

**What stays generic (no arch dependency):**

- `scheduler.rs` (algorithm + state machine) — except TTBR swap + TPIDR
- `scheduling_algorithm.rs` (pure EEVDF math)
- `scheduling_context.rs` (budget/period accounting)
- `channel.rs`, `handle.rs`, `waitable.rs` (IPC + capabilities)
- `process.rs`, `thread.rs` (process model)
- `executable.rs` (ELF loading)
- `futex.rs` (PA-keyed wait/wake)
- `heap.rs`, `slab.rs`, `page_allocator.rs` (allocators)
- `device_tree.rs` (DTB parsing — ARM + RISC-V, skip on x86/ACPI)
- `sync.rs` (IrqMutex — needs arch for IRQ masking, but the lock logic is generic)
- `metrics.rs`, `syscall.rs` (dispatch logic)

**Interface sketch (needs design discussion):**

```rust
/// Architecture-specific operations the kernel requires.
/// Each architecture implements this as a module (not a trait object —
/// monomorphized at compile time, zero overhead).
mod arch {
    fn init_boot_cpu() -> BootInfo;
    fn init_secondary_cpu(core_id: usize);
    fn context_switch(old: *mut Context, new: *const Context);
    fn set_current_thread(ctx: *mut Context);  // TPIDR or equivalent
    fn current_core_id() -> usize;

    mod mmu {
        fn create_address_space() -> AddressSpace;
        fn map_page(asid, va, pa, flags);
        fn unmap_page(asid, va);
        fn switch_address_space(asid);
        fn invalidate_tlb(asid);
    }

    mod interrupts {
        fn init();
        fn enable_irq(irq: u32);
        fn disable_irq(irq: u32);
        fn acknowledge() -> u32;
        fn end_of_interrupt(token: u32);
        fn send_ipi(target_core: usize);
        fn mask_all();
        fn unmask_all();
    }

    mod timer {
        fn init();
        fn set_deadline(ns: u64);
        fn now_ns() -> u64;
        fn frequency() -> u64;
    }

    mod serial {
        fn init();
        fn put_byte(b: u8);
    }
}
```

**Deliverable:** The aarch64 implementation of this interface, extracted from existing code. The kernel compiles and all 2,313+ tests pass. No new architecture yet — this phase is about the interface, not the second implementation.

**Open questions:**

- Does the arch interface include device discovery (DTB/ACPI), or is that a separate concern? Leaning: separate, since it's consumed by userspace services, not the kernel's core loop.
- How does the page table abstraction handle architectural differences in levels, granules, and descriptor formats without leaking? Leaning: the `mmu` interface works in terms of VA/PA/flags, not descriptors.
- Does `Context` become arch-generic with arch-specific storage, or fully arch-defined? Leaning: fully arch-defined (register set IS the architecture), with generic field accessors (pc, sp, arg0-arg5).

---

### Phase 2: Capability Model

**Design question:** How do we make the handle system trustworthy enough that someone building a security-sensitive system would use this kernel?

**Current state:** 256 fixed slots per process, 2-bit rights (read/write), move semantics on `handle_send`, per-process syscall filtering (bitmask). Handle validation on every syscall. This is already ahead of Redox and comparable to early Zircon.

**Sub-phases:**

**2a. Rights attenuation** (settled design from kernel-hardening.md Gap 1):

- Widen rights bitfield to 8 bits: READ, WRITE, SIGNAL, WAIT, MAP, TRANSFER, CREATE, KILL
- `handle_send(target, handle, rights_mask)` — new handle gets `original & mask`
- ~50 lines changed. High value, low risk.

**2b. Dynamic handle table:**

- Replace fixed `[Option<HandleEntry>; 256]` with growable structure
- Two-level: base array of 256 (fast path, no allocation) + overflow pages on demand
- Matters for compound documents with many channels

**2c. Badging:**

- When creating a channel endpoint for a client, tag it with a badge value
- When a message arrives, the receiver sees the badge — identifies which client sent it
- Critical for shared services (one endpoint, many clients)
- seL4 pattern, well-understood

**Open questions:**

- Is 8-bit rights sufficient, or should we go wider (16/32) for future extensibility? Leaning: 16 bits (room to grow, still fits in a u16).
- Should badge be per-endpoint or per-handle? Leaning: per-endpoint (Zircon pattern — badge is on the kernel object, not the handle).

---

### Phase 3: Core Primitives

Each of these is a design discussion before implementation. They're ordered by how many things they unlock.

**3a. Virtual Memory Objects (VMOs):**

The foundational shared-memory abstraction. A VMO is a handle to a range of physical pages. Any process holding a VMO handle can map it into its address space. Channels carry messages; VMOs carry data.

- Current model: init allocates shared memory, maps it into processes before start. No runtime sharing.
- VMO model: any process can create a VMO, map/unmap it, send the handle via IPC. Dynamic, flexible, composable.
- Unlocks: zero-copy IPC for large data, shared libraries, memory-mapped files, framebuffer sharing without init knowing about it.
- Reference: Zircon `zx_vmo_create`, `zx_vmar_map`.

**3b. Pager interface / exception forwarding:**

When a process faults, instead of killing it, forward the fault to a designated pager process. The pager resolves the fault (e.g., reads from disk, decompresses, generates content) and replies. The faulting thread resumes.

- Unlocks: demand-paged filesystems, memory-mapped files, sandboxed decoders that lazily decode on access, debuggers (breakpoints are faults).
- This is §9.9 (COW kernel mechanics) generalized: instead of the kernel handling COW faults internally, it forwards them to userspace.
- Reference: seL4 fault endpoints, Zircon exception channels.
- Subsumes §9.9 entirely — COW becomes a userspace policy, not a kernel mechanism.

**3c. Signals / event objects:**

Lightweight notification without message payload. A handle you can signal and wait on. Like a channel but simpler — just a bitfield of pending signals. Multiple waiters supported.

- Current model: channels are the only waitable IPC. For "just wake me up," a 64-byte SPSC ring is overkill.
- Signals model: `event_create()`, `event_signal(handle, bits)`, `wait` already works.
- Unlocks: efficient notification for completion, state changes, process lifecycle events.
- Reference: Zircon `zx_event_create`, `zx_object_signal`.

**3d. Thread inspection / suspension:**

Suspend a thread (from another process), read its register state, resume it. The primitive for debuggers and profilers.

- Syscalls: `thread_suspend(handle)`, `thread_resume(handle)`, `thread_read_state(handle, buf)`.
- Low-risk addition — the scheduler already has suspended state.

**3e. Clock abstraction:**

Expose a proper monotonic clock to userspace. Currently `sys::counter()` returns raw CNTVCT_EL0 — architecture-specific, frequency varies by platform.

- Provide `clock_monotonic_ns() -> u64` as a vDSO-style fast path (mapped read-only page with kernel-maintained timestamp) or a lightweight syscall.
- Depends on Phase 1 (timer abstraction provides `now_ns()`).

**Open questions for each:** see design discussions (to be held per-primitive).

---

### Phase 4: Security Hardening

**4a. User ASLR:**

- Kernel PRNG (ChaCha20, ~50 lines, seeded from architectural counter + DTB entropy)
- Per-process randomized bases for code, heap, stack, SHM regions
- Bump allocators already support arbitrary starting points — decouple from `*_BASE` constants

**4b. Stack canaries:**

- `-Z stack-protector=all` compiler flag
- Per-thread random canary in TLS slot
- ~20 lines of kernel code

**4c. KASLR:**

- Randomize `KERNEL_VA_BASE` at boot
- Requires boot.S modification + RNG before MMU enable
- Well-understood (Linux, Zircon do this)

**4d. COW kernel mechanics (§9.9) — may be subsumed by Phase 3b:**

- If pager interface is implemented, COW becomes userspace policy
- If pager interface is NOT in scope, implement kernel-side COW fault handling
- Decision depends on Phase 3b design discussion outcome

---

### Phase 5: SMP Scalability (per-core ready queues + IPI)

**Why now, not later:** Per-core ready queues are not an optimization — they're the expected architecture for any SMP microkernel. seL4, Zircon, and QNX all use per-core scheduling. A global lock is fine at 4 cores / 5 processes, but someone evaluating this kernel for a real project will question whether it scales. More importantly: Phase 1 is already extracting the scheduler's arch-specific code (TTBR swap, TPIDR, DSB/ISB). Restructuring the ready queue _during_ that extraction is significantly cheaper than doing it as a separate pass after the arch abstraction settles.

**What's in scope:**

**5a. Per-core ready queues:**

- Split single `ready: VecDeque<Box<Thread>>` into `per_core[N].ready_queue`
- `schedule_inner` selects from the local queue — no global lock for the common path
- The global scheduler lock becomes a coordination lock (thread state transitions, blocked list) rather than the hot-path lock
- Wake-ups targeting the local core go directly to the local queue

**5b. IPI for cross-core wake:**

- When `channel_signal` / `try_wake` targets a thread on a different core, enqueue on target's queue + send SGI (software-generated interrupt)
- GIC SGI support already accessible via `interrupt_controller.rs`
- Target core's IRQ handler checks for new ready threads
- The two-phase wake pattern changes to: collect under source lock, enqueue on target core's queue (brief per-core lock), IPI

**5c. Lost-wakeup invariant across cores:**

- `wake_pending` flag must work with per-core locks
- Pattern: set `wake_pending` (release store) → IPI → target core reads `wake_pending` (acquire load) in IRQ handler
- Same correctness argument as current design, just across a lock boundary instead of within one

**What's NOT in scope (wait for metrics):**

- Work stealing (asymmetric load balancing)
- Fine-grained lock splitting (per-thread state locks, per-core blocked lists)
- NUMA awareness

**Open questions:**

- Thread-to-core affinity: should scheduling contexts pin threads to specific cores, or is the wake-targeting heuristic sufficient? Leaning: wake to "last core the thread ran on" (cache warmth), no hard affinity.
- Deferred ready/drops interaction: the per-core `deferred_ready` and `deferred_drops` lists (§12.1) are already per-core — they should compose naturally with per-core queues, but need verification.

---

### Phase 6: Packaging & Extraction

**6a. Standalone repository structure:**

- Kernel as its own crate / repo (or at minimum, its own Cargo workspace member with zero OS-specific dependencies)
- Clean build: `cargo build --target aarch64-unknown-none` produces a bootable ELF
- README: what it is, how to build, how to boot on QEMU, architecture, syscall table

**6b. API documentation:**

- Rustdoc for all public interfaces
- Syscall reference (manpage-style, one page per syscall)
- Architecture porting guide: "implement these N functions to port to a new architecture"

**6c. Example userspace:**

- Minimal "hello world" (sys_write)
- IPC example (two processes, channel communication)
- Driver example (interrupt-driven device)
- These become the onboarding path for anyone building on the kernel

---

## Phase Dependencies

```text
Phase 1 (Arch Abstraction) ───────────────────────────────────────┐
    │                                                             │
    ├──→ Phase 2 (Capabilities) ──→ Phase 3a (VMOs)               │
    │         │                        │                          │
    │         │                   Phase 3b (Pager) ──→ Phase 4d   │
    │         │                        │               (COW)      │
    │         └──→ Phase 3c (Signals)  │                          │
    │              Phase 3d (Thread inspect)                      │
    │              Phase 3e (Clock)                               │
    │                                                             │
    ├──→ Phase 4a-c (ASLR, canaries, KASLR) [independent]         │
    │                                                             │
    ├──→ Phase 5 (SMP Scalability) [natural extension of Phase 1] │
    │                                                             │
    └──→ Phase 6 (Packaging) [after all others] ──────────────────┘
```

Phase 1 is the prerequisite for everything. Phase 5 (SMP) is best done immediately after Phase 1 while scheduler internals are fresh from the arch extraction. Phase 2 should come early (capabilities inform how VMOs and signals work). Phases 3a-3e are mostly independent of each other. Phase 4 is independent of Phases 3 and 5. Phase 6 is last.

---

## Success Criteria

**Minimum (the kernel is better):**

- Architecture abstraction extracted, aarch64 is a leaf implementation
- Rights attenuation and dynamic handle table
- All existing tests pass
- Kernel compiles with no OS-specific references in generic code

**Target (the kernel is standalone):**

- All of minimum + VMOs + signals + pager interface
- Per-core ready queues + IPI wake
- User ASLR
- Standalone build with README and examples
- Someone unfamiliar with the project can boot it on QEMU in 5 minutes

**Stretch (the kernel is "the one"):**

- Second architecture (RISC-V or x86_64) implemented by following the porting guide
- Full Rustdoc API documentation
- Syscall reference
- Published (crate or repo)

---

## Estimated Scale

| Phase                 | New/changed lines (est.)    | Design discussions needed         |
| --------------------- | --------------------------- | --------------------------------- |
| 1. Arch abstraction   | ~1,500 (refactor, net ~0)   | 1 (the interface)                 |
| 2. Capabilities       | ~300–500                    | 1 (rights model)                  |
| 3. Core primitives    | ~800–1,200                  | 4–5 (one per primitive)           |
| 4. Security hardening | ~200–400                    | 0 (designs exist in research doc) |
| 5. SMP scalability    | ~600–900 (scheduler rework) | 1 (per-core queue design)         |
| 6. Packaging          | ~500 (docs, examples)       | 0                                 |

Total: ~3,900–4,500 lines of new/refactored code, 7–8 design discussions. The kernel grows from ~11K to ~15-16K lines while gaining significantly more capability.

---

## What This Does NOT Include

- Second architecture implementation (community contribution, enabled by Phase 1)
- Real hardware support (remains v0.13-equivalent, now v0.14)
- SMP work stealing / lock splitting — wait for `lock_spins` metrics under real workloads
- Spectre/Meltdown mitigations — see "Spectre/Meltdown Design Story" below
- Formal verification — different thesis entirely

---

## Spectre/Meltdown Design Story

Not in scope for implementation, but the architecture must support it. Someone building a multi-tenant system on this kernel needs a clear path to adding mitigations.

**What the kernel already provides (Meltdown-class):**

Split TTBR (§0.2) gives kernel/user page table isolation for free. Userspace TTBR0 literally cannot address kernel memory — this is the KPTI mitigation that Linux had to retrofit painfully, and ARM microkernels get it by default. No work needed.

**What someone would add (Spectre-class):**

| Mitigation                              | Where it goes            | Arch-specific?                          |
| --------------------------------------- | ------------------------ | --------------------------------------- |
| Speculation barrier on syscall entry    | `arch::syscall_entry()`  | Yes — `sb`/`csdb` (ARM), `lfence` (x86) |
| Speculation barrier before eret         | `arch::context_switch()` | Yes — same instructions                 |
| Indirect branch prediction invalidation | `arch::context_switch()` | Yes — BTI (ARM), IBPB (x86)             |
| Speculative store bypass disable        | `arch::init_boot_cpu()`  | Yes — SSBS (ARM), SPEC_CTRL (x86)       |
| Retpoline for indirect calls            | Compiler flag            | Toolchain, not kernel code              |

**Why the arch abstraction makes this tractable:**

After Phase 1, every one of these injection points is a single function in a single arch module. Adding a speculation barrier to `context_switch` is literally one instruction in one file. Without the arch abstraction, these same barriers would need to be sprinkled across `exception.S`, `scheduler.rs`, `main.rs`, and `syscall.rs` — fragile, easy to miss a path.

**The documentation story:** The architecture porting guide (Phase 6b) should include a "security hardening" section listing these injection points and what each architecture needs. This turns Spectre/Meltdown from "unsupported" into "supported by design, implement per your threat model."
