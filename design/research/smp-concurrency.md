# SMP Concurrency Model

How the kernel should protect shared state when multiple cores execute syscalls
concurrently. This document evaluates five approaches against this project's
constraints (capability-based microkernel, synchronous IPC, M4 Pro hardware) and
recommends a strategy.

## Current State: Implicit Big Kernel Lock

The `Kernel` struct owns all mutable state — five object tables, the scheduler,
and the IRQ table. Every syscall enters through
`Kernel::dispatch(&mut self, ...)`, and the `&mut self` borrow constitutes the
serialization point.

Today this works because **only the BSP (core 0) runs userspace threads**.
Secondary cores boot, initialize their `PerCpu` slot, and idle. The raw pointer
dereference in `svc_fast_handler` (`&mut *(pc.kernel_ptr as *mut Kernel)`) would
be instant undefined behavior if two cores entered the kernel simultaneously —
two `&mut` references to the same object.

```text
svc_fast_handler (exception.rs:311)
  → percpu_mut() → kernel_ptr (raw ptr, no lock)
  → &mut Kernel → dispatch(&mut self, ...)
```

The scheduler has per-core `RunQueue` instances, but they are fields of
`Scheduler`, which is a field of `Kernel`. Accessing any core's run queue
requires `&mut Kernel`.

### What needs protecting

| State                   | Access pattern                   | Contention profile               |
| ----------------------- | -------------------------------- | -------------------------------- |
| HandleTable (per-space) | Lookup on every syscall          | Hot reads, rare writes           |
| Endpoint                | Call/recv/reply (IPC fast path)  | Highest contention in a μkernel  |
| Scheduler               | Wake/block/pick_next (every IPC) | Per-core queues, cross-core wake |
| VMO pages               | Fault handler, map/unmap         | Moderate, bursty                 |
| Event                   | Signal/wait                      | Moderate                         |
| Thread                  | State changes, priority boost    | Tied to IPC                      |
| AddressSpace            | Mapping changes, ASID allocation | Rare after init                  |

The IPC fast path dominates: `sys_call` touches the caller's handle table, the
endpoint, the server thread's state, the scheduler (block caller, wake server),
and potentially the server's handle table (handle transfer). A single IPC round
trip reads or mutates **5+ kernel objects across 2 address spaces**.

---

## The Design Space

### 1. Explicit BKL (CLH queue lock)

Wrap all kernel entry in a single CLH (Craig-Landin-Hagersten) queue lock. The
`&mut Kernel` pattern remains; the lock makes it sound.

**Mechanism:** Before dereferencing `kernel_ptr`, each core acquires the CLH
lock. The CLH queue is a linked list of cache-line-aligned nodes — each core
spins on its predecessor's node, so the cache line bounces only between two
cores at a time (unlike test-and-set, which causes O(n) traffic).

**Evidence (Peters, Danis, Elphinstone — "For a Microkernel, a BIG Lock is
Fine", 2016):**

| Metric                        | BKL (CLH)  | Fine-grained | BKL overhead |
| ----------------------------- | ---------- | ------------ | ------------ |
| IPC fast path, 1 core, ARM    | +23%       | +70%         | 6 DMBs       |
| IPC fast path, 1 core, x86    | +3%        | +20%         | (implicit)   |
| Scalability, ARM Cortex-A9    | Perfect ≤4 | Better ≥5    | —            |
| Scalability, x86 28-core Xeon | Peak ≈5    | Peak ≈12     | —            |

On ARM, each CLH acquire/release costs ~6 DMB instructions. At ~3 cycles each on
M4 Pro, that is ~18 cycles of barrier overhead per syscall, plus the spin wait
when contended (cross-cluster: 50–70 cycles per cache-line transfer via SLC).

**Strengths:**

- Zero lock ordering concerns, zero deadlock risk
- No code changes to syscall handlers — just wrap dispatch entry
- Easiest to verify formally (seL4 chose this for their verified kernel)
- Clean baseline for measuring real contention before optimizing

**Weaknesses:**

- 14 cores contending on one lock with IPC-dominant workloads will serialize
  badly — the seL4 paper shows throughput collapse above ~5 cores
- Cross-cluster CLH handoff (P0→P1 or P→E) costs 50–70 cycles via SLC
- Priority inversion: a low-priority thread holding the BKL blocks all
  high-priority threads on every core

**Decomposition path:** Linux took 15 years (1996–2011) to remove its BKL. The
key lesson: Linux's BKL had release-on-sleep semantics (dropped when a thread
blocked, reacquired on wakeup), which created implicit dependencies impossible
to audit mechanically. A CLH lock without release-on-sleep is cleaner to
decompose because all critical sections are explicit.

---

### 2. Per-Object Locks (Zircon model)

Each kernel object gets its own lock. The kernel entry does not acquire a global
lock; instead, each syscall handler locks only the objects it touches.

**Mechanism (Zircon's actual implementation):**

| Object      | Lock type | Rationale                         |
| ----------- | --------- | --------------------------------- |
| HandleTable | RW + PI   | Hot reads (lookup), rare writes   |
| Dispatcher  | Mutex     | Per-object state changes          |
| Scheduler   | Per-CPU   | No lock — each CPU owns its queue |

Zircon uses `BrwLockPi` (reader-writer lock with priority inheritance) on the
per-process handle table — reader mode for concurrent lookups, writer mode for
creation/deletion. Each kernel object (Dispatcher) has its own `fbl::Mutex`.

**IPC fast path lock sequence:** process handle table (reader) → endpoint lock →
server thread lock → scheduler (per-CPU, no lock).

**Lock ordering:** Zircon maintains dozens of orderings enforced by a runtime
lockdep validator that tracks a directed graph of lock acquisitions. Same-class
locks use memory address ordering (acquire lower-addressed lock first).

**Strengths:**

- Maximum concurrency — independent IPC to different endpoints is fully parallel
- Per-CPU scheduler requires no cross-core synchronization for local operations
- Scales well to many cores

**Weaknesses:**

- Lock ordering complexity is proportional to the number of object types × the
  number of syscalls. For our 5 object types and 30 syscalls, the ordering graph
  is manageable but non-trivial.
- Fine-grained locking on ARM is expensive: the seL4 paper measured **+70%**
  overhead on the IPC fast path (16 DMBs per round trip). On M4 Pro at ~3
  cycles/DMB, that is ~48 cycles of pure barrier cost.
- Priority inheritance across multiple locks is complex. Zircon limits futex PI
  to single ownership (no transitive chains).
- Essentially impossible to verify formally — Zircon has no verified concurrency
  properties.
- Data structure surgery: our flat `ObjectTable<T>` arrays must become
  individually lockable, breaking the `&mut self` borrow pattern.

**Adaptation to this kernel:** Our handle table is already per-address-space
(good — no global table). The main structural change would be replacing the
single `Kernel` struct with individually-locked object tables and a lock-free
per-CPU scheduler. Each `ObjectTable<T>` would need a concurrent slot allocator
and per-slot or per-slab locking.

---

### 3. Per-Core Kernel Instances (seL4 multikernel)

Each core runs its own kernel instance with its own copy of the data it manages.
Cross-core coordination uses explicit IPI messages. No shared mutable kernel
state.

**Mechanism (seL4 RFC 0170, implemented):**

- Each core owns a private capability space (CNode tree), scheduler, and page
  tables
- Cross-core capability transfer: the source core sends an IPI with the
  capability description; the destination core installs it locally
- `IRQControl` capability creates `SGISignalCap` capabilities — each targets a
  single remote core. 16 SGI IDs per core pair on GICv3.
- Shared memory between processes on different cores: both cores map the same
  physical pages, but the kernel objects (VMOs, address spaces) are local to
  each core's instance

**Strengths:**

- Zero lock contention — each core's kernel is a sequential program
- Maximum isolation — a bug on core 0 cannot corrupt core 1's kernel state
- Verification-friendly — single-core proofs apply per instance
- Maps well to Apple Silicon's cluster topology (P-cluster 0, P-cluster 1,
  E-cluster could each be a "kernel island")

**Weaknesses:**

- Cross-core IPC adds a message hop: ~600–1000+ cycles vs ~400 for intra-core
  (seL4 benchmarks). For a microkernel where IPC is the critical path, this is a
  significant tax on any cross-core service call.
- State replication: kernel objects like endpoints that are shared across
  processes on different cores must be coordinated via messages. An endpoint
  with a server on core 0 and clients on cores 1–13 requires 13 IPI round trips
  to distribute.
- Significant infrastructure: need IPI dispatch, message formats, per-core
  capability migration protocol, cross-core page table management
- The scheduler becomes topology-aware by necessity — you want communicating
  threads on the same core to avoid the IPI penalty, which is a hard scheduling
  problem (co-scheduling / gang scheduling)

**Current seL4 status:** Proofcraft presented progress toward a verified static
multikernel at seL4 Summit 2024. This is seL4's agreed long-term path. The
earlier BKL-based SMP kernel (seL4 12.0.0+) ships today with a CLH lock.

---

### 4. Per-Cluster BKL

One BKL per hardware cluster. The scheduler is topology-aware, placing
communicating threads in the same cluster when possible. Intra-cluster IPC
serializes only against threads in the same cluster (5-way contention at worst).
Cross-cluster IPC acquires both cluster locks.

**Mechanism (novel — no precedent system ships this):**

- 3 CLH locks, one per cluster: `P0_LOCK`, `P1_LOCK`, `E_LOCK`
- Each cluster owns a partition of kernel state. Object tables are global but
  each object has a home cluster.
- Intra-cluster syscall: acquire home cluster's lock
- Cross-cluster syscall (e.g., IPC between P0 and P1): acquire both locks in
  fixed order (P0 < P1 < E)

| Scenario                     | Contention | Lock acquire cost   |
| ---------------------------- | ---------- | ------------------- |
| Client/server same P-cluster | 5 cores    | ~18 cycles (L2)     |
| Client on P0, server on P1   | 10 cores   | ~50–70 cycles (SLC) |
| Client on P, server on E     | 14 cores   | ~50–70 cycles (SLC) |

**Strengths:**

- Maps directly to M4 Pro's physical topology
- Intra-cluster IPC sees only L2-speed contention (5 cores — well within the
  seL4 paper's "BKL is fine" range)
- Simpler than per-object locking (3 locks vs dozens)
- Progressive path: start with 1 BKL, split to 3 when contention data proves the
  need

**Weaknesses:**

- Cross-cluster operations require two locks → deadlock risk (manageable with
  fixed ordering, but still 2× acquire cost)
- Object "home cluster" assignment is a new concept that complicates object
  creation and migration
- Scheduler must be topology-aware to realize the benefit (otherwise random
  placement makes most IPC cross-cluster)
- No precedent — untested in production. Closest analog is NUMA-aware locking in
  Linux, but that evolved from per-subsystem locks, not from a BKL.
- Hardware-specific — the strategy is meaningless on a machine with different
  topology

---

### 5. QNX-Style Preemptible Single-Thread

One thread in the kernel at a time, enforced by a single lock, but the kernel is
fully preemptible. If a higher-priority thread needs the kernel, the current
in-kernel thread is preempted and its operation restarted.

**Mechanism (QNX Neutrino SMP):**

- Single kernel lock (like BKL) but with preemption
- When a core receives an IPI indicating a higher-priority thread is ready, the
  current kernel-mode thread yields the kernel lock
- The preempted thread's in-progress operation is abandoned and will restart
  from the beginning when it reacquires the lock
- Most kernel operations complete in microseconds, so restarts are rare in
  practice

**QNX's argument:** For a true microkernel, kernel time is a small fraction of
total CPU time. The kernel handles only IPC, scheduling, and interrupt dispatch.
Placing many locks on the fast path slows the common case more than occasional
contention costs on a single lock.

**Strengths:**

- Zero lock ordering concerns (single lock)
- Priority-aware: high-priority work preempts low-priority kernel work, unlike a
  pure BKL where the holder runs to completion
- Low complexity — simpler than fine-grained, with better priority properties
  than a basic BKL

**Weaknesses:**

- Still serializes everything — same throughput ceiling as BKL
- Kernel operations must be idempotent or restartable, which constrains how
  syscall handlers are written. A partially-completed handle transfer, priority
  boost, or endpoint state change must either complete atomically or be safely
  abandonable.
- Restart cost: if a high-priority IPC preempts a low-priority VMO fault
  mid-page-walk, the page walk restarts from scratch. For short operations (IPC)
  this is fine; for longer ones (bulk VMO mapping) the wasted work adds up.
- Requires IPI infrastructure for cross-core preemption notification

---

## ARM64 / M4 Pro Specifics

These hardware facts constrain the design (see `m4-pro-hardware.md` for full
reference):

**Barrier costs (M1 Firestorm, expected ±1–2 cycles on M4):**

| Instruction | Cycles | Use                               |
| ----------- | ------ | --------------------------------- |
| DMB         | ~3     | Data memory barrier (all forms)   |
| DSB         | 17     | Data sync barrier (after TLBI)    |
| ISB         | 28     | Instruction sync (pipeline flush) |
| CASAL       | 7      | Compare-and-swap, acq/rel         |
| LDAPR       | ~3     | Load-acquire (RCPC)               |
| STLR        | ~3     | Store-release                     |

A CLH lock acquire involves at minimum: 1 CASAL (7 cycles) + spin on predecessor
(0 if uncontended, 50–70 cycles cross-cluster via SLC) + 1 DMB (~3 cycles).
Release: 1 STLR (~3 cycles) + SEV (1 cycle). Minimum uncontended: ~14 cycles.
Contended across clusters: ~80 cycles.

**Cache topology:**

- 128-byte cache lines (L2/SLC level, 64 bytes at L1)
- Intra-cluster L2 hit: ~18 cycles
- Cross-cluster via SLC: 50–70 cycles
- DRAM: ~340 cycles
- False sharing at 128 bytes is the primary concern — two locks within 128 bytes
  cause cross-cluster cache-line bouncing

**TLBI:** Hardware broadcast. No IPI needed for TLB shootdowns. Cost: DSB (17
cycles) + ISB (28 cycles) = 45 cycles minimum on the issuing core, regardless of
core count. This is a significant advantage over x86 and other ARM systems that
require software-managed shootdowns.

**Memory ordering:** ARMv8 weak ordering with LDAR/STLR for acquire/release. TSO
mode available per-core (~9% slower) but irrelevant for kernel code using
explicit barriers.

---

## Analysis: Eliminating by Performance

All five approaches can be made correct. The filter is throughput on 14 cores
with IPC-dominant workloads.

### Eliminated: BKL and QNX preemptible

Both serialize all kernel entry to one thread at a time. On 14 cores, this means
13 cores spin while 1 works. The seL4 paper shows throughput collapse above ~5
cores. QNX's preemptibility improves priority behavior but does not improve
throughput — only one core executes kernel code at a time. These models treat
the kernel as a sequential bottleneck and accept the cost. With 14 cores, the
cost is unacceptable.

### Eliminated: Per-cluster BKL

Three locks (one per M4 Pro cluster) improve over the global BKL, but within a
cluster, all IPC still serializes. Two cores on the same P-cluster doing
independent IPC to different endpoints wait on each other. The maximum intra-
cluster parallelism is 1. Per-object locking's maximum intra-cluster parallelism
is 5 (one per core). Strictly inferior.

### Eliminated: Per-core multikernel

Zero contention intra-core, but every cross-core IPC pays ~600–1000 cycles for
the IPI round trip. In a document-centric OS, shared services are the norm: font
server, document store, theme service, accessibility service. A font server on
core 3 serving requests from 10 other cores means every request pays the IPI
penalty. Per-object locking would let those 10 cores contend only on the font
endpoint's lock (~100 cycles held, ticket-fair handoff). The multikernel pays
6–10× more per cross-core call to a shared service.

The multikernel's advantage — zero overhead for intra-core IPC — requires a
scheduler that perfectly co-locates communicating threads. With shared services,
perfect co-location is impossible: the font server cannot be on every core
simultaneously. The performance advantage is conditional on a scheduling
property that cannot be guaranteed for this workload.

### Winner: Per-object locking

The only model that provides full parallelism for independent operations. 14
cores doing IPC to 14 different endpoints run without any mutual interference.

**The +70% ARM overhead argument (seL4 paper) does not apply.** That measurement
compares fine-grained locking against a _lockless single-core baseline_. Our
baseline is not lockless — it is a BKL that serializes everything. The correct
comparison is:

| Scenario (14 cores, independent IPC)    | BKL           | Per-object           |
| --------------------------------------- | ------------- | -------------------- |
| Kernel throughput                       | 1× (serial)   | up to 14× (parallel) |
| Per-call overhead (uncontended)         | ~14 cycles    | ~48 cycles           |
| Per-call overhead (13 cores contending) | ~1000+ cycles | ~48 cycles           |

Per-object locking costs +34 cycles per call on the uncontended path relative to
the BKL. But the BKL's uncontended path only exists when 13 of 14 cores are
idle. Under any real load, the BKL's contention cost dwarfs the barrier
overhead.

**IPC fast path lock sequence (adapted from Zircon):**

```text
sys_call:
  1. Caller HandleTable — RW lock, reader mode (concurrent lookups)
  2. Endpoint — SpinLock (serializes access to this endpoint)
  3. Server Thread — SpinLock (wake + priority boost)
  4. Scheduler — per-CPU, no lock (each core owns its RunQueue)

sys_reply:
  1. Server HandleTable — RW lock, writer mode (consume reply cap)
  2. Caller Thread — SpinLock (write reply + wake)
  3. Scheduler — per-CPU, no lock
```

**Lock ordering:** Fixed, acyclic: HandleTable → Endpoint → Thread → Scheduler.
No cycles possible. Same-type locks (two HandleTables, two Threads) use object
ID ordering — lower ID acquired first.

### Against the design principles

- **"Simple everywhere"** — The concurrency model is not simple. But "simple"
  means the _interfaces_ are simple; essential complexity belongs in leaf nodes.
  The kernel's internal locking is a leaf node — it is invisible to syscall
  callers. The syscall ABI does not change. Userspace sees the same interface
  whether the kernel uses a BKL or per-object locks.
- **"Essential complexity pushed into leaf nodes"** — This is exactly that. The
  complexity of fine-grained locking is confined inside the kernel's dispatch
  layer. Every service, every userspace library, every application is
  unaffected.
- **"Isolate uncertain decisions behind interfaces"** — The lock discipline is
  entirely internal. If it proves wrong, the syscall ABI still holds. The
  kernel's internal architecture can be reworked without breaking userspace.

---

## Decision: Per-Object Locking

Per-object locking, modeled on Zircon's architecture, adapted to this kernel's
data structures. Not as a future phase. As the SMP concurrency model.

### Structural changes

The `Kernel` struct dissolves. Its fields become individually-protected global
state:

```text
Current:
  Kernel { vmos, events, endpoints, threads, spaces, scheduler, irqs }
  → All behind &mut self (implicit BKL)

Target:
  ObjectTable<Vmo>       → RW lock on table, SpinLock per Vmo
  ObjectTable<Event>     → RW lock on table, SpinLock per Event
  ObjectTable<Endpoint>  → RW lock on table, SpinLock per Endpoint
  ObjectTable<Thread>    → RW lock on table, SpinLock per Thread
  ObjectTable<AddressSpace> → RW lock on table, SpinLock per AddressSpace
  Scheduler              → per-CPU RunQueue (no lock, CPU-local)
  IrqTable               → SpinLock (rare access)
```

Each `ObjectTable<T>` has two levels of protection:

1. **Table-level RW lock:** Reader mode for slot lookup (the common path — every
   syscall). Writer mode for slot allocation and deallocation (rare — only
   object creation and destruction). Reader mode is concurrent across all cores.
2. **Per-object SpinLock:** Protects the object's internal state. Acquired after
   the table lookup, so the table lock is not held while the object lock is held
   (no nested locking between table and object).

The HandleTable (already per-AddressSpace) gets a RW lock with priority
inheritance, following Zircon's `BrwLockPi` pattern. Handle lookup takes reader
mode; handle creation/deletion takes writer mode.

### Scheduler becomes CPU-local

The scheduler is the most latency-sensitive component — it runs on every IPC
(block caller, wake server, pick next thread). Making it lock-free for the
local-CPU path eliminates contention entirely for the common case.

Each core owns its `RunQueue`. The `Scheduler` struct becomes a thin coordinator
for cross-core wake:

- **Local operations** (`pick_next`, `block_current`, `rotate`): No lock. Called
  only by the current core's syscall handler or timer ISR.
- **Cross-core wake** (`wake(thread, target_core)`): Acquire target core's
  `RunQueue` SpinLock, enqueue thread, release lock, send IPI. The target core's
  IPI handler checks `reschedule_pending` and context-switches if a higher-
  priority thread is now queued.
- **IPI mechanism:** Use the existing `PerCpu::reschedule_pending` field
  (already allocated, currently unused). ARM GICv3 SGI for the interrupt. IPI
  handler in the exception vector.

### Lock discipline

**Global lock ordering (lower number acquired first):**

```text
1. HandleTable (RW, per-AddressSpace) — by AddressSpace ID
2. ObjectTable<Endpoint> (RW, reader mode)
3. Endpoint (SpinLock) — by EndpointId
4. ObjectTable<Thread> (RW, reader mode)
5. Thread (SpinLock) — by ThreadId
6. RunQueue (SpinLock, per-CPU) — by core_id
7. ObjectTable<Vmo> (RW, reader mode)
8. Vmo (SpinLock) — by VmoId
9. ObjectTable<Event> (RW, reader mode)
10. Event (SpinLock) — by EventId
11. ObjectTable<AddressSpace> (RW, reader mode)
12. AddressSpace (SpinLock) — by AddressSpaceId
13. IrqTable (SpinLock)
```

Same-type locks: acquire in ascending object ID order. This is the same strategy
Zircon uses (memory address ordering) adapted to our ID-indexed tables.

**No table writer locks on the IPC path.** The IPC fast path (call/recv/reply)
only creates/destroys reply caps (HandleTable writer lock) and looks up existing
objects (table reader locks). Object creation/destruction happens outside the
fast path.

### Reader-writer lock with priority inheritance

The HandleTable RW lock needs priority inheritance to avoid unbounded priority
inversion. A high-priority IPC call that needs a handle lookup must not be
blocked indefinitely by a low-priority thread that is creating handles.

Implementation: track the highest-priority waiter. When a reader holds the lock
and a writer is waiting, boost all current readers to the writer's priority.
When the writer holds the lock and readers are waiting, boost the writer to the
highest waiting reader's priority.

This follows Zircon's `BrwLockPi` pattern. It is the most complex single
component in the locking design, but it is confined to one type (`RwLockPi`) and
used in one place (HandleTable).

### The dispatch rewrite

The `dispatch(&mut self, ...)` method signature changes. Instead of taking
`&mut Kernel`, each `sys_*` handler becomes a free function that acquires the
specific locks it needs:

```rust
// Before: everything through &mut self
impl Kernel {
    fn sys_call(&mut self, current: ThreadId, args: &[u64; 6]) -> Result<...>
}

// After: each handler acquires what it needs
fn sys_call(current: ThreadId, args: &[u64; 6]) -> Result<...> {
    let space = current_space(current);       // per-CPU, no lock
    let ep_handle = space.handles.read()      // HandleTable reader lock
        .lookup(args[0] as u32)?;
    let ep = ENDPOINTS.read()                 // ObjectTable reader lock
        .get(ep_handle.object_id)?;
    let mut ep_guard = ep.lock();             // Endpoint SpinLock
    // ... transfer message, wake server, block caller
}
```

### IPI infrastructure

Required for cross-core wake (scheduler) and cross-core preemption (priority
inheritance). Minimal implementation:

1. **GICv3 SGI send:** Write to `ICC_SGI1R_EL1` with target core's affinity and
   SGI ID. One SGI ID for reschedule, one for generic kernel IPI.
2. **SGI handler:** In the IRQ exception vector, read `ICC_IAR1_EL1` to
   acknowledge. If reschedule SGI: set `reschedule_pending`, return to
   context-switch check. If kernel IPI: dispatch to per-core work queue.
3. **GICv3 init per core:** Already done during secondary core boot
   (`init_gic_percpu`). Add SGI routing configuration.

### Contention instrumentation

Per-lock contention counters from the start. Each SpinLock and RwLock tracks:

- `wait_cycles`: cumulative cycles spent spinning (CNTVCT_EL0 delta)
- `acquires`: total acquisitions
- `contentions`: acquisitions that had to spin (wait_cycles > 0)

Expose via `SYSTEM_INFO` subcodes or a dedicated `PERF_INFO` syscall. This data
validates the lock ordering and granularity choices in production.

---

## References

- Peters, Danis, Elphinstone. "For a Microkernel, a Big Lock Is Fine." APSys
  2015 / arXiv:1609.08372. Authoritative BKL-vs-fine-grained comparison on seL4.
- seL4 multikernel IPI API: RFC 0170
  (sel4.github.io/rfcs/implemented/0170-multikernel-ipi-api.html)
- seL4 CLH lock correctness: "Practical Rely/Guarantee Verification of seL4
  Lock" (arXiv:2407.20559)
- Zircon lockdep: fuchsia.dev/fuchsia-src/concepts/kernel/lockdep-design
- QNX Neutrino SMP: qnx.com developers/docs — "How the SMP microkernel works"
- Barrelfish: "The Multikernel: A New OS Architecture for Scalable Multicore
  Systems" (SOSP 2009)
- Shapiro et al. "EROS: A Fast Capability System" (SOSP 1999)
- M4 Pro hardware specifics: `design/research/m4-pro-hardware.md`
- Barrier/instruction timings: Dougall Johnson, Apple M1 Firestorm
  (dougallj.github.io/applecpu/firestorm-int.html)
