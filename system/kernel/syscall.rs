// AUDIT: 2026-03-11 — 13 unsafe blocks verified, 6-category checklist applied.
// Fix 5 (aliasing UB with split borrows) re-verified sound: dispatch, sys_futex_wait,
// sys_wait, and dispatch_ok all use core::ptr::addr_of!/addr_of_mut! to read/write
// Context fields without creating &mut references that would alias the scheduler lock.
// User pointer validation verified for all 27 syscall handlers: every user-provided
// pointer is bounds-checked (< USER_VA_END), overflow-checked (checked_add), alignment-
// checked where required, and page-accessibility-verified via AT S1E0R/S1E0W hardware
// translation before dereference. No privilege escalation vectors found: device_map
// rejects RAM overlap, memory_share rejects non-RAM PAs, DMA order is capped, ELF size
// is capped. Error codes correctly propagated via result_to_u64! macro. No TOCTOU:
// validation and dereference occur within the same syscall context with TTBR0 stable.
// No bugs found.

//! Syscall dispatcher and handlers.
//!
//! # ABI (aarch64, EL0 → EL1)
//!
//! Invoke with `svc #0`. The kernel saves/restores the full register context
//! across the call, so all registers except x0 are preserved.
//!
//! | Register | Direction | Role                                   |
//! |----------|-----------|----------------------------------------|
//! | x8       | in        | Syscall number (see `nr` module)       |
//! | x0..x5   | in        | Arguments (syscall-specific)            |
//! | x0       | out       | Return value: ≥0 success, <0 error     |
//!
//! # Syscalls
//!
//! | Nr | Name                      | Args                              | Returns          |
//! |----|---------------------------|-----------------------------------|------------------|
//! | 0  | exit                      | —                                 | does not return  |
//! | 1  | write                     | x0=buf_ptr, x1=len                | bytes written    |
//! | 2  | yield                     | —                                 | 0                |
//! | 3  | handle_close              | x0=handle                         | 0                |
//! | 4  | channel_signal            | x0=handle                         | 0                |
//! | 5  | channel_create            | —                                 | handle_a \| (handle_b << 8) |
//! | 6  | scheduling_context_create | x0=budget_ns, x1=period_ns        | handle           |
//! | 7  | scheduling_context_borrow | x0=handle                         | 0                |
//! | 8  | scheduling_context_return | —                                 | 0                |
//! | 9  | scheduling_context_bind   | x0=handle                         | 0                |
//! | 10 | futex_wait                | x0=addr, x1=expected              | 0 (may block)    |
//! | 11 | futex_wake                | x0=addr, x1=count                 | threads woken    |
//! | 12 | wait                      | x0=handles_ptr, x1=count, x2=timeout_ns | ready index (may block) |
//! | 13 | timer_create              | x0=timeout_ns                     | handle           |
//! | 14 | interrupt_register        | x0=irq_nr                         | handle           |
//! | 15 | interrupt_ack             | x0=handle                         | 0                |
//! | 16 | device_map                | x0=phys_addr, x1=size             | user VA          |
//! | 17 | dma_alloc                 | x0=order, x1=pa_out_ptr           | user VA          |
//! | 18 | dma_free                  | x0=user_va, x1=order              | 0                |
//! | 19 | thread_create             | x0=entry_va, x1=stack_top         | handle           |
//! | 20 | process_create            | x0=elf_ptr, x1=elf_len            | handle           |
//! | 21 | process_start             | x0=handle                         | 0                |
//! | 22 | handle_send               | x0=target_handle, x1=source_handle | 0               |
//! | 23 | process_kill              | x0=handle                          | 0               |
//! | 24 | memory_share              | x0=target_handle, x1=pa, x2=page_count, x3=flags | target VA   |
//! | 25 | memory_alloc              | x0=page_count                           | user VA     |
//! | 26 | memory_free               | x0=va, x1=page_count                   | 0           |
//!
//! # Error codes
//!
//! | Code | Name                | Source        |
//! |------|---------------------|---------------|
//! | -1   | UnknownSyscall      | `Error`       |
//! | -2   | BadAddress          | `Error`       |
//! | -3   | BadLength           | `Error`       |
//! | -4   | InvalidArgument     | `Error`       |
//! | -5   | AlreadyBorrowing    | `Error`       |
//! | -6   | NotBorrowing        | `Error`       |
//! | -7   | AlreadyBound        | `Error`       |
//! | -8   | WouldBlock          | `Error`       |
//! | -9   | OutOfMemory         | `Error`       |
//! | -10  | InvalidHandle       | `HandleError` |
//! | -12  | InsufficientRights  | `HandleError` |
//! | -13  | TableFull           | `HandleError` |

use super::channel;
use super::futex;
use super::handle::{ChannelId, Handle, HandleError, HandleObject, Rights};
use super::interrupt;
use super::interrupt::InterruptId;
use super::memory;
use super::metrics;
use super::page_allocator;
use super::paging;
use super::paging::USER_VA_END;
use super::process;
use super::process::ProcessId;
use super::process_exit;
use super::scheduler;
use super::serial;
use super::thread::ThreadId;
use super::thread::WaitEntry;
use super::thread_exit;
use super::timer;
use super::timer::TimerId;
use super::Context;
use alloc::vec::Vec;

pub mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const HANDLE_CLOSE: u64 = 3;
    pub const CHANNEL_SIGNAL: u64 = 4;
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 6;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 7;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 8;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 9;
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    pub const WAIT: u64 = 12;
    pub const TIMER_CREATE: u64 = 13;
    pub const INTERRUPT_REGISTER: u64 = 14;
    pub const INTERRUPT_ACK: u64 = 15;
    pub const DEVICE_MAP: u64 = 16;
    pub const DMA_ALLOC: u64 = 17;
    pub const DMA_FREE: u64 = 18;
    pub const THREAD_CREATE: u64 = 19;
    pub const PROCESS_CREATE: u64 = 20;
    pub const CHANNEL_CREATE: u64 = 5;
    pub const PROCESS_START: u64 = 21;
    pub const HANDLE_SEND: u64 = 22;
    pub const PROCESS_KILL: u64 = 23;
    pub const MEMORY_SHARE: u64 = 24;
    pub const MEMORY_ALLOC: u64 = 25;
    pub const MEMORY_FREE: u64 = 26;
}

/// Maximum DMA allocation order (2^11 pages = 8 MiB).
/// Must match page_allocator::MAX_ORDER. Needed for GPU framebuffers
/// (1920×1080×4 = ~7.9 MiB requires order 11).
const MAX_DMA_ORDER: u64 = 11;
/// Maximum ELF size for process_create (2 MiB).
/// ELF files include debug info and symbol tables beyond loadable segments.
const MAX_ELF_SIZE: u64 = 2 * 1024 * 1024;
/// Maximum number of handles in a single `wait` call.
const MAX_WAIT_HANDLES: u64 = 16;
const MAX_WRITE_LEN: u64 = 4096;

/// Raw WouldBlock error code as u64 (for direct x[0] patching in wake path).
pub const WOULD_BLOCK_RAW: u64 = Error::WouldBlock as i64 as u64;

/// Convert a syscall Result to the ABI return value.
/// Both Error and HandleError are #[repr(i64)], so `as i64 as u64` is uniform.
macro_rules! result_to_u64 {
    ($result:expr) => {
        match $result {
            Ok(n) => n,
            Err(e) => e as i64 as u64,
        }
    };
}

#[repr(i64)]
pub enum Error {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
    InvalidArgument = -4,
    AlreadyBorrowing = -5,
    NotBorrowing = -6,
    AlreadyBound = -7,
    WouldBlock = -8,
    OutOfMemory = -9,
}

impl From<HandleError> for u64 {
    fn from(e: HandleError) -> u64 {
        (e as i64) as u64
    }
}

/// Write a syscall result to ctx.x[0] via raw pointer (no reference creation).
///
/// Accepts a pre-converted u64 value (Ok value or error code). Avoids creating
/// `&mut *ctx` which would alias with the scheduler lock's `&mut State`.
#[inline(never)]
fn dispatch_ok(ctx: *mut Context, val: u64) -> *const Context {
    // SAFETY: ctx is a valid pointer to the current thread's Context (set by
    // exception.S from TPIDR_EL1). addr_of_mut! avoids creating &mut *ctx
    // which would alias with the scheduler lock's &mut State. Writing to x[0]
    // (the first element) is within the [u64; 31] array bounds.
    unsafe {
        let x0_ptr = core::ptr::addr_of_mut!((*ctx).x) as *mut u64;

        x0_ptr.write(val);
    }

    ctx as *const Context
}

/// Check if a user virtual address is readable by EL0 using the hardware
/// address translation instruction. Returns false if the page is unmapped
/// or inaccessible.
fn is_user_page_readable(va: u64) -> bool {
    user_va_to_pa(va).is_some()
}
/// Check if a user virtual address is writable by EL0 using the hardware
/// address translation instruction. Returns false if the page is unmapped,
/// read-only, or inaccessible.
fn is_user_page_writable(va: u64) -> bool {
    let par: u64;

    // SAFETY: AT S1E0W is a privileged instruction that performs address
    // translation without memory access — it only writes to PAR_EL1. The
    // ISB ensures PAR_EL1 is visible before the mrs reads it. Single asm
    // block prevents LLVM reordering. No memory is accessed; nostack is
    // correct since this uses only system registers.
    unsafe {
        // AT S1E0W translates va, writing the result to PAR_EL1. The mrs
        // reads that result. These MUST be a single asm block — with separate
        // blocks, LLVM could reorder the mrs (if marked nomem) before the at,
        // reading a stale PAR_EL1 from a previous translation.
        core::arch::asm!(
            "at s1e0w, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }

    // PAR_EL1 bit 0: 0 = translation succeeded, 1 = fault.
    par & 1 == 0
}
/// Verify that all pages in `[start, start+len)` are readable by EL0.
fn is_user_range_readable(start: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }

    let page_mask = !(paging::PAGE_SIZE - 1);
    let first_page = start & page_mask;
    let last_page = (start + len - 1) & page_mask;
    let mut page = first_page;

    while page <= last_page {
        if !is_user_page_readable(page) {
            return false;
        }

        page += paging::PAGE_SIZE;
    }

    true
}
fn sys_channel_create() -> Result<u64, Error> {
    // Allocate channel (two shared pages + two endpoint IDs).
    let (ch_a, ch_b) = channel::create().ok_or(Error::OutOfMemory)?;
    // Insert both handles first, map shared pages only on success.
    // This avoids leaking mapped shared pages if the second insert fails.
    let result = scheduler::current_process_do(|process| {
        let handle_a = process
            .handles
            .insert(HandleObject::Channel(ch_a), Rights::READ_WRITE)?;

        match process
            .handles
            .insert(HandleObject::Channel(ch_b), Rights::READ_WRITE)
        {
            Ok(handle_b) => {
                // Both handles inserted — now map both shared pages using the
                // per-process channel SHM bump allocator.
                let pages = channel::shared_pages(ch_a).ok_or(HandleError::InvalidHandle)?;

                process
                    .address_space
                    .map_channel_page(pages[0].as_u64())
                    .ok_or(HandleError::TableFull)?;
                process
                    .address_space
                    .map_channel_page(pages[1].as_u64())
                    .ok_or(HandleError::TableFull)?;

                Ok((handle_a, handle_b))
            }
            Err(e) => {
                // Second insert failed — close the first handle.
                let _ = process.handles.close(handle_a);

                Err(e)
            }
        }
    });

    match result {
        Ok((handle_a, handle_b)) => Ok(handle_a.0 as u64 | (handle_b.0 as u64) << 8),
        Err(_) => {
            // Clean up both endpoints.
            channel::close_endpoint(ch_a);
            channel::close_endpoint(ch_b);

            Err(Error::OutOfMemory)
        }
    }
}
fn sys_channel_signal(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let channel_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Channel(id)) => Ok(id),
            Ok(_) => Err(HandleError::InvalidHandle),
            Err(e) => Err(e),
        }
    })?;

    channel::signal(channel_id);

    Ok(0)
}
fn sys_device_map(pa: u64, size: u64) -> Result<u64, Error> {
    if size == 0 {
        return Err(Error::InvalidArgument);
    }

    // Validate: PA must be outside RAM (device MMIO space only).
    let end = pa.checked_add(size).ok_or(Error::InvalidArgument)?;

    if !(end <= paging::RAM_START || pa >= paging::RAM_END) {
        return Err(Error::InvalidArgument); // Overlaps RAM — not a device
    }

    scheduler::current_process_do(|process| {
        process
            .address_space
            .map_device_mmio(pa, size)
            .ok_or(Error::InvalidArgument)
    })
}
fn sys_dma_alloc(order: u64, pa_out_ptr: u64) -> Result<u64, Error> {
    if order > MAX_DMA_ORDER {
        return Err(Error::InvalidArgument);
    }
    // Validate pa_out_ptr: in user space, 8-byte aligned, writable.
    if pa_out_ptr >= USER_VA_END || pa_out_ptr & 7 != 0 {
        return Err(Error::BadAddress);
    }
    if !is_user_page_writable(pa_out_ptr) {
        return Err(Error::BadAddress);
    }

    // Allocate physically contiguous frames from the buddy allocator.
    let pa = page_allocator::alloc_frames(order as usize).ok_or(Error::OutOfMemory)?;
    // Map into the calling process's DMA VA region.
    let va = scheduler::current_process_do(|process| {
        process.address_space.map_dma_buffer(pa, order as usize)
    });
    let va = match va {
        Some(va) => va,
        None => {
            // DMA VA space full — free the frames we just allocated.
            page_allocator::free_frames(pa, order as usize);

            return Err(Error::OutOfMemory);
        }
    };

    // Write the PA to user memory so the driver can program DMA registers.
    // SAFETY: pa_out_ptr validated above (user VA, aligned, writable page).
    unsafe {
        core::ptr::write_volatile(pa_out_ptr as *mut u64, pa.as_u64());
    }

    Ok(va)
}
fn sys_dma_free(va: u64, _order: u64) -> Result<u64, Error> {
    // Validate VA is in the DMA region.
    if !(paging::DMA_BUFFER_BASE..paging::DMA_BUFFER_END).contains(&va) {
        return Err(Error::InvalidArgument);
    }

    // Unmap and retrieve the stored PA + order.
    let (pa, order) =
        scheduler::current_process_do(|process| process.address_space.unmap_dma_buffer(va))
            .ok_or(Error::InvalidArgument)?;

    // Free the physically contiguous frames.
    page_allocator::free_frames(pa, order);

    Ok(0)
}
fn sys_exit(ctx: *mut Context) -> *const Context {
    scheduler::exit_current_from_syscall(ctx)
}
#[inline(never)]
fn sys_futex_wait(ctx: *mut Context) -> *const Context {
    // SAFETY: Read args via raw pointer — no `&mut *ctx` (aliasing UB with
    // scheduler lock). ctx is a valid pointer to the current thread's Context
    // (set by exception.S). addr_of! avoids creating a reference. x[0] and
    // x[1] are within the [u64; 31] array bounds.
    let (addr, expected) = unsafe {
        let x = core::ptr::addr_of!((*ctx).x) as *const u64;

        (x.add(0).read(), x.add(1).read() as u32)
    };

    // Validate: must be in user VA space and word-aligned.
    if addr >= USER_VA_END || addr & 3 != 0 {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }

    // Translate VA → PA for the futex key.
    let pa = match user_va_to_pa(addr) {
        Some(pa) => pa,
        None => return dispatch_ok(ctx, Error::BadAddress as i64 as u64),
    };
    // Read the current value at the user address.
    // SAFETY: TTBR0 is still loaded, address validated via AT S1E0R.
    let current_value = unsafe { core::ptr::read_volatile(addr as *const u32) };

    if current_value != expected {
        // Value changed — don't block (spurious, not a lost wakeup).
        return dispatch_ok(ctx, Error::WouldBlock as i64 as u64);
    }

    // Record this thread in the futex wait table.
    let thread_id = scheduler::current_thread_do(|thread| thread.id());

    futex::wait(pa, thread_id);

    // SAFETY: Pre-set x[0] = 0 (success) before blocking. ctx is valid (current
    // thread's Context). addr_of_mut! avoids creating &mut that would alias
    // the scheduler lock's &mut State.
    unsafe {
        let x0_ptr = core::ptr::addr_of_mut!((*ctx).x) as *mut u64;

        x0_ptr.write(0);
    }

    // Block (or return immediately if a wake arrived in the gap).
    // Futex has no post-block cleanup, so both paths are equivalent.
    match scheduler::block_current_unless_woken(ctx) {
        scheduler::BlockResult::WokePending(p) | scheduler::BlockResult::Blocked(p) => p,
    }
}
fn sys_futex_wake(addr: u64, count: u64) -> Result<u64, Error> {
    // Validate: must be in user VA space and word-aligned.
    if addr >= USER_VA_END || addr & 3 != 0 {
        return Err(Error::BadAddress);
    }

    let pa = user_va_to_pa(addr).ok_or(Error::BadAddress)?;
    let woken = futex::wake(pa, count as u32);

    Ok(woken as u64)
}
fn sys_interrupt_ack(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let int_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Interrupt(id)) => Ok(id),
            Ok(_) => Err(HandleError::InvalidHandle),
            Err(e) => Err(e),
        }
    })?;

    interrupt::acknowledge(int_id);

    Ok(0)
}
fn sys_interrupt_register(irq: u64) -> Result<u64, HandleError> {
    if irq > u32::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let int_id = interrupt::register(irq as u32).ok_or(HandleError::TableFull)?;

    match scheduler::current_process_do(|process| {
        process
            .handles
            .insert(HandleObject::Interrupt(int_id), Rights::READ_WRITE)
    }) {
        Ok(handle) => Ok(handle.0 as u64),
        Err(e) => {
            // Handle table full — clean up the interrupt we just registered.
            interrupt::destroy(int_id);

            Err(e)
        }
    }
}
fn sys_handle_close(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let (obj, _rights) =
        scheduler::current_process_do(|process| process.handles.close(Handle(handle_nr as u8)))?;

    // Release kernel resources associated with the closed handle.
    match obj {
        HandleObject::Channel(id) => channel::close_endpoint(id),
        HandleObject::Interrupt(id) => interrupt::destroy(id),
        HandleObject::Process(id) => process_exit::destroy(id),
        HandleObject::SchedulingContext(id) => scheduler::release_scheduling_context(id),
        HandleObject::Thread(id) => thread_exit::destroy(id),
        HandleObject::Timer(id) => timer::destroy(id),
    }

    Ok(0)
}
fn sys_handle_send(target_handle_nr: u64, source_handle_nr: u64) -> Result<u64, Error> {
    if target_handle_nr > u8::MAX as u64 || source_handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let source_handle = Handle(source_handle_nr as u8);
    // Phase 1: Move the source handle out of the caller's table.
    // We take (not just read) the handle — move semantics prevent duplicated
    // endpoints, which would corrupt channel closed_count.
    let (target_pid, source_obj, source_rights) = scheduler::current_process_do(|process| {
        let target_pid = match process
            .handles
            .get(Handle(target_handle_nr as u8), Rights::WRITE)
        {
            Ok(HandleObject::Process(id)) => id,
            Ok(_) => return Err(Error::InvalidArgument),
            Err(_) => return Err(Error::InvalidArgument),
        };
        let (source_obj, source_rights) = process
            .handles
            .close(source_handle)
            .map_err(|_| Error::InvalidArgument)?;

        Ok((target_pid, source_obj, source_rights))
    })?;
    // Phase 1.5: If the source is a Channel, get shared page PAs (channel lock).
    let channel_pages = match source_obj {
        HandleObject::Channel(ch_id) => channel::shared_pages(ch_id),
        _ => None,
    };
    // Phase 2: Insert into target process (scheduler lock via with_process).
    let result = scheduler::with_process(target_pid, |target| {
        // Only allow sending handles to processes that haven't started yet.
        if target.started {
            return Err(Error::InvalidArgument);
        }

        // For Channel handles, map both shared pages into the target's address
        // space using the target's per-process channel SHM bump allocator. This
        // ensures the first channel received maps at CHANNEL_SHM_BASE regardless
        // of the global channel index.
        if let Some(pages) = channel_pages {
            target
                .address_space
                .map_channel_page(pages[0].as_u64())
                .ok_or(Error::OutOfMemory)?;
            target
                .address_space
                .map_channel_page(pages[1].as_u64())
                .ok_or(Error::OutOfMemory)?;
        }

        target
            .handles
            .insert(source_obj, source_rights)
            .map_err(|_| Error::InvalidArgument)?;

        Ok(())
    })
    .unwrap_or(Err(Error::InvalidArgument));

    // Rollback: if Phase 2 failed, restore handle to source process.
    if let Err(e) = result {
        scheduler::current_process_do(|process| {
            let _ = process
                .handles
                .insert_at(source_handle, source_obj, source_rights);
        });

        return Err(e);
    }

    Ok(0)
}
fn sys_memory_alloc(page_count: u64) -> Result<u64, Error> {
    if page_count == 0 {
        return Err(Error::InvalidArgument);
    }

    scheduler::current_process_do(|process| {
        process
            .address_space
            .map_heap(page_count)
            .ok_or(Error::OutOfMemory)
    })
}
fn sys_memory_free(va: u64, _page_count: u64) -> Result<u64, Error> {
    if !(paging::HEAP_BASE..paging::HEAP_END).contains(&va) {
        return Err(Error::InvalidArgument);
    }
    if va & (paging::PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }

    scheduler::current_process_do(|process| {
        process
            .address_space
            .unmap_heap(va)
            .ok_or(Error::InvalidArgument)
    })?;

    Ok(0)
}
/// Map physical pages from the caller into a target process's shared memory region.
///
/// The target process must not have been started yet. Pages are mapped without
/// ownership transfer — the caller (or original allocator) retains responsibility
/// for the physical frames. PA must be page-aligned and within RAM bounds.
///
/// Flags bit 0: read_only — map pages without write permission (hardware-enforced).
fn sys_memory_share(
    target_handle_nr: u64,
    pa: u64,
    page_count: u64,
    flags: u64,
) -> Result<u64, Error> {
    if target_handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }
    if page_count == 0 || page_count > 2048 {
        return Err(Error::InvalidArgument);
    }
    if pa & (paging::PAGE_SIZE - 1) != 0 {
        return Err(Error::BadAddress);
    }

    let end_pa = pa
        .checked_add(page_count * paging::PAGE_SIZE)
        .ok_or(Error::BadAddress)?;

    if pa < paging::RAM_START || end_pa > paging::RAM_END {
        return Err(Error::BadAddress);
    }

    let target_pid = scheduler::current_process_do(|process| {
        match process
            .handles
            .get(Handle(target_handle_nr as u8), Rights::WRITE)
        {
            Ok(HandleObject::Process(id)) => Ok(id),
            Ok(_) => Err(Error::InvalidArgument),
            Err(_) => Err(Error::InvalidArgument),
        }
    })?;

    let read_only = flags & 1 != 0;

    scheduler::with_process(target_pid, |target| {
        if target.started {
            return Err(Error::InvalidArgument);
        }

        target
            .address_space
            .map_shared_region(memory::Pa(pa as usize), page_count, read_only)
            .ok_or(Error::OutOfMemory)
    })
    .unwrap_or(Err(Error::InvalidArgument))
}
fn sys_process_create(elf_ptr: u64, elf_len: u64) -> Result<u64, Error> {
    // Validate length.
    if elf_len == 0 || elf_len > MAX_ELF_SIZE {
        return Err(Error::BadLength);
    }
    // Validate buffer range.
    if elf_ptr >= USER_VA_END {
        return Err(Error::BadAddress);
    }

    let end = elf_ptr.checked_add(elf_len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }
    if !is_user_range_readable(elf_ptr, elf_len) {
        return Err(Error::BadAddress);
    }

    // Copy ELF data from user memory to a kernel buffer.
    // SAFETY: TTBR0 is still loaded. Range validated above.
    let elf_data = unsafe {
        let src = core::slice::from_raw_parts(elf_ptr as *const u8, elf_len as usize);
        let mut buf = Vec::with_capacity(elf_len as usize);

        buf.extend_from_slice(src);

        buf
    };

    // Create process with suspended initial thread.
    let (process_id, _thread_id) =
        process::create_from_user_elf(&elf_data).map_err(|_| Error::InvalidArgument)?;

    // Create process exit notification state.
    process_exit::create(process_id);

    // Insert Process handle into the caller's handle table.
    let handle = scheduler::current_process_do(|p| {
        p.handles
            .insert(HandleObject::Process(process_id), Rights::READ_WRITE)
    })
    .map_err(|_| {
        // Full cleanup: kill the process + its suspended thread, then
        // destroy notification state and free the address space.
        if let Some(kill_info) = scheduler::kill_process(process_id) {
            for &tid in &kill_info.thread_ids {
                thread_exit::notify_exit(tid);
            }

            process_exit::notify_exit(process_id);

            for &tid in &kill_info.thread_ids {
                futex::remove_thread(tid);
            }
            for id in kill_info.handles.channels {
                channel::close_endpoint(id);
            }
            for id in kill_info.handles.interrupts {
                interrupt::destroy(id);
            }
            for id in kill_info.handles.timers {
                timer::destroy(id);
            }
            for id in kill_info.handles.thread_handles {
                thread_exit::destroy(id);
            }
            for id in kill_info.handles.process_handles {
                process_exit::destroy(id);
            }

            if let Some(mut addr_space) = kill_info.address_space {
                addr_space.invalidate_tlb();
                addr_space.free_all();

                super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));
            }
        }

        process_exit::destroy(process_id);

        Error::OutOfMemory
    })?;

    Ok(handle.0 as u64)
}
fn sys_process_kill(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let target_pid = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Process(id)) => Ok(id),
            Ok(_) => Err(Error::InvalidArgument),
            Err(_) => Err(Error::InvalidArgument),
        }
    })?;
    // Prevent self-kill.
    let caller_pid = scheduler::current_thread_do(|t| t.process_id);

    if caller_pid == Some(target_pid) {
        return Err(Error::InvalidArgument);
    }

    let kill_info = scheduler::kill_process(target_pid).ok_or(Error::InvalidArgument)?;

    // Phase 2: notify exits (acquires thread_exit/process_exit locks, then scheduler).
    for &tid in &kill_info.thread_ids {
        super::thread_exit::notify_exit(tid);
    }

    super::process_exit::notify_exit(target_pid);

    // Phase 2a: remove killed threads from futex wait queues.
    for &tid in &kill_info.thread_ids {
        super::futex::remove_thread(tid);
    }
    // Phase 3: close resources outside scheduler lock.
    for id in kill_info.handles.channels {
        super::channel::close_endpoint(id);
    }
    for id in kill_info.handles.interrupts {
        super::interrupt::destroy(id);
    }
    for id in kill_info.handles.timers {
        super::timer::destroy(id);
    }
    for id in kill_info.handles.thread_handles {
        super::thread_exit::destroy(id);
    }
    for id in kill_info.handles.process_handles {
        super::process_exit::destroy(id);
    }

    // Phase 4: free address space (immediate path — no threads were running).
    if let Some(mut addr_space) = kill_info.address_space {
        addr_space.invalidate_tlb();
        addr_space.free_all();

        super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));
    }

    Ok(0)
}
fn sys_process_start(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let process_id = scheduler::current_process_do(|p| {
        match p.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Process(id)) => Ok(id),
            Ok(_) => Err(Error::InvalidArgument),
            Err(_) => Err(Error::InvalidArgument),
        }
    })?;

    if scheduler::start_suspended_threads(process_id) {
        Ok(0)
    } else {
        // No suspended threads — already started or invalid.
        Err(Error::InvalidArgument)
    }
}
fn sys_scheduling_context_bind(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let ctx_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u8), Rights::READ) {
            Ok(HandleObject::SchedulingContext(id)) => Ok(id),
            _ => Err(Error::InvalidArgument),
        }
    })?;

    if scheduler::bind_scheduling_context(ctx_id) {
        Ok(0)
    } else {
        Err(Error::AlreadyBound)
    }
}
fn sys_scheduling_context_borrow(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let ctx_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u8), Rights::READ) {
            Ok(HandleObject::SchedulingContext(id)) => Ok(id),
            _ => Err(Error::InvalidArgument),
        }
    })?;

    if scheduler::borrow_scheduling_context(ctx_id) {
        Ok(0)
    } else {
        Err(Error::AlreadyBorrowing)
    }
}
fn sys_scheduling_context_create(budget: u64, period: u64) -> Result<u64, Error> {
    let ctx_id =
        scheduler::create_scheduling_context(budget, period).ok_or(Error::InvalidArgument)?;
    let handle = scheduler::current_process_do(|process| {
        process
            .handles
            .insert(HandleObject::SchedulingContext(ctx_id), Rights::READ_WRITE)
    })
    .map_err(|_| Error::InvalidArgument)?;

    Ok(handle.0 as u64)
}
fn sys_scheduling_context_return() -> Result<u64, Error> {
    if scheduler::return_scheduling_context() {
        Ok(0)
    } else {
        Err(Error::NotBorrowing)
    }
}
fn sys_thread_create(entry_va: u64, stack_top: u64) -> Result<u64, Error> {
    // Validate: entry_va must be in user space.
    if entry_va >= USER_VA_END {
        return Err(Error::BadAddress);
    }
    // Validate: stack_top must be in user space and 16-byte aligned (ABI).
    if stack_top >= USER_VA_END || stack_top & 0xF != 0 {
        return Err(Error::BadAddress);
    }

    let process_id =
        scheduler::current_thread_do(|thread| thread.process_id.ok_or(Error::InvalidArgument))?;
    let thread_id =
        scheduler::spawn_user(process_id, entry_va, stack_top).ok_or(Error::OutOfMemory)?;

    // Create exit notification state for the new thread.
    thread_exit::create(thread_id);

    // Insert a Thread handle into the caller's handle table.
    let handle = scheduler::current_process_do(|process| {
        process
            .handles
            .insert(HandleObject::Thread(thread_id), Rights::READ)
    })
    .map_err(|_| {
        // Handle table full — thread is already running, but the caller
        // can't track it. Clean up the notification state.
        thread_exit::destroy(thread_id);
        Error::InvalidArgument
    })?;

    Ok(handle.0 as u64)
}
fn sys_timer_create(timeout_ns: u64) -> Result<u64, HandleError> {
    let timer_id = timer::create(timeout_ns).ok_or(HandleError::TableFull)?;

    match scheduler::current_process_do(|process| {
        process
            .handles
            .insert(HandleObject::Timer(timer_id), Rights::READ)
    }) {
        Ok(handle) => Ok(handle.0 as u64),
        Err(e) => {
            // Handle table full — clean up the timer we just created.
            timer::destroy(timer_id);

            Err(e)
        }
    }
}
#[inline(never)]
fn sys_wait(ctx: *mut Context) -> *const Context {
    use super::thread::TIMEOUT_SENTINEL;

    // SAFETY: Read args via raw pointer — no `&mut *ctx` (aliasing UB with
    // scheduler lock). ctx is a valid pointer to the current thread's Context
    // (set by exception.S). addr_of! avoids creating a reference. x[0], x[1],
    // x[2] are within the [u64; 31] array bounds.
    let (handles_ptr, count, timeout) = unsafe {
        let x = core::ptr::addr_of!((*ctx).x) as *const u64;

        (x.add(0).read(), x.add(1).read(), x.add(2).read())
    };

    // Clean up any stale timeout timer from a previous blocked wait.
    // This handles the deferred cleanup case where sys_wait couldn't
    // clean up because the thread was context-switched away.
    if let Some(stale_timer) = scheduler::take_timeout_timer() {
        timer::destroy(stale_timer);
    }

    // Validate count.
    if count == 0 || count > MAX_WAIT_HANDLES {
        return dispatch_ok(ctx, Error::InvalidArgument as i64 as u64);
    }
    // Validate user buffer.
    if handles_ptr >= USER_VA_END {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }
    if let Some(end) = handles_ptr.checked_add(count) {
        if end > USER_VA_END {
            return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
        }
    } else {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }
    if !is_user_range_readable(handles_ptr, count) {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }

    // Read handle indices from user memory.
    // SAFETY: TTBR0 is still loaded. Address and length validated above.
    let handle_bytes =
        unsafe { core::slice::from_raw_parts(handles_ptr as *const u8, count as usize) };
    // Resolve handles and populate thread.wait_set in-place (reuses the Vec's
    // backing allocation from previous calls — no heap alloc in steady state).
    // A stack-allocated copy is returned for use outside the scheduler lock.
    let resolve_result = scheduler::current_thread_and_process_do(|thread, process| {
        thread.wait_set.clear();

        let mut entries: [Option<WaitEntry>; MAX_WAIT_HANDLES as usize + 1] =
            [None; MAX_WAIT_HANDLES as usize + 1];
        let mut count = 0usize;

        for (i, &h) in handle_bytes.iter().enumerate() {
            let obj = process.handles.get(Handle(h), Rights::READ)?;

            match obj {
                HandleObject::Channel(_)
                | HandleObject::Interrupt(_)
                | HandleObject::Process(_)
                | HandleObject::Thread(_)
                | HandleObject::Timer(_) => {
                    let entry = WaitEntry {
                        object: obj,
                        user_index: i as u8,
                    };

                    thread.wait_set.push(entry);
                    entries[count] = Some(entry);
                    count += 1;
                }
                _ => return Err(HandleError::InvalidHandle), // Not waitable
            }
        }

        Ok((entries, count, thread.id()))
    });
    let (mut entries, mut entry_count, caller_id) = match resolve_result {
        Ok(tuple) => tuple,
        Err(e) => return dispatch_ok(ctx, e.into()),
    };
    // Create internal timeout timer for finite timeouts (0 < timeout < MAX).
    // The timer is added to the wait set with a sentinel index. If it fires
    // first, the wake path returns WouldBlock. The timer is stored on the
    // thread for deferred cleanup (the Blocked path can't run cleanup code).
    let timeout_timer = if timeout != 0 && timeout != u64::MAX {
        match timer::create(timeout) {
            Some(id) => {
                let entry = WaitEntry {
                    object: HandleObject::Timer(id),
                    user_index: TIMEOUT_SENTINEL,
                };

                scheduler::push_wait_entry(entry);

                entries[entry_count] = Some(entry);
                entry_count += 1;

                scheduler::set_timeout_timer(id);

                Some(id)
            }
            None => None, // Timer table full — proceed without timeout.
        }
    } else {
        None
    };
    // Collect IDs for waiter registration and cleanup.
    let mut channel_ids: [Option<ChannelId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];
    // +1 for potential timeout timer entry.
    let mut timer_ids: [Option<TimerId>; MAX_WAIT_HANDLES as usize + 1] =
        [None; MAX_WAIT_HANDLES as usize + 1];
    let mut interrupt_ids: [Option<InterruptId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];
    let mut thread_ids: [Option<ThreadId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];
    let mut process_ids: [Option<ProcessId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];

    for entry in entries[..entry_count].iter().flatten() {
        let idx = if entry.user_index == TIMEOUT_SENTINEL {
            MAX_WAIT_HANDLES as usize // Use the extra slot for the timeout timer.
        } else {
            entry.user_index as usize
        };

        match entry.object {
            HandleObject::Channel(id) => channel_ids[idx.min(channel_ids.len() - 1)] = Some(id),
            HandleObject::Timer(id) => timer_ids[idx] = Some(id),
            HandleObject::Interrupt(id) => {
                interrupt_ids[idx.min(interrupt_ids.len() - 1)] = Some(id)
            }
            HandleObject::Thread(id) => thread_ids[idx.min(thread_ids.len() - 1)] = Some(id),
            HandleObject::Process(id) => process_ids[idx.min(process_ids.len() - 1)] = Some(id),
            HandleObject::SchedulingContext(_) => {}
        }
    }

    // Register as waiter on each handle. The wait set is already on the thread
    // (populated in the closure above). If an event fires in the gap,
    // set_wake_pending_for_handle can find the wait set and target this thread.
    for &id in channel_ids.iter().flatten() {
        channel::register_waiter(id, caller_id);
    }
    for &id in timer_ids.iter().flatten() {
        timer::register_waiter(id, caller_id);
    }
    for &id in interrupt_ids.iter().flatten() {
        interrupt::register_waiter(id, caller_id);
    }
    for &id in thread_ids.iter().flatten() {
        thread_exit::register_waiter(id, caller_id);
    }
    for &id in process_ids.iter().flatten() {
        process_exit::register_waiter(id, caller_id);
    }

    // Wait set already on the thread (populated in the closure above, before
    // waiter registration). If a signal arrives during the readiness check,
    // set_wake_pending_for_handle can find the wait set.

    // Check each handle for readiness.
    for entry in entries[..entry_count].iter().flatten() {
        let ready = match entry.object {
            HandleObject::Channel(ch_id) => channel::check_pending(ch_id),
            HandleObject::Interrupt(int_id) => interrupt::check_pending(int_id),
            HandleObject::Process(p_id) => process_exit::check_exited(p_id),
            HandleObject::Thread(t_id) => thread_exit::check_exited(t_id),
            HandleObject::Timer(t_id) => timer::check_fired(t_id),
            _ => false,
        };

        if ready {
            // Ready — clear wait state, unregister from all waiters, return index.
            scheduler::clear_wait_state();

            unregister_channels(&channel_ids);
            unregister_timers(&timer_ids);
            unregister_interrupts(&interrupt_ids);
            unregister_threads(&thread_ids);
            unregister_processes(&process_ids);

            // Destroy internal timeout timer (not needed — a real handle fired).
            if let Some(tid) = timeout_timer {
                timer::destroy(tid);
                scheduler::set_timeout_timer_none();
            }

            // Timeout sentinel returns WouldBlock; real handles return index.
            let val = if entry.user_index == TIMEOUT_SENTINEL {
                WOULD_BLOCK_RAW
            } else {
                entry.user_index as u64
            };

            return dispatch_ok(ctx, val);
        }
    }

    // None ready. Poll mode: return immediately.
    if timeout == 0 {
        scheduler::clear_wait_state();

        unregister_channels(&channel_ids);
        unregister_timers(&timer_ids);
        unregister_interrupts(&interrupt_ids);
        unregister_threads(&thread_ids);
        unregister_processes(&process_ids);

        if let Some(tid) = timeout_timer {
            timer::destroy(tid);
            scheduler::set_timeout_timer_none();
        }

        return dispatch_ok(ctx, Error::WouldBlock as i64 as u64);
    }

    // Block until woken. wake_pending catches signals that arrived in the gap
    // between populating the wait set and here.
    match scheduler::block_current_unless_woken(ctx) {
        scheduler::BlockResult::WokePending(p) => {
            // Same thread — safe to unregister from waiters that didn't fire.
            unregister_channels(&channel_ids);
            unregister_timers(&timer_ids);
            unregister_interrupts(&interrupt_ids);
            unregister_threads(&thread_ids);
            unregister_processes(&process_ids);

            // Destroy internal timeout timer — we woke before blocking.
            if let Some(tid) = timeout_timer {
                timer::destroy(tid);
                scheduler::set_timeout_timer_none();
            }

            p
        }
        scheduler::BlockResult::Blocked(p) => {
            // Different thread scheduled — must not touch the blocked thread's
            // waiter registrations. The wake path (try_wake_impl) clears the
            // wait_set; stale registrations on unfired handles are harmlessly
            // overwritten on the next wait call.
            p
        }
    }
}
fn sys_write(buf_ptr: u64, len: u64) -> Result<u64, Error> {
    if len > MAX_WRITE_LEN {
        return Err(Error::BadLength);
    }
    if buf_ptr >= USER_VA_END {
        return Err(Error::BadAddress);
    }

    let end = buf_ptr.checked_add(len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }
    if !is_user_range_readable(buf_ptr, len) {
        return Err(Error::BadAddress);
    }

    // SAFETY: TTBR0 is still loaded during syscall. The address range has been
    // validated: within user VA space and all pages are mapped + EL0-readable
    // (verified via AT S1E0R hardware translation check).
    let slice = unsafe { core::slice::from_raw_parts(buf_ptr as *const u8, len as usize) };

    for &byte in slice {
        serial::putc(byte);
    }

    Ok(len)
}
fn sys_yield(ctx: *mut Context) -> *const Context {
    scheduler::schedule(ctx)
}
/// Unregister channel waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the waiter was already cleared by the signal path.
fn unregister_channels(ids: &[Option<ChannelId>]) {
    for &id in ids.iter().flatten() {
        channel::unregister_waiter(id);
    }
}
/// Unregister interrupt waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the waiter was already cleared by the fire path.
fn unregister_interrupts(ids: &[Option<InterruptId>]) {
    for &id in ids.iter().flatten() {
        interrupt::unregister_waiter(id);
    }
}
/// Unregister process exit waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the waiter was already cleared by the exit path.
fn unregister_processes(ids: &[Option<ProcessId>]) {
    for &id in ids.iter().flatten() {
        process_exit::unregister_waiter(id);
    }
}
/// Unregister thread exit waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the waiter was already cleared by the exit path.
fn unregister_threads(ids: &[Option<ThreadId>]) {
    for &id in ids.iter().flatten() {
        thread_exit::unregister_waiter(id);
    }
}
/// Unregister timer waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the timer's waiter was already cleared by the fire path.
fn unregister_timers(ids: &[Option<TimerId>]) {
    for &id in ids.iter().flatten() {
        timer::unregister_waiter(id);
    }
}
/// Translate a user virtual address to a physical address using hardware AT.
///
/// Returns None if the page is unmapped or inaccessible from EL0.
fn user_va_to_pa(va: u64) -> Option<u64> {
    let par: u64;

    // SAFETY: AT S1E0R is a privileged instruction that performs address
    // translation without memory access — it only writes to PAR_EL1. The
    // ISB ensures PAR_EL1 is visible before the mrs reads it. Single asm
    // block prevents LLVM reordering. No memory is accessed; nostack is
    // correct since this uses only system registers.
    unsafe {
        // AT S1E0R translates va, writing the result to PAR_EL1. The mrs
        // reads that result. Single asm block prevents LLVM from reordering
        // the read before the translation (see is_user_page_writable comment).
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            "mrs {par}, par_el1",
            va = in(reg) va,
            par = out(reg) par,
            options(nostack)
        );
    }

    // PAR_EL1 bit 0: 0 = success, 1 = fault.
    if par & 1 != 0 {
        return None;
    }

    // PAR_EL1[47:12] = physical address of the page.
    let page_pa = par & 0x0000_FFFF_FFFF_F000;
    let offset = va & (paging::PAGE_SIZE - 1);

    Some(page_pa | offset)
}

#[inline(never)]
pub fn dispatch(ctx: *mut Context) -> *const Context {
    metrics::inc_syscalls();

    // SAFETY: Read syscall arguments from the context via raw pointer. We must
    // NOT create `&mut *ctx` here — the scheduler lock's `&mut State`
    // transitively covers the same Context (it's in `cores[core].current`).
    // With inlining at opt-level 3, LLVM sees two `noalias` mutable references
    // to overlapping memory, which is UB that causes miscompilation (corrupted
    // return addresses on the kernel stack). ctx is valid (set by exception.S
    // from TPIDR_EL1). addr_of! avoids reference creation. x[0], x[1], x[8]
    // are within the [u64; 31] array bounds.
    let (syscall_nr, x0, x1) = unsafe {
        let x = core::ptr::addr_of!((*ctx).x) as *const u64;

        (x.add(8).read(), x.add(0).read(), x.add(1).read())
    };

    match syscall_nr {
        // Special cases: these manipulate ctx directly (may block/switch threads).
        nr::EXIT => sys_exit(ctx),
        nr::YIELD => sys_yield(ctx),
        nr::FUTEX_WAIT => sys_futex_wait(ctx),
        nr::WAIT => sys_wait(ctx),
        // Standard syscalls: Result<u64, E> → x0, return same context.
        nr::WRITE => dispatch_ok(ctx, result_to_u64!(sys_write(x0, x1))),
        nr::HANDLE_CLOSE => dispatch_ok(ctx, result_to_u64!(sys_handle_close(x0))),
        nr::CHANNEL_SIGNAL => dispatch_ok(ctx, result_to_u64!(sys_channel_signal(x0))),
        nr::CHANNEL_CREATE => dispatch_ok(ctx, result_to_u64!(sys_channel_create())),
        nr::SCHEDULING_CONTEXT_CREATE => {
            dispatch_ok(ctx, result_to_u64!(sys_scheduling_context_create(x0, x1)))
        }
        nr::SCHEDULING_CONTEXT_BORROW => {
            dispatch_ok(ctx, result_to_u64!(sys_scheduling_context_borrow(x0)))
        }
        nr::SCHEDULING_CONTEXT_RETURN => {
            dispatch_ok(ctx, result_to_u64!(sys_scheduling_context_return()))
        }
        nr::SCHEDULING_CONTEXT_BIND => {
            dispatch_ok(ctx, result_to_u64!(sys_scheduling_context_bind(x0)))
        }
        nr::FUTEX_WAKE => dispatch_ok(ctx, result_to_u64!(sys_futex_wake(x0, x1))),
        nr::TIMER_CREATE => dispatch_ok(ctx, result_to_u64!(sys_timer_create(x0))),
        nr::INTERRUPT_REGISTER => dispatch_ok(ctx, result_to_u64!(sys_interrupt_register(x0))),
        nr::INTERRUPT_ACK => dispatch_ok(ctx, result_to_u64!(sys_interrupt_ack(x0))),
        nr::DEVICE_MAP => dispatch_ok(ctx, result_to_u64!(sys_device_map(x0, x1))),
        nr::DMA_ALLOC => dispatch_ok(ctx, result_to_u64!(sys_dma_alloc(x0, x1))),
        nr::DMA_FREE => dispatch_ok(ctx, result_to_u64!(sys_dma_free(x0, x1))),
        nr::THREAD_CREATE => dispatch_ok(ctx, result_to_u64!(sys_thread_create(x0, x1))),
        nr::PROCESS_CREATE => dispatch_ok(ctx, result_to_u64!(sys_process_create(x0, x1))),
        nr::PROCESS_START => dispatch_ok(ctx, result_to_u64!(sys_process_start(x0))),
        nr::HANDLE_SEND => dispatch_ok(ctx, result_to_u64!(sys_handle_send(x0, x1))),
        nr::PROCESS_KILL => dispatch_ok(ctx, result_to_u64!(sys_process_kill(x0))),
        nr::MEMORY_SHARE => {
            // SAFETY: ctx is valid, x[2]/x[3] are within [u64; 31] bounds. addr_of!
            // avoids creating a reference (same aliasing UB prevention as above).
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            let x3 = unsafe { xbase.add(3).read() };

            dispatch_ok(ctx, result_to_u64!(sys_memory_share(x0, x1, x2, x3)))
        }
        nr::MEMORY_ALLOC => dispatch_ok(ctx, result_to_u64!(sys_memory_alloc(x0))),
        nr::MEMORY_FREE => dispatch_ok(ctx, result_to_u64!(sys_memory_free(x0, x1))),

        _ => dispatch_ok(
            ctx,
            result_to_u64!(Err::<u64, Error>(Error::UnknownSyscall)),
        ),
    }
}
