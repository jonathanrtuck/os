# Project Status

**Last updated:** 2026-05-03

## Current State

**Kernel rewrite: BLOCKING + FAST PATH COMPLETE (v0.7).** All 5 kernel
objects, 30 syscalls (all implemented — no stubs), capability-based access
control, blocking IPC, SVC fast path, lazy FP, and framekernel discipline.
241 tests passing, fuzz-tested, boot-to-userspace verified in release mode.

The previous implementation (v0.1-v0.6) is preserved at tag `v0.6-pre-rewrite`.

## v0.7 Kernel Architecture

- **5 kernel objects:** VMO, Endpoint, Event, Thread, Address Space
- **30 syscalls:** All implemented — blocking IPC (call/recv/reply),
  event_wait, thread lifecycle, space_destroy, vmo_map_into, vmo_set_pager,
  system_info
- **Framekernel discipline:** all `unsafe` confined to `frame/` module (44
  blocks), `#![deny(unsafe_code)]` enforced at crate root
- **Capability-based access control:** handle table with rights attenuation,
  generation-count revocation
- **Fixed-priority preemptive scheduler:** 4 priority levels, per-core run
  queues, round-robin within priority, priority inheritance
- **Per-CPU data:** PerCpu struct in TPIDR_EL1 (2-cycle access), current
  thread, kernel pointer, reschedule flag
- **SVC fast path:** minimal register save (~64 bytes vs 832-byte TrapFrame),
  direct dispatch via per-CPU kernel pointer (no function pointer indirection)
- **Lazy FP/SIMD:** CPACR_EL1 trap on first FP use, per-core owner tracking,
  conditional save/restore in exception path
- **Schedule/block/wake:** blocking syscalls call schedule() which picks next
  thread and context-switches. Safe dual-reference via ObjectTable::get_pair_mut
- **Interrupt-to-event bridge:** device SPIs (INTID 32+) bound to events via
  irq_bind/irq_ack
- **Init bootstrap:** separate `#![no_std]` binary compiled, objcopied, and
  embedded in kernel via `include_bytes!`. Boot-to-EL0 verified in release mode
- **Fuzz harness:** cargo-fuzz targets for single and sequential syscall fuzzing
- **Benchmark suite:** CNTVCT_EL0 cycle-count harness with 10x structural
  regression thresholds

### What's Implemented

| Module          | Description                                           | Tests |
| --------------- | ----------------------------------------------------- | ----- |
| `vmo.rs`        | VMO: pages, COW snapshots, sealing, resize, pager     | 16    |
| `handle.rs`     | Handle table: alloc/close/dup with rights attenuation  | 7     |
| `address_space` | VA allocator, mapping records, destroy lifecycle       | 20    |
| `event.rs`      | Level-triggered signal bits, waiter queue              | 14    |
| `endpoint.rs`   | Sync IPC: call/recv/reply, priority inheritance        | 16    |
| `thread.rs`     | Thread lifecycle, scheduler, multi-core run queues     | 16    |
| `sched.rs`      | Block/wake/yield/exit, context switch integration      | 6     |
| `syscall.rs`    | Dispatch table (30 syscalls), Kernel struct            | 28    |
| `fault.rs`      | Data abort handler (COW/lazy/pager classification)     | 4     |
| `irq.rs`        | Interrupt-to-event bridge                              | 22    |
| `table.rs`      | Heap-backed O(1) object table, safe dual-reference     | 6     |
| `bootstrap.rs`  | Init environment setup                                 | 8     |
| `pipeline.rs`   | Multi-service integration tests                        | 8     |
| `frame/`        | AArch64 platform (boot, MMU, GIC, timer, page tables)  | 65    |
| `types.rs`      | ID newtypes, Rights, Priority, SyscallError            | 5     |

### What's Next

1. **Page table population** — map init code/stack pages into the hardware page
   table so the CPU can actually fetch instructions from userspace VAs
2. **GIC redistributor masking** — mask device IRQ after handle_irq, unmask on
   irq_ack
3. **Service manifest** — init parses manifest and launches child services
4. **Userspace libraries** — syscall wrappers, IPC helpers, standard allocator

## Design Spec

- **Spec:** `design/research/kernel-userspace-interface.md`
- **Hardware companion:** `design/research/m4-pro-kernel-design.md`
- **Data/control plane split:** shared memory for bulk data, endpoints for
  control messages, events for synchronization

## Previous Milestones (v0.1-v0.6)

All preserved in git history. Key achievements from the prototype:

- **v0.3:** Rendering foundation (Metal GPU, scene graph, text rendering,
  animation, visual polish)
- **v0.4:** Document store (filesystem, COW snapshots, metadata queries,
  undo/redo)
- **v0.5:** Rich text (piece table, style palette, a11y roles)
- **v0.6:** Kernel (arch abstraction, capabilities, VMOs, pager, signals,
  SMP/EEVDF, ASLR, PAC/BTI)
