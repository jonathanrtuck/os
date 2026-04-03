//! Public syscall wrapper functions.

use crate::{
    asm::{nr, result, syscall0, syscall1, syscall2, syscall3, syscall4},
    types::{
        ChannelHandle, InterruptHandle, ProcessHandle, SchedHandle, SyscallResult, ThreadHandle,
        TimerHandle, VmoHandle,
    },
};

/// VMO flags for contiguous allocation.
const VMO_FLAG_CONTIGUOUS: u64 = 1 << 0;
/// VMO map flags.
const VMO_MAP_READ: u64 = 1 << 0;
const VMO_MAP_WRITE: u64 = 1 << 1;
/// VMO op_range operations.
const VMO_OP_LOOKUP: u64 = 2;

/// Tracks a DMA allocation: VA → (VMO handle, PA, page count).
struct DmaEntry {
    va: u64,
    pa: u64,
    handle: u16,
    pages: u64,
}

/// Static DMA tracking table. 64 entries is far more than any process needs.
static mut DMA_TABLE: [Option<DmaEntry>; 64] = {
    // SAFETY: array init — no references, no aliasing.
    const NONE: Option<DmaEntry> = None;

    [NONE; 64]
};

/// Close a handle by raw u16 value.
fn handle_close_raw(handle: u16) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}

/// Create a channel with two endpoints.
///
/// Returns `(handle_a, handle_b)` — both handles refer to endpoints of the
/// same shared-memory channel. The shared page is automatically mapped into
/// the calling process.
pub fn channel_create() -> SyscallResult<(ChannelHandle, ChannelHandle)> {
    let raw = unsafe { syscall0(nr::CHANNEL_CREATE) as i64 };
    let val = result(raw)?;

    Ok((ChannelHandle(val as u16), ChannelHandle((val >> 16) as u16)))
}
/// Signal the peer on a channel (write direction).
pub fn channel_signal(handle: ChannelHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::CHANNEL_SIGNAL, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Map a device's MMIO region into this process's address space.
///
/// The kernel maps `size` bytes starting at physical address `pa` with device
/// memory attributes (non-cacheable). Returns the user VA on success.
pub fn device_map(pa: u64, size: u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall2(nr::DEVICE_MAP, pa, size) as i64 };

    result(raw).map(|v| v as usize)
}
/// Allocate a DMA-capable buffer (2^order contiguous physical pages).
///
/// Returns the user VA of the mapped buffer. The physical address is written
/// to `pa_out` for programming device DMA registers.
///
/// Implemented via VMOs: creates a contiguous VMO, maps it, and queries
/// the base PA via VMO_OP_LOOKUP.
pub fn dma_alloc(order: u32, pa_out: &mut u64) -> SyscallResult<usize> {
    let pages = 1u64 << order;
    // 1. Create contiguous VMO.
    let handle = vmo_create(pages, VMO_FLAG_CONTIGUOUS, 0)?;
    // 2. Map into self (target=0).
    let va = vmo_map(handle, VMO_MAP_READ | VMO_MAP_WRITE, 0)?;
    // 3. Get base PA (for device DMA register programming).
    let pa = vmo_op_range(handle, VMO_OP_LOOKUP, 0, 0)?;

    *pa_out = pa;

    // 4. Store in tracking table for dma_free and memory_share.
    // SAFETY: single-threaded userspace; no concurrent access to DMA_TABLE.
    unsafe {
        for slot in DMA_TABLE.iter_mut() {
            if slot.is_none() {
                *slot = Some(DmaEntry {
                    va,
                    pa,
                    handle: handle.0,
                    pages,
                });

                return Ok(va as usize);
            }
        }
    }

    // Table full — should never happen in practice.
    Err(crate::SyscallError::OutOfMemory)
}
/// Free a DMA buffer previously allocated with `dma_alloc`.
pub fn dma_free(va: u64, _order: u32) -> SyscallResult<()> {
    // Look up the entry by VA.
    // SAFETY: single-threaded userspace; no concurrent access to DMA_TABLE.
    let entry = unsafe {
        DMA_TABLE
            .iter_mut()
            .find_map(|slot| {
                if let Some(e) = slot {
                    if e.va == va {
                        return slot.take();
                    }
                }

                None
            })
            .ok_or(crate::SyscallError::InvalidArgument)?
    };

    // Unmap and close the VMO handle.
    let _ = vmo_unmap(va, entry.pages);

    handle_close_raw(entry.handle)
}
/// Terminate the calling process. Does not return.
pub fn exit() -> ! {
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") nr::EXIT,
            options(noreturn, nostack),
        );
    }
}
/// Wait on a futex. Blocks if the 32-bit value at `addr` equals `expected`.
///
/// Returns `Ok(())` on success (was woken). Returns `Err(WouldBlock)` if the
/// value at `addr` != `expected` (no block occurred).
pub fn futex_wait(addr: *const u32, expected: u32) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::FUTEX_WAIT, addr as u64, expected as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Wake up to `count` threads waiting on a futex at `addr`.
///
/// Returns the number of threads actually woken.
pub fn futex_wake(addr: *const u32, count: u32) -> SyscallResult<u32> {
    let raw = unsafe { syscall2(nr::FUTEX_WAKE, addr as u64, count as u64) as i64 };

    result(raw).map(|v| v as u32)
}
/// Close a handle, releasing the associated kernel resource.
pub fn handle_close(handle: u16) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Read the badge on a handle.
pub fn handle_get_badge(handle: u16) -> SyscallResult<u64> {
    let raw = unsafe { syscall1(nr::HANDLE_GET_BADGE, handle as u64) as i64 };

    result(raw)
}
/// Send a handle to a suspended child process, optionally attenuating rights.
///
/// Moves the handle at `source_handle` (in the caller's table) into the
/// target process identified by `target_handle` (which must be a Process
/// handle). The target must not have been started yet. For channel handles,
/// the shared page is also mapped into the target's address space.
///
/// `rights_mask` controls which rights the target receives (bitwise AND with
/// the source handle's rights). Pass 0 to preserve all rights from the source.
pub fn handle_send(
    target_handle: ProcessHandle,
    source_handle: u16,
    rights_mask: u32,
) -> SyscallResult<()> {
    let raw = unsafe {
        syscall3(
            nr::HANDLE_SEND,
            target_handle.0 as u64,
            source_handle as u64,
            rights_mask as u64,
        ) as i64
    };

    result(raw)?;

    Ok(())
}
/// Set the badge on a handle. The badge is an opaque u64 value that
/// travels with the handle through `handle_send`. Services use badges
/// to identify which client a handle was sent to.
pub fn handle_set_badge(handle: u16, badge: u64) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::HANDLE_SET_BADGE, handle as u64, badge) as i64 };

    result(raw)?;

    Ok(())
}
/// Acknowledge an interrupt, allowing the device to fire again.
///
/// Clears the pending flag and re-enables the IRQ in the GIC. Must be called
/// after processing each interrupt.
pub fn interrupt_ack(handle: InterruptHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::INTERRUPT_ACK, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Register for a hardware interrupt. Returns a waitable handle.
///
/// The handle becomes ready when the IRQ fires. Use `wait` to block until
/// the interrupt occurs, then call `interrupt_ack` after processing.
pub fn interrupt_register(irq: u32) -> SyscallResult<InterruptHandle> {
    let raw = unsafe { syscall1(nr::INTERRUPT_REGISTER, irq as u64) as i64 };

    result(raw).map(|v| InterruptHandle(v as u16))
}
/// Allocate anonymous heap memory (demand-paged, zero-filled on first touch).
///
/// Returns the user VA of the start of the allocated region. The region is
/// `page_count * PAGE_SIZE` bytes. Pages are not physically allocated until touched.
pub fn memory_alloc(page_count: u64) -> SyscallResult<usize> {
    let raw = unsafe { syscall1(nr::MEMORY_ALLOC, page_count) as i64 };

    result(raw).map(|v| v as usize)
}
/// Free a heap allocation previously obtained from `memory_alloc`.
///
/// `va` must be the value returned by `memory_alloc`. Frees all physical
/// pages that were demand-paged in the region and reclaims the virtual
/// address range.
pub fn memory_free(va: usize, page_count: u64) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::MEMORY_FREE, va as u64, page_count) as i64 };

    result(raw)?;

    Ok(())
}
/// Map physical pages into a target process's shared memory region.
///
/// Implemented via VMOs: looks up the VMO handle that backs the given PA
/// (from a previous `dma_alloc`), then maps the VMO into the target process
/// via cross-process `vmo_map`.
///
/// When `read_only` is true, pages are mapped without write permission.
pub fn memory_share(
    target_handle: ProcessHandle,
    pa: u64,
    _page_count: u64,
    read_only: bool,
) -> SyscallResult<usize> {
    // Look up the VMO handle by PA from the DMA tracking table.
    // SAFETY: single-threaded userspace; no concurrent access to DMA_TABLE.
    let vmo_handle = unsafe {
        DMA_TABLE
            .iter()
            .find_map(|slot| {
                if let Some(e) = slot {
                    if e.pa == pa {
                        return Some(crate::types::VmoHandle(e.handle));
                    }
                }
                None
            })
            .ok_or(crate::SyscallError::InvalidArgument)?
    };
    let flags = if read_only {
        VMO_MAP_READ
    } else {
        VMO_MAP_READ | VMO_MAP_WRITE
    };
    // Cross-process vmo_map: target_handle identifies the target process.
    let va = vmo_map(vmo_handle, flags, target_handle.0 as u64)?;

    Ok(va as usize)
}
/// Create a process from an ELF binary in memory. Returns a waitable handle.
///
/// The child process starts suspended — call `process_start` with the returned
/// handle to make its thread runnable. The handle becomes ready when the child's
/// last thread exits.
pub fn process_create(elf_ptr: *const u8, elf_len: usize) -> SyscallResult<ProcessHandle> {
    let raw = unsafe { syscall2(nr::PROCESS_CREATE, elf_ptr as u64, elf_len as u64) as i64 };

    result(raw).map(|v| ProcessHandle(v as u16))
}
/// Kill a process, terminating all its threads.
///
/// The handle must be a Process handle with write rights. All threads in the
/// target process are terminated and full cleanup runs.
pub fn process_kill(handle: ProcessHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::PROCESS_KILL, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Set the syscall filter mask for a suspended child process.
///
/// Bit N set in `mask` allows syscall number N. Bit N clear blocks it
/// (returns `SyscallBlocked`). EXIT (nr 0) is always allowed regardless
/// of the mask. Must be called before `process_start`.
pub fn process_set_syscall_filter(handle: ProcessHandle, mask: u64) -> SyscallResult<()> {
    let r = unsafe { syscall2(nr::PROCESS_SET_SYSCALL_FILTER, handle.0 as u64, mask) };

    result(r as i64).map(|_| ())
}
/// Start a suspended child process.
///
/// Makes all suspended threads in the process identified by `handle` runnable.
pub fn process_start(handle: ProcessHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::PROCESS_START, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Bind a scheduling context to the calling thread.
///
/// The thread must not already have a context bound.
pub fn scheduling_context_bind(handle: SchedHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::SCHEDULING_CONTEXT_BIND, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Borrow another scheduling context (context donation).
///
/// Saves the current context and switches to the one identified by `handle`.
pub fn scheduling_context_borrow(handle: SchedHandle) -> SyscallResult<()> {
    let raw = unsafe { syscall1(nr::SCHEDULING_CONTEXT_BORROW, handle.0 as u64) as i64 };

    result(raw)?;

    Ok(())
}
/// Create a scheduling context with the given budget and period (both in ns).
///
/// Returns the handle index.
pub fn scheduling_context_create(budget: u64, period: u64) -> SyscallResult<SchedHandle> {
    let raw = unsafe { syscall2(nr::SCHEDULING_CONTEXT_CREATE, budget, period) as i64 };

    result(raw).map(|v| SchedHandle(v as u16))
}
/// Return a borrowed scheduling context, restoring the saved one.
pub fn scheduling_context_return() -> SyscallResult<()> {
    let raw = unsafe { syscall0(nr::SCHEDULING_CONTEXT_RETURN) as i64 };

    result(raw)?;

    Ok(())
}
/// Create a new thread in the calling process.
///
/// The thread starts at `entry_va` with user stack pointer `stack_top`.
/// Returns a waitable handle (becomes ready on thread exit).
pub fn thread_create(entry_va: u64, stack_top: u64) -> SyscallResult<ThreadHandle> {
    let raw = unsafe { syscall2(nr::THREAD_CREATE, entry_va, stack_top) as i64 };

    result(raw).map(|v| ThreadHandle(v as u16))
}
/// Create a one-shot timer that fires after `timeout_ns` nanoseconds.
///
/// Returns a waitable handle. Wait on it via `wait` to block until the deadline.
pub fn timer_create(timeout_ns: u64) -> SyscallResult<TimerHandle> {
    let raw = unsafe { syscall1(nr::TIMER_CREATE, timeout_ns) as i64 };

    result(raw).map(|v| TimerHandle(v as u16))
}
/// Create a Virtual Memory Object. Returns a VMO handle.
///
/// `flags`: 0 for lazy (demand-paged), `VMO_FLAG_CONTIGUOUS` (1) for
/// physically contiguous. `type_tag`: opaque u64 for IPC type safety.
pub fn vmo_create(size_pages: u64, flags: u64, type_tag: u64) -> SyscallResult<VmoHandle> {
    let raw = unsafe { syscall3(nr::VMO_CREATE, size_pages, flags, type_tag) as i64 };

    result(raw).map(|v| VmoHandle(v as u16))
}
/// Map a VMO into an address space. Returns user VA.
///
/// `flags`: `VMO_MAP_READ` (1), `VMO_MAP_WRITE` (2), or both.
/// `target`: 0 = map into self, otherwise a Process handle for cross-process.
pub fn vmo_map(handle: VmoHandle, flags: u64, target: u64) -> SyscallResult<u64> {
    let raw = unsafe { syscall3(nr::VMO_MAP, handle.0 as u64, flags, target) as i64 };

    result(raw)
}
/// Perform an operation on a VMO page range.
///
/// Operations: COMMIT (0), DECOMMIT (1), LOOKUP (2).
/// LOOKUP returns the base PA of a contiguous VMO (offset/count ignored).
pub fn vmo_op_range(handle: VmoHandle, op: u64, offset: u64, count: u64) -> SyscallResult<u64> {
    let raw = unsafe { syscall4(nr::VMO_OP_RANGE, handle.0 as u64, op, offset, count) as i64 };

    result(raw)
}
/// Unmap a VMO region.
pub fn vmo_unmap(va: u64, size_pages: u64) -> SyscallResult<()> {
    let raw = unsafe { syscall2(nr::VMO_UNMAP, va, size_pages) as i64 };

    result(raw)?;

    Ok(())
}
/// Wait for an event on one or more handles.
///
/// Blocks until any handle in `handles` has a pending event or the timeout
/// expires. Returns the index of the first ready handle (0-based).
/// Timeout of `u64::MAX` waits forever; `0` polls without blocking.
pub fn wait(handles: &[u16], timeout_ns: u64) -> SyscallResult<usize> {
    let raw = unsafe {
        syscall3(
            nr::WAIT,
            handles.as_ptr() as u64,
            handles.len() as u64,
            timeout_ns,
        ) as i64
    };

    result(raw).map(|v| v as usize)
}
/// Yield the current timeslice to the scheduler.
pub fn yield_now() {
    unsafe {
        syscall0(nr::YIELD);
    }
}
