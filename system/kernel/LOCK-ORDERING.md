# Kernel Lock Ordering

Cross-file analysis of every lock in the kernel. Documents all `IrqMutex` instances,
acquisition order constraints, and interrupt-safety properties.

**Audit date:** 2026-03-11
**Scope:** All 35 source files in `system/kernel/`
**Method:** Traced every `IrqMutex` static, every `.lock()` call, and every code path
that acquires multiple locks (directly or transitively).

---

## 1. Lock Inventory

All locks are `IrqMutex<T>` — ticket spinlock with IRQ masking (DAIF.I set on
acquire, restored on release). There are no bare `Mutex` or other lock types.

| #   | Lock Name                 | File                   | Type                                    | Purpose                                                                                       |
| --- | ------------------------- | ---------------------- | --------------------------------------- | --------------------------------------------------------------------------------------------- |
| 1   | `scheduler::STATE`        | scheduler.rs:127       | `IrqMutex<State>`                       | Global run queue, per-core state, process table, scheduling contexts, blocked/suspended lists |
| 2   | `channel::STATE`          | channel.rs:64          | `IrqMutex<State>`                       | Channel table (endpoints, shared pages, pending signals, waiters)                             |
| 3   | `timer::TIMERS`           | timer.rs:56            | `IrqMutex<TimerTable>`                  | Timer objects (deadlines, waiter registry)                                                    |
| 4   | `interrupt::TABLE`        | interrupt.rs:55        | `IrqMutex<InterruptTable>`              | Interrupt registrations (IRQ→slot mapping, waiter registry)                                   |
| 5   | `futex::WAIT_TABLE`       | futex.rs:47            | `IrqMutex<WaitTable>`                   | Futex wait queues (64 hash buckets, PA-keyed)                                                 |
| 6   | `thread_exit::STATE`      | thread_exit.rs:42      | `IrqMutex<WaitableRegistry<ThreadId>>`  | Thread exit notification (readiness + waiters)                                                |
| 7   | `process_exit::STATE`     | process_exit.rs:21     | `IrqMutex<WaitableRegistry<ProcessId>>` | Process exit notification (readiness + waiters)                                               |
| 8   | `page_allocator::STATE`   | page_allocator.rs:36   | `IrqMutex<State>`                       | Buddy allocator free lists (physical page frames)                                             |
| 9   | `slab::SLAB`              | slab.rs:32             | `IrqMutex<SlabState>`                   | Slab allocator caches (6 size classes)                                                        |
| 10  | `heap::ALLOC_LOCK`        | heap.rs:41             | `IrqMutex<()>`                          | Linked-list allocator free list                                                               |
| 11  | `memory::KERNEL_PT_LOCK`  | memory.rs:39           | `IrqMutex<()>`                          | Kernel TTBR1 page table modifications (break-block, guard pages)                              |
| 12  | `serial::LOCK`            | serial.rs:22           | `IrqMutex<()>`                          | UART output serialization (prevents interleaved multi-core output)                            |
| 13  | `address_space_id::STATE` | address_space_id.rs:32 | `IrqMutex<State>`                       | ASID bitmap and generation counter                                                            |

**Total: 13 distinct IrqMutex instances across 13 files.**

Additionally, these modules use atomics (no lock):

- `metrics.rs` — per-core `AtomicU64` counters (Relaxed ordering, no lock needed)
- `timer.rs` — `CNTFRQ: AtomicU64`, `TICKS: AtomicU64` (read-only after init / monotonic)

---

## 2. Lock Ordering DAG

The following directed acyclic graph defines the only permitted acquisition
orders. An edge `A → B` means "A may be held when B is acquired." Cycles
are forbidden.

```text
                  ┌── channel ──┐
                  │             │
  timer ──────────┤             ├──→ scheduler ──┬──→ page_allocator
                  │             │                │
  interrupt ──────┤             │                ├──→ address_space_id
                  │             │                │
  thread_exit ────┤             │                └──→ (heap allocs via Vec)
                  │             │
  process_exit ───┘             │
                                │
  futex ────────────────────────┘

  slab ──→ page_allocator

  memory (KERNEL_PT_LOCK) ──→ page_allocator

  serial: leaf (never held with any other lock)

  heap (ALLOC_LOCK): leaf (never held with any other lock;
                     slab::try_alloc runs before ALLOC_LOCK)
```

### Ordering Levels

Assign each lock a numeric level. A lock at level N may only acquire locks
at level N+1 or higher. Self-acquisition is forbidden (ticket spinlock
deadlocks).

| Level | Lock(s)                                                     | Rationale                                           |
| ----- | ----------------------------------------------------------- | --------------------------------------------------- |
| 0     | channel, timer, interrupt, thread_exit, process_exit, futex | "Event source" locks — acquire scheduler to wake    |
| 1     | scheduler                                                   | Central coordinator — calls page_allocator, ASID    |
| 2     | page_allocator, address_space_id, slab                      | Resource management — leaf or near-leaf             |
| 3     | memory (KERNEL_PT_LOCK)                                     | Kernel page table — acquires page_allocator         |
| 3     | heap (ALLOC_LOCK)                                           | Heap allocator — leaf (slab runs outside this lock) |
| ∞     | serial                                                      | Output-only leaf — never nests with any lock        |

Note: KERNEL_PT_LOCK and ALLOC_LOCK are at the same level but never
interact (no code path acquires both).

---

## 3. Multi-Lock Code Paths (Verified)

### 3.1 Two-Phase Wake Pattern (Level 0 → Level 1)

All event-source modules use the same two-phase pattern:

1. Acquire own lock (level 0), collect waiter ThreadId, **release own lock**.
2. Call `scheduler::try_wake_for_handle()` or `scheduler::set_wake_pending_for_handle()`
   which acquires the scheduler lock (level 1).

Locks are **never nested** — the level-0 lock is released before the level-1
lock is acquired. This pattern is used by:

- `channel::signal()` — channel lock → release → scheduler lock
- `channel::close_endpoint()` — channel lock → release → scheduler lock
- `timer::check_expired()` — timer lock → release → scheduler lock
- `timer::destroy()` — timer lock → release → scheduler lock
- `interrupt::handle_irq()` — interrupt lock → release → scheduler lock
- `interrupt::destroy()` — interrupt lock → release → scheduler lock
- `thread_exit::notify_exit()` — thread_exit lock → release → scheduler lock
- `thread_exit::destroy()` — thread_exit lock → release → scheduler lock
- `process_exit::notify_exit()` — process_exit lock → release → scheduler lock
- `process_exit::destroy()` — process_exit lock → release → scheduler lock
- `futex::wake()` — futex lock → release → scheduler lock

**Verified correct:** No code path holds a level-0 lock while acquiring the
scheduler lock.

### 3.2 channel::setup_endpoint() (Level 0 → Level 1, sequential)

```text
channel::STATE.lock()  → read shared page PAs → release
scheduler::with_process()  → map pages + insert handle → release
```

Sequential, not nested. Correct.

### 3.3 channel::create() (page_allocator → Level 0)

```text
page_allocator::alloc_frame()  → allocate page → release
page_allocator::alloc_frame()  → allocate page → release
channel::STATE.lock()  → insert channel → release
```

Page allocator is acquired and released **before** the channel lock. No nesting.

### 3.4 Scheduler → page_allocator (Level 1 → Level 2)

Inside `schedule_inner()` → `maybe_cleanup_killed_process()`:

```text
scheduler::STATE held (level 1)
  → addr_space.free_all()
    → page_allocator::free_frame() (acquires level 2) — repeated
    → page_allocator::free_frames() (acquires level 2) — repeated
  → address_space_id::free() (acquires level 2)
```

This is a true nested acquisition: **scheduler → page_allocator** and
**scheduler → address_space_id**. Correct per ordering rules (level 1 → 2).

This path only executes for killed processes with threads running on other cores
(rare deferred cleanup path).

### 3.5 Scheduler → address_space_id (Level 1 → Level 2)

Same as 3.4 — `address_space_id::free()` called under scheduler lock in
`maybe_cleanup_killed_process()`.

### 3.6 Slab → page_allocator (Level 2 → Level 2)

Inside `slab::try_alloc()`:

```text
slab::SLAB.lock() (level 2)
  → SlabCache::alloc()
    → SlabCache::grow()
      → page_allocator::alloc_frame() (level 2)
```

**True nested acquisition: slab → page_allocator.** Both are at level 2.
This is safe because there is no reverse path (page_allocator never acquires
the slab lock). The ordering is strictly slab → page_allocator, never reversed.

### 3.7 KERNEL_PT_LOCK → page_allocator (Level 3 → Level 2)

Inside `memory::alloc_kernel_stack_guarded()`:

```text
KERNEL_PT_LOCK.lock() (level 3)
  → page_allocator::alloc_frame() (level 2)
```

The documented ordering comment in `memory.rs:38` says:
`"Lock ordering: KERNEL_PT_LOCK → page allocator lock (never the reverse)."`

Note: This is level 3 → 2, which appears to violate the level hierarchy.
However, it is safe because there is no code path from page_allocator to
KERNEL_PT_LOCK. These two locks form a simple DAG edge with no reverse.

### 3.8 heap::alloc() → slab::try_alloc() (separate locks)

Inside `GlobalAlloc::alloc()`:

```text
slab::try_alloc() → acquires SLAB lock, releases it
ALLOC_LOCK.lock() → linked-list alloc
```

Sequential, not nested. The slab lock is released before ALLOC_LOCK is acquired.

### 3.9 exit_current_from_syscall (multiple phases)

This is the most complex multi-lock path in the kernel:

```text
Phase 1: scheduler::STATE.lock() → collect ExitInfo → release
Phase 2: thread_exit::notify_exit() → thread_exit lock → release → scheduler lock → release
         process_exit::notify_exit() → process_exit lock → release → scheduler lock → release
Phase 2a: futex::remove_thread() → futex lock → release
Phase 3: close_handle_categories() → each subsystem lock → release → scheduler lock → release
Phase 4: page_allocator (via addr_space.free_all()) → release
         address_space_id::free() → release
Phase 5: scheduler::STATE.lock() → mark exited → schedule_inner → release
```

All acquisitions are sequential (lock → release → next lock). Phase 5 re-acquires
the scheduler lock, which is valid because all prior locks have been released.

### 3.10 sys_process_kill (multiple phases)

```text
Phase 1: scheduler::kill_process() → scheduler lock → release
Phase 2: thread_exit::notify_exit() → thread_exit lock → release → scheduler lock → release
         process_exit::notify_exit() → process_exit lock → release → scheduler lock → release
Phase 2a: futex::remove_thread() → futex lock → release
Phase 3: channel::close_endpoint(), interrupt::destroy(), timer::destroy(), etc.
         Each: own lock → release → scheduler lock → release
Phase 4: page_allocator (via addr_space.free_all()) → release
         address_space_id::free() → release
```

All sequential. Correct.

### 3.11 sys_wait (multiple subsystem locks)

```text
scheduler lock → resolve handles, populate wait_set → release
channel::register_waiter() → channel lock → release  (×N)
timer::register_waiter() → timer lock → release  (×N)
interrupt::register_waiter() → interrupt lock → release  (×N)
thread_exit::register_waiter() → thread_exit lock → release  (×N)
process_exit::register_waiter() → process_exit lock → release  (×N)
channel::check_pending() → channel lock → release  (×N)
...readiness checks with individual locks...
scheduler::block_current_unless_woken() → scheduler lock → release (or block)
```

All sequential. No two subsystem locks are held simultaneously.

### 3.12 IRQ handler path (timer → scheduler)

```text
irq_handler():
  interrupt_controller::acknowledge() → no lock (MMIO)
  timer::handle_irq():
    timer::check_expired() → timer lock → release → scheduler lock → release
  OR interrupt::handle_irq() → interrupt lock → release → scheduler lock → release
  scheduler::schedule() → scheduler lock → release
  interrupt_controller::end_of_interrupt() → no lock (MMIO)
```

Timer/interrupt locks are released before the scheduler lock is acquired. The
subsequent `scheduler::schedule()` acquires the scheduler lock independently.

---

## 4. Interrupt Safety Analysis

All 13 locks use `IrqMutex`, which masks IRQs (DAIF.I) on acquire and restores
on release. This means:

- **No lock can be interrupted while held.** Timer IRQs are deferred until
  the lock is released. This prevents interrupt-time reentry into any locked
  critical section.

- **The timer IRQ handler acquires locks.** `timer::handle_irq()` acquires
  `timer::TIMERS` and (transitively, via two-phase wake) `scheduler::STATE`.
  `interrupt::handle_irq()` acquires `interrupt::TABLE` and (transitively)
  `scheduler::STATE`. Both are safe because:
  1. IRQ masking prevents these handlers from running while any IrqMutex is held.
  2. The locks acquired in IRQ context follow the same ordering as non-IRQ paths.

- **The scheduler lock is acquired in IRQ context.** `irq_handler()` calls
  `scheduler::schedule()` after timer/interrupt handling. This is safe because
  IRQ masking ensures no nested IRQ can attempt to re-acquire it.

- **Panic handler bypasses locks.** `serial::panic_puts()` and `metrics::panic_dump()`
  deliberately bypass their respective locks to avoid deadlock when panicking
  while a lock is held. Output may be garbled but the kernel won't hang.

### Self-Deadlock Prevention

Ticket spinlocks deadlock on self-acquisition (the thread takes ticket N+1
but `now_serving` is stuck at N because the thread is spinning instead of
releasing). IRQ masking prevents the timer ISR from re-entering a held lock.
No code path acquires the same lock twice (verified by inspection).

---

## 5. Circular Dependency Analysis

### Method

For each lock pair where a directed edge exists in the DAG (§2), verify that
no reverse edge exists anywhere in the codebase.

| Forward Edge                    | Reverse Edge Exists? | Evidence                                                 |
| ------------------------------- | -------------------- | -------------------------------------------------------- |
| channel → scheduler             | No                   | `scheduler` never calls `channel::STATE.lock()`          |
| timer → scheduler               | No                   | `scheduler` reads `timer::counter()` (atomic, no lock)   |
| interrupt → scheduler           | No                   | `scheduler` never calls `interrupt::TABLE.lock()`        |
| thread_exit → scheduler         | No                   | `scheduler` never calls `thread_exit::STATE.lock()`      |
| process_exit → scheduler        | No                   | `scheduler` never calls `process_exit::STATE.lock()`     |
| futex → scheduler               | No                   | `scheduler` never calls `futex::WAIT_TABLE.lock()`       |
| scheduler → page_allocator      | No                   | `page_allocator` never calls `scheduler::STATE.lock()`   |
| scheduler → address_space_id    | No                   | `address_space_id` never calls `scheduler::STATE.lock()` |
| slab → page_allocator           | No                   | `page_allocator` never calls `slab::SLAB.lock()`         |
| KERNEL_PT_LOCK → page_allocator | No                   | `page_allocator` never calls `KERNEL_PT_LOCK.lock()`     |

**Result: No circular dependencies found.** The lock ordering forms a strict DAG.

### Allocation-induced nesting

Any code that calls `Vec::push()`, `Box::new()`, etc. under a lock will
transitively acquire `ALLOC_LOCK` (via `GlobalAlloc::alloc()`), which may
also acquire `SLAB.lock()` and then `page_allocator::STATE.lock()`.

The scheduler lock (level 1) holds `Vec` operations (e.g., `s.queue.ready.push()`).
This means: **scheduler → ALLOC_LOCK → SLAB → page_allocator** is a possible
transitive path. This is consistent with the ordering (level 1 → 3 → 2 → 2),
and no reverse path exists.

---

## 6. Summary

| Property                 | Status                                                 |
| ------------------------ | ------------------------------------------------------ |
| Total locks              | 13 IrqMutex instances                                  |
| Circular dependencies    | **None found**                                         |
| Lock ordering violations | **None found**                                         |
| Self-deadlock risk       | Prevented by IRQ masking (no reentry)                  |
| Interrupt safety         | All locks mask IRQs; IRQ handler follows same ordering |
| Panic safety             | Serial and metrics bypass locks in panic handler       |

The kernel's lock ordering is sound. All multi-lock code paths either use
sequential lock-release-lock patterns (two-phase wake) or follow the strict
DAG ordering documented above.
