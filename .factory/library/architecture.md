# Architecture

**What belongs here:** Architectural decisions, patterns discovered, subsystem relationships.

---

## Kernel Subsystems

Detailed rationale in `system/kernel/DESIGN.md` (1462 lines). Key subsystems:

### Memory (boot.S, memory.rs, paging.rs, page_allocator.rs, heap.rs, slab.rs)

- Split TTBR: TTBR1 for kernel (upper VA), TTBR0 for user (lower VA, swapped on context switch)
- Three-tier allocation: slab (<=2KiB), linked-list (variable), buddy (page frames)
- W^X enforced: .text RX, .rodata RO, .data/.bss RW
- Dealloc routes by address range, not size class (prevents cross-allocator contamination)

### Process (process.rs, process_exit.rs, address_space.rs, address_space_id.rs, executable.rs)

- ELF64 loading with demand paging (first code page + top stack page eagerly mapped)
- ASID-based TLB isolation (8-bit ASID pool)
- Full cleanup on exit: drain handles, invalidate TLB, free pages + page tables, release ASID

### Scheduling (scheduler.rs, scheduling_algorithm.rs, scheduling_context.rs, thread.rs, per_core.rs)

- EEVDF algorithm (Earliest Eligible Virtual Deadline First)
- 4 SMP cores, per-core idle threads
- Deferred thread drops (free kernel stack after switching away, not during)
- Scheduling contexts for per-content-type CPU budgets

### Synchronization (sync.rs, futex.rs, waitable.rs, channel.rs, handle.rs)

- IrqMutex: spinlock with interrupt masking (DAIF manipulation)
- Ring buffer IPC channels (2 pages per channel, 64-byte fixed messages)
- Waitable abstraction: channels, timers, process exit, thread exit, interrupts
- Handle table per process (typed handles: channel endpoints, timers, processes)

### Hardware (interrupt.rs, interrupt_controller.rs, timer.rs, device_tree.rs, serial.rs, power.rs)

- GICv2 interrupt controller
- ARM generic timer (250 Hz tick)
- FDT device tree parser
- PSCI for SMP boot

### Syscall (syscall.rs)

- 27 syscalls via SVC instruction
- Context passed as raw pointer (aliasing fix from Fix 5)
- User pointer validation via AT S1E0R + PAR_EL1

## Lock Ordering (to be mapped during milestone 6)

Known locks:

- `scheduler::STATE` (IrqMutex) — the big lock
- `channel::CHANNELS` (IrqMutex)
- `timer::TIMERS` (IrqMutex)
- `process_exit::STATE` (IrqMutex)
- `thread_exit::STATE` (IrqMutex)
- Various per-subsystem IrqMutex instances

All use IrqMutex (interrupt-masking spinlock). Ordering must be verified.
