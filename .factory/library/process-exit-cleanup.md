# Process Exit Cleanup Trace

Complete step-by-step trace of ALL resource reclamation during process exit.
Covers both the normal exit path (last-thread-exit) and the kill path (process_kill),
including the deferred cleanup path for killed processes with threads on other cores.

**Verified:** 2026-03-14
**Bug found and fixed:** Internal timeout timer leak (timer table slots leaked when
threads were killed while blocked in `wait` with finite timeout).
**Test file:** `system/test/tests/process_exit_cleanup.rs` (18 tests)

---

## Resource Categories

A process may own the following resources at exit time:

| Category | Location | Cleanup mechanism |
|----------|----------|-------------------|
| **Handles** (Channel, Timer, Interrupt, Thread, Process, SchedulingContext) | `process.handles` (HandleTable) | `drain()` → `categorize_handles()` → per-type cleanup |
| **Scheduling context bind/borrow refs** | `thread.scheduling.{context_id, saved_context_id}` | `release_thread_context_ids()` or `release_context_inner()` |
| **Internal timeout timers** | `thread.timeout_timer` | ⚠️ **Was missing** — now collected by `kill_process` and `exit_current_from_syscall` |
| **Futex wait entries** | `futex::WAIT_TABLE` buckets | `futex::remove_thread(tid)` |
| **Waiter registrations** | `channel/timer/interrupt/thread_exit/process_exit` waiter slots | Stale refs tolerated (dead ThreadId, no-op on wake) |
| **DMA buffers** | `address_space.dma_allocations` | `free_all()` frees each `(pa, order)` |
| **Owned page frames** (code, data, stack, demand-paged heap) | `address_space.owned_frames` | `free_all()` frees each frame |
| **Heap allocation metadata** | `address_space.heap_allocations` | `free_all()` clears the Vec |
| **Page table frames** (L1, L2, L3) | Nested in L0 table structure | `free_all()` walks L0→L1→L2→L3 and frees all |
| **L0 page table** | `address_space.l0_pa` | `free_all()` frees at the end |
| **TLB entries** | Hardware TLB tagged with ASID | `invalidate_tlb()` → `TLBI ASIDE1IS` |
| **ASID** | `address_space_id` bitmap | `address_space_id::free(asid)` |
| **Kernel stack** | `thread.stack_alloc_pa` | `Thread::Drop` → `clear_kernel_guard_page()` + `free_frames()` |
| **Thread object** | `Box<Thread>` in scheduler state | Deferred drop (via `deferred_drops` in `schedule_inner`) |
| **Process slot** | `scheduler::State.processes[pid]` | `.take()` sets slot to `None` |

---

## Path 1: Normal Exit (Last-Thread-Exit)

**Entry:** `sys_exit` → `scheduler::exit_current_from_syscall(ctx)`
**File:** `scheduler.rs`

### Phase 1 (Scheduler Lock)

1. **Release scheduling context bind/borrow refs:**
   - Take `thread.scheduling.context_id` and `thread.scheduling.saved_context_id`
   - Call `release_context_inner(s, id)` for each (decrements ref_count, frees slot if 0)

2. **Collect internal timeout timer (Phase 1b):**
   - Take `thread.timeout_timer` from current thread
   - If present, call `timer::destroy(timer_id)` **outside scheduler lock**
   - ⚠️ **This was the bug — previously not cleaned up**

3. **Decrement thread_count:**
   - `process.thread_count -= 1`
   - If `thread_count == 0`: this is the last thread (full cleanup)

4. **Drain handle table (last thread only):**
   - `process.handles.drain()` → collect all `HandleObject` values
   - `categorize_handles(objects, s)`:
     - Channels → `categories.channels`
     - Timers → `categories.timers`
     - Interrupts → `categories.interrupts`
     - Thread handles → `categories.thread_handles`
     - Process handles → `categories.process_handles`
     - SchedulingContexts → **released immediately** via `release_context_inner(s, id)`

5. **Take the Process object (last thread only):**
   - `s.processes[pid].take()` removes the process slot

### Phase 2 (Outside Scheduler Lock)

6. **Notify thread exit:**
   - `thread_exit::notify_exit(thread_id)` → marks entry ready, wakes waiter

7. **Notify process exit (last thread only):**
   - `process_exit::notify_exit(process_id)` → marks entry ready, wakes waiter

8. **Remove from futex wait queues:**
   - `futex::remove_thread(thread_id)` → scans all 64 buckets, removes entries

### Phase 3 (Outside Scheduler Lock, Last Thread Only)

9. **Close handle-tracked resources:**
   - `channel::close_endpoint(id)` for each channel → frees shared pages when both close
   - `timer::destroy(id)` for each timer → frees slot, wakes waiter
   - `interrupt::destroy(id)` for each interrupt → disables IRQ in GIC, wakes waiter
   - `thread_exit::destroy(id)` for each thread handle → removes entry, wakes waiter
   - `process_exit::destroy(id)` for each process handle → removes entry, wakes waiter

### Phase 4 (Outside Scheduler Lock, Last Thread Only)

10. **Invalidate TLB:**
    - `addr_space.invalidate_tlb()` → `DSB ISHST + TLBI ASIDE1IS + DSB ISH + ISB`

11. **Free all address space resources:**
    - `addr_space.free_all()`:
      - Clear `heap_allocations` Vec
      - Free DMA buffers: for each `DmaAllocation`, call `page_allocator::free_frames(pa, order)`
      - Free owned frames: for each `Pa` in `owned_frames`, call `page_allocator::free_frame(pa)`
      - Walk page tables and free L3, L2, L1 table frames
      - Free L0 table frame
      - Set `freed = true`

12. **Release ASID:**
    - `address_space_id::free(asid)` → clears bit in bitmap

### Phase 5 (Scheduler Lock)

13. **Mark thread Exited and reschedule:**
    - `thread.mark_exited()`
    - `schedule_inner()` → thread goes to `deferred_drops`
    - Next `schedule_inner` call: `deferred_drops.clear()` drops the thread
    - `Thread::Drop`: remaps guard page → `free_frames(stack_pa, order)` (kernel stack freed)

---

## Path 2: Kill (process_kill)

**Entry:** `sys_process_kill` → `scheduler::kill_process(target_pid)`
**File:** `scheduler.rs` (kill_process), `syscall.rs` (sys_process_kill)

### Phase 1 (Scheduler Lock, in `kill_process`)

1. **Remove threads from ready queue:**
   - For each thread: `release_thread_context_ids(s, thread)` (scheduling context refs)
   - Collect `thread.timeout_timer` if present
   - Thread dropped (kernel stack freed via `Thread::Drop`)

2. **Remove threads from blocked list:**
   - Same: release sched ctx refs, collect timeout_timer, drop thread

3. **Remove threads from suspended list:**
   - Same: release sched ctx refs, collect timeout_timer, drop thread

4. **Mark running threads on other cores as Exited:**
   - Take `scheduling.context_id` and `scheduling.saved_context_id`
   - Collect `timeout_timer` from running thread
   - `thread.mark_exited()`
   - Count `running_count`

5. **Release deferred scheduling context IDs**

6. **Drain handle table + categorize:**
   - Same as normal exit Phase 1 step 4
   - SchedulingContext handles released immediately

7. **Take or defer process:**
   - If `running_count == 0`: take process for immediate address space cleanup
   - If `running_count > 0`: set `process.killed = true`, `thread_count = running_count`

### Phase 2 (Outside Scheduler Lock, in `sys_process_kill`)

8. **Notify exits for all killed threads:**
   - `thread_exit::notify_exit(tid)` for each thread
   - `process_exit::notify_exit(target_pid)`

9. **Remove from futex wait queues:**
   - `futex::remove_thread(tid)` for each thread

### Phase 3 (Outside Scheduler Lock)

10. **Close handle-tracked resources:**
    - Same as normal exit Phase 3

11. **Destroy internal timeout timers:**
    - `timer::destroy(id)` for each collected `timeout_timer`
    - ⚠️ **This was the bug — previously not cleaned up**

### Phase 4 (Outside Scheduler Lock)

12. **Free address space (immediate path):**
    - If `kill_info.address_space.is_some()`:
      - `invalidate_tlb()` + `free_all()` + `address_space_id::free(asid)`

---

## Path 3: Deferred Cleanup (Killed Process, Threads on Other Cores)

**Entry:** `schedule_inner` → `maybe_cleanup_killed_process`
**File:** `scheduler.rs`

When `kill_process` finds threads still running on other cores:
- Sets `process.killed = true`
- Sets `process.thread_count = running_count`
- Returns `address_space = None` (deferred)

Each running thread eventually triggers `schedule_inner`:

1. **`park_old`:** Thread is Exited → pushed to `deferred_drops`
2. **`maybe_cleanup_killed_process`:**
   - Decrement `process.thread_count`
   - If `thread_count == 0`:
     - Take the process from the slot
     - `addr_space.invalidate_tlb()`
     - `addr_space.free_all()`
     - `address_space_id::free(asid)`
3. **Next `schedule_inner`:** `deferred_drops.clear()` drops the thread → kernel stack freed

**Note:** The thread's `timeout_timer` was already collected in Phase 1 step 4 of the
kill path (from the running thread before marking it Exited). The timer is destroyed
in Phase 3 step 11 of the kill path. No timer leak on the deferred path.

---

## Bug Found and Fixed

### Internal timeout timer leak

**Bug:** When a thread was killed while blocked in `wait` with a finite timeout, the
internal timeout timer (stored in `thread.timeout_timer`) was leaked. This timer is
NOT tracked in the process's handle table — it's an internal kernel resource created
by `sys_wait` for timeout functionality.

**Impact:** Each leaked timer consumed one of the 32 global timer table slots. Over
time, killing processes with active timeouts could exhaust the timer table, preventing
new timers from being created.

**Affected paths:**
- `kill_process`: threads removed from ready/blocked/suspended/running had their
  `timeout_timer` silently dropped without destroying the timer slot
- `exit_current_from_syscall`: the exiting thread's `timeout_timer` was not cleaned up
  (normally cleaned up by the next `sys_wait` call, but there is no next call during exit)

**Fix:**
1. `kill_process` now collects `timeout_timer` from ALL killed threads (ready, blocked,
   suspended, and running on other cores) into `KillInfo.timeout_timers`
2. `sys_process_kill` destroys all collected timeout timers in Phase 3a
3. `exit_current_from_syscall` takes and destroys the current thread's `timeout_timer`
   in a new Phase 1b, before the scheduler lock is released

**Test coverage:** `system/test/tests/process_exit_cleanup.rs` — 18 model-based tests

---

## Soft Leak: Stale Waiter Registrations

**Status:** Documented, not fixed (acceptable).

When a thread is killed while it has `stale_waiters` from a previous blocked `wait`,
those waiter registrations remain in the channel/timer/interrupt/thread_exit/process_exit
modules. These are just `ThreadId` values pointing to a dead thread. Effects:
- If the handle fires: tries to wake dead thread → `try_wake` returns false → `set_wake_pending` sets a flag on a thread that's already dropped (the ThreadId won't match any live thread in the scheduler, so `set_wake_pending_inner` is a no-op)
- If the handle is destroyed: tries to wake waiter → same no-op
- If a new thread reuses the same ThreadId: theoretically could cause a spurious wake. But ThreadIds are monotonically increasing (`next_id` never wraps), so ID reuse doesn't happen in practice.

**Conclusion:** Not worth the complexity of cleanup. The waiter slots are overwritten
on the next `register_waiter` call by a different thread, and the stale waiter
registration has no observable effect.
