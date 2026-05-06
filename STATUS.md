# Project Status

## Current State: Kernel Complete, Userspace Next

The kernel is complete and verified. The userspace has not yet been built.

**Branch:** `kernel-verification` (ready to merge to `main`)

## Kernel

30 syscalls across 5 object types. ~28K LOC Rust. Framekernel discipline: all
`unsafe` confined to `frame/` module, enforced at compile time.

**Object types:** VMO (8 syscalls), Endpoint (2 + call/recv/reply), Event (5),
Thread (5), Address Space (2), plus handle_dup/close/info, clock_read,
system_info.

**Scheduler:** Multi-core fixed-priority preemptive, 256 levels, per-CPU
`SpinLock<PerCoreState>` (no global lock). SMP up to 8 cores.

**SMP concurrency:** Per-object locking via ConcurrentTable (per-slot
TicketLock + atomic generations). Per-CPU scheduler locks. IPI infrastructure
(GICv3 SGI for cross-core wake). Syscall dispatch as free functions accessing
global ConcurrentTable state — no global kernel lock. Atomic refcounts
(lock-free increment/decrement). Debug-mode lockdep validator (8 lock classes,
ordering verification).

**IPC:** Synchronous call/recv/reply via endpoints. Priority inheritance. Up to
128 bytes data + 4 handle transfers per message. One-shot reply caps.

**Memory:** VMOs with COW snapshots, sealing, lazy allocation, pager-backed
fault handling, cross-space mapping. 16 KiB pages (Apple Silicon native).

### Verification Summary

12-phase verification campaign (`design/kernel-verification-plan.md`):

| Phase                 | Key results                                       |
| --------------------- | ------------------------------------------------- |
| 0. Spec Review        | Interaction matrix, state machines, 16 invariants |
| 1. Unsafe Audit       | 85 blocks in 15 files, all clean                  |
| 2. Property Testing   | 33 proptests                                      |
| 3. Fuzzing            | 4 targets, 218K+ runs, zero crashes               |
| 4. Miri               | All host-target tests pass                        |
| 5. Coverage           | 96-100% on all critical files                     |
| 6. Mutation Testing   | Zero non-equivalent survivors                     |
| 7. Sanitizers         | ASan clean on all 704 tests                       |
| 8. Concurrency        | Cross-core IPC, SMP stress on 4 vCPUs             |
| 9. Error Injection    | All 12 error codes explicitly tested              |
| 10. Static Analysis   | Clippy pedantic, both targets clean               |
| 11. Bare-Metal + Perf | 14 benchmarks + 3 workloads, baselines set        |
| 12. Regression Infra  | Pre-commit + nightly gates, Makefile targets      |

**Bugs found and fixed:** 20. Discovery curve flattened to zero.

**Test suite:** 551 tests, 4 fuzz targets, 33 property tests, 16 invariant
checks, 34 bare-metal integration tests, 14 per-syscall benchmarks + 3 workload
benchmarks.

**Performance gates:** Per-benchmark statistical thresholds (P99 + 3σ) in
`kernel/bench_baselines.toml`. Regression = bug.

## SMP Remaining (Optional)

Per-object locking is structurally complete. Two items deferred:

- **Multi-core benchmarks** — `ipc_call_reply_2core` (two cores, independent
  endpoints, should show ~2x throughput). Needs multi-core thread execution
  harness.
- **HandleTable RwSpinLock** — concurrent handle lookups within the same address
  space. Currently serialized via AddressSpace slot lock; benefits only
  multi-threaded services. Requires extracting HandleTable from AddressSpace or
  adding RW mode to ConcurrentTable slot locks.

## What's Next

Build the userspace on the verified kernel. The design docs
(`design/userspace.md`, `design/architecture.md`) describe the target. Key
components:

1. **Init process** — root task that spawns all services
2. **IPC libraries** — userspace event rings and state registers built on kernel
   endpoints/events/VMOs
3. **Document pipeline** — document, layout, presenter services
4. **Render service** — Metal GPU scene graph renderer
5. **Store service** — COW filesystem with undo

The kernel's ABI is frozen. Changes driven by userspace needs will add syscalls
or extend existing ones, never break the existing interface.

## Session Resume

To resume work: read this file, check `git log --oneline` for recent commits,
read MEMORY.md for cross-session context.
