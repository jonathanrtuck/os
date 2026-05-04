# Kernel Completion Plan

Complete the v0.7 kernel: 12 items across 4 phases. Every placeholder removed,
every syscall wired end-to-end, hardware paths functional.

## Verification Scope

Host-target tests (currently 241, target ~290+), bare-metal compilation, zero
TODOs/placeholders. Actual hardware boot requires the hypervisor on the M4 Pro —
noted where bare-metal-only verification is needed.

## Syscall ABI

Current ABI: `args: [u64; 6]` in, `(u64, u64)` out. Message data (up to 128
bytes) exceeds register capacity, so `sys_call`/`sys_recv`/`sys_reply` accept
user pointers for message buffers and handle arrays. A new `frame/user_mem.rs`
module provides unsafe user-memory read/write — keeping `#![deny(unsafe_code)]`
intact outside `frame/`.

Multi-wait (step 1.4) fits in 6 registers as 3 × (handle, mask) pairs. Return
packs `(handle_id << 32) | fired_bits_low32`. No user pointers needed.

## Performance Discipline

IPC is the single hottest path in a microkernel — every service interaction,
compositor frame, and document operation flows through
`sys_call`/`sys_recv`/`sys_reply`. The SVC fast path saves only ~64 bytes (vs
832-byte TrapFrame) and dispatches via per-CPU pointer. Heap allocations inside
`dispatch()` undermine this optimization: each allocation touches allocator
metadata across cache lines, polluting the 32 KiB L1d.

**Rules for code on the IPC and event paths:**

1. **Zero heap allocation.** The ABI defines fixed bounds — 128-byte messages, 3
   wait events, bounded handle counts. Use inline storage sized to those bounds.
   `Vec` is forbidden on any path reachable from `sys_call`, `sys_recv`,
   `sys_reply`, `sys_event_wait`, or `sys_event_signal`.
2. **O(1) or O(small-constant) hot paths.** No linear scans of unbounded tables
   from hot syscalls. Gate expensive scans behind fast checks (flags, counts).
3. **Measure before and after.** The benchmark suite must cover IPC round-trip
   latency. Any step that touches the IPC path must not regress the benchmark
   beyond the 10x structural threshold.

---

## Phase 1: IPC Completeness

Host-testable, no new unsafe outside `frame/`.

### Step 1.0 — Hot-path allocation removal

**Files:** `event.rs`, `endpoint.rs`, `config.rs`

Existing hot paths return heap-allocated `Vec` on every call. Fix before
building on top of them.

- **`Event::signal` return type:** replace `Vec<WakeInfo>` with inline storage.
  Add a stack-friendly return type:

  ```rust
  pub struct WakeList {
      items: [WakeInfo; config::MAX_WAITERS_PER_EVENT],
      len: usize,
  }
  ```

  `MAX_WAITERS_PER_EVENT` is 16, `WakeInfo` is 12 bytes → 196 bytes on the
  stack. Acceptable for a 32 KiB kernel stack. `Event::signal` populates this
  inline; callers iterate `.items[..len]` without heap allocation.

- **`Endpoint.recv_waiters`:** replace `Vec<ThreadId>` with a fixed array
  `[Option<ThreadId>; config::MAX_RECV_WAITERS]`. Add
  `MAX_RECV_WAITERS: usize = 4` to `config.rs`. Typical: 1 (single server
  blocked on recv). 4 covers multi-server endpoints with headroom.

  Update `add_recv_waiter`, `remove_recv_waiter`, `drain_recv_waiters` to
  operate on the fixed array. `drain_recv_waiters` returns the array + count (or
  a small `DrainList` struct) instead of `Vec<ThreadId>`.

- **`Endpoint.send_queue`:** keep `Vec<PendingCall>` (PendingCall is ~200 bytes;
  a 16-element inline array would add 3.2 KiB to every Endpoint). But
  pre-allocate in the constructor: `Vec::with_capacity(4)`. This avoids
  reallocation for the common case (1–3 concurrent callers). The capacity grows
  only under unusual contention.

- **`Endpoint.active_replies`:** same treatment — `Vec::with_capacity(4)`.

- Add `config::MAX_IPC_HANDLES: usize = 8` — maximum handles transferred in a
  single IPC operation. 8 covers bootstrap scenarios (code VMO + stack VMO +
  endpoint + event + spare). Used by Step 1.2.

**Tests (~3):**

1. WakeList correctly reports all woken threads (replaces existing Vec test)
2. Fixed recv_waiters array exhaustion returns BufferFull
3. drain_recv_waiters returns correct set after mixed add/remove

### Step 1.1 — Message data passthrough

**Files:** `frame/user_mem.rs` (new), `frame/mod.rs`, `syscall.rs`

- Add `frame/user_mem.rs`:
  - `read_user_message(ptr: usize, len: usize) -> Result<Message, SyscallError>`
    — reads user bytes directly into a stack-allocated `Message`. Validates
    `len <= MSG_SIZE`, then copies into `Message.data[..len]` and sets
    `Message.len`. **No intermediate `Vec<u8>` allocation.**
  - `write_user_bytes(ptr: usize, data: &[u8]) -> Result<(), SyscallError>`
  - `#[cfg(target_os = "none")]`: validates VA range, copies via raw pointer.
    Test target: direct pointer cast (same address space).
  - Both paths are unsafe, confined to `frame/`.

- `sys_call` changes:
  - `args[1]` = msg_ptr, `args[2]` = msg_len
  - Call `read_user_message(msg_ptr, msg_len)`, store result directly in
    `PendingCall.message`. Zero heap allocation.

- `sys_recv` changes:
  - `args[1]` = out_buf_ptr, `args[2]` = out_buf_capacity
  - After dequeuing a call, write message bytes to user buffer via
    `write_user_bytes`.
  - Return: `(reply_cap_id << 32) | msg_len`.

- `sys_reply` changes:
  - `args[2]` = msg_ptr, `args[3]` = msg_len
  - Call `read_user_message`, deliver to blocked caller. Store reply message in
    PendingCall so caller's `sys_call` return path can read it.

- Add reply message storage: extend `ActiveReply` (or add a parallel structure)
  to hold the reply `Message` so that when the caller unblocks, the `sys_call`
  return path can write the reply data back to the caller's output buffer. The
  caller's output buffer pointer needs to be saved at call time (store in
  PendingCall or on the Thread).

**Tests (~5):**

1. send+recv message round-trip (bytes match)
2. empty message (ptr=0, len=0 is valid)
3. oversized message (>128 bytes) rejected
4. reply carries message data back to caller
5. recv with insufficient buffer capacity returns error

### Step 1.2 — Handle transfer over IPC

**Files:** `syscall.rs`, `handle.rs`, `endpoint.rs`

- Replace `PendingCall.handles: Vec<Handle>` with inline storage:

  ```rust
  pub struct PendingCall {
      pub caller: ThreadId,
      pub priority: Priority,
      pub message: Message,
      pub handles: [Option<Handle>; config::MAX_IPC_HANDLES],
      pub handle_count: u8,
      pub badge: u32,
  }
  ```

  `Handle` is ~20 bytes, 8 slots = 160 bytes inline. Total PendingCall becomes
  ~310 bytes. Acceptable — send_queue holds at most 16, and is heap-allocated
  via Vec (pre-allocated in Step 1.0). The hot-path `sys_call` no longer
  allocates for handle staging.

- `HandleTable::remove(id: HandleId) -> Option<Handle>` — new method. Extracts a
  handle from the table (not close — no cleanup, just removal).

- `sys_call` changes:
  - `args[3]` = handles_ptr (array of `u32` HandleIds in user memory)
  - `args[4]` = handles_count
  - Validate `handles_count <= MAX_IPC_HANDLES`. For each handle ID: look up in
    caller's handle table via `remove()`, stage into `PendingCall.handles[i]`.
  - Atomic: if any handle lookup fails, restore all previously removed handles
    and return error. No partial transfer.

- `sys_recv` changes:
  - `args[3]` = handles_out_ptr, `args[4]` = handles_out_capacity
  - After dequeuing, install each staged `Handle` into the server's handle table
    via `HandleTable::allocate_from(handle)` (new method — installs a pre-built
    Handle rather than constructing one).
  - Write new handle IDs to user output buffer.
  - Pack received handle count into return value.

- `sys_reply` changes:
  - `args[4]` = handles_ptr, `args[5]` = handles_count
  - Same transfer mechanics: server removes handles, stages them inline.
  - Caller's `sys_call` return path installs them.

**Tests (~6):**

1. transfer single handle, verify type+rights preserved
2. transfer multiple handles
3. invalid handle ID fails entire call (no partial transfer)
4. caller no longer has the handle after transfer
5. transferred handle is usable by receiver
6. recv with zero handle capacity works (handles discarded or error)

### Step 1.3 — Channel-event auto-signal

**Files:** `endpoint.rs`, `syscall.rs`

- `Endpoint::enqueue_call`: after pushing to send_queue, return
  `self.bound_event` so the caller can signal it. Change return type from
  `Result<(), SyscallError>` to `Result<Option<(EventId, u64)>, SyscallError>`
  where the tuple is `(event_id, signal_bits)`. Use a constant
  `ENDPOINT_READABLE_BIT: u64 = 1`.

- `sys_call` in `syscall.rs`: after `enqueue_call`, if it returned
  `Some((event_id, bits))`, signal that event and wake any waiters. Uses the new
  `WakeList` return from `Event::signal` (Step 1.0) — no allocation in the
  signal+wake path.

- `sys_recv` in `syscall.rs`: after draining a call, if the queue is now empty
  and bound_event exists, clear the readable bit on the event.

  Note: `drain_recv_waiters` now uses the fixed-array return from Step 1.0. The
  wake loop iterates inline storage, not a heap Vec.

- New syscall `ENDPOINT_BIND_EVENT` (number 29, replacing `IRQ_ACK`):
  - `args[0]` = endpoint_handle, `args[1]` = event_handle, `args[2]` = bits
  - Validates both handles (Endpoint with WRITE right, Event with SIGNAL right).
  - Calls `endpoint.bind_event(event_id)`.
  - If endpoint already has pending calls, immediately signal the event.

**Tests (~4):**

1. signal fires on enqueue when event is bound
2. no signal without binding
3. clear on drain when queue empties
4. binding when calls already queued signals immediately

### Step 1.4 — Multi-wait for event_wait

**Files:** `syscall.rs`, `event.rs`, `thread.rs`

- `Thread`: add fixed-size multi-wait state. **No `Vec` — the ABI guarantees at
  most 3 events.**

  ```rust
  wait_events: [u32; 3],  // object IDs of events being waited on
  wait_count: u8,         // 0 = not multi-waiting
  ```

  Add methods:
  - `set_wait_events(ids: &[u32])` — copies into the fixed array, sets count
  - `take_wait_events() -> ([u32; 3], u8)` — returns and clears

  This adds 13 bytes to Thread (3 × 4 + 1), negligible vs the existing ~120
  bytes per Thread.

- `Event::remove_waiter(thread_id: ThreadId)` — already exists (line 115 of
  event.rs). Verify it works correctly when called from the multi-wait cleanup
  path.

- `sys_event_wait` new arg layout:
  - `args[0]` = handle0, `args[1]` = mask0
  - `args[2]` = handle1, `args[3]` = mask1
  - `args[4]` = handle2, `args[5]` = mask2
  - Handle = 0 means unused slot. 1–3 events supported per call.

- Flow:
  1. Validate all non-zero handles (Event type, WAIT right).
  2. Check each for already-signaled bits. If any match, return immediately:
     `(handle_id << 32) | (fired_bits & 0xFFFF_FFFF)`.
  3. If none match: store event object IDs in thread's `wait_events` (fixed
     array copy, no allocation), add thread as waiter on each event, block.
  4. On wake: scan events for the one that fired. Remove thread from all other
     events' waiter lists via `remove_waiter()`. Clear thread's `wait_events`.
     Return `(handle_id << 32) | fired_bits`.
  5. The waking event is identified by checking which event has matching bits
     after wake (level-triggered, so the bits are still set).

  **Wake cleanup cost:** up to 2 calls to `remove_waiter`, each scanning up to
  `MAX_WAITERS_PER_EVENT` (16) slots = 32 comparisons worst case. This is
  inherent to level-triggered multi-wait (Linux epoll pays the same cost).
  Acceptable but should be tracked by the benchmark suite.

- Backward-compatible: single event wait is `args[2..5] = 0`.

**Tests (~7):**

1. single event (backward compat with existing tests)
2. two events, first fires → returns first's handle+bits
3. two events, second fires → returns second's handle+bits
4. three events, middle fires
5. already-signaled returns immediately (no block)
6. waiter removed from non-fired events on wake
7. handle=0 slots skipped correctly

---

## Phase 2: Syscall Surface Cleanup

### Step 2.1 — Replace irq_bind/irq_ack with event-based interrupts

**Files:** `syscall.rs`, `irq.rs`, `event.rs`,
`frame/arch/aarch64/exception.rs`, `frame/arch/aarch64/gic.rs`

- Rename syscall 28: `IRQ_BIND` → `EVENT_BIND_IRQ`. Same semantics as current
  `irq_bind` but with event handle validation:
  - `args[0]` = event_handle (must be Event type with SIGNAL right)
  - `args[1]` = intid
  - `args[2]` = signal_bits
  - Validate handle, extract event object ID, call `IrqTable::bind()`.

- Remove syscall 29 (`IRQ_ACK`):
  - Delete `num::IRQ_ACK` constant.
  - Dispatch returns `UnknownSyscall` for number 29.
  - Reassign number 29 to `ENDPOINT_BIND_EVENT` (from step 1.3).

- Modify `sys_event_clear` to auto-unmask IRQs:

  **Performance gate:** most events have no IRQ bindings. A linear scan of
  `IrqTable` (up to `MAX_IRQS = 1024` entries) on every `event_clear` would add
  O(1024) work to a hot path for no benefit.

  Add `irq_bound: bool` to `Event` (1 byte). Set to `true` by `EVENT_BIND_IRQ`,
  cleared when the binding is removed. In `sys_event_clear`:

  ```rust
  if event.irq_bound {
      let intids = irqs.intids_for_event_bits(event_id, cleared_bits);
      for intid in intids { gic::unmask_spi(intid); }
  }
  ```

  This makes the common case (non-IRQ events) a single branch, always not-taken.

- Add `IrqTable::intids_for_event_bits(event_id, bits) -> (array, count)`: scan
  bindings, return INTIDs where `binding.event_id == event_id` and
  `binding.signal_bits & bits != 0`. Return inline array (not Vec) — max
  concurrent IRQ bindings per event is small (typically 1–2).

- Also clear the `ack_pending` flag in IrqTable.

- Update syscall count comment from 30 to 30 (same count, different assignment:
  28=EVENT_BIND_IRQ, 29=ENDPOINT_BIND_EVENT).

**Tests (~6):**

1. event_bind_irq validates handle type (VMO handle rejected)
2. PPI (intid < 32) rejected
3. double-bind same INTID fails
4. clear with IRQ-bound bits triggers unmask (testable via IrqTable state)
5. unbind on event destroy cleans up binding
6. event_clear on non-IRQ-bound event skips scan (irq_bound == false)

### Step 2.2 — Extend thread_create_in with initial_handles

**Files:** `syscall.rs`, `handle.rs`

- `sys_thread_create_in` arg changes:
  - `args[0]` = space_handle
  - `args[1]` = entry
  - `args[2]` = stack_vmo_handle
  - `args[3]` = arg
  - `args[4]` = handles_ptr (array of HandleId values to duplicate into target
    space)
  - `args[5]` = handles_count

- For each handle ID in the array:
  - Look up in caller's handle table (do NOT remove — duplicate).
  - Install copy in target space's handle table.
  - The new thread receives handles at well-known indices (0, 1, 2, ...).

- Atomic: if any handle installation fails, roll back all installed handles in
  the target space and return error.

- The thread's `arg` register (`args[3]`) can encode the handle count so the new
  thread knows how many bootstrap handles to expect.

**Tests (~4):**

1. initial handles installed in target space at sequential indices
2. empty handles (count=0) works
3. invalid handle fails atomically (no partial install)
4. source handles not removed (duplicated, not transferred)

---

## Phase 3: Hardware Platform Completion

All changes in `frame/` — unsafe code with SAFETY comments.

### Step 3.1 — Page table population for init

**Files:** `bootstrap.rs`, `main.rs`, `frame/user_mem.rs`

- Add to `frame/user_mem.rs`:
  - `write_phys(pa: PhysAddr, offset: usize, data: &[u8])` — bare-metal memcpy
    to physical address + offset. Needed to populate code pages before mapping.
    `#[cfg(target_os = "none")]` only.
  - `zero_phys(pa: PhysAddr, len: usize)` — zero-fill a physical page.

- `bootstrap::create_init` changes (wrap hardware ops in
  `#[cfg(target_os = "none")]` blocks):
  1. Call `page_table::create_page_table()` → `(root, asid)`.
  2. Store root in `AddressSpace.page_table_root` and asid in
     `AddressSpace.asid`.
  3. For each page of init_binary:
     - `page_alloc::alloc_page()` → `pa`
     - `write_phys(pa, 0, &init_binary[offset..offset+PAGE_SIZE])`
     - `page_table::map_page(root, VirtAddr(INIT_CODE_VA + offset), pa, Perms::RX)`
  4. For each stack page:
     - `page_alloc::alloc_page()` → `pa`
     - `zero_phys(pa, PAGE_SIZE)`
     - `page_table::map_page(root, VirtAddr(INIT_STACK_VA + offset), pa, Perms::RW)`

- `main.rs` changes:
  - Before `enter_userspace`, load the page table:

    ```rust
    let space = kern.spaces.get(space_id.0).unwrap();
    page_table::switch_table(
        PhysAddr(space.page_table_root()),
        page_table::Asid(space.asid()),
    );
    ```

  - Remove the existing test page table create/map/destroy block (lines 36-42) —
    it was a smoke test, now subsumed.

- `AddressSpace`: add `pub fn page_table_root(&self) -> usize` and
  `pub fn asid(&self) -> u8` accessors (if not already present).

**Tests:** Existing host-target bootstrap tests continue to pass (they don't
call hardware page table functions). Bare-metal verification: init executes
`system_info` syscall and exits. **Requires hypervisor.**

### Step 3.2 — GIC redistributor/distributor mask/unmask

**Files:** `frame/arch/aarch64/gic.rs`, `frame/arch/aarch64/exception.rs`

- Add constants:
  - `GICD_ICENABLER: usize = 0x0180` — interrupt clear-enable registers
  - `GICD_ISENABLER: usize = 0x0100` — already exists but unused for SPIs

- Add `pub fn mask_spi(intid: u32)`:

  ```rust
  let reg = GICD_ICENABLER + ((intid / 32) as usize) * 4;
  let bit = intid % 32;
  mmio::write32(dist_base + reg, 1 << bit);
  ```

  Only valid for INTID >= 32 (SPIs). Assert or early-return for PPIs.

- Add `pub fn unmask_spi(intid: u32)`:

  ```rust
  let reg = GICD_ISENABLER + ((intid / 32) as usize) * 4;
  let bit = intid % 32;
  mmio::write32(dist_base + reg, 1 << bit);
  ```

- Store distributor base address in a static (it's currently local to
  `init_distributor`). Add `static DIST_BASE: AtomicUsize` initialized during
  `init()`.

- `handle_irq` in `exception.rs`: after calling device IRQ handler, call
  `gic::mask_spi(intid)`. Replaces the TODO at line 133.

**Tests (~2):** Register offset calculation tests (intid 32 → offset 0x4, bit 0;
intid 63 → offset 0x4, bit 31; intid 64 → offset 0x8, bit 0). Bare-metal:
interrupt delivery works. **Requires hypervisor.**

### Step 3.3 — Fault handler resolution

**Files:** `fault.rs`, `frame/fault_resolve.rs` (new)

- New `frame/fault_resolve.rs` — unsafe resolution operations:
  - `resolve_cow(root: PhysAddr, asid: Asid, vaddr: usize, old_pa: PhysAddr) -> Result<(), SyscallError>`:
    1. `page_alloc::alloc_page()` → new_pa
    2. Copy PAGE_SIZE bytes from old_pa to new_pa
       (`core::ptr::copy_nonoverlapping`)
    3. `page_table::map_page(root, VirtAddr(vaddr), new_pa, Perms::RW)`
    4. `page_table::invalidate_page(asid, VirtAddr(vaddr))`
    5. `page_alloc::release(old_pa)` — decrement refcount on shared page

  - `resolve_lazy(root: PhysAddr, vaddr: usize, perms: Perms) -> Result<(), SyscallError>`:
    1. `page_alloc::alloc_page()` → pa
    2. `zero_phys(pa, PAGE_SIZE)`
    3. `page_table::map_page(root, VirtAddr(vaddr), pa, perms)`

  - `resolve_pager(kernel: &mut Kernel, current: ThreadId, vmo_id: VmoId, page_idx: usize) -> FaultAction`:
    1. Get pager endpoint from VMO
    2. Construct fault message: `(vmo_id, page_idx, fault_type)`
    3. Enqueue as a PendingCall on the pager endpoint
    4. Block current thread (same as `sys_call` blocking)
    5. On wake, the pager has supplied the page via `vmo_write_page` or
       equivalent — map it and resume
    6. Note: this is the most complex path and may need a VMO method
       `supply_page(page_idx, pa)` that the pager's reply handler calls.

- `fault.rs` changes: replace the three placeholder returns with calls to
  `frame::fault_resolve::*`. Gate behind `#[cfg(target_os = "none")]` — test
  target keeps classification-only behavior.

- To get the physical address for COW resolution, need the VMO to track physical
  addresses of its pages. Currently `Vmo` uses
  `pages: [Option<NonNull<u8>>; MAX_PAGES_INLINE]` (virtual pointers on host,
  physical addresses on bare metal). On bare metal, the page addresses stored in
  VMO are physical — use them directly.

**Tests (~4):**

1. COW resolution allocates new page and decrements old refcount (host test
   using mock/simulated page operations)
2. Lazy allocation produces zeroed page
3. Pager dispatch sends fault message on correct endpoint (integration test with
   endpoint)
4. Write-to-sealed still kills (existing test preserved)

---

## Phase 4: Userspace Foundation

### Step 4.1 — Userspace syscall library (libsys)

**Files:** new `libsys/` crate

- `libsys/Cargo.toml`: `#![no_std]`, no dependencies.
- `libsys/.cargo/config.toml`: target = aarch64-unknown-none.

- `libsys/src/lib.rs`: re-export modules.

- `libsys/src/syscall.rs`: raw syscall function (extracted from init's current
  inline asm). One generic `raw_syscall(num, a0..a5) -> (u64, u64)`.

- `libsys/src/types.rs`: `Handle(u32)`, `Rights(u32)`, `Priority`,
  `SyscallError`, `Message`, `EventWaitResult`, `SystemInfoKey`.

- `libsys/src/vmo.rs`: typed wrappers:
  - `vmo_create(size, flags) -> Result<Handle, SyscallError>`
  - `vmo_map(handle, addr_hint, perms) -> Result<usize, SyscallError>`
  - `vmo_map_into(vmo, space, addr, perms) -> Result<usize, SyscallError>`
  - `vmo_unmap(addr) -> Result<(), SyscallError>`
  - `vmo_snapshot(handle) -> Result<Handle, SyscallError>`
  - `vmo_seal(handle) -> Result<(), SyscallError>`
  - `vmo_resize(handle, new_size) -> Result<(), SyscallError>`
  - `vmo_set_pager(vmo, endpoint) -> Result<(), SyscallError>`

- `libsys/src/ipc.rs`: typed wrappers:
  - `endpoint_create() -> Result<Handle, SyscallError>`
  - `call(endpoint, msg, handles) -> Result<(Message, Vec<Handle>), SyscallError>`
  - `recv(endpoint, msg_buf, handles_buf) -> Result<(ReplyCap, usize, usize), SyscallError>`
  - `reply(endpoint, reply_cap, msg, handles) -> Result<(), SyscallError>`
  - `endpoint_bind_event(endpoint, event, bits) -> Result<(), SyscallError>`

- `libsys/src/event.rs`:
  - `event_create() -> Result<Handle, SyscallError>`
  - `event_signal(handle, bits) -> Result<(), SyscallError>`
  - `event_wait(items: &[(Handle, u64)]) -> Result<(Handle, u64), SyscallError>`
  - `event_clear(handle, bits) -> Result<(), SyscallError>`
  - `event_bind_irq(event, intid, bits) -> Result<(), SyscallError>`

- `libsys/src/thread.rs`:
  - `thread_create(entry, stack_vmo, arg) -> Result<Handle, SyscallError>`
  - `thread_exit(code) -> !`
  - `thread_set_priority(handle, priority) -> Result<(), SyscallError>`
  - `thread_set_affinity(handle, hint) -> Result<(), SyscallError>`

- `libsys/src/space.rs`:
  - `space_create() -> Result<Handle, SyscallError>`
  - `space_destroy(handle) -> Result<(), SyscallError>`

- `libsys/src/handle.rs`:
  - `handle_dup(handle, rights) -> Result<Handle, SyscallError>`
  - `handle_close(handle) -> Result<(), SyscallError>`
  - `handle_info(handle) -> Result<(ObjectType, Rights), SyscallError>`

- `libsys/src/system.rs`:
  - `clock_read() -> Result<u64, SyscallError>`
  - `system_info(key) -> Result<u64, SyscallError>`

- Update `init/Cargo.toml`: add `libsys = { path = "../libsys" }`.
- Rewrite `init/src/main.rs` to use libsys wrappers instead of raw asm.

**Tests:** libsys is `#![no_std]` targeting bare metal — no host tests. Verified
by init compilation + bare-metal execution.

### Step 4.2 — Service manifest and init spawn

**Files:** `init/src/main.rs`, `init/src/manifest.rs` (new)

- Define manifest format. Simplest viable: a static Rust array compiled into
  init, describing each service:

  ```rust
  struct ServiceEntry {
      name: &'static str,
      code_vmo_handle_index: u32,  // bootstrap handle index
      priority: Priority,
  }
  ```

  The kernel bootstrap (step 2.2) passes code VMO handles for each service as
  initial handles to init.

- `init/src/manifest.rs`: hardcoded manifest for the initial service set. Start
  minimal — just a single "hello" test service that makes a syscall and exits.

- `init/src/main.rs` startup sequence:
  1. Read bootstrap handles from well-known indices.
  2. For each service in manifest: a. `space_create()` → space_handle b.
     `vmo_create(stack_size)` → stack_vmo c. `endpoint_create()` → (our_end,
     their_end) — for parent↔child IPC d.
     `thread_create_in(space, entry, stack_vmo, arg, [code_vmo, their_end])` —
     passes code VMO + endpoint as initial handles
  3. Wait on events for service lifecycle.

- Optionally: create a minimal test service binary (`svc_test/`) that just does
  `system_info` + `thread_exit`, to prove the spawn path works.

**Tests:** integration test via bare-metal boot. Init spawns a child service,
child makes a syscall, child exits, init observes exit event. **Requires
hypervisor.**

---

## Dependency Graph

```text
Phase 1 (sequential start, then parallel):
  1.0 hot-path alloc ──┬── must land first (1.1, 1.2, 1.3 build on it)
  1.1 msg passthrough ─┤
  1.2 handle transfer ─┼── independent of each other, depend on 1.0
  1.3 channel-event   ─┤
  1.4 multi-wait      ─┘── independent (Thread change, no Endpoint overlap)

Phase 2 (depends on Phase 1):
  2.1 irq replacement ─── depends on 1.3 (event binding concept)
                           depends on 3.2 (gic mask/unmask for event_clear)
  2.2 thread_create_in ── depends on 1.2 (handle transfer mechanics)

Phase 3 (hardware, partially parallel):
  3.1 page tables ────── standalone (uses existing page_alloc + page_table)
  3.2 GIC masking ────── standalone (used by 2.1)
  3.3 fault handler ──── depends on 3.1 (page alloc infra, same module)

Phase 4 (depends on everything):
  4.1 libsys ─────────── depends on 1.1, 1.2, 1.4, 2.1 (final syscall ABI)
  4.2 service manifest ─ depends on 2.2, 4.1
```

## Implementation Order (Sequential)

1. **1.0** Hot-path allocation removal (Event, Endpoint, config constants)
2. **1.1** Message data passthrough
3. **1.2** Handle transfer (uses inline PendingCall.handles from 1.0)
4. **1.4** Multi-wait
5. **1.3** Channel-event auto-signal (uses WakeList + DrainList from 1.0)
6. **3.2** GIC mask/unmask (needed by 2.1)
7. **2.1** IRQ replacement (needs 1.3 + 3.2)
8. **2.2** thread_create_in initial handles (needs 1.2)
9. **3.1** Page table population
10. **3.3** Fault handler resolution
11. **4.1** Userspace syscall library
12. **4.2** Service manifest

## Post-Completion Checklist

- [ ] All host-target tests pass (target: 290+)
- [ ] `cargo test --target aarch64-apple-darwin` clean
- [ ] Zero `TODO`, `FIXME`, `placeholder`, or `Real implementation:` in source
- [ ] Bare-metal build succeeds: `cargo build --release`
- [ ] Fuzz targets still compile and run
- [ ] Benchmark suite still runs
- [ ] Zero heap allocations in sys_call/sys_recv/sys_reply/sys_event_wait paths
- [ ] IPC round-trip benchmark shows no regression from baseline
- [ ] STATUS.md updated to reflect completion
- [ ] Syscall count and table in doc comments match reality
- [ ] Every `unsafe` block has a `// SAFETY:` comment
- [ ] `#![deny(unsafe_code)]` enforced outside `frame/`
