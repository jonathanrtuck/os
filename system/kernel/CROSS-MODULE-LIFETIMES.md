# Cross-Module Lifetime & Ownership Audit

Analysis of cross-module lifetime invariants that span multiple kernel files.
Verifies that kernel objects are created, referenced, and destroyed in the
correct order across module boundaries.

**Audit date:** 2026-03-11
**Scope:** All cross-module ownership relationships in `system/kernel/`
**Method:** Traced each invariant through all code paths, verified safety.

---

## 1. Handle Close While Channel Is Blocked

**Modules:** channel.rs, scheduler.rs, handle.rs
**Invariant:** Closing a channel handle must wake any thread blocked on the
peer endpoint's channel.

### Analysis

When `channel::close_endpoint(id)` is called (from `handle_close` via
`close_handle_categories`):

1. Under channel lock: takes the peer's waiter (`ch.waiter[peer_ep].take()`),
   clears own waiter, increments `closed_count`.
2. Releases channel lock.
3. If peer had a waiter: calls `scheduler::try_wake_for_handle(waiter_id, reason)`.
   If that returns false (thread not yet blocked), calls
   `scheduler::set_wake_pending_for_handle(waiter_id, reason)`.

The two-phase wake pattern (collect under channel lock, wake under scheduler lock)
prevents lock ordering violations and ensures the blocked thread is always woken.

**Verdict: SAFE.** The peer's blocked thread is always woken on close. The
`set_wake_pending_for_handle` fallback handles the race where close arrives
before the peer has blocked.

---

## 2. Thread Drop After Exit Notification

**Modules:** thread.rs, thread_exit.rs, scheduler.rs
**Invariant:** A thread's memory must remain valid until all exit notifications
have been delivered.

### Analysis

In `scheduler::exit_current_from_syscall()`:

- **Phase 1** (scheduler lock): Collect exit info, decrement `thread_count`.
  The thread is still `current` on its core.
- **Phase 2** (no scheduler lock): Call `thread_exit::notify_exit(thread_id)`.
  This acquires the thread_exit lock, marks the entry ready, takes the waiter,
  releases the lock, then wakes the waiter via the scheduler.
- **Phase 5** (scheduler lock): Mark thread Exited, call `schedule_inner`.
  The thread is parked in `deferred_drops`.

The thread is only dropped at the start of the NEXT `schedule_inner` call
(`s.deferred_drops.clear()`), by which time:

- We're running on a different thread's kernel stack.
- All notifications have been delivered (Phase 2 completed).
- The thread is unreachable from any queue (not in ready, blocked, suspended,
  or current).

**Verdict: SAFE.** Exit notification completes before the thread is dropped.
The deferred drop mechanism prevents use-after-free.

---

## 3. Address Space Deallocation After Process Exit

**Modules:** address_space.rs, process.rs, scheduler.rs
**Invariant:** Page tables must not be freed while any thread is still using
them (i.e., while TTBR0 points to this address space on any core).

### Analysis

Two paths:

**Normal exit (last thread):** `exit_current_from_syscall` Phase 4 frees the
address space. At this point:

- The thread is still Running on its core, but using the kernel stack (EL1).
- `swap_ttbr0` in `schedule_inner` (Phase 5) switches TTBR0 before the old
  thread is parked. After the switch, no core references the old TTBR0.
- TLB invalidation (`addr_space.invalidate_tlb()`) is called before `free_all()`.

**Kill process with running threads:** `scheduler::kill_process` marks running
threads as Exited and sets `process.killed = true` with `thread_count` = number
of still-running threads. Each time a running thread is parked (reaped by
`schedule_inner`), `maybe_cleanup_killed_process` decrements the count. When
it reaches zero, the address space is freed inline. At that point, all threads
have been context-switched away from this TTBR0.

**Verdict: SAFE.** Both paths ensure no core references the address space's
page tables when they are freed.

---

## 4. Timer Callback on Dead Thread

**Modules:** timer.rs, scheduler.rs, thread.rs
**Invariant:** A timer firing for a dead thread must not corrupt state.

### Analysis

When `timer::check_expired()` runs (from the timer IRQ handler):

1. Under timer lock: finds expired timers, collects `(TimerId, ThreadId)` pairs
   via `waiters.notify(id)`.
2. Releases timer lock.
3. For each fired timer: calls `scheduler::try_wake_for_handle(thread_id, ...)`.

If the thread has already exited:

- `try_wake_impl` searches blocked list, cores, ready queue. An Exited thread
  may be in `deferred_drops` (unreachable) or already dropped.
- If found in blocked/ready: `thread.wake()` returns false (only Blocked → Ready).
- If found on a core (current): `thread.wake()` returns false (Running/Exited → no transition).
- If not found: returns false.
- Fallback `set_wake_pending_for_handle`: sets `wake_pending = true` on the thread.
  This is harmless — an Exited thread is never scheduled again, so the flag is
  never consumed.

Additionally, the thread's `timeout_timer` field is cleaned up at the start
of the next `sys_wait` call (`take_timeout_timer` + `timer::destroy`), preventing
stale timers from accumulating.

**Verdict: SAFE.** Timer firing for a dead thread is a harmless no-op. The wake
attempt fails gracefully.

---

## 5. Process Slot Leak in create_from_user_elf (BUG — FIXED)

**Modules:** process.rs, scheduler.rs
**Invariant:** If process creation partially fails, no resources should leak.

### Bug Description

In `create_from_user_elf()` (and `spawn_from_elf()`):

```rust
let process_id = scheduler::create_process(addr_space);  // Adds to table
let thread_id = scheduler::spawn_user_suspended(...)       // May fail (OOM)
    .ok_or("out of memory")?;                              // Returns Err
```

If `spawn_user_suspended` returns `None` (OOM for kernel stack), the `?` operator
returns `Err`, but `create_process()` has already:

1. Assigned a ProcessId
2. Pushed `Some(Process { address_space, ... })` into `State.processes`

The orphaned process slot remains in `State.processes` forever:

- The caller (`sys_process_create`) receives `Err` and doesn't know the ProcessId.
- The Process (containing Box<AddressSpace>) is never dropped.
- The address space's frames, page tables, and ASID are permanently leaked.

### Fix

Added `scheduler::remove_empty_process(pid)` which clears the orphaned slot.
The `AddressSpace::Drop` impl handles cleanup (TLB invalidation + frame + ASID
deallocation). Both `create_from_user_elf` and `spawn_from_elf` now call this
on the error path.

**Verdict: BUG FIXED.** Both functions now clean up orphaned process slots
when thread creation fails.

---

## 6. Panic Point Audit (.unwrap() / .expect())

**Baseline count:** 41 (4 in main.rs, 1 in thread.rs, 36 in scheduler.rs)

All 41 calls reviewed. Categories:

| Category                                | Count | Justification                                                                                                                       |
| --------------------------------------- | ----- | ----------------------------------------------------------------------------------------------------------------------------------- |
| Boot-time initialization                | 5     | main.rs (4) + thread.rs (1). Failure = unrecoverable.                                                                               |
| Kernel invariant: current thread exists | 18    | `cores[core].current.as_mut().expect(...)`. A core always has a current thread after init(). Violation = corrupted scheduler state. |
| Kernel invariant: idle thread exists    | 1     | `cores[core].idle.take().expect(...)`. Set during init.                                                                             |
| Post-validation access                  | 10    | Process/thread confirmed to exist by prior guard.                                                                                   |
| User thread assertion                   | 3     | `.expect("not a user thread")` — only user threads call these syscall paths.                                                        |
| Process existence                       | 4     | `.expect("process not found")` — process_id comes from a live thread.                                                               |

**Inline justification comments added** to the less-obvious cases (kill_process
unwraps, exit path unwraps).

**Verdict: All 41 calls justified.** None are in fallible paths. Each is either
a boot-time panic or a kernel invariant assertion. The count remains at 41.

---

## Summary

| Cross-Module Invariant                     | Status            |
| ------------------------------------------ | ----------------- |
| Handle close wakes blocked channel peer    | ✅ Safe           |
| Thread drop after exit notification        | ✅ Safe           |
| Address space freed after all threads exit | ✅ Safe           |
| Timer callback on dead thread              | ✅ Safe           |
| Process slot leak on spawn failure         | 🐛 Fixed          |
| All .unwrap()/.expect() justified          | ✅ 41/41 reviewed |
