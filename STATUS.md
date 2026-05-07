# Project Status

## Current State: Kernel Finalization In Progress

The kernel is functionally complete and verified. A finalization pass is in
progress to bring IPC performance to the theoretical optimum before moving to
userspace.

**Branch:** `main`

## Kernel

30 syscalls across 5 object types. ~28K LOC Rust. Framekernel discipline: all
`unsafe` confined to `frame/` module, enforced at compile time.

**Object types:** VMO (8 syscalls), Endpoint (2 + call/recv/reply), Event (5),
Thread (5), Address Space (2), plus handle_dup/close/info, clock_read,
system_info.

**Scheduler:** Multi-core fixed-priority preemptive, 256 levels, per-CPU
`SpinLock<PerCoreState>` (no global lock). SMP up to 8 cores. Thread creation
load-balanced via `least_loaded_core()` with IPI to wake remote cores.

**SMP concurrency:** Per-object locking via ConcurrentTable (per-slot
TicketLock + atomic generations). Per-CPU scheduler locks. IPI infrastructure
(GICv3 SGI for cross-core wake). Syscall dispatch as free functions accessing
global ConcurrentTable state — no global kernel lock. Atomic refcounts
(lock-free increment/decrement). Debug-mode lockdep validator (8 lock classes,
ordering verification).

**IPC:** Synchronous call/recv/reply via endpoints. Priority inheritance. Up to
128 bytes data + 4 handle transfers per message. One-shot reply caps. Badge
passed from caller's handle.

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

**Bugs found and fixed:** 24 total (20 during verification, 4 during
finalization — IPC reply cap delivery, thread placement, badge wiring,
thread_exit ordering).

**Test suite:** 551 tests, 4 fuzz targets, 33 property tests, 16 invariant
checks, 34 bare-metal integration tests, 14 per-syscall benchmarks + 3 workload
benchmarks + 3 SMP benchmarks.

**Performance gates:** Per-benchmark statistical thresholds (P99 + 3σ) in
`kernel/bench_baselines.toml`. Regression = bug.

### SMP benchmark results (2026-05-06, 4 cores under hypervisor)

```text
IPC null round-trip (2-core):       4444 cyc/rtt
object churn (1-core):              5979 cyc/iter
object churn (multi-core, 4 cores): 7227 cyc/iter  scaling 3.3x / 4
cross-core wake (event ping-pong):  4795 cyc/rtt  (~2398 one-way)
```

## Kernel Finalization: Remaining Work

The goal is a kernel that never needs revisiting. These items must be completed
before starting userspace.

### 1. Direct process switch for IPC (next)

When a client calls and a server is waiting, the kernel currently enqueues the
server on the run queue, blocks the client, picks the next thread from the
queue, and context-switches. This round-trips through the scheduler when the
kernel already knows it wants to switch to the server.

Direct process switch skips the scheduler entirely: save client registers, load
server registers, done. This is the highest-performance correct implementation
for synchronous IPC. seL4 does this. The current IPC round-trip is ~4444 cycles;
direct switch should significantly reduce it.

The reply path has a similar opportunity: if the server will immediately recv
again, the kernel could switch directly back to the caller instead of going
through the scheduler.

### 2. Topology hints

`set_affinity` stores Performance/Efficiency/Any hints but nothing reads them.
On M4 Pro bare metal these map to P-core and E-core clusters. Under the
hypervisor (4 identical vCPUs) they have no effect, making this unverifiable
without real hardware. This may be deferred with an explicit rationale per the
"unverifiable work does not ship" rule.

### 3. Benchmark baselines

Single-core baselines in `bench_baselines.toml` need re-running after the
Endpoint struct optimization (2432→352 bytes) and thread load balancing. Run
`make bench-check` and update the baselines. Add SMP benchmark baselines from
`make bench-smp`.

### 4. HandleTable RwSpinLock decision

Concurrent handle lookups within the same address space are serialized by the
AddressSpace slot lock. A dedicated RwSpinLock would allow parallel lookups. The
bench-smp data doesn't isolate this — a targeted benchmark measuring handle
lookup scaling would inform the decision.

## What's After Finalization

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
read MEMORY.md for cross-session context. The next task is item 1 above: direct
process switch for IPC.
