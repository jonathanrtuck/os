# Project Status

**Last updated:** 2026-05-04

## Current State

**Kernel rewrite: COMPLETE (v0.7).** All 5 kernel objects, 30 syscalls
(all implemented end-to-end), capability-based access control, blocking
IPC with message data and handle transfer, multi-wait events, SVC fast
path, lazy FP, event-based interrupts, page table population, fault
resolution, and framekernel discipline. 271 tests passing, fuzz-tested,
boot-to-userspace verified in release mode. Userspace syscall library
(libsys) and service manifest ready.

The previous implementation (v0.1-v0.6) is preserved at tag `v0.6-pre-rewrite`.

## v0.7 Kernel Architecture

- **5 kernel objects:** VMO, Endpoint, Event, Thread, Address Space
- **30 syscalls:** All implemented end-to-end — blocking IPC with message
  data passthrough and handle transfer (call/recv/reply), multi-wait
  event_wait (up to 3 events), channel-event auto-signal, event-based
  interrupts (EVENT_BIND_IRQ replaces IRQ_BIND/IRQ_ACK), endpoint_bind_event,
  thread_create_in with initial handle duplication
- **Framekernel discipline:** all `unsafe` confined to `frame/` module (65
  blocks), `#![deny(unsafe_code)]` enforced at crate root
- **Capability-based access control:** handle table with rights attenuation,
  generation-count revocation
- **Fixed-priority preemptive scheduler:** 4 priority levels, per-core run
  queues, round-robin within priority, priority inheritance
- **Per-CPU data:** PerCpu struct in TPIDR_EL1 (2-cycle access), current
  thread, kernel pointer, reschedule flag
- **SVC fast path:** minimal register save (~64 bytes vs 832-byte TrapFrame),
  direct dispatch via per-CPU kernel pointer (no function pointer indirection)
- **Zero heap allocation on IPC hot paths:** WakeList, DrainList, StagedHandles
  all use inline fixed-size storage. Vec eliminated from sys_call, sys_recv,
  sys_reply, sys_event_wait, and sys_event_signal paths
- **Lazy FP/SIMD:** CPACR_EL1 trap on first FP use, per-core owner tracking,
  conditional save/restore in exception path
- **IPC message passthrough:** 128-byte messages via user memory pointers,
  no kernel-side allocation. Reply message written directly to caller's buffer
- **Handle transfer:** atomic removal from caller's table, inline staging in
  PendingCall, installation in receiver's table. Rollback on failure
- **Multi-wait events:** sys_event_wait accepts 3 handle/mask pairs, returns
  (handle_id << 32) | fired_bits, cleans up non-fired waiter registrations
- **Channel-event auto-signal:** endpoint enqueue signals bound event,
  recv clears when queue empties. ENDPOINT_BIND_EVENT syscall
- **Event-based interrupts:** EVENT_BIND_IRQ binds SPI to event bits,
  event_clear auto-acks and unmasks via GIC. No separate IRQ_ACK syscall
- **Page table population:** bootstrap allocates physical pages, copies
  init binary, maps code (RX) and stack (RW), switches page table before
  enter_userspace
- **Fault resolution:** COW copy (alloc, copy, remap, invalidate TLB),
  lazy allocation (alloc, zero, map). Pager dispatch classified
- **GIC SPI mask/unmask:** IRQ handler masks SPIs after device handler,
  event_clear unmasks via GICD_ISENABLER
- **Init bootstrap:** separate `#![no_std]` binary compiled, objcopied, and
  embedded in kernel via `include_bytes!`. Boot-to-EL0 verified in release mode
- **Userspace syscall library (libsys):** typed wrappers for all 30 syscalls,
  init rewritten to use libsys
- **Service manifest:** static manifest in init, spawn loop creates space +
  thread + initial handles per service
- **Fuzz harness:** cargo-fuzz targets for single and sequential syscall fuzzing
- **Benchmark suite:** CNTVCT_EL0 cycle-count harness with 10x structural
  regression thresholds

### What's Implemented

| Module          | Description                                           | Tests |
| --------------- | ----------------------------------------------------- | ----- |
| `vmo.rs`        | VMO: pages, COW snapshots, sealing, resize, pager     | 16    |
| `handle.rs`     | Handle table: alloc/close/dup with rights attenuation  | 7     |
| `address_space` | VA allocator, mapping records, destroy lifecycle       | 20    |
| `event.rs`      | Level-triggered signal bits, waiter queue, irq_bound   | 14    |
| `endpoint.rs`   | Sync IPC: call/recv/reply, priority inheritance        | 19    |
| `thread.rs`     | Thread lifecycle, scheduler, multi-core, multi-wait    | 16    |
| `sched.rs`      | Block/wake/yield/exit, context switch integration      | 6     |
| `syscall.rs`    | Dispatch table (30 syscalls), Kernel struct            | 41    |
| `fault.rs`      | Data abort handler: COW/lazy/pager resolution          | 4     |
| `irq.rs`        | Interrupt-to-event bridge, intids_for_event_bits       | 22    |
| `table.rs`      | Heap-backed O(1) object table, safe dual-reference     | 6     |
| `bootstrap.rs`  | Init environment setup, page table population          | 8     |
| `pipeline.rs`   | Multi-service integration tests                        | 8     |
| `frame/`        | AArch64 platform (boot, MMU, GIC, timer, page tables)  | 71    |
| `types.rs`      | ID newtypes, Rights, Priority, SyscallError            | 5     |
| `libsys/`       | Userspace syscall library (30 typed wrappers)          | —     |

### What's Next

1. **Bare-metal integration test** — boot with hypervisor, verify init
   executes syscall and exits with correct code
2. **First service** — a test service that makes an IPC call to init
3. **Compositor service** — scene graph rendering via shared VMOs
4. **Filesystem service** — COW snapshots for undo/redo

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
