# kernel

Bare-metal aarch64 microkernel targeting QEMU’s `virt` machine and the [native macOS hypervisor](https://github.com/jonathanrtuck/hypervisor). Provides memory management, scheduling, IPC, and interrupt forwarding. Everything else — drivers, display pipeline, input — runs in userspace.

Boots with 4 SMP cores via PSCI, drops from EL2 to EL1, sets up the MMU with split TTBR (TTBR1 for kernel, TTBR0 per-process), and runs a preemptive EEVDF scheduler with handle-based scheduling contexts. Spawns a single init process; init orchestrates the rest (microkernel pattern). Targets aarch64 only — the assembly, page table setup, and hardware interaction are all ARM-specific.

For detailed design rationale, see [`DESIGN.md`](DESIGN.md).

## building

```sh
cd system && cargo build
```

## testing

```sh
cd system/test && cargo test -- --test-threads=1
```

~2,153 tests covering memory management, scheduling, IPC, processes, ELF loading, interrupt handling, syscalls, OOM fault injection, adversarial stress/fuzz scenarios, drawing, fonts, animation, layout, scene graph, and compositing.

### stress testing

```sh
cd system/test && ./stress.sh 45
```

Boots the kernel under QEMU and runs a sustained workload for the given number of seconds.

## features

- **SMP** — 4 cores via PSCI CPU_ON, per-core stacks/timers/GIC
  - Ticket spinlock with IRQ masking for all shared state
- **Preemptive scheduling** — EEVDF (Earliest Eligible Virtual Deadline First), tickless
  - Proportional-fair CPU sharing with latency differentiation (shorter slice = earlier deadline)
  - Scheduling contexts: handle-based kernel objects (budget/period) for temporal isolation
  - Context donation: OS service borrows editor’s context to bill rendering work correctly
  - Kernel enforces mechanism + algorithm; OS service owns policy (content-type-aware budgets)
  - Tickless idle: timer reprogrammed to next deadline, IPI for cross-core wakeup
- **Virtual memory** — 4-level page tables, per-process address spaces
  - Split TTBR: TTBR1 for kernel (shared), TTBR0 per-process (swapped on context switch)
  - W^X enforcement — no page is both writable and executable
  - Demand paging via VMAs (pages allocated on fault, not at spawn)
  - 8-bit ASID with generation-based recycling
- **Memory management** — layered allocator strategy
  - Buddy allocator for contiguous page frames (4 KiB – 8 MiB)
  - Slab caches for small kernel objects (64 – 2048 bytes, O(1))
  - Linked-list heap with coalescing for variable-size allocations
- **Processes** — ELF64 loading, per-process address spaces, full cleanup on exit
  - User code at EL0, kernel at EL1
  - 64 KiB user stack with guard page
  - Per-process handle table (256 slots, read/write rights)
- **IPC** — shared-memory channels with signal/wait notification
  - Handle-based access control, kernel-mediated creation
  - Lost-wakeup safe (persistent signal flag)
- **Devices**
  - GICv3 interrupt controller (hardware-backed on hypervisor, emulated on QEMU)
  - ARM virtual timer (CNTV\_\*, tickless, per-core PPI)
  - PL011 UART (TX, SMP-safe)
  - PL031 RTC (wall-clock time from host)
  - pvpanic (paravirtual panic notification — hypervisor captures crash report)
  - Virtio-mmio v2 device discovery, MMIO mapping, interrupt forwarding to userspace
- **Crash reporting** — panic handler outputs diagnostics via UART, then signals the hypervisor via pvpanic MMIO write. The hypervisor captures vCPU registers and serial log into a timestamped crash report. Fallback: PSCI SYSTEM_OFF for clean VM termination.

## audit

Comprehensive bug audit of all 33 `.rs` files, 2 `.S` files, and `link.ld`. Every `unsafe` site verified with a `SAFETY` comment explaining the soundness argument.

**Bugs found and fixed:**

- `align_up` integer overflow on near-`usize::MAX` addresses
- ELF `page_count` overflow on large segments
- Timer deadline arithmetic saturation (could wrap to the past)
- GIC distributor init missing barrier ordering (DSB/ISB)
- Channel `close_count` saturation (increment past maximum)
- Process slot leak on spawn failure

**Cross-file analyses produced:**

- [`LOCK-ORDERING.md`](LOCK-ORDERING.md) — maps all 13 `IrqMutex` instances, verifies no circular dependencies
- [`SAFETY-MAP.md`](SAFETY-MAP.md) — cross-cutting safety invariants (TPIDR chain, handle lifecycle, process exit, ASID, allocator routing)

## source files

```text
boot.S                   — boot trampoline, coarse page tables, EL2→EL1 drop, secondary entry
exception.S              — exception vectors, context save/restore (upper VA)
main.rs                  — kernel entry, IRQ/SVC dispatch, boot logging, memory map, pvpanic
context.rs               — CPU register state struct (matches exception.S offsets)
process.rs               — process creation from ELF binaries (demand-paged VMAs)
executable.rs            — pure functional ELF64 parser (PT_LOAD segments)
device_tree.rs           — FDT parser (discovers hardware from firmware device tree)
futex.rs                 — fast userspace mutex (PA-keyed wait table, lost-wakeup prevention)
memory.rs                — TTBR1 L3 refinement, W^X, PA/VA conversion
heap.rs                  — linked-list allocator (first-fit, coalescing, 16 MiB) + slab routing
slab.rs                  — power-of-two slab caches (64–2048B) for small kernel objects
page_allocator.rs        — buddy allocator for contiguous 2^n page frames (4 KiB–8 MiB)
address_space_id.rs      — 8-bit ASID allocator (generation-based recycling, lazy revalidation)
address_space.rs         — per-process TTBR0 page tables (4-level), demand paging fault handler
memory_region.rs         — virtual memory area tracking (sorted list, binary search lookup)
channel.rs               — IPC channels (shared memory ring buffers + signal/wait notification)
handle.rs                — per-process handle table (256 slots, read/write rights)
paging.rs                — page table constants, memory layout, user VA map
sync.rs                  — IrqMutex (ticket spinlock + IRQ masking, SMP-safe)
scheduling_algorithm.rs  — pure EEVDF algorithm (vruntime, eligibility, virtual deadline)
scheduling_context.rs    — pure budget/period logic (charge, replenish)
scheduler.rs             — SMP-aware EEVDF scheduler, scheduling context management, per-core state
thread.rs                — thread struct, state machine (Ready/Running/Blocked/Exited), scheduling fields
metrics.rs               — atomic counters (syscalls, page faults, context switches, lock contention)
process_exit.rs          — process exit notification (waitable handles)
thread_exit.rs           — thread exit notification (waitable handles)
waitable.rs              — generic WaitableRegistry<Id> (shared pattern for exit/timer/interrupt)
interrupt.rs             — interrupt registration table (IRQ forwarding to userspace handles)
syscall.rs               — syscall dispatcher (28 syscalls)
per_core.rs              — per-core data structures (online flag, core ID via MPIDR)
power.rs                 — PSCI CPU_ON wrapper (HVC #0) for secondary core boot
interrupt_controller.rs  — GICv3 distributor + redistributor (per-core init, IPI wakeup)
timer.rs                 — ARM virtual timer (tickless, per-core PPI, deadline reprogramming)
serial.rs                — PL011 UART driver (TX only, SMP-safe locking + panic-safe bypass)
memory_mapped_io.rs      — volatile MMIO helpers (read8/read32/write8/write32)
link.ld                  — kernel linker script (upper VA via TTBR1, split physical/virtual sections)
```

## scope & limitations

**Hardware target:**

- QEMU `virt` machine (aarch64) with GICv3, and the [native macOS hypervisor](https://github.com/jonathanrtuck/hypervisor) on Apple Silicon
- DTB parser discovers hardware; GIC + virtio-mmio + pvpanic addresses wired to device init
- Virtio-mmio v2 transport (requires QEMU `-global virtio-mmio.force-legacy=false`)
- 4 cores tested, up to 8 supported (via `MAX_CORES` constant)

**Current limitations:**

- Global scheduler lock (fine for ≤8 cores)
- 256-slot handle table per process (fixed, no growth)
- Linked-list heap for allocations >2 KiB (slab for ≤2 KiB)

**Not targeted:** x86_64, POSIX, network stack, hard realtime.

**Blocked on OS design decisions:** filesystem (COW required by undo architecture), full syscall surface, OS service process.

## references

- [bahree/rust-microkernel](https://github.com/bahree/rust-microkernel) — primary reference for the initial boot sequence and assembly. The boot.S structure, exception vectors, and context save/restore originated there, with modifications for W^X page table permissions, GICv3 interrupt handling, and the scheduler’s context switch model.
