# Project Status

## Current State: Kernel Complete — Userspace Next

The kernel is complete: functionally verified, performance-optimized, and ready
for userspace. All finalization items are resolved or explicitly deferred with
rationale.

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
IPC null round-trip (2-core):       3847 cyc/rtt  (was 4444, -13% from direct switch)
object churn (1-core):              5989 cyc/iter
object churn (multi-core, 4 cores): 7236 cyc/iter  scaling 3.3x / 4
cross-core wake (event ping-pong):  4788 cyc/rtt  (~2394 one-way)
```

## Kernel Finalization: Complete

All finalization items resolved. The kernel is ready for userspace.

### 1. Direct process switch for IPC — DONE

When a server is blocked in recv and a client calls, the kernel now
context-switches directly caller→server without touching the run queue.
`sched::direct_switch` marks the caller Blocked, the server Running, and
switches registers. Zero scheduler overhead for the common IPC fast path.

On the reply path, the kernel compares caller and server priorities. If the
caller should preempt (caller priority >= server's post-reply priority), it uses
`sched::wake_and_switch` to swap directly. Otherwise, normal wake.

Impact: cross-core IPC round-trip dropped from 4444 to 3847 cycles (−13.4%).

### 2. Topology hints — DEFERRED

`set_affinity` stores Performance/Efficiency/Any hints but nothing reads them.
On M4 Pro bare metal these map to P-core and E-core clusters. Under the
hypervisor (4 identical vCPUs) they have no effect. Per the "unverifiable work
does not ship" rule, this is deferred until bare-metal testing is available. The
syscall and storage are in place; only the scheduler read path is missing.

### 3. Benchmark baselines — CHECKED

All 17 benchmarks pass regression thresholds after the Endpoint struct
optimization, thread load balancing, and direct switch changes. SMP benchmarks
confirm 3.3x/4 scaling. `make bench-check` is green.

### 4. HandleTable RwSpinLock — DEFERRED

Concurrent handle lookups within the same address space are serialized by the
AddressSpace slot lock. An RwSpinLock would allow parallel reads. This is an
internal optimization with no ABI impact — it can be added when multi-threaded
userspace workloads reveal contention. Current SMP scaling (3.3x/4) suggests the
slot lock is not the primary bottleneck.

## What's Next: Userspace

Build the userspace on the verified kernel. The design docs
(`design/userspace.md`, `design/architecture.md`) describe the target. Key
components:

1. **Init process** — root task that spawns all services
2. **IPC libraries** — userspace event rings and state registers built on kernel
   endpoints/events/VMOs (started: `userspace/ipc/`)
3. **Document pipeline** — document, layout, presenter services
4. **Render service** — Metal GPU scene graph renderer
5. **Store service** — COW filesystem with undo

The kernel's ABI is frozen. Changes driven by userspace needs will add syscalls
or extend existing ones, never break the existing interface.

## Session Resume

To resume work: read this file, check `git log --oneline` for recent commits,
read MEMORY.md for cross-session context. The next task is building the init
process and continuing the IPC library (`userspace/ipc/`).
