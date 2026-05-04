# Kernel Hardening Plan

Expert review of the v0.7 kernel found 13 issues across three categories:
correctness bugs, architectural gaps, and performance limitations. This plan
addresses every finding with the most correct and highest performance solution,
not the easiest fix. Target: a kernel that could withstand review by a
professional OS researcher.

## Principles

1. **Correct first.** A fast wrong answer is worse than a slow right one.
2. **Highest performance among correct solutions.** When multiple correct
   approaches exist, choose the one with the best steady-state performance on M4
   Pro (14 cores, 128-byte cache lines, 16 KiB pages, LSE atomics).
3. **No compromises for simplicity.** If the correct solution is complex, accept
   the complexity. Simple-but-wrong is technical debt.

---

## Phase 1: Correctness Bugs

Targeted fixes. Each is a clear bug with a clear fix. No architectural redesign
needed.

### 1.1 — Thread generation counter

**Bug:** `Thread::generation()` returns 0 unconditionally. Thread handles are
never invalidated by generation-count revocation. After a thread exits and its
slot is reused, stale handles silently refer to the new thread.

**Fix:** Add `generation: u64` field to `Thread`. Increment in
`Thread::revoke()` (same pattern as VMO, Event, Endpoint). Wire generation into
`ObjectTable::dealloc` — when a slot is freed and later reused, the new occupant
starts with generation 0 but any handle stamped with generation N will fail the
check.

Actually, the correct approach: the ObjectTable itself should track generation
per-slot, not the objects. When a slot is deallocated and reallocated, the
generation increments. This way every object type gets revocation uniformly and
objects don't need to carry their own generation.

**Files:** `table.rs`, `thread.rs`, `syscall.rs` (handle validation)

**Design:**

```rust
ObjectTable {
    entries: Vec<Option<T>>,
    generations: Vec<u64>,    // per-slot, incremented on dealloc
    ...
}
```

On `dealloc(idx)`: increment `generations[idx]`. On `alloc`: the returned index
carries the current generation. Handle allocation stores this generation. Handle
lookup compares stored generation vs. current — mismatch means the object was
replaced.

Remove the per-object `generation` field and `revoke()` method from Vmo,
Endpoint, Event, and AddressSpace. Thread's hardcoded `0` return disappears.
Generation tracking moves to the single correct place: the slot allocator.

**Tests:**

1. Alloc, dealloc, realloc — generation increments
2. Handle created before dealloc fails lookup after realloc
3. Handle created after realloc succeeds

### 1.2 — Address space destruction resource leak

**Bug:** `sys_space_destroy` calls `spaces.dealloc()` and closes the caller's
handle, but does not: kill threads in the space, close handles in the space's
handle table, unmap VMOs, free page table pages, release ASID, or signal
peer-closed on endpoints.

**Fix:** Full teardown sequence in `sys_space_destroy`:

1. Walk the space's thread list (see below). For each thread: mark Exited,
   remove from scheduler, dealloc.
2. Close every handle in the target space's handle table. For each endpoint
   handle: call `close_peer()` on the endpoint, wake all blocked callers with
   PeerClosed error.
3. Unmap every mapping in the target space. Decrement VMO refcounts.
4. On bare metal: `page_table::destroy_page_table(root, asid)` — walks and frees
   L3 table pages, invalidates ASID TLB entries, releases ASID.
5. `spaces.dealloc(target_id)`.
6. Close caller's handle to the space.

**Per-space thread list.** The naive approach scans all MAX_THREADS (512) slots
to find threads in the destroyed space. This is O(512) when it should be
O(threads_in_space). Add an intrusive linked list of thread IDs per address
space:

```rust
// In AddressSpace:
thread_head: Option<u32>,   // first thread ID in this space

// In Thread:
space_next: Option<u32>,    // next thread in same space
space_prev: Option<u32>,    // prev thread in same space
```

`thread_create` / `thread_create_in`: insert into the target space's list.
`thread_exit` / dealloc: remove from the space's list. `space_destroy`: walk
`thread_head → space_next → ...` — touches only the space's own threads.

**Files:** `syscall.rs`, `address_space.rs`, `thread.rs`, `endpoint.rs`

**Tests:**

1. Destroy space kills threads in that space
2. Destroy space closes handles, endpoints get PeerClosed
3. Destroy space unmaps VMOs, mapping count returns to 0
4. Destroy own space rejected (already implemented)
5. Double-destroy returns InvalidHandle

### 1.3 — ASID allocator deduplication

**Bug:** Two independent ASID allocators: `Kernel::next_asid` (a simple counter
in syscall.rs) and `page_table::alloc_asid()` (an atomic bitmap in
page_table.rs). They diverge on bare metal.

**Fix:** Delete `Kernel::next_asid` and `Kernel::alloc_asid()`. All ASID
allocation flows through `page_table::alloc_asid()`. On test builds (where
page_table is stubbed), provide a test-mode allocator that uses the same atomic
bitmap interface.

The `AddressSpace::new()` constructor should NOT take an ASID parameter.
Instead, ASID allocation happens at space creation time in the syscall layer,
which calls `page_table::alloc_asid()` and passes the result to the space.

**Files:** `syscall.rs`, `page_table.rs`, `bootstrap.rs`

### 1.4 — clock_read returns actual time

**Bug:** `sys_clock_read` returns 0.

**Fix:** Read CNTVCT_EL0 (virtual timer counter, readable from all exception
levels on ARM64). Divide by CNTFRQ_EL0 to convert to nanoseconds. On test
builds, return a monotonic counter from `std::time::Instant`.

Actually — CNTVCT_EL0 is readable from EL0 directly. The spec notes `clock_read`
exists as a fallback and for time virtualization. For highest performance,
userspace should read CNTVCT_EL0 directly (zero syscall cost). But the syscall
must still work correctly for when virtualized time matters.

**Files:** `syscall.rs`, `frame/arch/aarch64/timer.rs`

```rust
fn sys_clock_read(&self, _args: &[u64; 6]) -> Result<u64, SyscallError> {
    #[cfg(target_os = "none")]
    { Ok(frame::arch::timer::read_counter()) }
    #[cfg(not(target_os = "none"))]
    { Ok(0) } // host tests: monotonic mock or zero
}
```

---

## Phase 2: Security Boundary

The user_mem module is the kernel's largest security gap. Fixing it correctly
requires architecture-specific work.

### 2.1 — User pointer validation with LDTR/STTR

**Problem:** `user_mem::read_user_message` and all related functions cast a
user-provided `usize` to a raw pointer and dereference it without validation. A
malicious process can pass kernel VA addresses and leak kernel memory, or pass
unmapped addresses and crash the kernel.

**The correct solution for ARM64:** Use `LDTR`/`STTR` (Load/Store Register with
Translation at EL0). These instructions, when executed at EL1, perform the
memory access using EL0's translation regime and permissions. If the address is
unmapped or has incorrect permissions, a data abort occurs — which the kernel
can catch via a registered recovery handler.

This is the hardware-designed mechanism for exactly this purpose. It is what
Linux, Fuchsia/Zircon, and seL4 use for user memory access.

**Design:**

1. **Recovery table.** A static table of `(faulting_pc, recovery_pc)` pairs. The
   data abort handler checks: if the faulting PC is in the recovery table, jump
   to the recovery PC instead of panicking. The recovery PC sets a "fault
   occurred" flag and returns.

2. **Faulting copy functions.** Replace the current `copy_nonoverlapping` calls
   with LDTR/STTR-based copy loops using 64-bit transfers:

   ```rust
   // In frame/user_mem.rs (bare metal)
   pub fn copy_from_user(dst: &mut [u8], src_va: usize) -> Result<(), SyscallError> {
       // LDTR X (64-bit) loop: 8 bytes per instruction
       // 128-byte message = 16 LDTR instructions
       // Fixup entry in recovery table for each LDTR
       // On fault: return Err(SyscallError::InvalidArgument)
   }

   pub fn copy_to_user(dst_va: usize, src: &[u8]) -> Result<(), SyscallError> {
       // STTR X (64-bit) loop: 8 bytes per instruction
       // On fault: return Err(SyscallError::InvalidArgument)
   }
   ```

   Use 64-bit LDTR/STTR, not byte-level LDTRB. A 128-byte IPC message copies in
   16 LDTR instructions (~16 cycles, L1-hot). Byte-level would be 128
   instructions — 8x slower for no benefit. Handle the sub-8-byte tail with
   LDTRB for the final 0-7 bytes.

3. **Data abort handler update.** In `exception.rs`, the EL1 sync data abort
   handler (EC 0x25) checks the recovery table. If a match: set x0 to error,
   jump to recovery. If no match: fatal exception (existing behavior).

**Why not page table walk?** Walking the page table costs ~80-160 cycles on M4
Pro (2-level walk, each level is a memory access). LDTR/STTR costs ~3 cycles for
valid addresses (L1 hit) and faults naturally for invalid ones. The common case
(valid address) is 25-50x faster.

**Why not just check the VA range?** Range checking (is the address in
userspace?) catches kernel VA attacks but not unmapped-address attacks.
LDTR/STTR catches both, with the MMU doing the actual permission check at
hardware speed.

**Files:** `frame/user_mem.rs`, `frame/arch/aarch64/exception.rs`

**Tests:**

1. Valid user pointer: data copied correctly (existing tests pass)
2. Null pointer with nonzero len: returns error (existing test passes)
3. Kernel VA address: returns error, kernel does not crash
4. On bare metal: unmapped address returns error, kernel continues

### 2.2 — Bound-check user buffer sizes

**Problem:** Even with LDTR/STTR, a large `len` parameter could cause the copy
to read across page boundaries into unrelated mappings. The kernel should
validate: `(ptr, ptr+len)` fits within a single plausible buffer and doesn't
wrap around.

**Fix:** Before any copy, validate:

```rust
if ptr.checked_add(len).is_none() { return Err(InvalidArgument); }
if ptr + len > USER_VA_END { return Err(InvalidArgument); }
```

`USER_VA_END` is the end of the EL0 virtual address range (2^36 for T0SZ=28, 16
KiB granule). Any address above this is kernel space.

**Files:** `frame/user_mem.rs`

---

## Phase 3: SMP Concurrency Model

The most significant architectural change. The current `&mut Kernel` pattern
serializes all syscalls through a single mutable borrow. On a 14-core M4 Pro,
this means 13 idle cores during every syscall.

**Design principle:** Lock-free for individual data structure operations. Locks
only where composite atomicity has no known lock-free decomposition. This
matches what the highest-performance production kernels do — seL4 uses lock-free
fastpath operations, Linux uses RCU for read-heavy paths. The M4 Pro's LSE
atomics (`LDADD`, `CAS`, `SWP`, `LDSET`) execute in 4-6 cycles uncontended and
are specifically designed for lock-free structures.

The critical difference from locks under contention: a lock creates a convoy —
if the lock holder is preempted by a timer interrupt, every other core spins for
the remainder of its quantum (microseconds). A CAS retry costs 6 cycles. On a
14-core machine, the convoy cost dominates at any non-trivial contention level.

### 3.1 — Lock-free ObjectTable and HandleTable

**Goal:** `ObjectTable::get`, `get_mut`, `alloc`, and `dealloc` are lock-free.
Multiple cores access the same table concurrently without any lock.

**Design:** Each slot is an `AtomicPtr<T>` (or `AtomicU64` encoding a tagged
pointer). Operations use CAS:

```rust
pub struct ObjectTable<T, const MAX: usize> {
    slots: Vec<AtomicPtr<T>>,       // null = free
    generations: Vec<AtomicU64>,     // per-slot, incremented on dealloc
    free_stack: AtomicU32,           // Treiber stack head (lock-free)
    free_next: Vec<AtomicU32>,       // per-slot next pointer for free stack
    count: AtomicUsize,
}
```

**Alloc (lock-free Treiber stack pop):**

```rust
loop {
    let head = free_stack.load(Acquire);
    if head == EMPTY { return None; }
    let next = free_next[head].load(Relaxed);
    if free_stack.compare_exchange_weak(head, next, AcqRel, Relaxed).is_ok() {
        // head is ours — no other core can claim it
        slots[head].store(Box::into_raw(Box::new(value)), Release);
        count.fetch_add(1, Relaxed);
        return Some(head);
    }
    // CAS failed — another core got it first. Retry (~6 cycles).
}
```

**Dealloc (lock-free Treiber stack push):**

```rust
let ptr = slots[idx].swap(null_mut(), AcqRel);
// ptr is now exclusively ours
drop(Box::from_raw(ptr));
generations[idx].fetch_add(1, Release);
loop {
    let head = free_stack.load(Relaxed);
    free_next[idx].store(head, Relaxed);
    if free_stack.compare_exchange_weak(head, idx, Release, Relaxed).is_ok() {
        break;
    }
}
count.fetch_sub(1, Relaxed);
```

**Get (wait-free):** `slots[idx].load(Acquire)` — a single atomic load. This is
the IPC hot path (handle lookup reads the object). Zero synchronization cost
beyond the load itself (~3 cycles, L1 hit).

**Get_mut consideration:** Mutable access to an object while other cores might
read it requires either per-object interior mutability or a protocol that
ensures exclusive access. For kernel objects that are always accessed through
syscalls (one syscall at a time per thread, and each object has a natural
owner), the access patterns are:

- **Events:** Signal (write bits) races with wait (read bits). `fetch_or` for
  signal, load for check — both atomic. Waiter list needs CAS-based slots (see
  3.3).
- **Endpoints:** Enqueue (MPSC write) races with dequeue (single consumer).
  Lock-free MPSC queue (see 3.4).
- **VMOs:** Page alloc/read are rarely concurrent on the same VMO. Per-VMO
  SpinLock is acceptable for the rare mutation case (resize, seal).
- **Address spaces:** Handle lookup is read-only (hot). Handle install/close is
  write (rare). Per-space RwLock for the handle table (see 3.2).

### 3.2 — Lock-free handle lookup, CAS-based handle mutation

**The handle table is the single hottest data structure in the kernel.** Every
syscall starts with a handle lookup. This must be zero-overhead.

**Design:** HandleTable entries are `AtomicU64`-encoded slots. A handle is
packed into 64 bits:

```rust
// Packed handle: [type:3][rights:9][generation:20][object_id:32]
// Total: 64 bits — fits in a single AtomicU64 for atomic read/write
```

Wait — Handle also carries a badge (u32). That pushes past 64 bits. Two
approaches:

**Approach A: Two-word slot.** `[AtomicU64 packed_handle, AtomicU32 badge]`.
Lookup reads the packed handle atomically (one load). Badge is read separately
only when needed (IPC receive path, not every syscall). The hot-path lookup
(type check + rights check + object ID extraction) is a single atomic load + bit
manipulation.

**Approach B: Separate badge table.** Badges in a parallel array, only accessed
during IPC. Keeps the handle slot at exactly 64 bits.

**Recommendation:** Approach A. Two adjacent atomics fit in the same cache line.
The handle lookup path touches one atomic; the IPC path touches both. No
contention between lookup and badge-read (both are reads).

**Lookup (wait-free):** Load `AtomicU64`, extract fields, verify generation
against `ObjectTable.generations[object_id]`. Zero locks. ~5 instructions.

**Allocate (lock-free):** Pop from per-table Treiber free stack (same pattern as
ObjectTable). CAS the slot from `EMPTY` to the new packed value.

**Close (lock-free):** CAS the slot from current value to `EMPTY`. Push index
onto free stack. If CAS fails (concurrent close or dup), retry or return error.

**Duplicate (lock-free):** Read source slot (atomic load). Allocate new slot
(free stack pop). CAS new slot from `EMPTY` to new packed value with reduced
rights.

**Files:** `handle.rs`, `config.rs`

### 3.3 — Lock-free event signal and wait

**Event bits:** Already a natural fit for lock-free. `signal` is
`fetch_or(bits)` — a single `LDSET` instruction on LSE (~4 cycles). `clear` is
`fetch_and(!bits)` — `LDCLR`. `check` is a plain load.

Change `Event.bits` from `u64` to `AtomicU64`. The signal/check/clear operations
become single atomic instructions with no locking.

**Waiter list:** The waiter array is `[Option<Waiter>; MAX_WAITERS_PER_EVENT]`
(16 slots). Replace with `[AtomicU64; MAX_WAITERS_PER_EVENT]` where each slot
encodes `(thread_id:32, mask:32)` or 0 for empty.

**add_waiter (lock-free):** Scan for an empty slot. CAS from 0 to
`(thread_id << 32 | mask)`. If CAS fails (another core claimed this slot), try
next slot. O(16) worst case scan, but with 16 slots and typically 1-2 waiters,
this is O(1) in practice.

**signal + wake (lock-free):** After `fetch_or` on bits, scan waiter slots. For
each slot where `bits & mask != 0`, CAS from current value to 0 (claim the
waiter). If CAS succeeds, we own this waiter and must wake the thread. If CAS
fails, another signal got it — skip. This is correct under concurrent signals:
each waiter is woken exactly once.

```rust
pub fn signal(&self, bits: u64) -> WakeList {
    self.bits.fetch_or(bits, Release);
    let current_bits = self.bits.load(Acquire);
    let mut woken = WakeList::new();
    for slot in &self.waiters {
        let val = slot.load(Acquire);
        if val == 0 { continue; }
        let mask = val & 0xFFFF_FFFF;
        if current_bits & mask != 0 {
            if slot.compare_exchange(val, 0, AcqRel, Relaxed).is_ok() {
                let tid = (val >> 32) as u32;
                woken.push(WakeInfo { thread_id: ThreadId(tid), fired_bits: current_bits & mask });
            }
        }
    }
    woken
}
```

**remove_waiter (lock-free):** Scan for the slot matching the thread ID, CAS
to 0. Used by multi-wait cleanup.

**Files:** `event.rs`

### 3.4 — Lock-free endpoint send queue (MPSC)

The endpoint send queue is the IPC hot path. Multiple callers enqueue
concurrently; one server dequeues. This is textbook MPSC.

**Design:** Per-priority-level bounded MPSC ring buffers. 4 priority levels,
each a lock-free bounded MPSC queue. This replaces both the `Vec<PendingCall>`
and the linear priority scan in one step.

```rust
struct MpscRing<T, const N: usize> {
    slots: [UnsafeCell<MaybeUninit<T>>; N],
    state: [AtomicU8; N],   // 0=empty, 1=writing, 2=ready, 3=reading
    write_idx: AtomicU32,   // monotonically increasing, mod N
    read_idx: AtomicU32,    // monotonically increasing, mod N
}
```

**Enqueue (lock-free, multiple producers):**

```rust
loop {
    let idx = write_idx.fetch_add(1, AcqRel) % N;
    // CAS state[idx] from EMPTY to WRITING
    if state[idx].compare_exchange(EMPTY, WRITING, AcqRel, Relaxed).is_ok() {
        slots[idx].get().write(MaybeUninit::new(item));
        state[idx].store(READY, Release);
        return Ok(());
    }
    // Slot not empty — queue full at this priority. Back off.
    write_idx.fetch_sub(1, Relaxed);
    return Err(BufferFull);
}
```

**Dequeue (lock-free, single consumer — the server):**

```rust
fn dequeue_highest(&mut self) -> Option<(PendingCall, ReplyCapId)> {
    // Check from highest priority to lowest
    for pri in (0..NUM_PRIORITY_LEVELS).rev() {
        let ring = &self.queues[pri];
        let idx = ring.read_idx.load(Acquire) % N;
        if ring.state[idx].compare_exchange(READY, READING, AcqRel, Relaxed).is_ok() {
            let item = unsafe { ring.slots[idx].get().read().assume_init() };
            ring.state[idx].store(EMPTY, Release);
            ring.read_idx.fetch_add(1, Relaxed);
            return Some((item, self.issue_reply_cap()));
        }
    }
    None
}
```

This gives O(4) = O(1) dequeue (check 4 priority levels), O(1) enqueue (one
CAS), zero heap allocation, and no locks. Under contention (multiple callers at
same priority), each CAS retry costs ~6 cycles — no convoy.

**Capacity:** Each priority level gets N slots. With N=4 per level, total = 16
(matching MAX_PENDING_PER_ENDPOINT). This limits each priority tier to 4
concurrent callers. For the target workload (~3 concurrent callers total), this
is ample. If a level fills, BufferFull is returned — the same behavior as the
current Vec-based queue when it hits the limit.

**Active replies:** Remains a small inline array with per-slot CAS (same pattern
as event waiters). `consume_reply` CAS's the matching slot to empty. Max 16
slots, typically 1.

**Recv waiters:** Same CAS-slot pattern as event waiters. Max 4 slots.

**Files:** `endpoint.rs`, `config.rs`

### 3.5 — Lock-free scheduler (per-core MPSC)

The scheduler's per-core RunQueue is accessed by:

- **Local core:** `pick_next` (dequeue), `rotate_current` — single consumer
- **Any core:** `wake` → `enqueue` onto a target core — multiple producers

This is MPSC per core. The same bounded lock-free MPSC ring from 3.4 applies,
one per priority level per core.

```rust
pub struct RunQueue {
    queues: [MpscRing<ThreadId, 16>; NUM_PRIORITY_LEVELS],
    current: AtomicU32,  // current thread ID, or IDLE
}
```

**pick_next (single consumer, local core):** Check priority levels high to low,
dequeue first ready. O(4).

**enqueue (any core, lock-free):** Push thread ID into the target core's
priority ring. Single CAS.

**Block current (local core):** Set `current` to IDLE via atomic store. No lock
needed — only the local core modifies `current`.

Cross-core wake is the only contention point: two cores simultaneously waking
threads assigned to the same target core. The MPSC ring handles this with
independent CAS's — no convoy, no spinning.

**Files:** `sched.rs`, `thread.rs`

### 3.6 — Lock-free handle table mutation (concurrent threads in same space)

Handle lookup is lock-free (3.2). But handle install (during IPC receive) and
handle close mutate the free list, which requires coordination if two threads in
the same space make concurrent syscalls.

In practice, threads within the same address space share a handle table, and
concurrent handle mutation is possible (thread A receives handles via IPC while
thread B closes handles). The CAS-based free stack from 3.2 handles this: both
operations are lock-free CAS on the free list head.

However, `allocate_at` (used by `thread_create_in` for initial handles at
well-known indices) needs to CAS a specific slot, which may conflict with a
concurrent `allocate` that happens to pick the same free slot. The Treiber stack
pop ensures this can't happen — `allocate` pops from the stack (claiming a slot
no one else can), while `allocate_at` CAS's a specific slot from EMPTY. If
`allocate_at` finds the slot non-empty, it fails. No race.

**Files:** `handle.rs`

### 3.7 — Dispatch signature change and Kernel struct

**The change:** `Kernel::dispatch(&mut self, ...)` →
`Kernel::dispatch(&self, ...)`

The Kernel struct no longer needs SpinLock wrappers around each table, because
the tables themselves are lock-free:

```rust
pub struct Kernel {
    pub vmos: ObjectTable<Vmo, MAX_VMOS>,
    pub events: ObjectTable<Event, MAX_EVENTS>,
    pub endpoints: ObjectTable<Endpoint, MAX_ENDPOINTS>,
    pub threads: ObjectTable<Thread, MAX_THREADS>,
    pub spaces: ObjectTable<AddressSpace, MAX_ADDRESS_SPACES>,
    pub irqs: IrqTable,
    pub scheduler: Scheduler,
}
```

Each ObjectTable uses internal atomics. Each Event uses atomic bits and CAS
waiter slots. Each Endpoint uses MPSC rings. The Scheduler uses per-core MPSC
rings. No table-level locks exist.

Every syscall handler changes from `fn sys_foo(&mut self, ...)` to
`fn sys_foo(&self, ...)`. The compiler enforces that no handler takes `&mut` on
the Kernel — all mutation goes through atomic operations on the individual
objects.

**VMO page slots: AtomicUsize, not SpinLock.** VMO page slots are `AtomicUsize`
(physical address or 0 for unallocated). `alloc_page_at` uses CAS from 0 to
new_addr — two concurrent faults on the same page race to allocate; one wins,
the loser's page is freed. `page_at` is a plain atomic load. `replace_page`
(COW) is a CAS from old_addr to new_addr. All hot-path VMO operations (fault
resolution, page read) are lock-free.

**Resize** is the only VMO operation that changes the page array's length. It is
not on any hot path (resize is an explicit syscall, not a fault handler
operation). Resize takes a per-VMO SpinLock that is never touched by the fault
path. This is a SpinLock that protects a single rare operation on a single
object — not a shared data structure.

**Files:** Every method on `Kernel` in `syscall.rs`, `sched.rs`, `fault.rs`

### 3.8 — Handling composite operations

Two operations require multi-step atomicity that doesn't decompose into
independent CAS's:

**Handle transfer (IPC call path):** Remove handles from sender's table, stage
in PendingCall, install in receiver's table. The staging area (PendingCall
inside the MPSC ring slot) provides natural isolation: once the sender CAS's
handles out of its table, they live in the ring slot. When the receiver
dequeues, it owns the PendingCall exclusively (MPSC single-consumer guarantee).
Installation into the receiver's table uses CAS on the free list. Rollback if
installation fails: the handles are returned to the sender's table via CAS. Each
step is an independent CAS; composite atomicity is provided by the PendingCall
acting as an escrow. This IS lock-free.

**Address space destruction (1.2):** CAS the space's ObjectTable slot to a
DESTROYING sentinel (a distinguished pointer value). All concurrent operations
on that space immediately fail with InvalidHandle — they load the slot, see
DESTROYING, and return error. The destroying core then proceeds with teardown
(kill threads, close handles, unmap VMOs, free page tables) without any lock,
because no other core can access the space. After teardown, CAS the slot from
DESTROYING to NULL (free). Lock-free: one CAS to linearize the destruction, one
CAS to free the slot.

**Files:** `syscall.rs`, `address_space.rs`

---

## Phase 4: Eliminate Heap Allocation from Kernel Objects

The Endpoint `send_queue` and `active_replies` use `Vec`, which means the heap
allocator is on the IPC critical path. The config already defines
`MAX_PENDING_PER_ENDPOINT = 16` — the bound exists; it just isn't enforced
structurally.

### 4.1 — Per-priority MPSC rings for send_queue

**Supersedes the original flat InlineRing design.** Phase 3.4 specifies
lock-free per-priority MPSC rings as the send queue implementation. This step is
the non-concurrent precursor: implement the ring buffer data structure and
priority-ordered dequeue before adding the atomic operations in Phase 3.

**Replace** `send_queue: Vec<PendingCall>` with:

```rust
struct PrioritySendQueue {
    queues: [InlineRing<PendingCall, 4>; NUM_PRIORITY_LEVELS],
    total: u16,
}
```

4 priority levels × 4 slots = 16 total. Enqueue: push to caller's priority ring.
O(1). Dequeue: check from highest priority, pop first non-empty. O(4) = O(1). No
linear priority scan.

Phase 3.4 later converts `InlineRing` to `MpscRing` (atomic state per slot). The
data structure shape is identical; only the synchronization changes.

**Memory cost:** `PendingCall` is ~310 bytes. 4 slots × 4 levels = 16 slots = ~5
KiB per endpoint. With MAX_ENDPOINTS = 256, total = ~1.3 MiB.

### 4.2 — Inline array for active_replies

**Replace** `active_replies: Vec<ActiveReply>` with:

```rust
active_replies: [Option<ActiveReply>; MAX_PENDING_PER_ENDPOINT],
active_reply_count: u8,
```

`ActiveReply` is ~24 bytes. 16 slots = 384 bytes. Negligible.

`consume_reply` scans for matching cap_id. With ≤16 entries, this is a
cache-line-local scan.

### 4.3 — Eliminate Vec from Endpoint::close_peer

`close_peer` currently returns `Vec<ThreadId>`. Replace with an inline return
type (same pattern as WakeList/DrainList):

```rust
pub struct CloseList {
    items: [ThreadId; MAX_PENDING_PER_ENDPOINT + MAX_PENDING_PER_ENDPOINT + MAX_RECV_WAITERS],
    len: usize,
}
```

The maximum blocked threads on an endpoint is: send_queue callers + active_reply
callers + recv waiters. With current limits: 16 + 16 + 4 = 36 entries × 4 bytes
= 144 bytes. Stack-friendly.

### 4.4 — Eliminate Vec from IrqTable

`IrqTable::bindings: Vec<Option<IrqBinding>>` — replace with a fixed array:

```rust
bindings: [Option<IrqBinding>; MAX_IRQS],
```

`MAX_IRQS = 1024`, `IrqBinding` is ~17 bytes. Total: ~17 KiB. This is a one-time
allocation at kernel init. Using a fixed array means the IrqTable can be a
static, avoiding heap allocation entirely during init.

---

## Phase 5: Data Structure Performance

### 5.1 — Free-list allocator for HandleTable and ObjectTable

**Problem:** Handle allocation scans 512 slots linearly. ObjectTable allocation
scans from a hint. On the IPC path (handle transfer installs handles in the
receiver's table), this is O(n) worst case per handle.

**Fix:** Treiber stack (lock-free linked free list) through empty slots. This
step implements the data structure; Phase 3 converts it to use atomic operations
for concurrent access.

**HandleTable:**

```rust
pub struct HandleTable {
    slots: [Slot; MAX_HANDLES],
    free_head: u32,          // Treiber stack head
    free_next: [u32; MAX_HANDLES],  // per-slot next-free pointer
    count: usize,
}

enum Slot {
    Occupied(Handle),
    Empty,
}
```

On `allocate`: pop from `free_head`, set `free_head = free_next[old_head]`.
O(1). On `close`: push to `free_head`:
`free_next[idx] = free_head; free_head = idx`. O(1). `lookup`: direct index.
O(1).

Phase 3.2 converts `free_head` to `AtomicU32` and push/pop to CAS loops. The
structural change happens here; the atomicity upgrade happens there.

**ObjectTable:** Same treatment. Replace the linear scan + `free_hint` with a
Treiber stack. Add per-slot `generations: [u64; MAX]` (from 1.1).

```rust
pub struct ObjectTable<T, const MAX: usize> {
    entries: Vec<Option<T>>,
    free_head: u32,
    free_next: Vec<u32>,
    generations: Vec<u64>,
    count: usize,
}
```

O(1) alloc, O(1) dealloc, O(1) lookup. Phase 3.1 converts to atomics.

**Files:** `handle.rs`, `table.rs`

### 5.2 — Sorted mappings for find_mapping

**Problem:** `AddressSpace::find_mapping` does a linear scan of all mappings. On
the fault path (every page fault), this is O(n) where n is the number of
mappings in the space.

**Fix:** Keep mappings sorted by `va_start`. Use binary search for lookup.

```rust
pub fn find_mapping(&self, addr: usize) -> Option<&MappingRecord> {
    let idx = self.mappings
        .partition_point(|m| m.va_start + m.size <= addr);
    self.mappings.get(idx)
        .filter(|m| addr >= m.va_start && addr < m.va_start + m.size)
}
```

`map_vmo`: insert at sorted position (use `partition_point` + `insert`).
`unmap`: binary search + `remove`.

O(log n) lookup instead of O(n). For the typical case (10-20 mappings per
space), this is 4-5 comparisons instead of 10-20.

**Why not an interval tree?** An augmented BST would give O(log n) for all
operations including insertion. But with MAX_MAPPINGS = 128, a sorted array with
binary search has better cache locality — the array is contiguous in memory (one
or two cache lines for the search), while a tree chases pointers across cache
lines. For n ≤ 128, the cache advantage of the sorted array dominates the
algorithmic advantage of the tree. Insertion is O(n) due to shifting, but
map/unmap are slow-path operations (not on the IPC or fault hot path at steady
state).

**Files:** `address_space.rs`

---

## Phase 6: Missing Functionality

### 6.1 — Priority inheritance through endpoints

**Problem:** Priority boost/release methods exist on Thread but are never
called. A high-priority caller blocked on a low-priority server causes priority
inversion.

**Fix:** Wire priority inheritance into the IPC path.

**On `sys_call`:**

- After enqueuing the call, check if a server is blocked on recv for this
  endpoint.
- If the server's effective priority < caller's priority, boost the server.
- Store the boosted thread ID in the endpoint for later release.

**On `sys_reply`:**

- After completing the reply, release the priority boost on the server thread.
- If more callers are queued, re-boost to the new highest caller priority.

**On `sys_recv` (blocking path):**

- When the server blocks on recv and the queue has pending calls, boost the
  server to the highest queued caller's priority.

`Endpoint` already has `highest_caller_priority()`. Thread already has
`boost_priority()` and `release_boost()`. The wiring is:

```rust
// In sys_call, after enqueue:
if let Some(server_tid) = ep.active_server() {
    let caller_pri = caller_thread.effective_priority();
    let server = threads.get_mut(server_tid);
    server.boost_priority(caller_pri);
}

// In sys_reply, after consume_reply:
let server = threads.get_mut(current);
if let Some(next_pri) = ep.highest_caller_priority() {
    server.boost_priority(next_pri);
} else {
    server.release_boost();
}
```

Add `active_server: Option<ThreadId>` to Endpoint. Set when a server calls recv
and dequeues a call. Cleared on reply.

**Files:** `syscall.rs`, `endpoint.rs`, `thread.rs`

**Tests:**

1. High-priority caller boosts low-priority server
2. Reply releases boost
3. Multiple callers: boost tracks highest
4. No boost when server is already higher priority

### 6.2 — User-buffer multi-wait (beyond 3 events)

**Problem:** `sys_event_wait` is hardcoded to 3 events via register encoding.

**Fix:** Dual-path interface — register-fast for ≤3, user-buffer for >3.

**Register path (count ≤ 3):** args[0..5] encode 3 × (handle, mask) pairs. Zero
memory access — all data arrives in registers from the SVC instruction. This is
the performance-critical path: the compositor and OS service event loops wait on
1-3 events at 120fps. Eliminating a user-memory round-trip on this path saves
one L1 miss (~4ns) per frame minimum, and avoids LDTR overhead and fault-handler
risk entirely.

**Buffer path (count > 3):** args[0] = user_ptr to array of
`(u32 handle, u64 mask)` pairs, args[1] = count, args[2..5] = 0. Detected by:
args[1] > 0 && args[2] == 0 && args[3] == 0. User pointer validated via LDTR
from Phase 2. Add `MAX_MULTI_WAIT: usize = 32` — 32 events × 12 bytes = 384
bytes on the kernel stack.

**Why two paths instead of always user-buffer:** The register path has strictly
fewer instructions, zero memory traffic, and zero fault-handler risk. For the
dominant case (≤3 events), it is measurably faster. The buffer path exists for
extensibility, not as the primary interface. This is the same split seL4 uses: a
register-encoded fast path for the common IPC case, and a slow path for
overflow.

The return value encoding doesn't change: `(handle_id << 32) | fired_bits`.

**Files:** `syscall.rs`, `config.rs`

---

## Dependency Graph

```text
Phase 1 (correctness bugs) — no dependencies, do first:
  1.1 generation counter — foundational for 5.1
  1.2 space_destroy — needs 5.1 (free-list tables), 4.x (inline storage)
  1.3 ASID dedup
  1.4 clock_read

Phase 2 (security) — independent of Phase 1:
  2.1 LDTR/STTR user_mem — needs frame/ only
  2.2 bound-check — trivial, do before 2.1

Phase 3 (SMP — lock-free) — depends on 4.x and 5.x (structures must be
  final before adding atomics):
  3.1 lock-free ObjectTable (atomics on 5.1's free list)
  3.2 lock-free HandleTable (atomics on 5.1's handle slots)
  3.3 lock-free Event (atomic bits + CAS waiter slots)
  3.4 lock-free Endpoint MPSC (atomics on 4.1's priority rings)
  3.5 lock-free Scheduler (per-core MPSC)
  3.6 lock-free handle mutation (CAS on free stack)
  3.7 dispatch &self refactor (all syscall handlers)
  3.8 composite operation protocols (transfer escrow, destroy sentinel)

Phase 4 (eliminate heap) — do before Phase 3:
  4.1 per-priority inline rings for send_queue
  4.2 inline active_replies
  4.3 inline close_peer return
  4.4 fixed IrqTable

Phase 5 (data structures) — do before Phase 3:
  5.1 free-list HandleTable + ObjectTable
  5.2 sorted mappings

Phase 6 (functionality) — after Phase 3:
  6.1 priority inheritance (needs lock-free endpoint model)
  6.2 extended multi-wait (needs 2.1 for secure user copy)
```

## Implementation Order

```text
── Data structures first (safe Rust, testable) ──────────────────
 1. 1.1  generation counter in ObjectTable  (foundational)
 2. 5.1  free-list HandleTable + ObjectTable (O(n)→O(1), prepares for 3.x)
 3. 4.4  fixed IrqTable array               (trivial Vec removal)
 4. 4.1  per-priority InlineRing send_queue  (prepares for 3.4 MPSC)
 5. 4.2  inline active_replies              (same pattern)
 6. 4.3  inline close_peer return           (same pattern)
 7. 5.2  sorted mappings + binary search    (self-contained)
 8. 1.3  ASID dedup                         (small)
 9. 1.4  clock_read                         (trivial)

── Correctness fix needing above structures ─────────────────────
10. 1.2  space_destroy full teardown + per-space thread list

── Security boundary (frame/ unsafe, isolated) ──────────────────
11. 2.2  bound-check user buffers           (immediate security win)
12. 2.1  LDTR/STTR 64-bit user_mem          (recovery table + asm)

── Lock-free SMP (convert structures to atomics) ────────────────
13. 3.1  lock-free ObjectTable              (AtomicPtr slots, CAS free stack)
14. 3.2  lock-free HandleTable              (AtomicU64 packed slots, CAS free stack)
15. 3.3  lock-free Event                    (AtomicU64 bits, CAS waiter slots)
16. 3.4  lock-free Endpoint MPSC            (atomic state per ring slot)
17. 3.5  lock-free Scheduler                (per-core MPSC rings)
18. 3.7  dispatch(&self) refactor           (large mechanical change)
19. 3.8  composite operation protocols      (transfer escrow, destroy sentinel)

── Functionality (on the lock-free foundation) ──────────────────
20. 6.1  priority inheritance               (wired through lock-free endpoints)
21. 6.2  extended multi-wait                (uses secure user copy from 2.1)
```

Steps 1-10 are safe-Rust-only changes verified by the existing test suite. Steps
11-12 add unsafe code in `frame/` only. Steps 13-19 convert data structures to
use atomics — each step is independently testable because single-threaded CAS
degenerates to unconditional success. Steps 20-21 build new functionality on the
lock-free foundation.

## Verification

After all phases:

- All existing tests pass (single-threaded CAS = no behavior change)
- New tests for each fix (specified per section)
- Concurrent stress tests: N threads issuing syscalls simultaneously on a
  multi-core host, verifying no data races (Miri + ThreadSanitizer)
- Fuzz target: concurrent syscall sequences (new target using threads)
- No `Vec` reachable from sys_call/sys_recv/sys_reply/sys_event_wait
- No lock acquisition on the IPC hot path (sys_call/sys_recv/sys_reply)
- `dispatch` takes `&self`, not `&mut self`
- Every user pointer goes through LDTR/STTR on bare metal
- ObjectTable tracks generation per-slot via atomics
- space_destroy performs full teardown via DESTROYING sentinel
- IPC round-trip benchmark on bare metal: measure lock-free vs. baseline
