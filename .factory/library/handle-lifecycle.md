# Handle Lifecycle Map

Per-handle-type lifecycle trace covering creation, transfer, close, and cleanup.
Covers all 6 handle types across handle.rs, syscall.rs, process.rs, channel.rs,
timer.rs, interrupt.rs, thread_exit.rs, process_exit.rs, scheduling_context.rs.

**Verified:** 2026-03-14
**Test file:** `system/test/tests/handle_lifecycle.rs` (30 tests)

---

## 1. Channel (HandleObject::Channel(ChannelId))

### Creation Paths

1. **`sys_channel_create` (syscall #5):**
   - `channel::create()` allocates 2 physical pages, returns `(ChannelId, ChannelId)`.
   - Both endpoint handles inserted into caller's handle table.
   - Both pages mapped via per-process channel SHM bump allocator.
   - **Rollback:** If second handle insert fails → close first handle. If first page
     map succeeds but second fails → unmap first page PTE, close both handles.
     On any failure after `channel::create()` → `close_endpoint(ch_a)` + `close_endpoint(ch_b)`.

2. **Boot-time (`main.rs`):**
   - `channel::create()` → `channel::setup_endpoint(ch_b, init_pid)` (maps pages + inserts handle).
   - Kernel closes its own endpoint (`ch_a`) immediately.

### Transfer

- **`sys_handle_send` (syscall #22):** Move semantics (close from source, insert into target).
  For Channel handles, both shared pages are mapped into target's address space.
  **Rollback:** If target insert fails → unmap both pages from target → re-insert
  handle at original source slot via `insert_at`.

### Close

- **`sys_handle_close` (syscall #3):** `table.close(handle)` → `channel::close_endpoint(id)`.
  - Increments `closed_count`.
  - Wakes peer's waiter (two-phase: channel lock → release → scheduler lock).
  - When `closed_count == 2`: frees both shared physical pages.
  - Guard: `closed_count >= 2` early return prevents double-free.

### Process Exit Cleanup

- Handle table drained by `categorize_handles()`.
- Each `ChannelId` → `channel::close_endpoint()` via `close_handle_categories()`.
- Both `exit_current_from_syscall` (last thread) and `kill_process` paths drain handles.

### Leak Analysis: **No leaks.** ✅

---

## 2. Timer (HandleObject::Timer(TimerId))

### Creation

- **`sys_timer_create` (syscall #13):**
  - `timer::create(timeout_ns)` allocates a slot in the 32-slot timer table.
  - Handle inserted into caller's handle table.
  - **Rollback:** If handle insert fails → `timer::destroy(timer_id)`.

### Transfer

- Timers can be transferred via `handle_send` (generic handle move).
  No special resource setup needed (unlike Channels which need page mapping).

### Close

- **`sys_handle_close`:** `table.close(handle)` → `timer::destroy(id)`.
  - Clears the timer slot.
  - Wakes any thread blocked in `sys_wait` on this timer (via `WaitableRegistry::destroy`).

### Process Exit Cleanup

- Handle table drained → `timer::destroy()` for each TimerId.

### Internal Timeout Timers

- `sys_wait` creates internal timeout timers with `TIMEOUT_SENTINEL` index.
- Stored on the thread's `timeout_timer` field.
- Destroyed either on successful `sys_wait` return (non-blocked path) or
  at the start of the next `sys_wait` call (`take_timeout_timer`).

### Leak Analysis: **No leaks.** ✅

---

## 3. Interrupt (HandleObject::Interrupt(InterruptId))

### Creation

- **`sys_interrupt_register` (syscall #14):**
  - `interrupt::register(irq)` allocates a slot, enables IRQ in GIC.
  - Handle inserted into caller's handle table.
  - **Rollback:** If handle insert fails → `interrupt::destroy(int_id)`.

### Transfer

- Can be transferred via `handle_send` (generic handle move).

### Close

- **`sys_handle_close`:** `table.close(handle)` → `interrupt::destroy(id)`.
  - Clears the slot, takes the IRQ number.
  - Disables the IRQ in the GIC distributor.
  - Wakes any thread blocked in `sys_wait` on this interrupt.

### Process Exit Cleanup

- Handle table drained → `interrupt::destroy()` for each InterruptId.

### Leak Analysis: **No leaks.** ✅

---

## 4. Thread (HandleObject::Thread(ThreadId))

### Creation

- **`sys_thread_create` (syscall #19):**
  - `scheduler::spawn_user(process_id, entry_va, stack_top)` creates thread,
    increments `process.thread_count`, binds default scheduling context,
    pushes to ready queue.
  - `thread_exit::create(thread_id)` creates exit notification state.
  - Handle inserted into caller's handle table.
  - **Rollback:** If handle insert fails → `thread_exit::destroy(thread_id)`.
    The thread itself is NOT killed (already running). The caller loses tracking
    ability but the thread will exit normally and trigger proper cleanup.

### Transfer

- Can be transferred via `handle_send` (generic handle move).

### Close

- **`sys_handle_close`:** `table.close(handle)` → `thread_exit::destroy(id)`.
  - Removes the WaitableRegistry entry.
  - Wakes any thread blocked waiting for this thread's exit.

### Thread Exit Notification

- When a thread exits (`exit_current_from_syscall`):
  - `thread_exit::notify_exit(thread_id)` marks the entry ready and wakes the waiter.
  - If the entry was already destroyed (handle closed before exit), `notify` returns
    `None` — harmless no-op.

### Process Exit Cleanup

- Handle table drained → `thread_exit::destroy()` for each ThreadId.

### Leak Analysis: **No leaks.** ✅

---

## 5. Process (HandleObject::Process(ProcessId))

### Creation

- **`sys_process_create` (syscall #20):**
  - `process::create_from_user_elf(elf_data)` creates process + suspended thread.
  - `process_exit::create(process_id)` creates exit notification state.
  - Handle inserted into caller's handle table.
  - **Rollback:** If handle insert fails → `scheduler::kill_process(process_id)` +
    full cleanup (notify exits, close resources, free address space) +
    `process_exit::destroy(process_id)`.

### Transfer

- Can be transferred via `handle_send` (generic handle move).
  `handle_send` only works on unstarted processes.

### Close

- **`sys_handle_close`:** `table.close(handle)` → `process_exit::destroy(id)`.
  - Removes the WaitableRegistry entry.
  - Wakes any thread blocked waiting for this process's exit.

### Process Exit Notification

- When a process's last thread exits:
  - `process_exit::notify_exit(process_id)` marks the entry ready and wakes the waiter.
  - If the entry was already destroyed (handle closed before exit), `notify` returns
    `None` — harmless no-op.

### Process Exit Cleanup

- Handle table drained → `process_exit::destroy()` for each child ProcessId.

### `process_kill` (syscall #23)

- `scheduler::kill_process(target_pid)`:
  - Removes threads from ready/blocked/suspended, calls `release_thread_context_ids`.
  - Marks running threads as Exited.
  - Drains handle table, categorizes handles.
  - Returns `KillInfo` for caller to perform Phase 2+ cleanup.
- Caller performs: notify exits, close channels/timers/interrupts/thread/process handles,
  free address space (immediate or deferred via `maybe_cleanup_killed_process`).

### Leak Analysis: **No leaks.** ✅

---

## 6. SchedulingContext (HandleObject::SchedulingContext(SchedulingContextId))

### Creation

- **`sys_scheduling_context_create` (syscall #6):**
  - `scheduler::create_scheduling_context(budget, period)` allocates a slot with `ref_count=1`.
  - Handle inserted into caller's handle table.
  - **Rollback:** If handle insert fails → `scheduler::release_scheduling_context(ctx_id)`
    (decrements ref_count to 0, frees the slot).

### Reference Counting

| Operation | ref_count change |
|-----------|-----------------|
| `create_scheduling_context` | +1 (initial handle reference) |
| `scheduling_context_bind` | +1 (thread bind) |
| `scheduling_context_borrow` | +1 (thread borrow) |
| `scheduling_context_return` | -1 (returns borrow) |
| `handle_close` → `release_scheduling_context` | -1 (handle reference) |
| Thread exit (`exit_current_from_syscall`) | -1 per bound/borrowed ref |
| `kill_process` → `release_thread_context_ids` | -1 per thread's bound/borrowed ref |
| `bind_default_context` (kernel-spawned threads) | +1 |
| Process exit handle drain (`categorize_handles`) | -1 (released immediately) |

### Transfer

- **`handle_send`:** Move semantics — no ref_count change (same logical reference
  transferred from source to target).

### Close

- **`sys_handle_close`:** `table.close(handle)` → `scheduler::release_scheduling_context(id)`.
  - Decrements ref_count via `release_context_inner`.
  - If ref_count reaches 0: frees the slot, pushes ID to `free_context_ids` for reuse.

### Process Exit Cleanup

- Handle table drained by `categorize_handles()`.
- SchedulingContext handles are released immediately inside `categorize_handles`
  (under the scheduler lock) — they don't need deferred cleanup like channels.
- Thread context refs are released via `release_thread_context_ids` for each thread.

### Freed when `ref_count == 0` iff:
- All handles pointing to this context are closed, AND
- All threads bound to this context have exited (releasing their bind ref), AND
- All threads borrowing this context have returned (releasing their borrow ref).

### Leak Analysis: **No leaks.** ✅

---

## Cross-Cutting Verification

### Handle Drain Completeness

`HandleTable::drain()` yields ALL occupied slots and clears them. The `categorize_handles`
function sorts objects into typed buckets. `close_handle_categories` closes each:
- Channels → `channel::close_endpoint`
- Timers → `timer::destroy`
- Interrupts → `interrupt::destroy`
- Thread handles → `thread_exit::destroy`
- Process handles → `process_exit::destroy`
- SchedulingContexts → `release_context_inner` (immediate, under scheduler lock)

### Rollback Completeness (Error Paths)

Every syscall that creates a kernel resource AND inserts a handle has a rollback path
that cleans up the resource if the handle insert fails:
- `channel_create`: close both endpoints + free pages
- `timer_create`: destroy timer
- `interrupt_register`: destroy interrupt (disables IRQ)
- `thread_create`: destroy exit notification (thread continues running)
- `process_create`: kill process + destroy exit notification + free address space
- `scheduling_context_create`: release context (ref_count→0, freed)

### handle_send Rollback

If target insert fails, the source handle is restored via `insert_at` at its original
slot. For Channels, any mapped pages in the target are unmapped. The underlying
kernel resource is never orphaned.

### Double-Close Prevention

- `HandleTable::close()` returns `InvalidHandle` on empty slots.
- `channel::close_endpoint()` guards `closed_count >= 2` (prevents double-free).
- `release_context_inner` uses `saturating_sub` (prevents underflow).
- `WaitableRegistry::destroy()` returns `None` for missing entries.
