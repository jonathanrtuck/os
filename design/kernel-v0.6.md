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

**Settled interface (2026-04-01):**

Three open questions resolved through design discussion:

**Q1: Device discovery → SEPARATE.** DTB/ACPI is consumed by init (userspace) to find device addresses. On x86 it would be ACPI tables — completely different mechanism. Putting either inside the arch boundary would abstract _platform_, not _architecture_. `device_tree.rs` stays generic. Arch provides minimal boot info (RAM base/size, DTB pointer). Informed by the NT HAL lesson: the HAL's device-enumeration scope was eaten by firmware standardization (ACPI, UEFI). Abstract what genuinely varies between ISAs, nothing more.

**Q2: Page tables → VA/PA/permissions interface.** Walk logic, descriptor format, TLB invalidation are all arch-internal. The generic kernel never constructs descriptors. Arch owns the walk and calls the page allocator directly for intermediate table pages — same pattern as Linux (`arch/arm64/mm/mmu.c` calls `alloc_pages()`), Zircon (`ArmArchVmAspace::MapPages()` calls `AllocPage()`), and Redox. seL4 is the only kernel that externalizes table provisioning to userspace, but that's driven by formal verification constraints (eliminating implicit allocation from the proof), not applicable here.

**Q3: Context → fully arch-defined, generic accessors.** The register set IS the architecture. ARM64 Context has x[31], sp, elr, spsr, q[32]; x86_64 would have rax-r15, rip, rflags, xmm. Zero overlap. The abstraction is method-based: `pc()`, `set_sp()`, `arg(n)`, `set_user_mode()`. On aarch64, `pc()` returns `self.elr`; on x86_64 it would return `self.rip`.

**Additional decision: IRQ saved state → opaque `IrqState` newtype.** Zero-cost `#[repr(transparent)]` wrapper, consistent with the project's existing `Pa` newtype pattern. No production C kernel does this (C lacks zero-cost newtypes), but Rust enables it. `interrupts::mask_all() -> IrqState`, `interrupts::restore(IrqState)`.

**Additional decision: Serial console → arch for now, platform later.** PL011 is technically a device (board-specific), not architecture. Linux, Zircon, and seL4 all separate arch from platform. But a platform layer serves zero boards today. Serial (~60 lines) lives in arch with an explicit marker: "platform-specific, extract to `platform::` when a second board target arrives (v0.14)." Same applies to GIC base addresses and RAM geometry.

```rust
/// Architecture-specific operations the kernel requires.
/// Compile-time module selection via #[cfg(target_arch)], not trait objects.
/// Zero-overhead: all calls monomorphized/inlined.
mod arch {
    // --- Boot ---
    fn init_boot_cpu(dtb_ptr: *const u8) -> BootInfo;
    fn init_secondary_cpu(core_id: usize);

    // --- Context (fully arch-defined, generic accessors) ---
    struct Context { /* arch-specific fields */ }
    impl Context {
        fn new() -> Self;
        fn pc(&self) -> u64;
        fn set_pc(&mut self, pc: u64);
        fn sp(&self) -> u64;
        fn set_sp(&mut self, sp: u64);
        fn arg(&self, n: usize) -> u64;
        fn set_arg(&mut self, n: usize, val: u64);
        fn set_user_mode(&mut self);
        fn set_user_tls(&mut self, tls: u64);
        fn user_tls(&self) -> u64;
    }

    // --- Core identity ---
    fn core_id() -> u32;
    fn set_current_thread(ctx: *mut Context);

    // --- MMU (arch owns walk + allocation) ---
    mod mmu {
        struct PageTableRoot { /* opaque */ }
        enum PagePerm { UserRX, UserRW, UserRO, UserDeviceRW }

        fn init_kernel_tables();
        fn create() -> (PageTableRoot, Asid);
        fn map(root: &PageTableRoot, va: u64, pa: Pa, perm: PagePerm)
            -> Result<(), MapError>;
        fn unmap(root: &PageTableRoot, va: u64) -> Option<Pa>;
        fn switch(root: &PageTableRoot, asid: Asid);
        fn invalidate(asid: Asid);
        fn destroy(root: PageTableRoot, asid: Asid);
        fn set_kernel_guard(va: u64) -> bool;
        fn clear_kernel_guard(va: u64);
        fn is_user_accessible(va: u64, write: bool) -> bool;
    }

    // --- Interrupts ---
    mod interrupts {
        struct IrqState(/* opaque */);  // saved interrupt state

        fn init();
        fn enable_irq(irq: u32);
        fn disable_irq(irq: u32);
        fn acknowledge() -> u32;
        fn end_of_interrupt(irq: u32);
        fn send_ipi(target_core: u32);
        fn mask_all() -> IrqState;
        fn restore(saved: IrqState);
    }

    // --- Timer ---
    mod timer {
        fn init();
        fn set_deadline_ns(ns: u64);
        fn now_ns() -> u64;
        fn frequency() -> u64;
    }

    // --- Serial (platform-specific; extract to platform:: at v0.14) ---
    mod serial {
        fn init(base: usize);
        fn put_byte(b: u8);
    }

    // --- Power ---
    mod power {
        fn cpu_on(target: u64, entry: u64, ctx: u64) -> Result<(), i64>;
        fn system_off() -> !;
    }
}
```

**Deliverable:** The aarch64 implementation of this interface, extracted from existing code. The kernel compiles and all 2,313+ tests pass. No new architecture yet — this phase is about the interface, not the second implementation.

**Implementation plan:**

The extraction is mostly mechanical — moving existing code behind the interface. Ordered to keep the kernel compiling and tests passing at every step.

**Step 1: Create module structure.** `kernel/arch/mod.rs` (interface), `kernel/arch/aarch64/mod.rs` + submodules. Wire into `main.rs` with `#[cfg(target_arch = "aarch64")]`. Everything still compiles (new modules, no moves yet).

**Step 2: Move pure-arch files.** `context.rs` → `arch/aarch64/context.rs` (add accessor methods). `power.rs` → `arch/aarch64/power.rs`. `boot.S` → `arch/aarch64/boot.S`. `exception.S` → `arch/aarch64/exception.S`. `interrupt_controller.rs` → `arch/aarch64/interrupts.rs`. `paging.rs` descriptor constants → `arch/aarch64/` (VA layout constants stay as generic `layout.rs`). `memory.rs` → `arch/aarch64/mmu.rs` (kernel table refinement). Tests pass after each file move.

**Step 3: Extract arch from mixed files.** One file at a time, tests after each:

- `per_core.rs`: MPIDR read → `arch::core_id()`. Online tracking stays generic.
- `sync.rs`: DAIF save/restore → `arch::interrupts::mask_all()`/`restore()` with `IrqState`. Ticket spinlock stays generic.
- `timer.rs`: All 6 asm sites → `arch::timer::*`. Waiter tracking stays generic.
- `serial.rs`: PL011 register access → `arch::serial::*`. Lock wrapper stays generic.
- `scheduler.rs`: TPIDR_EL1 → `arch::set_current_thread()`. TTBR0 swap → `arch::mmu::switch()`. TLBI → `arch::mmu::invalidate()`.
- `syscall.rs`: AT S1E0R/W → `arch::mmu::is_user_accessible()`.
- `address_space.rs`: Page table walk + descriptor construction → `arch::mmu::map/unmap/create/destroy`. VMA management + budgets stay generic.

**Step 4: Verify.** Full test suite (2,313+ tests). Clippy clean. No `asm!` outside `arch/aarch64/`. No ARM64 register names outside `arch/aarch64/`. Grep audit for leakage.

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

**3a. Virtual Memory Objects (VMOs) — SETTLED (2026-04-01):**

The foundational shared-memory abstraction. A VMO is a handle to a range of physical pages. Any process holding a VMO handle can map it into its address space. Channels carry messages; VMOs carry data.

Design discussion held 2026-04-01. Cross-OS comparison (Zircon, seL4, Mach/XNU, QNX, L4 family, Linux, Redox) plus research survey (Theseus, Twizzler, RedLeaf, TreeSLS, Asterinas). Six design questions settled. Four novel features adopted from research. Full design below.

#### Design Principles

1. **VMO is THE memory object.** One abstraction for all shared memory. Subsumes `memory_share` (syscall #24). Subsumes `dma_alloc`/`dma_free` (syscalls #17/#18) via `VMO_CONTIGUOUS` flag. No parallel memory abstractions — when you build a new way, kill the old way.

2. **Capability-native.** VMO handles participate fully in the capability system: rights attenuation (READ, WRITE, MAP, APPEND, SEAL), transfer via `handle_send`, badges. Same model as channels, timers, interrupts.

3. **Ownership-typed (Theseus-inspired).** The kernel-internal `Vmo` type uses Rust ownership to prevent use-after-free and double-unmap at compile time. `Drop` unmaps all mappings and frees all pages. No manual `freed` flag. No other microkernel can do this — it's a Rust-specific advantage.

4. **Designed for the general case.** This is not "what our OS needs" — it's "what any consumer of this microkernel needs." Decisions evaluated against the full landscape of microkernel use cases.

#### Settled Decisions

**1. Size: Fixed at creation.**

Resize is architecturally wrong in a capability system. Zircon added `ZX_VMO_RESIZABLE` and immediately needed `ZX_VM_ALLOW_FAULTS` and `ZX_VM_REQUIRE_NON_RESIZABLE` — defensive flags that exist solely because resize creates a class of bugs where one process shrinks a VMO mapped in another, causing unexpected faults. seL4, NOVA, L4Re — the kernels optimizing for correctness — all chose fixed. The formally verified microkernel (seL4) chose fixed.

**2. Backing: Lazy by default (demand-paged, zero-fill on fault).**

Pages allocated on first touch, not at creation. Matches Zircon, Mach, L4 family, Linux. Existing demand-paging infrastructure (heap, ELF segments) reused. A database allocating 1 GiB shouldn't pay for pages it hasn't touched. A compositor allocating render targets for undrawn windows shouldn't commit memory upfront. seL4's eager model is a formal-verification constraint, not a design preference.

Explicit commit available via `vmo_op_range(COMMIT, offset, len)` for processes that need deterministic allocation (no faults on hot path).

**3. Contiguity: Non-contiguous by default. `VMO_CONTIGUOUS` flag for DMA.**

VMO is the single memory object type. Contiguous VMOs use the buddy allocator for 2^n contiguous frames (eager allocation — contiguity requires all pages allocated together). Every OS special-cases contiguous allocation (Zircon `zx_vmo_create_contiguous`, Linux CMA, QNX `MAP_PHYS`). Having two parallel abstractions (VMO + DMA buffer) means every consumer learns two APIs and the capability model has a gap.

Contiguous VMO restrictions: cannot snapshot (COW copy wouldn't be contiguous), always eager (all pages allocated at creation), always pinned (cannot decommit).

**4. VA placement: Kernel-picks (Tier 2), VMAR extension point documented.**

`vmo_map` maps into the process's shared memory region. Kernel picks the next available VA using the existing `VmaList` (sorted list, gap search). Optional `VMO_MAP_FIXED` for specific-VA mapping (fails on overlap, never silently replaces). This is where Mach, QNX, and Linux sit — adequate for all foreseeable use cases.

VMARs (Tier 3) are a documented future extension — see "VMAR Extension Point" below.

**5. Channels remain separate.**

Channels and VMOs are distinct IPC primitives: channels are message pipes (ordered, small), VMOs are shared memory (unordered, large). Every microkernel keeps them separate (Zircon channels + VMOs, seL4 endpoints + frames, L4 IPC + dataspaces). Future: channels carry VMO handles in messages.

#### Novel Features (Beyond Existing Microkernels)

**N1. Built-in generation numbers (versioned memory).**

Every VMO has a generation counter (u64). `vmo_snapshot()` increments the generation and COW-forks the page list. `vmo_restore(generation)` reverts to a previous snapshot. Bounded snapshot ring (configurable depth, default 64).

No other production microkernel offers versioned memory objects. COW is typically a filesystem concern (ZFS, Btrfs) or process-fork mechanism. Making COW a VMO primitive means any consumer gets point-in-time snapshots, undo, and concurrent-read-while-write for free.

Implementation: per-page reference counting (refcount stored alongside Pa in the page list). Write to a page with refcount > 1 triggers COW (allocate new page, copy, update current generation's page list, decrement old page's refcount). When a snapshot is dropped (ring wraps), walk its page list and decrement refcounts, freeing pages that hit zero.

Interaction with other features:

- Sealed VMOs reject `snapshot` and `restore` (content frozen). Existing snapshots remain readable.
- Contiguous VMOs cannot snapshot (COW would break contiguity).
- Append-only VMOs snapshot normally (captures the append frontier).

**N2. Append-only permission.**

New right: APPEND. A handle with APPEND (but not WRITE) can write at `offset >= committed_size` but cannot overwrite existing data. Enforced in `vmo_write` syscall.

Use cases: log-structured stores, audit trails, append-only document history. The document service can hand an APPEND-only VMO to an editor — the editor can add content but never modify or delete previous entries.

**N3. Seal (immutable freeze).**

`vmo_seal()` permanently freezes the VMO's content, permissions, and metadata. Irreversible. All subsequent mutating operations (write, snapshot, restore, seal) return `PermissionDenied`. Existing snapshots survive. Mappings remain valid (read-only).

Use cases: init creates a VMO with font data, seals it, sends READ+MAP handles to services. Services know by construction that the content will never change — no TOCTOU, no races, tamper-proof. Linux has `memfd_create(MFD_ALLOW_SEALING)` + `fcntl(F_ADD_SEALS)` for the same reason (Android uses it to replace ashmem).

Seal requires the SEAL right on the handle. Once sealed, the SEAL right is consumed (seal is monotonic — can't unseal, can't re-seal).

**N4. Content-type tag.**

Each VMO carries an optional `type_tag: u64` set at creation. The kernel doesn't interpret it — it's opaque metadata. Distinct from badges (which identify the sender). Type tags identify what's in the VMO.

Use cases: when VMO handles travel via IPC, the receiver checks `vmo_get_info().type_tag` to verify the content type matches expectations. Catches version mismatches, corrupted handles, and protocol errors without a side-channel. Inspired by RedLeaf's `RRef<T>` (OSDI '20), reduced to minimum viable form.

Type tag is immutable after creation (set in `vmo_create` flags). If content type changes, create a new VMO.

#### Page Commitment Tracking

Per-VMO page list using `BTreeMap<u64, (Pa, u32)>` where the key is the page offset, Pa is the physical address, and u32 is the reference count (for COW snapshot sharing).

- **Uncommitted page:** absent from the BTreeMap. Zero-fill on fault (or return zeros for `vmo_read` without allocating).
- **Committed page, refcount=1:** exclusively owned by the current generation. Writes go directly to the page.
- **Committed page, refcount>1:** shared between the current generation and N snapshots. Write triggers COW: allocate new page, copy content, insert new page at refcount=1, decrement old page's refcount.
- **Contiguous VMO:** BTreeMap pre-populated at creation with all pages at refcount=1. No faulting.

Why BTreeMap (not global hash table, not PTE-is-truth): VMO must be self-contained because it can be mapped into multiple processes. Its page state can't live in any one process's page tables. BTreeMap gives O(log n) lookup, sparse storage (uncommitted ranges cost nothing), and iteration for COW snapshots. Matches Zircon's `VmPageList` architecture. Mach's global hash table creates lock contention under SMP.

#### VMO-Backed VMAs

The fault handler currently handles `Backing::Anonymous` (heap) and `Backing::Code` (ELF). Add `Backing::Vmo`:

```rust
pub enum Backing {
    Anonymous,
    Code,
    Vmo { vmo_id: VmoId, offset: u64, writable: bool },
}
```

Fault path for VMO-backed VMA:

1. Find VMA via `VmaList` (existing binary search)
2. Compute VMO page offset: `(fault_va - vma.base) / PAGE_SIZE + vma.offset`
3. Look up page in VMO's BTreeMap
4. If committed with refcount=1: install PTE (read or read/write per VMA permissions)
5. If committed with refcount>1 and write fault: COW — allocate new page, copy, insert, install writable PTE
6. If uncommitted: allocate frame, zero-fill, insert into BTreeMap at refcount=1, install PTE
7. If uncommitted and `vmo_read` (not a mapped fault): return zeros without allocating

#### Syscall Surface (10 new → 40 total)

| Nr  | Syscall        | Args                                            | Returns    | Rights required       |
| --- | -------------- | ----------------------------------------------- | ---------- | --------------------- |
| 30  | `vmo_create`   | x0=size_pages, x1=flags, x2=type_tag            | handle     | — (creator gets ALL)  |
| 31  | `vmo_map`      | x0=handle, x1=map_flags, x2=fixed_va (if FIXED) | va         | MAP + (READ or WRITE) |
| 32  | `vmo_unmap`    | x0=va, x1=size_pages                            | 0          | — (caller unmaps own) |
| 33  | `vmo_read`     | x0=handle, x1=offset, x2=buf, x3=len            | bytes_read | READ                  |
| 34  | `vmo_write`    | x0=handle, x1=offset, x2=buf, x3=len            | written    | WRITE (or APPEND)     |
| 35  | `vmo_get_info` | x0=handle, x1=info_buf                          | 0          | — (any valid handle)  |
| 36  | `vmo_snapshot` | x0=handle                                       | generation | WRITE                 |
| 37  | `vmo_restore`  | x0=handle, x1=generation                        | 0          | WRITE                 |
| 38  | `vmo_seal`     | x0=handle                                       | 0          | SEAL                  |
| 39  | `vmo_op_range` | x0=handle, x1=op, x2=offset, x3=len             | 0          | varies by op          |

**Create flags (x1 of `vmo_create`):**

- `0` — normal VMO (lazy, non-contiguous)
- `VMO_CONTIGUOUS` (bit 0) — physically contiguous, eager allocation via buddy allocator

**Map flags (x1 of `vmo_map`):**

- `VMO_MAP_READ` (bit 0) — map readable
- `VMO_MAP_WRITE` (bit 1) — map writable (requires WRITE right)
- `VMO_MAP_FIXED` (bit 2) — map at VA in x2 (fails on overlap, never replaces silently)

**Op codes for `vmo_op_range`:**

- `VMO_OP_COMMIT` (0) — eagerly allocate pages in range (requires WRITE)
- `VMO_OP_DECOMMIT` (1) — free pages in range, revert to zero-fill (requires WRITE)

**`vmo_get_info` returns (written to user buffer):**

```rust
#[repr(C)]
struct VmoInfo {
    size_pages: u64,
    flags: u64,          // VMO_CONTIGUOUS, sealed status
    type_tag: u64,
    generation: u64,     // current generation number
    committed_pages: u64, // pages with physical backing
}
```

#### Rights Integration

| Right    | Bit | Gates                                                                                      |
| -------- | --- | ------------------------------------------------------------------------------------------ |
| READ     | 0   | `vmo_read`, `vmo_map` with `VMO_MAP_READ`                                                  |
| WRITE    | 1   | `vmo_write`, `vmo_map` with `VMO_MAP_WRITE`, `vmo_snapshot`, `vmo_restore`, `vmo_op_range` |
| MAP      | 4   | `vmo_map` (required for any mapping)                                                       |
| TRANSFER | 5   | `handle_send` (existing, applies to VMO handles)                                           |
| APPEND   | 8   | `vmo_write` at offset >= committed_size only (no overwrite)                                |
| SEAL     | 9   | `vmo_seal` (consumed on use — one-way, permanent)                                          |

APPEND and SEAL are new rights (bits 8 and 9). Existing rights mask (bits 0-7) unchanged. `Rights::ALL` widened to include bits 8-9. `Rights::from_raw()` mask widened accordingly.

A handle with READ+MAP but not WRITE → read-only mapping, read-only `vmo_read`. Cannot write, cannot map writable. Cannot snapshot or restore. Rights attenuation works as always: `original & mask`, monotonically decreasing.

A handle with APPEND+MAP → can map writable (for append), can `vmo_write` only past committed_size. Cannot overwrite.

#### HandleObject Extension

```rust
pub enum HandleObject {
    Channel(ChannelId),
    Interrupt(InterruptId),
    Process(ProcessId),
    SchedulingContext(SchedulingContextId),
    Thread(ThreadId),
    Timer(TimerId),
    Vmo(VmoId),           // NEW
}
```

#### Kernel-Internal Type

```rust
struct Vmo {
    /// Per-page tracking: offset → (physical address, refcount).
    /// Absent = uncommitted (zero-fill on fault or zero-return for vmo_read).
    /// Refcount > 1 = shared with snapshots (COW on write).
    pages: BTreeMap<u64, (Pa, u32)>,

    /// Fixed at creation, in pages.
    size_pages: u64,

    /// Creation flags (CONTIGUOUS, etc.).
    flags: VmoFlags,

    /// Opaque content-type tag. Set at creation, immutable.
    type_tag: u64,

    /// Current generation number. Incremented by vmo_snapshot().
    generation: u64,

    /// COW snapshot ring. Each snapshot is a clone of `pages` at the
    /// time of the snapshot (with shared refcounts). Bounded depth.
    snapshots: VecDeque<VmoSnapshot>,

    /// Maximum snapshot depth. Oldest snapshot dropped when exceeded.
    max_snapshots: usize,

    /// True after vmo_seal(). All mutating operations rejected.
    sealed: bool,

    /// Active mappings (process, VA range) for cleanup on Drop.
    mappings: Vec<VmoMapping>,
}

struct VmoSnapshot {
    generation: u64,
    pages: BTreeMap<u64, (Pa, u32)>,
}

struct VmoMapping {
    process_id: ProcessId,
    va_base: u64,
    page_count: u64,
}
```

`impl Drop for Vmo`: unmap all active mappings (walk `mappings`, unmap from each process's address space, invalidate TLB), then walk `pages` and all snapshot page lists decrementing refcounts and freeing frames that hit zero. Compiler guarantees no dangling references to the Vmo exist when Drop runs.

#### Deprecated Syscalls

VMOs subsume two existing syscalls:

| Nr  | Syscall        | Replacement                                     |
| --- | -------------- | ----------------------------------------------- |
| 17  | `dma_alloc`    | `vmo_create(pages, VMO_CONTIGUOUS)` + `vmo_map` |
| 18  | `dma_free`     | `handle_close` (Drop unmaps + frees)            |
| 24  | `memory_share` | `vmo_create` + `vmo_map` + `handle_send`        |

These syscalls remain functional for backward compatibility during the transition. Document them as deprecated in the syscall table. Remove in Phase 6 (packaging) after userspace is migrated.

#### VMAR Extension Point (Future — Not Phase 3a)

**What's missing:** `vmo_map` maps into a single flat shared region per process. Any process that can call `vmo_map` can map anywhere in that region. There's no way to confine a component to a sub-region of the VA space.

**What VMARs would add:**

- `Vmar` kernel object type (handle-based, capability-controlled)
- Every process gets a root VMAR at creation (replaces the flat region)
- `vmar_allocate(parent, size, flags) → (child_handle, base_va)` — carve a sub-region
- `vmar_map(vmar, vmo, offset, len, flags) → va` — map VMO into a specific VMAR
- `vmar_unmap(vmar, va, len)` — unmap within a VMAR
- `vmar_destroy(vmar)` — tear down sub-region and all contained mappings

**Where they'd live in code:**

- New file: `kernel/vmar.rs` — VMAR tree, sub-allocation, overlap checking
- `address_space.rs` — root VMAR replaces bump allocators; `VmaList` becomes per-VMAR
- `syscall.rs` — 4 new syscalls (allocate, map, unmap, destroy)
- `handle.rs` — new `HandleObject::Vmar` variant

**Compatibility:** `vmo_map(handle, flags)` continues to work — it maps into the root VMAR. When VMARs arrive, `vmo_map` becomes sugar for `vmar_map(root_vmar, ...)`. No API break.

**What VMARs enable:** Composable sandboxing — hand a library or plugin a sub-VMAR, and it can only map VMOs within its designated region. The VA-space equivalent of capability confinement. Zircon uses this for component framework isolation.

**Why deferred:** Pure additive change. The VMO API works without VMARs. Implementation cost ~400-600 lines for a feature no consumer needs until they're doing in-process sandboxing.

#### Implementation Plan (8 steps, testable at each)

Each step keeps the kernel compiling and all existing tests passing. New tests added at each step.

**Step 1: Vmo type + VmoId + storage.**
New file `kernel/vmo.rs`. The `Vmo` struct, `VmoId`, `VmoFlags`, `VmoSnapshot`, `VmoMapping`. Global VMO table (similar to channel/timer tables). `Drop` impl with full cleanup. `HandleObject::Vmo(VmoId)` variant. No syscalls yet.

**Step 2: `vmo_create` + `vmo_get_info` (syscalls 30, 35).**
Creation: allocate VmoId, insert into table, insert handle with ALL rights. Contiguous flag: buddy-allocate all pages eagerly. Normal: empty BTreeMap (lazy). Type tag stored. Tests: create normal VMO, create contiguous VMO, query info (size, flags, tag, generation=0, committed=0 for normal, committed=N for contiguous), close handle (Drop frees).

**Step 3: `vmo_map` + `vmo_unmap` + fault handler (syscalls 31, 32).**
Map: rights check (MAP + READ/WRITE), find free VA in shared region, create VMA with `Backing::Vmo`, record mapping in `Vmo.mappings`. Unmap: remove VMA, unmap pages, TLB invalidate, remove from `Vmo.mappings`. Fault handler: extend `handle_fault` for `Backing::Vmo` — look up page in VMO's BTreeMap, allocate+zero-fill if absent, install PTE. Tests: create VMO, map, touch (triggers fault → page committed), verify via `vmo_get_info` (committed_pages=1), unmap, remap elsewhere.

**Step 4: `vmo_read` + `vmo_write` (syscalls 33, 34).**
Read: rights check (READ), for each page in range — if committed, copy to user buf; if uncommitted, write zeros to user buf (no allocation). Write: rights check (WRITE or APPEND), for each page — if uncommitted, allocate+insert; copy from user buf to page. APPEND check: reject writes at offset < committed_size. Tests: write data, read back (match), read uncommitted (zeros), append-only semantics.

**Step 5: `vmo_snapshot` + `vmo_restore` (syscalls 36, 37).**
Snapshot: clone `pages` BTreeMap, increment all refcounts, push to snapshot ring (drop oldest if full, decrementing refcounts). Bump generation. Restore: find snapshot by generation, swap page lists (adjusting refcounts), update PTEs for all active mappings (remap to restored pages). COW fault path: on write fault to page with refcount > 1, allocate new page, copy content, insert at refcount=1, decrement old. Tests: write data, snapshot, write new data, verify old snapshot preserved via `vmo_read`, restore, verify content reverted. Stress: rapid snapshot/write/restore cycles.

**Step 6: `vmo_seal` (syscall 38).**
Set `sealed = true`. All mutating operations (`vmo_write`, `vmo_snapshot`, `vmo_restore`, `vmo_op_range(COMMIT/DECOMMIT)`) return `PermissionDenied`. Existing mappings become effectively read-only (remap writable PTEs as read-only on seal). Tests: seal, verify all mutations fail, verify reads still work, verify existing mappings work (read-only).

**Step 7: `vmo_op_range` (syscall 39).**
COMMIT: for each page in range, if uncommitted allocate+zero-fill+insert. DECOMMIT: for each page in range, if committed and refcount=1, free frame+remove from BTreeMap; if refcount>1 just decrement (page shared with snapshot). Tests: commit range, verify no fault on access; decommit, verify re-faults to zeros; decommit shared page (verify snapshot retains it).

**Step 8: Deprecation wiring.**
Add `vmo_create` + `vmo_map` path as alternative to `dma_alloc` and `memory_share`. Mark old syscalls deprecated in DESIGN.md syscall table. Do NOT remove them — userspace migration happens separately.

#### Research References

Design informed by cross-OS comparison and research survey:

**Production systems:** Zircon VMOs (zx_vmo_create, zx_vmar_map), seL4 Untyped/frames, Mach/XNU vm_object, QNX shm_ctl, L4Re dataspaces, Linux mmap/memfd, Redox schemes.

**Research systems:** Theseus OS (ownership-typed MappedPages — OSDI '20), Twizzler (object-relative pointers — USENIX ATC '20), RedLeaf (RRef<T> typed cross-domain memory — OSDI '20), TreeSLS (capability tree checkpointing — SOSP '23), Asterinas (framework/service safety split — USENIX ATC '25).

**Key insight:** No existing microkernel combines all four novel features (ownership-typed, versioned, permission-rich, content-tagged). Each exists in isolation in research systems. This kernel composes them into a single coherent abstraction.

**3b. Pager interface (SETTLED 2026-04-01):**

VMO-level pagers: a channel attached to a VMO that receives page fault notifications. When an uncommitted page is accessed, the kernel forwards the fault to the pager instead of zero-filling. The pager resolves the fault (reads from disk, decompresses, generates content), commits the page, and tells the kernel to wake blocked threads.

**Design:** Pager as VMO attribute (Zircon-inspired, not seL4's thread-level model). Different VMOs can have different pagers — the document service pages document VMOs, the filesystem service pages file VMOs.

**Exception dispatch priority chain** (designed for future extensibility):

1. Translation fault on pager-backed VMO → dispatch to VMO pager (this phase)
2. Any exception + process has exception handler → dispatch to process handler (future phase 3d)
3. Kill the process

The extension point for process-level exception handling (phase 3d: debuggers, breakpoints, illegal instructions) is a one-line addition to the fault handler. No refactoring needed.

**Syscalls:**

| Nr  | Name          | Args                                     | Returns | Rights       |
| --- | ------------- | ---------------------------------------- | ------- | ------------ |
| 25  | vmo_set_pager | x0=vmo_handle, x1=channel_handle         | 0       | WRITE on VMO |
| 26  | pager_supply  | x0=vmo_handle, x1=offset_pages, x2=count | 0       | WRITE on VMO |

**VMO changes:**

```rust
/// Optional pager channel. Kernel sends fault offsets here.
pager: Option<ChannelId>,
/// Page offsets with pending pager requests (deduplication).
pending_faults: BTreeSet<u64>,
```

**Pager ring protocol** (in channel shared page, kernel → pager direction):

```text
Bytes [0..8]:   write_head (u64, kernel increments)
Bytes [8..16]:  read_head (u64, pager increments after consuming)
Bytes [16..]:   entries[]: u64 page offsets
                capacity = (PAGE_SIZE - 16) / 8
```

Standard SPSC ring. Kernel produces, pager consumes. Pager reads `write_head`, processes entries up to it, updates `read_head`.

**Fault handler changes:**

`handle_fault_vmo` returns `FaultResult::NeedsPager { vmo_id, page_offset, channel_id }` when it finds an uncommitted page on a pager-backed VMO. The VMO lock is released before blocking.

`user_fault_handler` dispatches: write fault offset to pager ring → signal pager channel → block thread via `scheduler::block_current_for_pager(ctx, vmo_id, page_offset)` → schedule next thread.

**Thread blocking:**

New field `pager_wait: Option<(VmoId, u64)>` on Thread. `block_current_for_pager` marks the thread as blocked with this info. `pager_supply` scans the blocked list for matching (vmo_id, page_offset) and wakes them. Woken threads re-enter the fault handler and find committed pages.

**Deduplication:** `pending_faults` BTreeSet in VMO. First fault on page N adds to set + sends to pager. Subsequent faults on page N just block (no duplicate message). `pager_supply` removes from set.

**Pager death:** Channel close → wake all pager waiters → re-fault → no pager + uncommitted → kill process. Conservative: wrong data is worse than no data.

**Unlocks:** Demand-paged filesystems, memory-mapped files, sandboxed decoders that lazily decode on access. Future: debuggers via process-level exception handler (phase 3d).

**Implementation plan (6 steps, testable at each):**

**Step 1: Data structures.** Add `pager` and `pending_faults` to Vmo. `set_pager()` and `clear_pending()` methods. Tests: set/clear pager, pending fault tracking.

**Step 2: FaultResult enum + fault handler.** Change `handle_fault` from `bool` to `FaultResult`. `handle_fault_vmo` returns `NeedsPager` for uncommitted + pager. All existing callers updated. Tests: existing demand paging still works.

**Step 3: Thread pager_wait + scheduler.** Add `pager_wait` to Thread. `block_current_for_pager` and `wake_pager_waiters` in scheduler. Tests: block/wake roundtrip.

**Step 4: `vmo_set_pager` syscall.** Attach pager channel to VMO. Rights check. Tests: set pager, verify in VMO info.

**Step 5: Pager ring + fault dispatch.** Kernel writes fault offset to pager channel ring. Signal channel. Block thread. Tests: fault on pager VMO produces ring entry.

**Step 6: `pager_supply` syscall + pager death.** Supply wakes blocked threads. Channel close kills waiters. Tests: full lifecycle.

**References:** Zircon pager (zx_pager_create, zx_pager_supply_pages), seL4 fault endpoints, Mach external memory managers.

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
    │         │                      (versioned, sealed, typed)   │
    │         │                        │                          │
    │         │                   Phase 3b (Pager) ──→ Phase 4d   │
    │         │                        │               (COW→user) │
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

Note: Phase 3a now includes kernel-level COW (generation snapshots). Phase 3b (pager) extends this to userspace-controlled paging, which subsumes §9.9 (COW kernel mechanics) — the pager becomes the policy layer. Phase 4d is eliminated if 3b ships.

---

## Success Criteria

**Minimum (the kernel is better):**

- Architecture abstraction extracted, aarch64 is a leaf implementation
- Rights attenuation and dynamic handle table
- All existing tests pass
- Kernel compiles with no OS-specific references in generic code

**Target (the kernel is standalone):**

- All of minimum + VMOs (versioned, sealed, typed, lazy+COW) + signals + pager interface
- Per-core ready queues + IPI wake
- User ASLR
- Standalone build with README and examples
- Someone unfamiliar with the project can boot it on QEMU in 5 minutes
- VMOs subsume `dma_alloc`/`dma_free`/`memory_share` — one memory abstraction

**Stretch (the kernel is "the one"):**

- Second architecture (RISC-V or x86_64) implemented by following the porting guide
- Full Rustdoc API documentation
- Syscall reference
- Published (crate or repo)

---

## Estimated Scale

| Phase                 | New/changed lines (est.)    | Design discussions needed                     |
| --------------------- | --------------------------- | --------------------------------------------- |
| 1. Arch abstraction   | ~1,500 (refactor, net ~0)   | 1 (the interface) — DONE                      |
| 2. Capabilities       | ~300–500                    | 1 (rights model) — DONE                       |
| 3a. VMOs              | ~500–700                    | 1 — DONE (versioned, sealed, typed, lazy+COW) |
| 3b–e. Other prims     | ~400–600                    | 3–4 (pager, signals, clock, inspect)          |
| 4. Security hardening | ~200–400                    | 0 (designs exist in research doc)             |
| 5. SMP scalability    | ~600–900 (scheduler rework) | 1 (per-core queue design)                     |
| 6. Packaging          | ~500 (docs, examples)       | 0                                             |

Total: ~4,000–4,600 lines of new/refactored code, 6–7 design discussions. The kernel grows from ~11K to ~15-16K lines while gaining significantly more capability.

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
