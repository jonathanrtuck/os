# Kernel Lock Ordering DAG

Complete lock ordering verification with code evidence. Covers all 13 `IrqMutex`
instances, every acquisition site, and all multi-lock code paths.

**Verified:** 2026-03-14
**Canonical source:** `system/kernel/LOCK-ORDERING.md` (in-tree, updated in sync)

---

## 1. Lock Inventory (13 IrqMutex instances)

All kernel synchronization uses `IrqMutex<T>` — a ticket spinlock that masks
IRQs (DAIF.I) on acquire and restores on release. No other lock types exist.

| #  | Lock                      | File:Line                | Type                                    |
|----|---------------------------|--------------------------|-----------------------------------------|
| 1  | `scheduler::STATE`        | scheduler.rs:38          | `IrqMutex<State>`                       |
| 2  | `channel::STATE`          | channel.rs:63            | `IrqMutex<State>`                       |
| 3  | `timer::TIMERS`           | timer.rs:45              | `IrqMutex<TimerTable>`                  |
| 4  | `interrupt::TABLE`        | interrupt.rs:44          | `IrqMutex<InterruptTable>`              |
| 5  | `futex::WAIT_TABLE`       | futex.rs:47              | `IrqMutex<WaitTable>`                   |
| 6  | `thread_exit::STATE`      | thread_exit.rs:42        | `IrqMutex<WaitableRegistry<ThreadId>>`  |
| 7  | `process_exit::STATE`     | process_exit.rs:21       | `IrqMutex<WaitableRegistry<ProcessId>>` |
| 8  | `page_allocator::STATE`   | page_allocator.rs:25     | `IrqMutex<State>`                       |
| 9  | `slab::SLAB`              | slab.rs:28               | `IrqMutex<SlabState>`                   |
| 10 | `heap::ALLOC_LOCK`        | heap.rs:47               | `IrqMutex<()>`                          |
| 11 | `memory::KERNEL_PT_LOCK`  | memory.rs:33             | `IrqMutex<()>`                          |
| 12 | `serial::LOCK`            | serial.rs:25             | `IrqMutex<()>`                          |
| 13 | `address_space_id::STATE` | address_space_id.rs:32   | `IrqMutex<State>`                       |

---

## 2. Lock Ordering DAG

```
Level 0 (event sources):
  channel ─────────┐
  timer ───────────┤
  interrupt ───────┤──→ [release] ──→ Level 1: scheduler
  thread_exit ─────┤
  process_exit ────┤
  futex ───────────┘

Level 1 (coordinator):
  scheduler ──→ Level 2: page_allocator
  scheduler ──→ Level 2: address_space_id
  scheduler ──→ (implicit via Vec::push) ALLOC_LOCK → slab → page_allocator

Level 2 (resource management):
  slab ──→ page_allocator  (nested: grow() calls alloc_frame())
  KERNEL_PT_LOCK ──→ page_allocator  (nested: alloc_kernel_stack_guarded)

Leaves (never nested with others):
  serial::LOCK
  heap::ALLOC_LOCK  (slab::try_alloc runs and releases before ALLOC_LOCK)
```

### Ordering Levels

| Level | Locks                                                       |
|-------|-------------------------------------------------------------|
| 0     | channel, timer, interrupt, thread_exit, process_exit, futex |
| 1     | scheduler                                                   |
| 2     | page_allocator, address_space_id, slab                      |
| 3     | KERNEL_PT_LOCK, ALLOC_LOCK                                  |
| ∞     | serial (output-only leaf)                                   |

---

## 3. Two-Phase Wake Pattern (All 6 Waitable Types)

The kernel's central concurrency pattern: event source locks are **always
released before** acquiring the scheduler lock. This guarantees level-0
locks and the level-1 lock are never held simultaneously.

### Pattern

```
Phase 1: acquire event-source lock → collect waiter ThreadId → release lock
Phase 2: call scheduler::try_wake_for_handle(tid, reason) [acquires scheduler lock]
         OR scheduler::set_wake_pending_for_handle(tid, reason) [acquires scheduler lock]
```

### Verified Code Evidence

| Module       | Function                    | Phase 1 lock       | Phase 2 call                          |
|--------------|-----------------------------|---------------------|---------------------------------------|
| channel      | `signal()`                  | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| channel      | `close_endpoint()`          | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| timer        | `check_expired()`           | `TIMERS.lock()`     | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| timer        | `destroy()`                 | `TIMERS.lock()`     | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| interrupt    | `handle_irq()`              | `TABLE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| interrupt    | `destroy()`                 | `TABLE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| thread_exit  | `notify_exit()`             | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| thread_exit  | `destroy()`                 | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| process_exit | `notify_exit()`             | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| process_exit | `destroy()`                 | `STATE.lock()`      | `try_wake_for_handle` / `set_wake_pending_for_handle` |
| futex        | `wake()`                    | `WAIT_TABLE.lock()` | `try_wake` / `set_wake_pending`                       |

**All 11 wake paths verified:** level-0 lock released before scheduler lock acquired.

---

## 4. Every Lock Acquisition Site

### scheduler::STATE (scheduler.rs)

| Line | Function                        | Pattern      |
|------|---------------------------------|--------------|
| 623  | `bind_scheduling_context`       | single lock  |
| 658  | `block_current_unless_woken`    | single lock  |
| 700  | `borrow_scheduling_context`     | single lock  |
| 729  | `clear_wait_state`              | single lock  |
| 762  | `create_process`                | single lock  |
| 785  | `remove_empty_process`          | single lock  |
| 803  | `current_process_do`            | single lock  |
| 827  | `create_scheduling_context`     | single lock  |
| 859  | `current_thread_and_process_do` | single lock  |
| 883  | `current_thread_do`             | single lock  |
| 894  | `exit_current_from_syscall` P1  | phase 1 (release before P2) |
| 984  | `exit_current_from_syscall` P5  | re-acquire (all prior locks released) |
| 994  | `exit_current_from_syscall` P5b | re-acquire (NonLast path) |
| 1005 | `kill_process`                  | single lock  |
| 1043 | `release_scheduling_context`    | single lock  |
| 1075 | `return_scheduling_context`     | single lock  |
| 1198 | `schedule`                      | single lock → schedule_inner |
| 1208 | `set_timeout_timer`             | single lock  |
| 1230 | `set_timeout_timer_none`        | single lock  |
| 1238 | `set_wake_pending`              | single lock  |
| 1247 | `set_wake_pending_for_handle`   | single lock  |
| 1260 | `spawn_user`                    | single lock  |
| 1271 | `spawn_user_suspended`          | single lock  |
| 1301 | `start_suspended_threads`       | single lock  |
| 1330 | `push_wait_entry`               | single lock  |
| 1354 | `take_stale_waiters`            | single lock  |
| 1385 | `take_timeout_timer`            | single lock  |
| 1396 | `try_wake`                      | single lock  |
| 1406 | `try_wake_for_handle`           | single lock  |
| 1416 | `with_process`                  | single lock  |
| 1427 | `init`                          | single lock  |
| 1437 | `init_secondary`                | single lock  |

### channel::STATE (channel.rs)

| Line | Function               | Pattern               |
|------|------------------------|-----------------------|
| 79   | `check_pending`        | single lock           |
| 96   | `close_endpoint`       | lock → release → scheduler (two-phase) |
| 157  | `create`               | single lock (pages allocated before) |
| 171  | `register_waiter`      | single lock           |
| 184  | `setup_endpoint`       | lock → release → scheduler::with_process |
| 209  | `shared_pages`         | single lock           |
| 225  | `signal`               | lock → release → scheduler (two-phase) |
| 249  | `unregister_waiter`    | single lock           |

### timer::TIMERS (timer.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 103  | `check_expired`       | lock → release → scheduler (two-phase) |
| 131  | `check_fired`         | single lock           |
| 180  | `create`              | single lock           |
| 201  | `destroy`             | lock → release → scheduler (two-phase) |
| 286  | `register_waiter`     | single lock           |
| 292  | `unregister_waiter`   | single lock           |

### interrupt::TABLE (interrupt.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 71   | `acknowledge`         | single lock           |
| 83   | `check_pending`       | single lock           |
| 90   | `destroy`             | lock → release → scheduler (two-phase) |
| 122  | `handle_irq`          | lock → release → scheduler (two-phase) |
| 156  | `register`            | single lock           |
| 187  | `register_waiter`     | single lock           |
| 193  | `unregister_waiter`   | single lock           |

### futex::WAIT_TABLE (futex.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 66   | `remove_thread`       | single lock           |
| 78   | `wait`                | single lock           |
| 92   | `wake`                | lock → release → scheduler (two-phase) |

### thread_exit::STATE (thread_exit.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 46   | `check_exited`        | single lock           |
| 50   | `create`              | single lock           |
| 56   | `destroy`             | lock → release → scheduler (two-phase) |
| 68   | `notify_exit`         | lock → release → scheduler (two-phase) |
| 80   | `register_waiter`     | single lock           |
| 84   | `unregister_waiter`   | single lock           |

### process_exit::STATE (process_exit.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 25   | `check_exited`        | single lock           |
| 29   | `create`              | single lock           |
| 35   | `destroy`             | lock → release → scheduler (two-phase) |
| 46   | `notify_exit`         | lock → release → scheduler (two-phase) |
| 58   | `register_waiter`     | single lock           |
| 62   | `unregister_waiter`   | single lock           |

### page_allocator::STATE (page_allocator.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 93   | `alloc_frame`         | single lock           |
| 164  | `free_count`          | single lock           |
| 187  | `free_frame`          | single lock           |
| 226  | `free_frames`         | single lock           |
| 273  | test helper           | single lock           |

### slab::SLAB (slab.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 140  | `SlabState::alloc`    | lock (may nest → page_allocator::alloc_frame) |
| 155  | `SlabState::free`     | single lock           |

### heap::ALLOC_LOCK (heap.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 120  | `GlobalAlloc::alloc`  | single lock (slab runs before, not nested) |
| 201  | `GlobalAlloc::dealloc`| single lock           |

### memory::KERNEL_PT_LOCK (memory.rs)

| Line | Function                       | Pattern               |
|------|--------------------------------|-----------------------|
| 108  | `alloc_kernel_stack_guarded`   | lock (nests → page_allocator) |
| 236  | `free_kernel_stack_guarded`    | lock (nests → page_allocator) |

### serial::LOCK (serial.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 99   | `putc`                | single lock (leaf)    |
| 104  | `puts`                | single lock (leaf)    |
| 109  | `write_hex`           | single lock (leaf)    |

### address_space_id::STATE (address_space_id.rs)

| Line | Function              | Pattern               |
|------|-----------------------|-----------------------|
| 42   | `alloc`               | single lock           |
| 95   | `generation`          | single lock           |
| 103  | `free`                | single lock           |

---

## 5. Nested Lock Paths (True Nesting)

Only 3 nested lock paths exist. All are DAG-consistent (no reverse edges).

### 5.1 scheduler → page_allocator/address_space_id

**Path:** `schedule_inner` → `maybe_cleanup_killed_process` → `addr_space.free_all()` → `page_allocator::free_frame()`; `address_space_id::free()`

**Code evidence:** scheduler.rs `schedule_inner` holds `STATE` lock, calls `maybe_cleanup_killed_process` which frees pages and ASID.

**Safety:** page_allocator never acquires scheduler lock. address_space_id never acquires scheduler lock.

### 5.2 slab → page_allocator

**Path:** `slab::try_alloc` → `SlabCache::grow()` → `page_allocator::alloc_frame()`

**Code evidence:** slab.rs line 140 holds `SLAB.lock()`, grows a cache which calls `page_allocator::alloc_frame()` (acquires `STATE`).

**Safety:** page_allocator never acquires slab lock.

### 5.3 KERNEL_PT_LOCK → page_allocator

**Path:** `memory::alloc_kernel_stack_guarded` → `page_allocator::alloc_frame()`

**Code evidence:** memory.rs line 108 holds `KERNEL_PT_LOCK.lock()`, allocates page frames for the guarded stack.

**Safety:** page_allocator never acquires KERNEL_PT_LOCK.

### 5.4 Implicit: scheduler → ALLOC_LOCK → slab → page_allocator

**Path:** Any `Vec::push()` or `Box::new()` inside a scheduler lock calls `GlobalAlloc::alloc()`, which tries slab first (acquires `SLAB`), then linked-list (acquires `ALLOC_LOCK`). Slab growth acquires `page_allocator::STATE`.

**Code evidence:** scheduler.rs uses `Vec<Box<Thread>>` for ready/blocked/suspended queues.

**Safety:** No reverse path exists from page_allocator/slab/heap to scheduler.

---

## 6. Cycle-Freedom Proof

For every directed edge in the DAG, verified the reverse edge does NOT exist:

| Forward Edge                    | Reverse Exists? | Evidence                                                     |
|---------------------------------|-----------------|--------------------------------------------------------------|
| channel → scheduler             | No              | scheduler.rs has zero calls to `channel::STATE.lock()`       |
| timer → scheduler               | No              | scheduler.rs only reads `timer::counter()` (atomic, no lock) |
| interrupt → scheduler           | No              | scheduler.rs has zero calls to `interrupt::TABLE.lock()`     |
| thread_exit → scheduler         | No              | scheduler.rs has zero calls to `thread_exit::STATE.lock()`   |
| process_exit → scheduler        | No              | scheduler.rs has zero calls to `process_exit::STATE.lock()`  |
| futex → scheduler               | No              | scheduler.rs has zero calls to `futex::WAIT_TABLE.lock()`    |
| scheduler → page_allocator      | No              | page_allocator.rs has zero calls to `scheduler::STATE.lock()`|
| scheduler → address_space_id    | No              | address_space_id.rs has zero calls to `scheduler::STATE.lock()`|
| slab → page_allocator           | No              | page_allocator.rs has zero calls to `slab::SLAB.lock()`     |
| KERNEL_PT_LOCK → page_allocator | No              | page_allocator.rs has zero calls to `KERNEL_PT_LOCK.lock()` |

**Result: No cycles. The lock ordering is a strict DAG.**

---

## 7. Complex Multi-Lock Paths

### 7.1 sys_wait (syscall.rs)

```
scheduler lock → resolve handles, populate wait_set → release
channel::register_waiter → channel lock → release  (×N)
timer::register_waiter → timer lock → release  (×N)
interrupt::register_waiter → interrupt lock → release  (×N)
thread_exit::register_waiter → thread_exit lock → release  (×N)
process_exit::register_waiter → process_exit lock → release  (×N)
readiness checks → individual subsystem locks → release (×N)
scheduler::block_current_unless_woken → scheduler lock → block/wake
```

All sequential. No two locks held simultaneously.

### 7.2 exit_current_from_syscall (scheduler.rs + syscall.rs)

```
Phase 1: scheduler lock → collect ExitInfo → release
Phase 2: thread_exit::notify_exit → own lock → release → scheduler lock → release
         process_exit::notify_exit → own lock → release → scheduler lock → release
Phase 2a: futex::remove_thread → futex lock → release
Phase 3: close_handle_categories → each subsystem lock → release → scheduler lock → release
Phase 4: page_allocator (via addr_space.free_all) → release
         address_space_id::free → release
Phase 5: scheduler lock → mark exited → schedule_inner → release
```

All sequential. Phase 5 re-acquires scheduler lock after all prior locks released.

### 7.3 sys_process_kill (syscall.rs)

```
Phase 1: scheduler::kill_process → scheduler lock → release
Phase 2: thread_exit::notify_exit (×N) → own lock → release → scheduler lock → release
         process_exit::notify_exit → own lock → release → scheduler lock → release
Phase 2a: futex::remove_thread (×N) → futex lock → release
Phase 3: close resources (channels, interrupts, timers, etc.) → individual locks → release
Phase 4: page_allocator (via addr_space.free_all) → release
         address_space_id::free → release
```

All sequential. Identical pattern to exit.

### 7.4 IRQ handler (main.rs)

```
timer::handle_irq():
  timer::check_expired → timer lock → release → scheduler lock → release
  reprogram → no lock
OR interrupt::handle_irq():
  interrupt lock → release → scheduler lock → release
scheduler::schedule → scheduler lock → release
interrupt_controller::end_of_interrupt → no lock (MMIO)
```

Two-phase wake, then independent scheduler call. No nesting.

---

## 8. Interrupt Safety

- All 13 locks mask IRQs via DAIF.I on acquire → no timer/interrupt re-entry
  while any lock is held.
- IRQ handler acquires timer/interrupt locks and scheduler lock — safe because
  masking prevents recursive entry.
- Self-deadlock prevented: ticket spinlock + IRQ masking means a core can never
  attempt to re-acquire a lock it already holds.
- Panic handler: `serial::panic_puts()` and `metrics::panic_dump()` bypass
  their locks to avoid deadlock when panicking with a lock held.

---

## Summary

| Property                 | Status                                                   |
|--------------------------|----------------------------------------------------------|
| Total locks              | 13 IrqMutex instances across 13 files                   |
| Total acquisition sites  | ~80 `.lock()` calls                                      |
| Circular dependencies    | **None found**                                           |
| Lock ordering violations | **None found**                                           |
| Two-phase wake verified  | **All 6 waitable types** (11 wake paths)                 |
| Self-deadlock risk       | Prevented by IRQ masking (no re-entry)                   |
| Nested lock paths        | 3 explicit + 1 implicit (all DAG-consistent)             |
| Complex multi-lock paths | 4 verified (sys_wait, exit, kill, IRQ handler)            |
