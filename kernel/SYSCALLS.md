# Syscall Reference

48 syscalls. Invoke via `svc #0`. Syscall number in `x8`, arguments in `x0`–`x5`, result in `x0`. Registers other than `x0` are preserved across the call.

Success: `x0 >= 0`. Error: `x0 < 0` (signed, see [Error Codes](#error-codes)).

## ABI

| Register | Direction | Role                                |
| -------- | --------- | ----------------------------------- |
| x8       | in        | Syscall number                      |
| x0–x5    | in        | Arguments (syscall-specific)        |
| x0       | out       | Return value (≥0 success, <0 error) |

## Error Codes

| Code | Name               | Description                                     |
| ---- | ------------------ | ----------------------------------------------- |
| -1   | UnknownSyscall     | Invalid syscall number                          |
| -2   | BadAddress         | Invalid user VA or unmapped memory              |
| -3   | BadLength          | Length out of bounds                            |
| -4   | InvalidArgument    | Argument rejected (alignment, state, etc.)      |
| -5   | AlreadyBorrowing   | Thread already borrowing a scheduling context   |
| -6   | NotBorrowing       | Thread not borrowing any scheduling context     |
| -7   | AlreadyBound       | Thread already has a bound scheduling context   |
| -8   | WouldBlock         | Futex value mismatch (compare-and-sleep failed) |
| -9   | OutOfMemory        | Page allocator or handle table exhausted        |
| -10  | PermissionDenied   | VMO sealed, wrong state, or insufficient rights |
| -11  | InvalidHandle      | Handle index out of range or slot empty         |
| -12  | InsufficientRights | Handle lacks required rights for this operation |
| -13  | TableFull          | Handle table at capacity (4096)                 |
| -15  | SyscallBlocked     | Syscall disabled by process filter mask         |

## Rights

Handles carry a rights bitmask (u32). Rights attenuate monotonically — a derived handle can only have a subset of its parent's rights.

| Bit | Name     | Checked by                                                                                                           |
| --- | -------- | -------------------------------------------------------------------------------------------------------------------- |
| 0   | READ     | vmo_read, thread_read_state, sched_ctx_borrow/bind                                                                   |
| 1   | WRITE    | vmo_write, vmo_map(RW), vmo_set_pager, process_start, process_set_syscall_filter, thread_suspend/resume, event_reset |
| 2   | SIGNAL   | channel_signal, event_signal, interrupt_ack                                                                          |
| 3   | WAIT     | (reserved)                                                                                                           |
| 4   | MAP      | vmo_map                                                                                                              |
| 5   | TRANSFER | handle_send (gate: source must have TRANSFER)                                                                        |
| 6   | CREATE   | (reserved)                                                                                                           |
| 7   | KILL     | process_kill                                                                                                         |
| 8   | SEAL     | vmo_seal                                                                                                             |
| 9   | APPEND   | vmo_write (append-only mode: APPEND without WRITE)                                                                   |
| 10  | DUPLICATE| handle_dup (gate: source must have DUPLICATE)                                                                        |

---

## Thread Lifecycle

### 0 — exit

Terminate the calling thread with an exit code.

```text
x8 = 0
x0 = exit_code    (i64, stored on the process)
```

Does not return. The exit code is stored on the process and can be retrieved via `process_get_exit_code` (syscall 47) after the process exits. If this is the last thread in a process, the process exits. Involuntary termination (process_kill) stores `i64::MIN` as a distinguishable sentinel.

### 2 — yield

Voluntarily yield the CPU.

```text
x8 = 2
Returns: x0 = 0
```

Invokes the scheduler. The calling thread may be immediately rescheduled if no other thread is ready.

### 35 — thread_create

Create a new thread in the calling process.

```text
x8 = 35
x0 = entry_va    (thread entry point, must be in user VA)
x1 = stack_top   (initial SP, must be 16-byte aligned, in user VA)
Returns: x0 = thread handle
Errors: BadAddress, OutOfMemory
```

The new thread begins execution at `entry_va` with `sp = stack_top`. It shares the parent process's address space and handle table.

### 36 — thread_suspend

Suspend a thread.

```text
x8 = 36
x0 = handle       (thread handle, requires WRITE)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument
```

The thread must not be the caller. A suspended thread can be inspected with `thread_read_state`.

### 37 — thread_resume

Resume a suspended thread.

```text
x8 = 37
x0 = handle       (thread handle, requires WRITE)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument
```

### 38 — thread_read_state

Read a suspended thread's register state.

```text
x8 = 38
x0 = handle       (thread handle, requires READ)
x1 = buf_va       (user VA of output buffer, must hold a Context)
Returns: x0 = size of Context in bytes
Errors: InvalidHandle, InsufficientRights, InvalidArgument, BadAddress
```

The thread must be suspended. Writes the full `Context` struct (registers, SPSR, FP/NEON state) to the buffer.

---

## Process Management

### 31 — process_create

Create a new process from an ELF binary.

```text
x8 = 31
x0 = elf_ptr      (user VA of ELF data)
x1 = elf_len      (byte count, max 16 MiB)
Returns: x0 = process handle
Errors: BadAddress, BadLength, InvalidArgument (bad ELF), OutOfMemory
```

Parses the ELF, creates an address space, loads segments, and creates an initial suspended thread. The process does not run until `process_start`. Use `handle_send` to transfer handles into the new process before starting it.

### 32 — process_start

Start a suspended process.

```text
x8 = 32
x0 = handle       (process handle, requires WRITE)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument (already started)
```

Resumes the process's initial thread. Can only be called once.

### 33 — process_kill

Terminate a process.

```text
x8 = 33
x0 = handle       (process handle, requires KILL)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument (self-kill)
```

Kills all threads, closes all handles, frees the address space. Cannot kill the calling process.

### 34 — process_set_syscall_filter

Set a syscall filter mask on a process.

```text
x8 = 34
x0 = handle       (process handle, requires WRITE)
x1 = mask         (u64 bitmask: bit N enables syscall N, for 0–31)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument (already started)
```

Must be called before `process_start`. Syscalls 32–45 are always allowed. EXIT (0) is always allowed regardless of the mask.

---

## Handles

### 3 — handle_close

Close a handle.

```text
x8 = 3
x0 = handle_nr    (handle table index)
Returns: x0 = 0
Errors: InvalidHandle
```

Releases the kernel object reference. If this was the last handle to the object, the object is destroyed.

### 4 — handle_send

Transfer a handle to another process.

```text
x8 = 4
x0 = target       (process handle in caller's table)
x1 = source       (handle to transfer)
x2 = rights_mask  (rights to attenuate; 0 = preserve all)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights (source lacks TRANSFER), InvalidArgument (target already started), TableFull
```

Inserts a copy of `source` (with attenuated rights) into the target process's handle table. The target process must not have been started yet. The source handle remains valid in the caller's table.

### 5 — handle_set_badge

Attach a badge value to a handle.

```text
x8 = 5
x0 = handle_nr
x1 = badge        (arbitrary u64)
Returns: x0 = 0
Errors: InvalidHandle
```

Badges are user-defined identifiers preserved through `handle_send`. Useful for demultiplexing when a server holds many client handles.

### 6 — handle_get_badge

Retrieve a handle's badge value.

```text
x8 = 6
x0 = handle_nr
Returns: x0 = badge (u64)
Errors: InvalidHandle
```

---

## IPC — Channels

### 7 — channel_create

Create a bidirectional IPC channel.

```text
x8 = 7
Returns: x0 = handle_a | (handle_b << 16)
Errors: OutOfMemory, TableFull
```

Allocates two shared pages (one per direction, SPSC ring buffers). Each endpoint is a separate handle. Messages are 64-byte fixed-size slots.

### 8 — channel_signal

Signal the far endpoint of a channel.

```text
x8 = 8
x0 = handle       (channel handle, requires SIGNAL)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

Wakes any thread waiting on the far endpoint. Used after writing to the shared ring buffer.

---

## Synchronization

### 9 — wait

Wait for any of multiple handles to become ready.

```text
x8 = 9
x0 = handles_ptr  (user VA of u16[] array)
x1 = count        (1–16)
x2 = timeout_ns   (nanoseconds; 0 = wait forever)
Returns: x0 = index of first ready handle
Errors: BadAddress, BadLength, InvalidHandle
May block.
```

Waitable handle types: channels, events, timers, interrupts, threads, processes. Returns the index (into the array) of the first handle that became ready.

### 10 — futex_wait

Compare-and-sleep on a futex word.

```text
x8 = 10
x0 = addr         (user VA of u32, must be 4-byte aligned)
x1 = expected     (expected value)
Returns: x0 = 0
Errors: BadAddress (alignment), WouldBlock (value mismatch)
May block.
```

Atomically checks `*addr == expected`. If equal, blocks until woken by `futex_wake`. If not equal, returns WouldBlock immediately. Keyed by physical address (works across shared memory).

### 11 — futex_wake

Wake threads blocked on a futex.

```text
x8 = 11
x0 = addr         (user VA of u32, must be 4-byte aligned)
x1 = count        (max threads to wake)
Returns: x0 = number of threads actually woken
Errors: BadAddress
```

---

## Timers & Clock

### 12 — timer_create

Create a one-shot timer.
text

```text
x8 = 12
x0 = timeout_ns   (nanoseconds from now)
Returns: x0 = timer handle
Errors: OutOfMemory, TableFull
```

The handle becomes ready (waitable) when the timeout fires.

### 13 — clock_get

Read the monotonic system clock.

```text
x8 = 13
Returns: x0 = nanoseconds since boot
```

Derived from the hardware counter. Resolution depends on counter frequency (24 MHz on Apple Silicon, 62.5 MHz on QEMU).

### 48 — timer_set

Reprogram an existing timer's deadline.

```text
x8 = 48
x0 = handle        (timer handle, requires WRITE right)
x1 = deadline_ns   (nanoseconds from now)
x2 = period_ns     (reserved for periodic timers, stored but not acted on)
Returns: x0 = 0
Errors: InvalidArgument, InvalidHandle, InsufficientRights
```

Clears the timer's fired state so it can be waited on again. Updates EARLIEST_DEADLINE cache and reprograms hardware timer if needed.

### 49 — timer_cancel

Disarm a timer without destroying it.

```text
x8 = 49
x0 = handle        (timer handle, requires WRITE right)
Returns: x0 = 0
Errors: InvalidArgument, InvalidHandle, InsufficientRights
```

The timer handle remains valid and can be re-armed with timer_set. Clears the fired state.

---

## Memory

### 14 — memory_alloc

Allocate heap pages.

```text
x8 = 14
x0 = page_count   (number of 16 KiB pages)
Returns: x0 = user VA of allocation
Errors: OutOfMemory, BadLength
```

Maps pages in the process's heap region. For new code, prefer VMOs (`vmo_create` + `vmo_map`) which support sharing, snapshots, and paging.

### 15 — memory_free

Free heap pages.

```text
x8 = 15
x0 = va           (must be page-aligned, in heap region)
x1 = page_count
Returns: x0 = 0
Errors: BadAddress, InvalidArgument
```

---

## Virtual Memory Objects (VMOs)

### 16 — vmo_create

Create a virtual memory object.

```text
x8 = 16
x0 = size_pages   (capacity in 16 KiB pages)
x1 = flags        (CONTIGUOUS=1, SNAPSHOT_ENABLED=2)
x2 = type_tag     (user-defined u64 content type)
Returns: x0 = VMO handle
Errors: OutOfMemory, TableFull
```

Normal VMOs are lazy (demand-paged). CONTIGUOUS pre-allocates all pages (for DMA). SNAPSHOT_ENABLED allows `vmo_snapshot`/`vmo_restore`.

### 17 — vmo_map

Map a VMO into an address space.

```text
x8 = 17
x0 = handle       (VMO handle, requires MAP)
x1 = flags        (READ=1, WRITE=2)
x2 = target       (process handle for cross-process map; 0 = self)
Returns: x0 = mapped user VA
Errors: InvalidHandle, InsufficientRights, OutOfMemory, PermissionDenied (sealed VMO + WRITE)
```

Cross-process mapping (`x2 != 0`) only works before the target process is started.

### 18 — vmo_unmap

Unmap a VMO region.

```text
x8 = 18
x0 = va           (mapped address)
x1 = size_pages
Returns: x0 = 0
Errors: BadAddress, InvalidArgument
```

### 19 — vmo_read

Read from a VMO into a user buffer.

```text
x8 = 19
x0 = handle       (VMO handle, requires READ)
x1 = offset       (byte offset into VMO)
x2 = buf_va       (destination buffer, must be writable)
x3 = len          (byte count)
Returns: x0 = bytes read
Errors: InvalidHandle, InsufficientRights, BadAddress, BadLength
```

### 20 — vmo_write

Write from a user buffer into a VMO.

```text
x8 = 20
x0 = handle       (VMO handle, requires WRITE or APPEND)
x1 = offset       (byte offset)
x2 = buf_va       (source buffer, must be readable)
x3 = len          (byte count)
Returns: x0 = bytes written
Errors: InvalidHandle, InsufficientRights, BadAddress, BadLength, PermissionDenied (sealed)
```

With APPEND right (without WRITE), writes are append-only.

### 21 — vmo_get_info

Query VMO metadata.

```text
x8 = 21
x0 = handle       (any valid VMO handle)
x1 = info_va      (user VA of VmoInfo output struct)
Returns: x0 = 0
Errors: InvalidHandle, BadAddress
```

VmoInfo contains: size_pages, committed_pages, flags, type_tag, snapshot_count.

### 22 — vmo_snapshot

Create a COW snapshot of a VMO.

```text
x8 = 22
x0 = handle       (VMO handle, requires WRITE)
Returns: x0 = generation ID
Errors: InvalidHandle, InsufficientRights, PermissionDenied (sealed/contiguous), OutOfMemory
```

VMO must have been created with SNAPSHOT_ENABLED. Snapshots use copy-on-write — only pages modified after the snapshot consume additional memory.

### 23 — vmo_restore

Restore a VMO to a previous snapshot.

```text
x8 = 23
x0 = handle       (VMO handle, requires WRITE)
x1 = generation   (snapshot ID from vmo_snapshot)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, InvalidArgument (bad generation)
```

Invalidates PTEs in all processes that have this VMO mapped.

### 24 — vmo_seal

Seal a VMO (make immutable).

```text
x8 = 24
x0 = handle       (VMO handle, requires SEAL)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

After sealing: no writes, no commits, no decommits, no pager attachment. All existing mappings become read-only. Irreversible.

### 25 — vmo_op_range

Perform a range operation on a VMO.

```text
x8 = 25
x0 = handle       (VMO handle)
x1 = op           (0=LOOKUP, 1=COMMIT, 2=DECOMMIT)
x2 = offset_pages (page offset)
x3 = page_count
Returns: x0 = (op-specific, see below)
Errors: InvalidHandle, InsufficientRights, InvalidArgument, OutOfMemory
```

| Op  | Name     | Returns         | Description                                              |
| --- | -------- | --------------- | -------------------------------------------------------- |
| 0   | LOOKUP   | base PA         | Get physical address of a committed page range (for DMA) |
| 1   | COMMIT   | pages committed | Eagerly allocate pages (pre-fault)                       |
| 2   | DECOMMIT | pages freed     | Release backing pages                                    |

### 26 — vmo_set_pager

Attach a pager to a VMO.

```text
x8 = 26
x0 = vmo_handle   (VMO handle, requires WRITE)
x1 = channel_handle (channel for pager communication)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, PermissionDenied (sealed)
```

When a page fault occurs on an uncommitted page, the kernel sends a fault request to the pager channel. The pager resolves the fault with `pager_supply`.

### 27 — pager_supply

Supply pages to resolve a pager fault.

```text
x8 = 27
x0 = vmo_handle   (VMO handle, requires WRITE)
x1 = offset_pages (page offset of supplied region)
x2 = page_count
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

Clears pending faults and wakes threads waiting on the faulted pages.

---

## Events

### 28 — event_create

Create a manual-reset event.

```text
x8 = 28
Returns: x0 = event handle
Errors: OutOfMemory, TableFull
```

Events are signaling primitives. Initially unsignaled.

### 29 — event_signal

Signal an event.

```text
x8 = 29
x0 = handle       (event handle, requires SIGNAL)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

Wakes all threads waiting on this event. The event remains signaled until explicitly reset.

### 30 — event_reset

Reset an event to unsignaled.

```text
x8 = 30
x0 = handle       (event handle, requires WRITE)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

---

## Scheduling

### 39 — scheduling_context_create

Create a scheduling context for deadline scheduling.

```text
x8 = 39
x0 = budget_ns    (CPU budget per period, in nanoseconds)
x1 = period_ns    (scheduling period, in nanoseconds)
Returns: x0 = scheduling context handle
Errors: OutOfMemory, TableFull
```

Scheduling contexts control CPU bandwidth allocation. Threads borrow or bind to a context to execute under its budget/period constraints.

### 40 — scheduling_context_borrow

Temporarily borrow a scheduling context.

```text
x8 = 40
x0 = handle       (scheduling context handle, requires READ)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, AlreadyBorrowing
```

The calling thread executes using the borrowed context's budget. Return with `scheduling_context_return`.

### 41 — scheduling_context_return

Return a borrowed scheduling context.

```text
x8 = 41
Returns: x0 = 0
Errors: NotBorrowing
```

### 42 — scheduling_context_bind

Bind a scheduling context as the thread's default.

```text
x8 = 42
x0 = handle       (scheduling context handle, requires READ)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights, AlreadyBound
```

Unlike borrow, bind is permanent for the thread's lifetime.

---

## Devices & Interrupts

### 43 — device_map

Map device MMIO into the caller's address space.

```text
x8 = 43
x0 = pa           (physical address, must not overlap RAM)
x1 = size         (byte count)
Returns: x0 = mapped user VA
Errors: BadAddress, OutOfMemory
```

Mapped as Device-nGnRE memory (no caching, no reordering). PA must be in device space, not RAM.

### 44 — interrupt_register

Register an interrupt handler.

```text
x8 = 44
x0 = irq          (IRQ number)
Returns: x0 = interrupt handle
Errors: OutOfMemory, TableFull
```

The handle becomes ready (waitable via `wait`) when the interrupt fires. After handling, call `interrupt_ack` to re-enable delivery.

### 45 — interrupt_ack

Acknowledge an interrupt.

```text
x8 = 45
x0 = handle       (interrupt handle, requires SIGNAL)
Returns: x0 = 0
Errors: InvalidHandle, InsufficientRights
```

Clears the pending state and re-enables interrupt delivery via the interrupt controller.

---

## Capability Duplication

### 46 — handle_dup

Duplicate a handle within the caller's handle table. The new handle references the same kernel object with optionally attenuated rights. The original handle's badge is copied to the duplicate.

```text
x8 = 46
x0 = handle       (source handle, requires DUPLICATE)
x1 = rights_mask  (0 = preserve all rights, non-zero = attenuate via AND)
Returns: x0 = new handle number
Errors: InvalidHandle, InsufficientRights, TableFull
```

The source handle must have the DUPLICATE right (bit 10). The new handle's rights are `original_rights AND rights_mask` when `rights_mask != 0`, or `original_rights` when `rights_mask == 0`. Rights can only be reduced (attenuated), never escalated.

---

## Process Inspection

### 47 — process_get_exit_code

Retrieve the exit code of an exited process.

```text
x8 = 47
x0 = process_handle  (requires READ)
Returns: x0 = exit code (i64 reinterpreted as u64)
Errors: InvalidArgument (still running or invalid handle), InvalidHandle
```

Only valid after the process has exited (its exit notification has fired). Returns `InvalidArgument` if the process is still running. Voluntary exit stores the value from the `exit` syscall's x0; involuntary termination (process_kill) stores `i64::MIN` (-9223372036854775808).

---

## Debug

### 1 — write

Write to the serial console (UART).

```text
x8 = 1
x0 = buf_ptr      (user VA of buffer)
x1 = len          (byte count, max 65536)
Returns: x0 = bytes written
Errors: BadAddress, BadLength
```

Validates the buffer address range. Output appears on the UART (serial console). Primarily for debugging and diagnostics.
