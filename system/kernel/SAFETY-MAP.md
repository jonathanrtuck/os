# Kernel Safety Invariant Map

Comprehensive cross-cutting safety invariants for the Document OS kernel.
Synthesizes all milestone 5 findings: lock ordering, TPIDR_EL1 chain,
handle lifecycle, process exit cleanup, allocator routing, ASID lifecycle,
scheduling context ref_count, and emergency stack sizing.

**Created:** 2026-03-14
**Source files:** 35 .rs + 2 .S across `system/kernel/`
**Companion documents:**

- `LOCK-ORDERING.md` — full lock acquisition site table
- §2 below — TPIDR_EL1 invariant chain (consolidated from former TPIDR-CHAIN.md)
- `DESIGN.md` — architectural rationale for each subsystem

---

## 1. Lock Ordering DAG

All kernel synchronization uses `IrqMutex<T>` — a ticket spinlock that masks
IRQs (DAIF.I) on acquire and restores on release. 13 instances total.

### Ordering Levels

| Level | Locks                                                       | Rationale                                    |
| ----- | ----------------------------------------------------------- | -------------------------------------------- |
| 0     | channel, timer, interrupt, thread_exit, process_exit, futex | Event sources — release before scheduler     |
| 1     | scheduler                                                   | Coordinator — never held with level 0        |
| 2     | page_allocator, address_space_id, slab                      | Resource managers — acquired under scheduler |
| 3     | KERNEL_PT_LOCK, ALLOC_LOCK                                  | Low-level — acquired under level 2           |
| ∞     | serial                                                      | Output-only leaf — never nests with others   |

### DAG Edges (verified cycle-free)

```text
Level 0 → [release] → Level 1 (two-phase wake pattern, 11 paths)
Level 1 → Level 2 (schedule_inner → free_all, address_space_id::free)
Level 2: slab → page_allocator (grow)
Level 3: KERNEL_PT_LOCK → page_allocator (alloc_kernel_stack_guarded)
Implicit: scheduler → ALLOC_LOCK → slab → page_allocator (via Vec::push)
```

### Two-Phase Wake Pattern

The kernel's central concurrency pattern: event source locks are **always
released before** acquiring the scheduler lock.

```text
Phase 1: acquire event lock → collect waiter ThreadId → release lock
Phase 2: scheduler::try_wake_for_handle(tid) [acquires scheduler lock]
```

All 6 waitable types (channel, timer, interrupt, thread_exit, process_exit,
futex) follow this pattern across 11 wake paths. Verified: no code path holds
a level-0 lock and the scheduler lock simultaneously.

### Nested Lock Paths (3 explicit + 1 implicit)

1. **scheduler → page_allocator/address_space_id:** `schedule_inner` →
   `maybe_cleanup_killed_process` → `addr_space.free_all()` + `address_space_id::free()`
2. **slab → page_allocator:** `slab::try_alloc` → `SlabCache::grow()` →
   `page_allocator::alloc_frame()`
3. **KERNEL_PT_LOCK → page_allocator:** `memory::alloc_kernel_stack_guarded` →
   `page_allocator::alloc_frame()`
4. **Implicit (scheduler → ALLOC_LOCK → slab → page_allocator):** Any
   `Vec::push()` or `Box::new()` under the scheduler lock triggers GlobalAlloc.

**Cycle-freedom proof:** For every directed edge, verified the reverse edge
does NOT exist by searching for lock acquisitions in the target module.

### Interrupt Safety

- All 13 locks mask IRQs → no timer/interrupt re-entry while any lock is held.
- Self-deadlock prevented: ticket spinlock + IRQ masking.
- Panic handler: `serial::panic_puts()` bypasses locks to avoid deadlock.

**Full details:** `LOCK-ORDERING.md`

---

## 2. TPIDR_EL1 Invariant Chain

**Invariant:** On every core, `TPIDR_EL1` always points to the current thread's
`Context` struct (at offset 0 of the `Thread`).

This is the backbone of context save/restore — `exception.S` reads `TPIDR_EL1`
to locate the save area on every exception entry.

### Structural Foundation

- `Thread.context` is the first field (`#[repr(C)]`), enforced by compile-time
  assertion: `offset_of!(Thread, context) == 0`.
- All threads are `Box<Thread>` — heap-allocated with stable addresses.
- `TPIDR_EL1` is NOT saved in Context — it points _to_ the Context.

### Write Sites (6 total)

| #   | Location                                          | When                 | IRQ State             |
| --- | ------------------------------------------------- | -------------------- | --------------------- |
| 1   | `scheduler::init()` (scheduler.rs:1047)           | Core 0 boot          | Disabled (no GIC yet) |
| 2   | `scheduler::init_secondary()` (scheduler.rs:1076) | Secondary core boot  | Disabled (lock held)  |
| 3   | `schedule_inner()` (scheduler.rs:431) **PRIMARY** | Every context switch | Disabled (lock held)  |
| 4   | `exception.S:347` (exc_irq)                       | After IRQ handler    | Disabled (exception)  |
| 5   | `exception.S:372` (exc_lower_sync)                | After SVC handler    | Disabled (exception)  |
| 6   | `exception.S:390` (exc_user_fault)                | After fault handler  | Disabled (exception)  |

Write 3 is the critical one (Fix 17). Writes 4–6 are defense-in-depth (redundant).

### Read Sites (4 total)

| #   | Location                                  | Purpose                      |
| --- | ----------------------------------------- | ---------------------------- |
| 1   | `save_context` (exception.S:223)          | Locate register save area    |
| 2   | `exc_fatal` (exception.S:193)             | Diagnostic (range-validated) |
| 3   | `handler_returned_null` (exception.S:402) | Diagnostic                   |
| 4   | `kernel_fault_handler` (main.rs:541)      | Diagnostic (range-validated) |

### Fix 17 (2026-03-14): TPIDR Race Under SMP

**Bug:** `schedule_inner` returned the new thread's Context pointer. Exception.S
wrote `TPIDR_EL1` AFTER the Rust handler returned, but the `IrqMutex` guard's
`Drop` restored DAIF (re-enabling IRQs) before the exception.S write. A timer
IRQ in the ~3-instruction window caused `save_context` to corrupt the old
thread's Context with kernel-mode state.

**Fix:** Write `msr tpidr_el1` inside `schedule_inner` while the lock is held.
No `nomem` option — prevents LLVM from reordering past the lock release.

**Validation:** 3000-key stress test with 4 SMP cores, no crashes.

**Full details:** previously in TPIDR-CHAIN.md (consolidated into this document).

---

## 3. Handle Lifecycle Per Type

Per-process handle table: 256 fixed-size slots. 6 handle types, each with
creation → transfer → close → cleanup lifecycle.

### Creation & Rollback

| Handle Type       | Creation Syscall                 | Rollback on Insert Failure                    |
| ----------------- | -------------------------------- | --------------------------------------------- |
| Channel           | `channel_create` (#5)            | Close both endpoints + free pages             |
| Timer             | `timer_create` (#13)             | `timer::destroy(id)`                          |
| Interrupt         | `interrupt_register` (#14)       | `interrupt::destroy(id)` (disables IRQ)       |
| Thread            | `thread_create` (#19)            | `thread_exit::destroy(id)` (thread continues) |
| Process           | `process_create` (#20)           | `kill_process` + `process_exit::destroy`      |
| SchedulingContext | `scheduling_context_create` (#6) | `release_scheduling_context` (ref→0, freed)   |

### Transfer (`handle_send`, syscall #22)

Move semantics: source handle closed, inserted into target. For Channel handles,
shared pages are mapped into target's address space. Rollback on failure:
re-insert source handle at original slot; unmap any channel pages from target.

### Close (`handle_close`, syscall #3)

Each type has a specific cleanup function:

- Channel → `channel::close_endpoint()` (frees pages when both endpoints close)
- Timer → `timer::destroy()` (frees slot, wakes waiter)
- Interrupt → `interrupt::destroy()` (disables IRQ in GIC, wakes waiter)
- Thread → `thread_exit::destroy()` (removes entry, wakes waiter)
- Process → `process_exit::destroy()` (removes entry, wakes waiter)
- SchedulingContext → `release_scheduling_context()` (decrements ref_count)

### Double-Close Prevention

- `HandleTable::close()` returns `InvalidHandle` on empty slots.
- `channel::close_endpoint()` guards `closed_count >= 2`.
- `release_context_inner` uses `saturating_sub`.
- `WaitableRegistry::destroy()` returns `None` for missing entries.

### Process Exit: Handle Drain

`HandleTable::drain()` yields ALL occupied slots. `categorize_handles()` sorts
by type. SchedulingContext handles are released immediately (under scheduler lock).
All other types are closed outside the lock via `close_handle_categories()`.

**Leak analysis:** No leaks found for any handle type. ✅

**Full details:** `.factory/library/handle-lifecycle.md`

---

## 4. Process Exit Cleanup

Two paths: normal exit (last-thread-exit via `exit_current_from_syscall`) and
kill (`process_kill` + deferred cleanup for running threads on other cores).

### Resource Categories Reclaimed

| Resource                            | Mechanism                                                   |
| ----------------------------------- | ----------------------------------------------------------- |
| Handles (all 6 types)               | `drain()` → `categorize_handles()` → per-type close         |
| Scheduling context bind/borrow refs | `release_thread_context_ids()` or `release_context_inner()` |
| Internal timeout timers             | Collected and destroyed (Fix: was previously leaked)        |
| Futex wait entries                  | `futex::remove_thread(tid)` scans all 64 buckets            |
| DMA buffers                         | `free_all()` frees each `(pa, order)`                       |
| Owned page frames                   | `free_all()` frees each frame                               |
| Page table frames (L1–L3)           | `free_all()` walks L0→L1→L2→L3                              |
| L0 page table                       | `free_all()` frees at end                                   |
| TLB entries                         | `invalidate_tlb()` → `TLBI ASIDE1IS`                        |
| ASID                                | `address_space_id::free(asid)`                              |
| Kernel stack                        | `Thread::Drop` → `free_frames(stack_pa, order)`             |
| Thread object                       | `deferred_drops` in `schedule_inner`                        |
| Process slot                        | `.take()` sets slot to `None`                               |

### Bug Found and Fixed

**Internal timeout timer leak:** Threads killed while blocked in `wait` with a
finite timeout leaked their internal timer (not tracked in handle table). Fixed:
`kill_process` and `exit_current_from_syscall` now collect and destroy timeout timers.

### Deferred Cleanup Path

When `kill_process` finds threads running on other cores: marks them Exited,
sets `process.killed = true`, `thread_count = running_count`. Each running thread
triggers `maybe_cleanup_killed_process` in `schedule_inner` when parked. When
`thread_count` reaches 0, address space is freed inline.

### Soft Leak: Stale Waiter Registrations

Documented, not fixed (acceptable). Dead ThreadId in waiter slots produces no-op
on wake attempts. ThreadIds are monotonically increasing (no reuse).

**Full details:** `.factory/library/process-exit-cleanup.md`

---

## 5. Allocator Routing

Three-tier allocation with address-based dealloc routing.

### Allocation Flow

```
GlobalAlloc::alloc(layout)
  ├─ size ≤ 2048 → slab::try_alloc() → O(1) from slab cache
  │   └─ slab full → slab::grow() → page_allocator::alloc_frame()
  │   └─ slab not init / grow fails → fall through to linked-list
  └─ size > 2048 OR slab miss → linked-list first-fit → O(n)
```

### Deallocation Routing (Address-Based, NOT Size-Based)

```
GlobalAlloc::dealloc(ptr, layout)
  ├─ ptr ∈ [region_start, region_end) → linked-list free
  └─ ptr ∉ heap region → slab::try_free()
```

**Why address-based:** During early boot (before `page_allocator::init()`), slab
can't grow, so the linked-list serves ALL allocations (including small ones). If
dealloc routed by size class, those early-boot frees would go to slab — corrupting
its free list with linked-list addresses.

### Page Frame Allocator (Buddy)

- Orders 0–11 (4 KiB – 8 MiB).
- `free_frames()` validates PA is page-aligned and within RAM range before writing.
- `alloc_frame()` returns zeroed pages (single frame = order 0).

### OOM Behavior

- `GlobalAlloc::alloc()` returns null on exhaustion.
- Rust's `handle_alloc_error` panics on null → kernel-heap OOM is fatal.
- All user-controlled paths (`process_create`, `thread_create`, `channel_create`)
  may trigger kernel-heap OOM. None return errors to userspace on OOM.
- Kernel heap (16 MiB) stores only kernel objects; user data lives in
  demand-paged user address spaces.

---

## 6. ASID Lifecycle

ASID (Address Space Identifier) tags TLB entries per-process. 8-bit ASIDs
(1–255, ASID 0 reserved for kernel). Generation-based recycling on exhaustion.

### Allocation (`address_space_id::alloc`)

**Called from:** `process::create_from_user_elf()` (process.rs:102).

1. Lock `STATE`.
2. Mark ASID 0 as always in-use (kernel reserved).
3. Search bitmap starting from `next_hint` for free ASID (1–255).
4. If found: set bit, advance hint, return `(Asid, generation)`.
5. If all 255 exhausted → **generation rollover** (see below).

### Generation Rollover (Exhaustion Path)

When all 255 ASIDs are in use:

1. Execute `DSB ISHST + TLBI VMALLE1IS + DSB ISH + ISB` — flushes ALL TLB
   entries across all cores (cfg-gated to `target_os = "none"`).
2. Increment `generation` counter (u64, no practical overflow).
3. Clear entire bitmap, re-reserve ASID 0.
4. Allocate ASID 1 for the caller, set `next_hint = 2`.

**Lazy revalidation:** Each `AddressSpace` stores `(asid, generation)`. On
context switch, if the thread's generation != global generation, the ASID is
stale and must be re-acquired. (Design principle from Linux
`arch/arm64/mm/context.c`; lazy path not yet implemented — current kernel
doesn't check generation on switch.)

### Free (`address_space_id::free`)

**Called from 4 sites:**

| Site                                       | File:Line            | Context                           |
| ------------------------------------------ | -------------------- | --------------------------------- |
| `process::create_from_user_elf` error path | process.rs:106       | L0 alloc failed after ASID alloc  |
| `sys_process_kill` immediate cleanup       | syscall.rs:783       | No running threads on other cores |
| `exit_current_from_syscall` Phase 4        | syscall.rs:855       | Last thread exit                  |
| `maybe_cleanup_killed_process`             | scheduler.rs:259     | Deferred cleanup (killed process) |
| `AddressSpace::Drop` safety net            | address_space.rs:703 | Error path catchall               |

**Mechanism:** Clear the ASID's bit in the bitmap. ASID 0 free is a no-op (guard).
Double-free is harmless (clears already-clear bit).

### End-to-End Trace

```
process::create_from_user_elf()
  │ address_space_id::alloc() → (Asid(N), gen)
  │ AddressSpace::new(asid) → L0 page table allocated
  │ ... map segments, create thread ...
  ↓
[process runs — TTBR0 contains L0_PA | (ASID << 48)]
  ↓
process exit (last thread) OR process_kill
  │ addr_space.invalidate_tlb() → TLBI ASIDE1IS (flushes this ASID's entries)
  │ addr_space.free_all() → frees all page frames + table frames
  │ address_space_id::free(asid) → clears bitmap bit
  ↓
[ASID available for reuse by next alloc()]
```

### Invariants

1. **Every `alloc()` has a matching `free()`:** Verified across all 4 free sites +
   the `AddressSpace::Drop` safety net.
2. **TLB flushed before free:** Both exit and kill paths call `invalidate_tlb()`
   before `free_all()` and `address_space_id::free()`.
3. **Generation rollover flushes globally:** `TLBI VMALLE1IS` invalidates all
   TLB entries on all cores, making all stale ASIDs safe to reallocate.
4. **No use-after-free:** ASID is freed only after TLB invalidation + page frame
   deallocation. No stale TLB entry can reference freed memory.

---

## 7. Scheduling Context ref_count

Scheduling contexts are reference-counted kernel objects. Freed when ref_count
reaches 0.

### All Increment Sites (+1)

| Operation                         | Location          | When                                        |
| --------------------------------- | ----------------- | ------------------------------------------- |
| `create_scheduling_context`       | scheduler.rs:836  | Initial handle reference (ref_count=1)      |
| `scheduler::init` default context | scheduler.rs:1038 | State holds logical reference (ref_count=1) |
| `bind_scheduling_context`         | scheduler.rs:638  | Thread binds to context                     |
| `borrow_scheduling_context`       | scheduler.rs:717  | Thread borrows context (donation)           |
| `bind_default_context`            | scheduler.rs:166  | Kernel-spawned thread gets default context  |

### All Decrement Sites (-1)

| Operation                              | Location                    | When                                         |
| -------------------------------------- | --------------------------- | -------------------------------------------- |
| `release_scheduling_context`           | scheduler.rs:1239           | Handle close → `release_context_inner`       |
| `return_scheduling_context`            | scheduler.rs:1262           | Return borrowed context                      |
| `exit_current_from_syscall` Phase 1    | scheduler.rs:910,913        | Exiting thread releases bind+borrow          |
| `kill_process` ready/blocked/suspended | scheduler.rs:1115,1135,1155 | `release_thread_context_ids` per thread      |
| `kill_process` running threads         | scheduler.rs:1199           | Deferred context ID release                  |
| `categorize_handles`                   | scheduler.rs:188            | Handle drain releases SC handles immediately |

### `release_context_inner` (the single free path)

```rust
fn release_context_inner(s: &mut State, ctx_id: SchedulingContextId) {
    entry.ref_count = entry.ref_count.saturating_sub(1);
    if entry.ref_count == 0 {
        *slot = None;
        s.free_context_ids.push(ctx_id.0);  // ID available for reuse
    }
}
```

### `release_thread_context_ids` (helper for exit/kill)

```rust
fn release_thread_context_ids(s: &mut State, thread: &mut Thread) {
    if let Some(id) = thread.scheduling.context_id.take() {
        release_context_inner(s, id);  // -1 for bind
    }
    if let Some(id) = thread.scheduling.saved_context_id.take() {
        release_context_inner(s, id);  // -1 for saved (borrow)
    }
}
```

### ref_count Reaches 0 IFF

1. All handles pointing to this context are closed (each close → `-1`), AND
2. All threads bound to this context have exited (`release_thread_context_ids` → `-1`), AND
3. All threads borrowing this context have returned (`return_scheduling_context` → `-1`).

### Invariants

1. **No underflow:** `saturating_sub(1)` prevents underflow below 0.
2. **Freed exactly once:** `ref_count == 0` triggers `*slot = None` + push to
   `free_context_ids`. Subsequent decrements on a `None` slot are no-ops.
3. **All paths covered:** Both normal exit and kill paths release all per-thread
   refs via `release_thread_context_ids`. Handle drain releases handle refs via
   `categorize_handles`.

---

## 8. Emergency Stack Sizing

Per-core emergency exception stacks for `exc_fatal` (EL1 fault handler).

### Allocation (exception.S:447–449)

```asm
// Per-core emergency exception stacks (4 KiB × MAX_CORES).
// Must match per_core::MAX_CORES.
.global __exc_stacks
__exc_stacks:
  .space 4096 * 8
```

### Constant (per_core.rs:14)

```rust
pub const MAX_CORES: usize = 8;
```

### Verification

| Source               | Value                             | Match? |
| -------------------- | --------------------------------- | ------ |
| `exception.S:449`    | `.space 4096 * 8` (32 KiB)        | ✅     |
| `per_core.rs:14`     | `MAX_CORES = 8`                   | ✅     |
| `link.ld:84` comment | `4 KiB × MAX_CORES, page-aligned` | ✅     |

### Stack Selection (exception.S:200,408)

```asm
ldr x5, =__exc_stacks
mrs x6, mpidr_el1
and x6, x6, #0xFF          // core_id
add x6, x6, #1             // (core_id + 1)
lsl x6, x6, #12            // * 4096
add sp, x5, x6             // SP = __exc_stacks + (core_id + 1) * 4096
```

Each core gets a 4 KiB emergency stack. Core N uses bytes
`[__exc_stacks + N*4096, __exc_stacks + (N+1)*4096)`.

### Historical Bug (DESIGN.md §11.1, Fixed)

Previously `.space 4096 * 4` — only 4 cores. Core 4–7 would compute SP past the
allocation, clobbering adjacent `.bss` data. Fixed to `4096 * 8`.

### Invariant

**`exception.S` emergency stack allocation MUST equal `4096 * per_core::MAX_CORES`.**
Any change to `MAX_CORES` requires updating `exception.S` in sync.

---

## 9. Cross-Cutting Invariant Summary

| #   | Invariant                                                 | Verified By                            | Violation Symptom                 |
| --- | --------------------------------------------------------- | -------------------------------------- | --------------------------------- |
| 1   | Lock ordering is a strict DAG (no cycles)                 | Enumerated all ~80 `.lock()` sites     | Deadlock under SMP                |
| 2   | Two-phase wake: event lock released before scheduler lock | Verified 11 wake paths                 | Deadlock (level-0 + level-1 held) |
| 3   | `TPIDR_EL1` always points to current thread's Context     | 6 writes, 4 reads traced               | Context corruption, EC=0x21 crash |
| 4   | `TPIDR_EL1` updated under scheduler lock (Fix 17)         | Code review + stress test              | Stale TPIDR race window           |
| 5   | `Context` at offset 0 of `Thread`                         | `#[repr(C)]` + compile-time assertions | All register offsets wrong        |
| 6   | `Box<Thread>` for stable addresses                        | Code convention                        | Use-after-free on `save_context`  |
| 7   | Every handle creation has rollback on insert failure      | All 6 types verified                   | Resource leak                     |
| 8   | `handle_send` uses move semantics                         | Code review                            | Double-close → use-after-free     |
| 9   | Process exit reclaims ALL resources                       | 13 resource types traced               | Memory/ASID/timer leak            |
| 10  | Internal timeout timers cleaned up on kill/exit           | Bug fix verified                       | Timer table exhaustion            |
| 11  | Allocator dealloc routes by address, not size             | Code review                            | Cross-allocator contamination     |
| 12  | `free_frames()` validates PA alignment + range            | Code review                            | Silent corruption                 |
| 13  | Every ASID `alloc()` has matching `free()`                | 4 free sites + Drop safety net         | ASID exhaustion                   |
| 14  | TLB invalidated before ASID free                          | All 4 free paths checked               | Use-after-free via stale TLB      |
| 15  | Generation rollover flushes all TLB entries globally      | Code review                            | Stale ASID → wrong address space  |
| 16  | Scheduling context ref_count: no underflow                | `saturating_sub`                       | Premature free / slot corruption  |
| 17  | Scheduling context freed exactly when ref_count=0         | All +1/-1 sites traced                 | Leak or use-after-free            |
| 18  | Emergency stack sizing = 4096 × MAX_CORES                 | grep match (both = 8)                  | Stack overflow on core 4–7        |
| 19  | No `nomem` on system register writes                      | Code audit (34 asm blocks)             | Compiler reordering → races       |
| 20  | W^X enforcement on all user pages                         | `segment_attrs` in address_space.rs    | Code injection                    |
| 21  | Break-before-make on PTE updates                          | `map_inner` zeros + TLBI before write  | CONSTRAINED UNPREDICTABLE         |
| 22  | `deferred_drops` prevents use-after-free on thread stacks | schedule_inner pattern                 | Stack corruption                  |
