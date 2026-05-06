# SMP Per-Object Locking: Implementation Plan

Concrete plan to convert the kernel from implicit BKL (`&mut Kernel`) to
per-object locking. Six layers, each independently testable. All existing 704
tests must pass after each layer.

## Architecture Overview

```text
Before:
  svc_fast_handler → &mut *(kernel_ptr) → Kernel::dispatch(&mut self)
                     ↑ unsound: no lock

After:
  svc_fast_handler → dispatch(current, core_id, num, args)
                     ↓ free function
                     → acquire only the locks this syscall needs
                     → each ObjectTable is a global static
                     → scheduler is per-CPU, lock-free for local ops
                     → IPI for cross-core wake
```

### Key Simplification: No Priority Inheritance Needed

All kernel locks are spinlocks with IRQs disabled. A thread holding a spinlock
cannot be preempted — it runs to completion. Therefore priority inversion cannot
occur, and priority inheritance on the lock itself is unnecessary.

PI remains in the IPC layer (endpoint call/reply boosting), which is already
implemented. The locks themselves are simple spinlocks and RW spinlocks.

---

## Layer 0: RwSpinLock Primitive

**File:** `kernel/src/frame/arch/aarch64/sync.rs`

Add `RwSpinLock` alongside the existing `TicketLock` and `SpinLock<T>`.

### Design

```text
State: AtomicU32
  bit 31     = WRITER flag
  bits 0–30  = reader count (max 2^31 - 1)

Reader acquire:
  1. Save + disable IRQs (DAIF)
  2. Loop:
     old = state.load(Acquire)
     if old & WRITER_BIT { wfe(); continue }
     if state.compare_exchange_weak(old, old + 1, AcqRel, Relaxed).is_ok() { break }
  3. Return saved DAIF

Reader release:
  1. state.fetch_sub(1, Release)
  2. If new state == WRITER_BIT → writer is waiting, SEV to wake it
  3. Restore DAIF

Writer acquire:
  1. Save + disable IRQs (DAIF)
  2. Set WRITER_BIT: loop { state.fetch_or(WRITER_BIT, AcqRel) }
     (This blocks new readers from entering)
  3. Spin until reader count == 0: while state.load(Acquire) != WRITER_BIT { wfe() }
  4. Return saved DAIF

Writer release:
  1. state.store(0, Release) — clears WRITER_BIT and reader count
  2. SEV to wake waiting readers/writers
  3. Restore DAIF
```

Cache-line aligned (128 bytes) like TicketLock. IRQs disabled while held.

### Safe wrapper: `RwSpinLock<T>`

```rust
pub struct RwSpinLock<T> {
    lock: RawRwSpinLock,
    data: UnsafeCell<T>,
}

impl<T> RwSpinLock<T> {
    pub fn read(&self) -> RwReadGuard<'_, T>;   // &T access
    pub fn write(&self) -> RwWriteGuard<'_, T>; // &mut T access
}
```

### Tests

- Basic read/write exclusion (single-threaded, verify state transitions)
- Multiple concurrent readers (verify reader count increments/decrements)
- Writer blocks new readers (verify WRITER_BIT blocks reader acquire)
- Alignment and size assertions

### Host-target compatibility

Like TicketLock: bare-metal uses WFE/SEV, host tests use `spin_loop()` hint.

---

## Layer 1: IPI Infrastructure

**Files:** `kernel/src/frame/arch/aarch64/gic.rs`, `sysreg.rs`, `exception.rs`,
`cpu.rs`

### 1a. Sysreg wrapper

Add to `sysreg.rs`:

```rust
pub fn set_icc_sgi1r_el1(val: u64);  // Write ICC_SGI1R_EL1
```

ICC_SGI1R_EL1 encoding (GICv3 with affinity routing):

- bits [55:48] = Aff3
- bits [39:32] = Aff2
- bits [23:16] = Aff1
- bits [15:0] = target list (bitmask of cores within cluster)
- bits [27:24] = INTID (SGI number, 0–15)
- bit [40] = IRM (0 = target list, 1 = all except self)

### 1b. Enable SGIs per redistributor

In `gic.rs:init_redistributor()`, change:

```rust
// Before: only enables virtual timer PPI
mmio::write32(redist_base + GICR_ISENABLER0, 1 << INTID_VTIMER);

// After: enable SGIs 0–15 and virtual timer
mmio::write32(redist_base + GICR_ISENABLER0, 0xFFFF | (1 << INTID_VTIMER));
```

### 1c. SGI send function

Add to `gic.rs`:

```rust
/// Send SGI to a specific core. `sgi_id` must be 0–15.
pub fn send_sgi(target_core: u32, sgi_id: u32);
```

Implementation: compute affinity from core_id (requires MPIDR table built during
boot from DTB or direct MPIDR reads), write ICC_SGI1R_EL1. Follow with ISB to
ensure the system register write takes effect.

Reserve SGI IDs:

- SGI 0 = RESCHEDULE (cross-core wake)
- SGI 1–15 = reserved for future use

### 1d. SGI handler

In `exception.rs:irq_handler()`, add dispatch for INTIDs 0–15:

```rust
match intid {
    0 => {
        // Reschedule SGI: set flag, actual reschedule happens at
        // exception return (check in eret path or after IRQ handler)
        percpu_mut().reschedule_pending = 1;
    }
    INTID_VTIMER => super::timer::handle_deadline(core),
    32.. => { /* existing SPI dispatch */ }
    _ => {}
}
```

### 1e. Reschedule check at exception return

After any IRQ handler returns, before ERET: if `reschedule_pending` is set,
clear it and invoke the scheduler's context-switch path. This is where the
per-CPU scheduler (Layer 2) integrates with the IPI mechanism.

### Tests

- SGI send/receive requires bare-metal (QEMU with 2+ cores)
- Unit test: verify GICR_ISENABLER0 bit pattern includes SGIs
- Integration test: boot 2 cores, send SGI from core 0 → core 1, verify delivery
  via a shared atomic counter

---

## Layer 2: Per-CPU Scheduler Extraction

**Files:** `kernel/src/thread.rs` (Scheduler, RunQueue), `kernel/src/sched.rs`
(new)

### Goal

The scheduler becomes CPU-local. No lock needed for the local-CPU fast path
(pick_next, block_current, rotate). Cross-core wake acquires the remote core's
lock and sends an IPI.

### 2a. Split Scheduler into PerCpuScheduler

Current structure:

```rust
pub struct Scheduler {
    cores: Vec<RunQueue>,           // One per core
    links: Vec<SchedLink>,          // Global, one per thread
}
```

The `links` array is the complication — it's a global array indexed by thread
ID, shared across all cores' RunQueues. Two cores manipulating the links
simultaneously would corrupt the linked lists.

**Solution:** Move SchedLink into the Thread struct. Each Thread owns its own
`next`/`prev` pointers. The RunQueue stores only head/tail/bitmap/current (all
per-CPU, no shared state).

New structure:

```rust
// Per-CPU, accessed only by owning core (no lock) or cross-core wake (lock)
#[repr(C, align(128))]  // Cache-line aligned
pub struct PerCpuScheduler {
    lock: TicketLock,             // For cross-core enqueue
    queue: RunQueue,              // The actual per-priority queues
    // Padding to ensure no false sharing
}

// Thread gains its own scheduling link
pub struct Thread {
    // ... existing fields ...
    sched_next: Option<u32>,      // Next thread in same priority queue
    sched_prev: Option<u32>,      // Prev thread in same priority queue
    assigned_core: u32,           // Which core's RunQueue this thread is in
}
```

**Invariant:** Thread.sched_next/sched_prev are only modified while holding the
PerCpuScheduler lock for Thread.assigned_core. Local-core operations acquire no
lock (the core owns its scheduler and no other core touches it without the
lock). Cross-core operations acquire the target's lock.

### 2b. Cross-core wake

```rust
pub fn wake(thread_id: ThreadId, target_core: u32) {
    if target_core == current_core() {
        // Local wake: no lock needed
        local_scheduler().enqueue(thread_id, priority);
    } else {
        // Cross-core wake: acquire remote scheduler lock, enqueue, IPI
        let remote = &SCHEDULERS[target_core as usize];
        let _guard = remote.lock.lock();
        remote.queue.enqueue(thread_id, priority);
        drop(_guard);
        gic::send_sgi(target_core, SGI_RESCHEDULE);
    }
}
```

### 2c. SchedLink migration

Move the `links: Vec<SchedLink>` from Scheduler into the Thread objects. Every
scheduler operation that currently indexes `self.links[tid]` instead accesses
the Thread's sched_next/sched_prev fields. This requires access to the Thread
while holding the scheduler — which means the Thread table must be accessible
without the BKL.

This creates a dependency on Layer 3 (concurrent ObjectTable for threads).
**Resolution:** Implement Layer 2 and Layer 3 together for the Thread table
specifically. The Thread ObjectTable becomes concurrent first, before the other
tables.

### Tests

- All existing scheduler tests adapted to new API
- Cross-core enqueue test (acquire remote lock, verify queue state)
- IPI delivery test (bare-metal: wake thread on remote core, verify it runs)

---

## Layer 3: Concurrent ObjectTable

**File:** `kernel/src/table.rs` (modify existing), `kernel/src/frame/slab.rs`

### Goal

ObjectTable supports concurrent access: readers don't block each other; writers
(alloc/dealloc) are exclusive; per-object mutation uses per-slot locks.

### 3a. Split into allocation state and object access

```rust
pub struct ObjectTable<T, const MAX: usize, S: Storage<T>> {
    storage: S,                          // Object storage (unchanged)
    generations: [AtomicU64; MAX],       // Atomic generation counters
    alloc: SpinLock<AllocState>,         // Allocation protected separately
    slot_locks: [TicketLock; MAX],       // Per-slot mutation lock
}

struct AllocState {
    free_head: u32,
    free_next: [u32; MAX],
    count: usize,
}
```

Wait — `[AtomicU64; MAX]` and `[TicketLock; MAX]` with const generics require
the types to implement `Copy` or use `MaybeUninit`. AtomicU64 is not Copy.
TicketLock is not Copy (contains AtomicU32).

**Practical solution:** Use `Vec` as today, but with atomic/lock wrappers:

```rust
pub struct ObjectTable<T, const MAX: usize, S: Storage<T>> {
    storage: S,
    generations: Vec<AtomicU64>,         // One per slot
    alloc: SpinLock<AllocState>,         // Protects free list + count
    slot_locks: Vec<TicketLock>,         // One per slot, 128-byte aligned
}
```

### 3b. Access patterns

**Lookup (read, no alloc lock needed):**

```rust
pub fn get(&self, idx: u32, expected_gen: u64) -> Option<&T> {
    let gen = self.generations[idx].load(Acquire);
    if gen != expected_gen { return None; }
    self.storage.get(idx as usize)
}
```

Generation is atomic — concurrent reads are safe. Storage slots are immutable
between alloc and dealloc (the object itself is protected by the slot lock, but
the storage.get() just returns a pointer to the pre-allocated slot).

**Mutate (per-slot lock):**

```rust
pub fn lock_slot(&self, idx: u32) -> SlotGuard<'_> {
    SlotGuard { lock: &self.slot_locks[idx as usize], daif: self.slot_locks[idx].lock() }
}

pub unsafe fn get_mut_unchecked(&self, idx: u32) -> &mut T {
    // SAFETY: caller holds slot lock
    &mut *self.storage.get_raw_mut(idx as usize)
}
```

The slot lock provides exclusive access to the object at that index. The caller
holds the lock, so `&mut T` is sound even though the table itself is `&self`.

**Allocate (alloc lock):**

```rust
pub fn alloc(&self, value: T) -> Option<(u32, u64)> {
    let mut alloc = self.alloc.lock();
    let head = alloc.free_head;
    if head == EMPTY { return None; }
    let i = head as usize;
    alloc.free_head = alloc.free_next[i];
    alloc.count += 1;
    drop(alloc);  // Release alloc lock before touching storage

    self.storage.place(i, value);  // Write to pre-allocated MaybeUninit slot
    let gen = self.generations[i].load(Acquire);
    Some((head, gen))
}
```

**Deallocate (alloc lock + slot lock):**

```rust
pub fn dealloc(&self, idx: u32) -> bool {
    let _slot = self.lock_slot(idx);  // Ensure no concurrent access
    // Verify occupied, drop value
    if !self.storage.remove(idx as usize) { return false; }
    self.generations[idx].fetch_add(1, Release);  // Bump generation

    let mut alloc = self.alloc.lock();
    alloc.free_next[idx as usize] = alloc.free_head;
    alloc.free_head = idx;
    alloc.count -= 1;
    true
}
```

### 3c. Storage trait changes

The `Storage<T>` trait needs `get_raw_mut` for use under the slot lock:

```rust
pub trait Storage<T> {
    // ... existing methods ...

    /// Raw mutable access — caller must ensure exclusive access (e.g., slot lock).
    unsafe fn get_raw_mut(&self, idx: usize) -> *mut T;
}
```

For InlineSlab: returns pointer into the MaybeUninit slot. For BoxStorage:
returns pointer into the Box.

The existing `get_mut(&mut self, ...)` remains for single-threaded contexts
(tests, boot). The new `get_raw_mut(&self, ...)` is for concurrent access under
the slot lock.

### 3d. HandleTable RwSpinLock

HandleTable is already per-AddressSpace. Wrap it in RwSpinLock:

```rust
// Inside AddressSpace:
handles: RwSpinLock<HandleTable>,

// Lookup path (reader):
let guard = space.handles.read();
let handle = guard.lookup(handle_id)?;
// guard dropped here — HandleTable unlocked
// Now acquire the target object's slot lock

// Mutate path (writer):
let mut guard = space.handles.write();
guard.allocate(handle)?;
```

### 3e. Refcount atomicity

Object refcounts (currently `refcount: usize` on Vmo, Endpoint, Event) become
`AtomicUsize`. Decrement returns the new value. If zero, the caller deallocates
via the ObjectTable's alloc lock.

```rust
pub refcount: AtomicUsize,

// On handle close:
let prev = obj.refcount.fetch_sub(1, Release);
if prev == 1 {
    // Last reference — deallocate
    fence(Acquire);  // Synchronize with all previous decrements
    TABLE.dealloc(idx);
}
```

### Tests

- All existing ObjectTable tests pass (single-threaded API preserved)
- New concurrent tests: parallel alloc/dealloc from multiple threads
- Generation atomicity: concurrent lookup during dealloc returns None
- Slot lock exclusion: two threads locking same slot serializes correctly

---

## Layer 4: Global State + Dispatch Rewrite

**Files:** `kernel/src/syscall.rs` (major rewrite), `kernel/src/state.rs` (new)

### 4a. Global statics

New file `kernel/src/state.rs`:

```rust
use core::sync::atomic::AtomicU32;

use crate::{
    address_space::AddressSpace,
    config,
    endpoint::Endpoint,
    event::Event,
    frame::slab::InlineSlab,
    irq::IrqTable,
    table::ObjectTable,
    thread::Thread,
};

// Each table is initialized during kernel_main, before any thread runs.
// After init, these are immutable references to the tables (the tables
// themselves are internally synchronized).

static mut VMOS: Option<ObjectTable<Vmo, {config::MAX_VMOS}, InlineSlab<Vmo>>> = None;
static mut EVENTS: Option<ObjectTable<Event, ...>> = None;
// ... etc for all tables ...

static ALIVE_THREADS: AtomicU32 = AtomicU32::new(0);

// Per-CPU schedulers (array, indexed by core_id)
static mut SCHEDULERS: Option<[PerCpuScheduler; config::MAX_CORES]> = None;

pub fn init(num_cores: usize) {
    unsafe {
        VMOS = Some(ObjectTable::new());
        EVENTS = Some(ObjectTable::new());
        // ...
        SCHEDULERS = Some(array::from_fn(|_| PerCpuScheduler::new()));
    }
}

// Safe accessors (panic if called before init)
pub fn vmos() -> &'static ObjectTable<...> {
    unsafe { VMOS.as_ref().unwrap_unchecked() }
}
pub fn events() -> &'static ObjectTable<...> { ... }
pub fn endpoints() -> &'static ObjectTable<...> { ... }
pub fn threads() -> &'static ObjectTable<...> { ... }
pub fn spaces() -> &'static ObjectTable<...> { ... }
pub fn scheduler(core: usize) -> &'static PerCpuScheduler { ... }
```

The `Option<T>` + `unwrap_unchecked` pattern is safe because init() is called
once during single-threaded boot, before any concurrent access. After init, the
references are `&'static` and never change.

### 4b. Dispatch becomes a free function

```rust
// In syscall.rs — no longer a method on Kernel

pub fn dispatch(
    current: ThreadId,
    core_id: usize,
    syscall_num: u64,
    args: &[u64; 6],
) -> (u64, u64) {
    let result = match syscall_num {
        num::VMO_CREATE => sys_vmo_create(current, args),
        num::CALL => sys_call(current, core_id, args),
        // ... all 30 syscalls ...
        _ => Err(SyscallError::InvalidArgument),
    };

    match result {
        Ok(value) => (0, value),
        Err(e) => (e as u64, 0),
    }
}
```

### 4c. svc_fast_handler update

```rust
extern "C" fn svc_fast_handler(
    a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64,
    syscall_num: u64,
) -> (u64, u64) {
    let args = [a0, a1, a2, a3, a4, a5];
    let pc = unsafe { super::cpu::percpu_mut() };
    pc.mark_syscall_entry();

    let current = ThreadId(pc.current_thread);
    let core_id = pc.core_id as usize;
    let result = crate::syscall::dispatch(current, core_id, syscall_num, &args);

    unsafe { super::cpu::percpu_mut() }.clear_syscall_entry();
    result
}
```

No lock acquisition at entry. Each handler acquires what it needs.

### 4d. Syscall handler rewrite (by access pattern)

Group handlers by lock pattern to minimize per-handler design work:

**Group A — Read-only (no mutation):** `HANDLE_INFO`, `CLOCK_READ`,
`SYSTEM_INFO`

- Acquire HandleTable reader (for HANDLE_INFO), read object, return.
- CLOCK_READ and SYSTEM_INFO touch no kernel state.

**Group B — Object creation (alloc + handle install):** `VMO_CREATE`,
`EVENT_CREATE`, `ENDPOINT_CREATE`, `SPACE_CREATE`, `THREAD_CREATE`

- Acquire ObjectTable alloc lock → allocate slot
- Acquire HandleTable writer → install handle
- Lock ordering: ObjectTable.alloc < HandleTable.write (Table alloc always
  first)

**Group C — Object mutation (handle lookup + slot lock):** `VMO_SEAL`,
`VMO_RESIZE`, `VMO_SET_PAGER`, `EVENT_SIGNAL`, `EVENT_CLEAR`, `EVENT_BIND_IRQ`,
`ENDPOINT_BIND_EVENT`, `THREAD_SET_PRIORITY`, `THREAD_SET_AFFINITY`

- Acquire HandleTable reader → lookup handle
- Acquire object slot lock → mutate
- Release both

**Group D — IPC (multi-object):** `CALL`, `RECV`, `REPLY`

- Most complex. Detailed below.

**Group E — Memory operations:** `VMO_MAP`, `VMO_MAP_INTO`, `VMO_UNMAP`,
`VMO_SNAPSHOT`

- Touch VMO + AddressSpace(s). Lock ordering defined.

**Group F — Handle lifecycle:** `HANDLE_DUP`, `HANDLE_CLOSE`

- DUP: HandleTable writer + refcount increment (atomic)
- CLOSE: HandleTable writer + refcount decrement (atomic) + conditional dealloc

**Group G — Destruction (complex):** `SPACE_DESTROY`, `THREAD_EXIT`

- Cascading cleanup. Multiple tables. Acquire in defined order.

### 4e. IPC handlers in detail

**sys_call (client calls server):**

```text
1. caller_space.handles.read()       → lookup endpoint handle
2. endpoints.lock_slot(ep_id)        → access endpoint
3. IF server waiting in recv_waiters:
   a. threads.lock_slot(server_tid)  → wake server, boost priority
   b. server_space.handles.write()   → install handles (if transferring)
   c. caller_space.handles.write()   → remove handles (if transferring)
   d. scheduler.enqueue(server)      → per-CPU, no lock or remote lock+IPI
   e. scheduler.block(caller)        → per-CPU, no lock
4. ELSE (no server waiting):
   a. endpoint.send_queue.enqueue(caller, message)
   b. scheduler.block(caller)
   c. IF endpoint.bound_event → events.lock_slot(event_id) → signal
```

Lock ordering: HandleTable(R) < Endpoint < Thread < HandleTable(W) < Scheduler

Note HandleTable appears twice: first as reader (lookup), then as writer (handle
transfer). The reader lock is released between the two acquisitions (it's not
nested). The endpoint slot lock is held across the handle transfer to ensure
atomicity.

**sys_recv (server receives call):**

```text
1. server_space.handles.read()       → lookup endpoint handle
2. endpoints.lock_slot(ep_id)        → access endpoint
3. IF pending call in send_queue:
   a. threads.lock_slot(caller_tid)  → access caller's staged message
   b. server_space.handles.write()   → install handles + reply cap
   c. endpoint.install_reply_cap()   → link reply cap to caller
4. ELSE (no pending call):
   a. endpoint.recv_waiters.push(server)
   b. scheduler.block(server)
```

**sys_reply (server replies to client):**

```text
1. server_space.handles.read()       → lookup reply cap handle
2. server_space.handles.write()      → consume reply cap (close handle)
3. threads.lock_slot(caller_tid)     → write reply, wake caller
4. IF handle transfer:
   a. server_space.handles.write()   → remove handles
   b. caller_space.handles.write()   → install handles
5. scheduler.wake(caller)            → per-CPU or remote+IPI
6. threads.lock_slot(server_tid)     → release priority boost
```

### Tests

- All 704 tests must pass with the new dispatch
- Benchmarks adapted to call `dispatch()` free function instead of
  `kern.dispatch(&mut self, ...)`
- Compare benchmark results against `bench_baselines.toml`

---

## Layer 5: Lock Ordering Validation (Debug Mode)

**File:** `kernel/src/lockdep.rs` (new)

In debug builds, track lock acquisitions per-core and verify ordering:

```rust
#[cfg(debug_assertions)]
thread_local! {
    static HELD_LOCKS: RefCell<Vec<LockId>> = RefCell::new(Vec::new());
}

pub fn assert_ordering(new_lock: LockId) {
    // Verify new_lock > all currently held locks
    // Panic with diagnostic if violated
}
```

On bare metal (no thread_local), use per-CPU storage in PerCpu struct:

```rust
pub struct PerCpu {
    // ... existing fields ...
    #[cfg(debug_assertions)]
    held_locks: [LockId; 8],  // Max nesting depth
    held_lock_count: u8,
}
```

Each lock type gets an ordering number matching the global lock ordering from
`smp-concurrency.md`:

```rust
#[repr(u8)]
enum LockClass {
    HandleTableRead = 1,
    EndpointTable = 2,
    Endpoint = 3,
    ThreadTable = 4,
    Thread = 5,
    Scheduler = 6,
    VmoTable = 7,
    Vmo = 8,
    // ...
}
```

### Tests

- Intentional out-of-order lock acquisition panics in debug mode
- All existing tests pass (verify no lock ordering violations in existing code)

---

## Layer 6: Benchmark Comparison

After all layers are complete and all 704 tests pass:

1. Run `bench_baselines.toml` benchmarks on bare metal (single core)
2. Compare against existing baselines
3. Expected regressions per syscall:
   - +1–2 ticks for simple syscalls (handle_info, event_signal) — barrier
     overhead from RwSpinLock reader acquire/release
   - +2–4 ticks for creation syscalls — alloc lock + handle table writer lock
   - +3–6 ticks for IPC — multiple lock acquisitions
4. New benchmarks to add:
   - `ipc_call_reply_2core`: Two cores doing IPC simultaneously to different
     endpoints. Should show near-2× throughput vs single core.
   - `handle_lookup_contended`: N cores looking up handles in the same space
     concurrently. Reader lock should show near-linear scaling.
   - `lock_contention_per_syscall`: Per-lock wait cycle counters (Layer 3
     instrumentation).
5. Update `bench_baselines.toml` with new thresholds
6. Update `STATUS.md`

---

## Completion Status

All structural layers are implemented. Remaining items are deferred pending
multi-core thread execution harness.

| Layer                      | Status   | Notes                                                                                                                                                                                                        |
| -------------------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| 0. RwSpinLock              | Done     | `frame/arch/aarch64/sync.rs`                                                                                                                                                                                 |
| 1. IPI                     | Done     | SGI send/receive, reschedule handler                                                                                                                                                                         |
| 2. Per-CPU Scheduler       | Done     | `Schedulers` struct, per-CPU `SpinLock<PerCoreState>`                                                                                                                                                        |
| 3. ConcurrentTable         | Done     | Per-slot TicketLock + AtomicU64 generations                                                                                                                                                                  |
| 3d. HandleTable RwSpinLock | Deferred | HandleTable is inside AddressSpace slot lock; RwSpinLock requires either extracting HandleTable or adding RW mode to ConcurrentTable slots. Benefits only multi-threaded services in the same address space. |
| 3e. Atomic refcounts       | Done     | `AtomicUsize` on Vmo, Endpoint, Event; `add_ref`/`release_ref` take `&self`                                                                                                                                  |
| 4. Global State + Dispatch | Done     | Free functions, `frame::state` globals                                                                                                                                                                       |
| 5. Lockdep                 | Done     | `frame/lockdep.rs`, 8 lock classes, debug_assertions only                                                                                                                                                    |
| 6. Benchmark Comparison    | Partial  | Single-core baselines verified (zero regression). Multi-core benchmarks (`ipc_call_reply_2core`, `handle_lookup_contended`) deferred — require multi-core thread execution harness.                          |

## Implementation Order and Dependencies

```text
Layer 0: RwSpinLock                    ← no dependencies
Layer 1: IPI (SGI send/receive)        ← no dependencies (parallel with L0)
Layer 2: Per-CPU Scheduler             ← depends on L1 (IPI for cross-core wake)
Layer 3: Concurrent ObjectTable        ← depends on L0 (RwSpinLock)
Layer 4: Global State + Dispatch       ← depends on L2 + L3 (everything)
Layer 5: Lock Ordering Validation      ← depends on L4 (needs locks to exist)
Layer 6: Benchmark Comparison          ← depends on L4 (needs new dispatch)
```

Layers 0 and 1 can be built in parallel. Layer 2 and 3 can be partially
parallelized (Thread table concurrent access is needed by both). Layer 4 is the
big integration. Layer 5 and 6 are validation.

---

## Risk Assessment

**Highest risk: Layer 4 (dispatch rewrite).** Every syscall handler changes
signature and lock discipline. The IPC handlers (CALL/RECV/REPLY) are the most
complex — each touches 4-5 kernel objects with interleaved lock acquire/release.
A lock ordering mistake here causes deadlock.

**Mitigation:** Implement Group A (read-only) and Group B (creation) first.
These are simple and exercise the new infrastructure without complex multi-lock
patterns. Then Group C (mutation). Then Group D (IPC) — the hardest. Then Group
F and G.

**Second highest risk: Layer 3 (concurrent ObjectTable).** The `get_raw_mut`
under slot lock pattern requires careful unsafe code. The generation-check
pattern (check → lock → re-check) must be airtight.

**Mitigation:** Extensive property testing. Fuzz the concurrent table with
randomized alloc/dealloc/get sequences from multiple threads.

---

## Files Changed Summary

| File                              | Change                                                                 |
| --------------------------------- | ---------------------------------------------------------------------- |
| `frame/arch/aarch64/sync.rs`      | Add RwSpinLock                                                         |
| `frame/arch/aarch64/sysreg.rs`    | Add ICC_SGI1R_EL1 wrapper                                              |
| `frame/arch/aarch64/gic.rs`       | Enable SGIs, add send_sgi()                                            |
| `frame/arch/aarch64/exception.rs` | SGI handler dispatch, reschedule check                                 |
| `frame/arch/aarch64/cpu.rs`       | PerCpu lockdep fields (debug), MPIDR table                             |
| `frame/slab.rs`                   | Add get_raw_mut to Storage trait                                       |
| `table.rs`                        | Concurrent ObjectTable with atomic gen + slot locks + alloc lock       |
| `thread.rs`                       | SchedLink in Thread, PerCpuScheduler, cross-core wake                  |
| `sched.rs` (new)                  | Scheduler module: per-CPU scheduler API                                |
| `state.rs` (new)                  | Global statics, init(), accessors                                      |
| `syscall.rs`                      | Kernel struct removed, dispatch becomes free fn, all sys\_\* rewritten |
| `handle.rs`                       | HandleTable wrapped in RwSpinLock (via AddressSpace)                   |
| `address_space.rs`                | HandleTable field becomes RwSpinLock<HandleTable>                      |
| `endpoint.rs`                     | refcount becomes AtomicUsize                                           |
| `event.rs`                        | refcount becomes AtomicUsize                                           |
| `vmo.rs`                          | refcount becomes AtomicUsize                                           |
| `lockdep.rs` (new)                | Debug-mode lock ordering validator                                     |
| `bench.rs`                        | Adapted to free-function dispatch, new multi-core benchmarks           |
| `invariants.rs`                   | Adapted to global state access                                         |
| `main.rs`                         | Call state::init() instead of Kernel::new()                            |
