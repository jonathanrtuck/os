//! Syscall dispatcher and handlers.
//!
//! # ABI (aarch64, EL0 → EL1)
//!
//! Invoke with `svc #0`. The kernel saves/restores the full register context
//! across the call, so all registers except x0 are preserved.
//!
//! | Register | Direction | Role                               |
//! |----------|-----------|------------------------------------|
//! | x8       | in        | Syscall number (see `nr` module)   |
//! | x0..x5   | in        | Arguments (syscall-specific)       |
//! | x0       | out       | Return value: ≥0 success, <0 error |
//!
//! # Syscalls (dense 0–36, grouped by abstraction layer)
//!
//! | Nr | Name                       | Args                                      | Returns                      |
//! |----|----------------------------|-------------------------------------------|------------------------------|
//! | 0  | exit                       | —                                         | does not return              |
//! | 1  | write                      | x0=buf_ptr, x1=len                        | bytes written                |
//! | 2  | yield                      | —                                         | 0                            |
//! | 3  | handle_close               | x0=handle                                 | 0                            |
//! | 4  | handle_send                | x0=target, x1=source, x2=rights_mask      | 0                            |
//! | 5  | handle_set_badge           | x0=handle, x1=badge                       | 0                            |
//! | 6  | handle_get_badge           | x0=handle                                 | badge                        |
//! | 7  | channel_create             | —                                         | handle_a \| (handle_b << 16) |
//! | 8  | channel_signal             | x0=handle                                 | 0                            |
//! | 9  | wait                       | x0=handles_ptr, x1=count, x2=timeout_ns   | ready index (may block)      |
//! | 10 | futex_wait                 | x0=addr, x1=expected                      | 0 (may block)                |
//! | 11 | futex_wake                 | x0=addr, x1=count                         | threads woken                |
//! | 12 | timer_create               | x0=timeout_ns                             | handle                       |
//! | 13 | memory_alloc               | x0=page_count                             | user VA                      |
//! | 14 | memory_free                | x0=va, x1=page_count                      | 0                            |
//! | 15 | vmo_create                 | x0=size_pages, x1=flags, x2=type_tag      | handle                       |
//! | 16 | vmo_map                    | x0=handle, x1=flags, x2=target (0=self)   | user VA                      |
//! | 17 | vmo_unmap                  | x0=va, x1=size_pages                      | 0                            |
//! | 18 | vmo_read                   | x0=handle, x1=offset, x2=buf_ptr, x3=len  | bytes read                   |
//! | 19 | vmo_write                  | x0=handle, x1=offset, x2=buf_ptr, x3=len  | bytes written                |
//! | 20 | vmo_get_info               | x0=handle, x1=info_ptr                    | 0                            |
//! | 21 | vmo_snapshot               | x0=handle                                 | generation                   |
//! | 22 | vmo_restore                | x0=handle, x1=generation                  | 0                            |
//! | 23 | vmo_seal                   | x0=handle                                 | 0                            |
//! | 24 | vmo_op_range               | x0=handle, x1=op, x2=offset, x3=count     | op-specific                  |
//! | 25 | process_create             | x0=elf_ptr, x1=elf_len                    | handle                       |
//! | 26 | process_start              | x0=handle                                 | 0                            |
//! | 27 | process_kill               | x0=handle                                 | 0                            |
//! | 28 | process_set_syscall_filter | x0=handle, x1=mask (u64)                  | 0                            |
//! | 29 | thread_create              | x0=entry_va, x1=stack_top                 | handle                       |
//! | 30 | scheduling_context_create  | x0=budget_ns, x1=period_ns                | handle                       |
//! | 31 | scheduling_context_borrow  | x0=handle                                 | 0                            |
//! | 32 | scheduling_context_return  | —                                         | 0                            |
//! | 33 | scheduling_context_bind    | x0=handle                                 | 0                            |
//! | 34 | device_map                 | x0=phys_addr, x1=size                     | user VA                      |
//! | 35 | interrupt_register         | x0=irq_nr                                 | handle                       |
//! | 36 | interrupt_ack              | x0=handle                                 | 0                            |
//!
//! # Error codes
//!
//! | Code | Name               | Source        |
//! |------|--------------------|---------------|
//! | -1   | UnknownSyscall     | `Error`       |
//! | -2   | BadAddress         | `Error`       |
//! | -3   | BadLength          | `Error`       |
//! | -4   | InvalidArgument    | `Error`       |
//! | -5   | AlreadyBorrowing   | `Error`       |
//! | -6   | NotBorrowing       | `Error`       |
//! | -7   | AlreadyBound       | `Error`       |
//! | -8   | WouldBlock         | `Error`       |
//! | -9   | OutOfMemory        | `Error`       |
//! | -10  | PermissionDenied   | `Error`       |
//! | -11  | InvalidHandle      | `HandleError` |
//! | -12  | InsufficientRights | `HandleError` |
//! | -13  | TableFull          | `HandleError` |
//! | -15  | SyscallBlocked     | `Error`       |

use alloc::vec::Vec;

use super::{
    channel, futex,
    handle::{ChannelId, Handle, HandleError, HandleObject, Rights},
    interrupt,
    interrupt::InterruptId,
    memory, metrics, page_allocator, paging,
    paging::USER_VA_END,
    process,
    process::ProcessId,
    process_exit, scheduler, serial,
    thread::{ThreadId, WaitEntry},
    thread_exit, timer,
    timer::TimerId,
    vmo, Context,
};

/// Syscall numbers. Grouped by abstraction layer, dense 0–36.
///
/// Pre-v1.0: renumber freely when syscalls are added or removed.
/// At v1.0: freeze. Numbering tells the kernel's conceptual story.
pub mod nr {
    // --- Runtime basics (0–2) ---
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    // --- Capability layer (3–6) ---
    pub const HANDLE_CLOSE: u64 = 3;
    pub const HANDLE_SEND: u64 = 4;
    pub const HANDLE_SET_BADGE: u64 = 5;
    pub const HANDLE_GET_BADGE: u64 = 6;
    // --- IPC (7–8) ---
    pub const CHANNEL_CREATE: u64 = 7;
    pub const CHANNEL_SIGNAL: u64 = 8;
    // --- Event loop (9) ---
    pub const WAIT: u64 = 9;
    // --- Userspace sync (10–11) ---
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    // --- Time (12) ---
    pub const TIMER_CREATE: u64 = 12;
    // --- Heap memory (13–14) ---
    pub const MEMORY_ALLOC: u64 = 13;
    pub const MEMORY_FREE: u64 = 14;
    // --- Virtual Memory Objects (15–24) ---
    pub const VMO_CREATE: u64 = 15;
    pub const VMO_MAP: u64 = 16;
    pub const VMO_UNMAP: u64 = 17;
    pub const VMO_READ: u64 = 18;
    pub const VMO_WRITE: u64 = 19;
    pub const VMO_GET_INFO: u64 = 20;
    pub const VMO_SNAPSHOT: u64 = 21;
    pub const VMO_RESTORE: u64 = 22;
    pub const VMO_SEAL: u64 = 23;
    pub const VMO_OP_RANGE: u64 = 24;
    // --- Process/thread lifecycle (25–29) ---
    pub const PROCESS_CREATE: u64 = 25;
    pub const PROCESS_START: u64 = 26;
    pub const PROCESS_KILL: u64 = 27;
    pub const PROCESS_SET_SYSCALL_FILTER: u64 = 28;
    pub const THREAD_CREATE: u64 = 29;
    // --- Scheduling (30–33) ---
    pub const SCHEDULING_CONTEXT_CREATE: u64 = 30;
    pub const SCHEDULING_CONTEXT_BORROW: u64 = 31;
    pub const SCHEDULING_CONTEXT_RETURN: u64 = 32;
    pub const SCHEDULING_CONTEXT_BIND: u64 = 33;
    // --- Device layer (34–36) ---
    pub const DEVICE_MAP: u64 = 34;
    pub const INTERRUPT_REGISTER: u64 = 35;
    pub const INTERRUPT_ACK: u64 = 36;
}

/// Maximum DMA allocation order — matches page_allocator::MAX_ORDER.
/// Derived from RAM geometry so resolution changes never require kernel updates.
const MAX_DMA_ORDER: u64 = (paging::RAM_SIZE_MAX / paging::PAGE_SIZE).ilog2() as u64;
/// Maximum ELF size for process_create (4 MiB).
/// Increased from 2 MiB to accommodate real HarfBuzz text shaping in core.
const MAX_ELF_SIZE: u64 = 4 * 1024 * 1024;
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
    /// VMO is sealed or operation requires a right the handle lacks.
    PermissionDenied = -10,
    SyscallBlocked = -15,
}

const VMO_MAP_READ: u64 = 1 << 0;
const VMO_MAP_WRITE: u64 = 1 << 1;
const VMO_OP_COMMIT: u64 = 0;
const VMO_OP_DECOMMIT: u64 = 1;
const VMO_OP_LOOKUP: u64 = 2;

impl From<HandleError> for u64 {
    fn from(e: HandleError) -> u64 {
        (e as i64) as u64
    }
}
impl From<HandleError> for Error {
    fn from(e: HandleError) -> Error {
        match e {
            HandleError::InvalidHandle => Error::InvalidArgument,
            HandleError::InsufficientRights => Error::InvalidArgument,
            HandleError::TableFull => Error::OutOfMemory,
            HandleError::SlotOccupied => Error::InvalidArgument,
        }
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
    super::arch::mmu::is_user_page_writable(va)
}
/// Verify that all pages in `[start, start+len)` are readable by EL0.
fn is_user_range_readable(start: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }

    let page_mask = !(paging::PAGE_SIZE - 1);
    let first_page = start & page_mask;
    // start + len - 1 cannot overflow: callers verify start + len <= USER_VA_END via checked_add
    let last_page = start.saturating_add(len).saturating_sub(1) & page_mask;
    let mut page = first_page;

    while page <= last_page {
        if !is_user_page_readable(page) {
            return false;
        }

        page += paging::PAGE_SIZE;
    }

    true
}
/// Verify that all pages in `[start, start+len)` are writable by EL0.
fn is_user_range_writable(start: u64, len: u64) -> bool {
    if len == 0 {
        return true;
    }

    let page_mask = !(paging::PAGE_SIZE - 1);
    let first_page = start & page_mask;
    let last_page = start.saturating_add(len).saturating_sub(1) & page_mask;
    let mut page = first_page;

    while page <= last_page {
        if !is_user_page_writable(page) {
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
            .insert(HandleObject::Channel(ch_a), Rights::ALL)?;

        match process
            .handles
            .insert(HandleObject::Channel(ch_b), Rights::ALL)
        {
            Ok(handle_b) => {
                // Both handles inserted — now map both shared pages using the
                // per-process channel SHM bump allocator.
                let pages = match channel::shared_pages(ch_a) {
                    Some(p) => p,
                    None => {
                        let _ = process.handles.close(handle_a);
                        let _ = process.handles.close(handle_b);

                        return Err(HandleError::InvalidHandle);
                    }
                };

                let va_a = match process.address_space.map_channel_page(pages[0].as_u64()) {
                    Some(va) => va,
                    None => {
                        let _ = process.handles.close(handle_a);
                        let _ = process.handles.close(handle_b);

                        return Err(HandleError::TableFull);
                    }
                };

                if process
                    .address_space
                    .map_channel_page(pages[1].as_u64())
                    .is_none()
                {
                    // Second map failed — unmap the first page's PTE.
                    process.address_space.unmap_channel_page(va_a);
                    let _ = process.handles.close(handle_a);
                    let _ = process.handles.close(handle_b);

                    return Err(HandleError::TableFull);
                }

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
        Ok((handle_a, handle_b)) => Ok(handle_a.0 as u64 | (handle_b.0 as u64) << 16),
        Err(_) => {
            // Clean up both endpoints.
            channel::close_endpoint(ch_a);
            channel::close_endpoint(ch_b);

            Err(Error::OutOfMemory)
        }
    }
}
fn sys_channel_signal(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u16::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let channel_id = scheduler::current_process_do(|process| {
        match process
            .handles
            .get(Handle(handle_nr as u16), Rights::SIGNAL)
        {
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

    if !(end <= paging::RAM_START || pa >= paging::ram_end()) {
        return Err(Error::InvalidArgument); // Overlaps RAM — not a device
    }

    scheduler::current_process_do(|process| {
        process
            .address_space
            .map_device_mmio(pa, size)
            .ok_or(Error::InvalidArgument)
    })
}
// dma_alloc and dma_free REMOVED — replaced by VMO syscalls (30-39).
// Userspace sys library provides the same API via VMO-backed implementation.
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
    if handle_nr > u16::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let int_id = scheduler::current_process_do(|process| {
        match process
            .handles
            .get(Handle(handle_nr as u16), Rights::SIGNAL)
        {
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
            .insert(HandleObject::Interrupt(int_id), Rights::ALL)
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
    if handle_nr > u16::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let (obj, _rights, _badge) =
        scheduler::current_process_do(|process| process.handles.close(Handle(handle_nr as u16)))?;

    // Release kernel resources associated with the closed handle.
    match obj {
        HandleObject::Channel(id) => channel::close_endpoint(id),
        HandleObject::Interrupt(id) => interrupt::destroy(id),
        HandleObject::Process(id) => process_exit::destroy(id),
        HandleObject::SchedulingContext(id) => scheduler::release_scheduling_context(id),
        HandleObject::Thread(id) => thread_exit::destroy(id),
        HandleObject::Timer(id) => timer::destroy(id),
        HandleObject::Vmo(id) => {
            let freed_pages = vmo::destroy(id);

            for pa in freed_pages {
                page_allocator::free_frame(pa);
            }
        }
    }

    Ok(0)
}
fn sys_handle_get_badge(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u16::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    scheduler::current_process_do(|process| process.handles.get_badge(Handle(handle_nr as u16)))
}
fn sys_handle_send(
    target_handle_nr: u64,
    source_handle_nr: u64,
    rights_mask: u64,
) -> Result<u64, Error> {
    if target_handle_nr > u16::MAX as u64 || source_handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // Attenuation mask: 0 means "preserve all rights from source."
    let mask = if rights_mask == 0 {
        Rights::ALL
    } else {
        Rights::from_raw(rights_mask as u32)
    };
    let source_handle = Handle(source_handle_nr as u16);
    // Phase 1: Move the source handle out of the caller's table.
    // We take (not just read) the handle — move semantics prevent duplicated
    // endpoints, which would corrupt channel closed_count.
    let (target_pid, source_obj, source_rights, source_badge) =
        scheduler::current_process_do(|process| {
            let target_pid = match process
                .handles
                .get(Handle(target_handle_nr as u16), Rights::WRITE)
            {
                Ok(HandleObject::Process(id)) => id,
                Ok(_) => return Err(Error::InvalidArgument),
                Err(_) => return Err(Error::InvalidArgument),
            };
            // Verify the source handle has TRANSFER right before moving it.
            // Without this check, any handle could be delegated to another process.
            process
                .handles
                .get_entry(source_handle, Rights::TRANSFER)
                .map_err(|_| Error::InvalidArgument)?;
            // Now close (move out). Can't fail — we just verified it exists.
            // close returns (object, rights, badge).
            let (source_obj, source_rights, source_badge) =
                process.handles.close(source_handle).unwrap();

            Ok((target_pid, source_obj, source_rights, source_badge))
        })?;
    // Attenuate: target handle gets only the rights present in BOTH the
    // source handle and the mask. Rights can only be removed, never added.
    let source_rights = source_rights.attenuate(mask);
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
        // of the global channel index. Track mapped VAs for rollback on failure.
        if let Some(pages) = channel_pages {
            let va_a = target
                .address_space
                .map_channel_page(pages[0].as_u64())
                .ok_or(Error::OutOfMemory)?;
            let va_b = match target.address_space.map_channel_page(pages[1].as_u64()) {
                Some(va) => va,
                None => {
                    // Second map failed — unmap the first page.
                    target.address_space.unmap_channel_page(va_a);
                    return Err(Error::OutOfMemory);
                }
            };

            if target
                .handles
                .insert_with_badge(source_obj, source_rights, source_badge)
                .is_err()
            {
                // Handle insert failed — unmap both pages.
                target.address_space.unmap_channel_page(va_a);
                target.address_space.unmap_channel_page(va_b);
                return Err(Error::InvalidArgument);
            }
        } else {
            target
                .handles
                .insert_with_badge(source_obj, source_rights, source_badge)
                .map_err(|_| Error::InvalidArgument)?;
        }

        Ok(())
    })
    .unwrap_or(Err(Error::InvalidArgument));

    // Rollback: if Phase 2 failed, restore handle to source process.
    if let Err(e) = result {
        scheduler::current_process_do(|process| {
            let _ =
                process
                    .handles
                    .insert_at(source_handle, source_obj, source_rights, source_badge);
        });

        return Err(e);
    }

    Ok(0)
}
fn sys_handle_set_badge(handle_nr: u64, badge: u64) -> Result<u64, HandleError> {
    if handle_nr > u16::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    scheduler::current_process_do(|process| {
        process.handles.set_badge(Handle(handle_nr as u16), badge)
    })?;

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
// memory_share REMOVED — replaced by VMO cross-process mapping (vmo_map with target handle).
// Userspace sys library provides the same API via VMO-backed implementation.
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
            .insert(HandleObject::Process(process_id), Rights::ALL)
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
            for id in kill_info.timeout_timers {
                timer::destroy(id);
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
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let target_pid = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u16), Rights::KILL) {
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
    // Phase 3a: destroy internal timeout timers that were NOT in the handle table.
    // These are internal resources from `wait` with a finite timeout. Without this,
    // the 32-slot global timer table leaks a slot per killed thread with an active
    // timeout.
    for id in kill_info.timeout_timers {
        super::timer::destroy(id);
    }

    // Phase 4: free address space (immediate path — no threads were running).
    if let Some(mut addr_space) = kill_info.address_space {
        addr_space.invalidate_tlb();
        addr_space.free_all();

        super::address_space_id::free(super::address_space_id::Asid(addr_space.asid()));
    }

    Ok(0)
}
fn sys_process_set_syscall_filter(handle_nr: u64, mask: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let process_id = scheduler::current_process_do(|p| {
        match p.handles.get(Handle(handle_nr as u16), Rights::WRITE) {
            Ok(HandleObject::Process(id)) => Ok(id),
            Ok(_) => Err(Error::InvalidArgument),
            Err(_) => Err(Error::InvalidArgument),
        }
    })?;

    scheduler::with_process(process_id, |target| {
        if target.started {
            return Err(Error::InvalidArgument);
        }

        target.syscall_mask = mask;

        Ok(0)
    })
    .unwrap_or(Err(Error::InvalidArgument))
}
fn sys_process_start(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let process_id = scheduler::current_process_do(|p| {
        match p.handles.get(Handle(handle_nr as u16), Rights::WRITE) {
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
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let ctx_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u16), Rights::READ) {
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
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let ctx_id = scheduler::current_process_do(|process| {
        match process.handles.get(Handle(handle_nr as u16), Rights::READ) {
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
            .insert(HandleObject::SchedulingContext(ctx_id), Rights::ALL)
    })
    .map_err(|_| {
        // Handle table full — release the scheduling context to avoid leaking
        // it (ref_count=1 would never reach 0 without a handle to close).
        scheduler::release_scheduling_context(ctx_id);
        Error::InvalidArgument
    })?;

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
            .insert(HandleObject::Thread(thread_id), Rights::ALL)
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
            .insert(HandleObject::Timer(timer_id), Rights::ALL)
    }) {
        Ok(handle) => Ok(handle.0 as u64),
        Err(e) => {
            // Handle table full — clean up the timer we just created.
            timer::destroy(timer_id);

            Err(e)
        }
    }
}
fn sys_vmo_create(size_pages: u64, flags: u64, type_tag: u64) -> Result<u64, Error> {
    if size_pages == 0 {
        return Err(Error::InvalidArgument);
    }

    let vmo_flags = vmo::VmoFlags::from_bits(flags as u32);

    // For contiguous VMOs, allocate all pages eagerly via the buddy allocator.
    if vmo_flags.contains(vmo::VmoFlags::CONTIGUOUS) {
        // Find the smallest power-of-two order that covers size_pages.
        let order = (64 - (size_pages - 1).leading_zeros()) as usize;
        let contig_pa = page_allocator::alloc_frames(order).ok_or(Error::OutOfMemory)?;
        let vmo_id = match vmo::create(size_pages, vmo_flags, type_tag) {
            Some(id) => id,
            None => {
                page_allocator::free_frames(contig_pa, order);
                return Err(Error::OutOfMemory);
            }
        };

        // Pre-commit all pages in the VMO.
        {
            let mut state = vmo::STATE.lock();

            if let Some(v) = state.get_mut(vmo_id) {
                for i in 0..size_pages {
                    let pa = memory::Pa(contig_pa.0 + (i as usize) * paging::PAGE_SIZE as usize);

                    v.commit_page(i, pa);
                }
            }
        }

        // Insert handle into caller's table.
        return scheduler::current_process_do(|process| {
            match process
                .handles
                .insert(HandleObject::Vmo(vmo_id), Rights::ALL)
            {
                Ok(handle) => Ok(handle.0 as u64),
                Err(e) => {
                    // Rollback: destroy VMO (which returns pages to free).
                    let freed = vmo::destroy(vmo_id);

                    for pa in freed {
                        page_allocator::free_frame(pa);
                    }

                    Err(Error::from(e))
                }
            }
        });
    }

    // Normal (lazy) VMO — no pages allocated yet.
    let vmo_id = vmo::create(size_pages, vmo_flags, type_tag).ok_or(Error::OutOfMemory)?;

    scheduler::current_process_do(|process| {
        match process
            .handles
            .insert(HandleObject::Vmo(vmo_id), Rights::ALL)
        {
            Ok(handle) => Ok(handle.0 as u64),
            Err(e) => {
                vmo::destroy(vmo_id);
                Err(Error::from(e))
            }
        }
    })
}
fn sys_vmo_get_info(handle_nr: u64, info_va: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // Validate output buffer (must fit VmoInfo).
    let info_size = core::mem::size_of::<vmo::VmoInfo>() as u64;
    let end = info_va.checked_add(info_size).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }
    if !is_user_range_writable(info_va, info_size) {
        return Err(Error::BadAddress);
    }

    // Any valid handle can query info (no specific rights required).
    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process
            .handles
            .get(Handle(handle_nr as u16), Rights::NONE)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    let info = vmo::get_info(vmo_id).ok_or(Error::InvalidArgument)?;

    // SAFETY: info_va was validated as writable user memory. VmoInfo is repr(C).
    unsafe {
        core::ptr::write(info_va as *mut vmo::VmoInfo, info);
    }

    Ok(0)
}
fn sys_vmo_map(handle_nr: u64, map_flags: u64, target_handle: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let readable = map_flags & VMO_MAP_READ != 0;
    let writable = map_flags & VMO_MAP_WRITE != 0;

    if !readable && !writable {
        return Err(Error::InvalidArgument);
    }

    // Determine required rights.
    let mut required = Rights::MAP;

    if readable {
        required = required.union(Rights::READ);
    }
    if writable {
        required = required.union(Rights::WRITE);
    }

    // Look up the VMO handle and get its ID + size.
    // Also resolve target process if cross-process mapping requested.
    let (vmo_id, size_pages, target_pid) = scheduler::current_process_do(|process| {
        let obj = process.handles.get(Handle(handle_nr as u16), required)?;

        let vmo_id = match obj {
            HandleObject::Vmo(id) => id,
            _ => return Err(HandleError::InvalidHandle),
        };

        let size = vmo::size_pages(vmo_id).ok_or(HandleError::InvalidHandle)?;

        // Resolve target: 0 = self, otherwise a Process handle in the caller's table.
        let target = if target_handle == 0 {
            None // Map into self
        } else {
            if target_handle > u16::MAX as u64 {
                return Err(HandleError::InvalidHandle);
            }
            let target_obj = process
                .handles
                .get(Handle(target_handle as u16), Rights::NONE)?;

            match target_obj {
                HandleObject::Process(pid) => Some(pid),
                _ => return Err(HandleError::InvalidHandle),
            }
        };

        Ok((vmo_id, size, target))
    })?;

    // Map into the target (or self) address space.
    match target_pid {
        None => {
            // Map into self.
            scheduler::current_process_with_pid_do(|pid, process| {
                let va = process
                    .address_space
                    .map_vmo(vmo_id, size_pages, writable)
                    .ok_or(Error::OutOfMemory)?;

                vmo::add_mapping(
                    vmo_id,
                    vmo::VmoMapping {
                        process_id: pid,
                        va_base: va,
                        page_count: size_pages,
                    },
                );

                Ok(va)
            })
        }
        Some(target) => {
            // Cross-process: map VMO into the target process's address space.
            scheduler::with_process(target, |process| {
                let va = process
                    .address_space
                    .map_vmo(vmo_id, size_pages, writable)
                    .ok_or(Error::OutOfMemory)?;

                vmo::add_mapping(
                    vmo_id,
                    vmo::VmoMapping {
                        process_id: target,
                        va_base: va,
                        page_count: size_pages,
                    },
                );

                Ok(va)
            })
            .unwrap_or(Err(Error::InvalidArgument))
        }
    }
}
fn sys_vmo_op_range(
    handle_nr: u64,
    op: u64,
    offset_pages: u64,
    page_count: u64,
) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // LOOKUP only needs MAP right; COMMIT/DECOMMIT need WRITE.
    let required = if op == VMO_OP_LOOKUP {
        Rights::MAP
    } else {
        Rights::WRITE
    };

    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process.handles.get(Handle(handle_nr as u16), required)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    match op {
        VMO_OP_LOOKUP => {
            // Return the base PA of a contiguous VMO. Non-contiguous → error.
            let state = vmo::STATE.lock();
            let vmo_obj = state.get(vmo_id).ok_or(Error::InvalidArgument)?;

            if !vmo_obj.is_contiguous() {
                return Err(Error::InvalidArgument);
            }

            // The base PA is the PA of page 0.
            match vmo_obj.lookup_page(0) {
                Some((pa, _)) => Ok(pa.as_u64() as u64),
                None => Err(Error::InvalidArgument), // Not committed (shouldn't happen for contiguous)
            }
        }
        VMO_OP_COMMIT => {
            if page_count == 0 {
                return Err(Error::InvalidArgument);
            }
            // Eagerly allocate and commit pages in the range.
            let size = vmo::size_pages(vmo_id).ok_or(Error::InvalidArgument)?;

            if offset_pages >= size || offset_pages + page_count > size {
                return Err(Error::InvalidArgument);
            }

            let mut committed = 0u64;

            for page_idx in offset_pages..offset_pages + page_count {
                // Check if already committed via get_info pattern.
                // We need to check page-by-page. Use the VMO table directly
                // through the module API.
                let mut state = vmo::STATE.lock();
                let vmo = state.get_mut(vmo_id).ok_or(Error::InvalidArgument)?;

                if vmo.is_sealed() {
                    return Err(Error::PermissionDenied);
                }

                if vmo.lookup_page(page_idx).is_some() {
                    continue; // Already committed
                }

                // Allocate and commit.
                let pa = super::page_allocator::alloc_frame().ok_or(Error::OutOfMemory)?;

                vmo.commit_page(page_idx, pa);

                committed += 1;
            }

            Ok(committed)
        }
        VMO_OP_DECOMMIT => {
            if page_count == 0 {
                return Err(Error::InvalidArgument);
            }

            let size = vmo::size_pages(vmo_id).ok_or(Error::InvalidArgument)?;

            if offset_pages >= size || offset_pages + page_count > size {
                return Err(Error::InvalidArgument);
            }

            let mut freed_count = 0u64;

            for page_idx in offset_pages..offset_pages + page_count {
                match vmo::decommit_page(vmo_id, page_idx) {
                    Some(Some(pa)) => {
                        super::page_allocator::free_frame(pa);
                        freed_count += 1;
                    }
                    Some(None) => {} // Not committed or shared — skip
                    None => return Err(Error::PermissionDenied), // Sealed
                }
            }

            // Invalidate PTEs for active mappings so decommitted pages re-fault
            // to zeros.
            let mappings = vmo::get_mappings(vmo_id);

            for mapping in &mappings {
                scheduler::with_process(mapping.process_id, |process| {
                    process
                        .address_space
                        .invalidate_vmo_pages(mapping.va_base, mapping.page_count);
                });
            }

            Ok(freed_count)
        }
        _ => Err(Error::InvalidArgument),
    }
}
fn sys_vmo_read(handle_nr: u64, offset: u64, buf_va: u64, len: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 || len == 0 {
        return Err(Error::InvalidArgument);
    }

    // Validate user buffer.
    let end = buf_va.checked_add(len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }
    if !is_user_range_writable(buf_va, len) {
        return Err(Error::BadAddress);
    }

    // Check handle rights.
    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process
            .handles
            .get(Handle(handle_nr as u16), Rights::READ)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    // Read from VMO into user buffer.
    // SAFETY: buf_va was validated as a writable user buffer above.
    // The TTBR0 page tables are still loaded (we're in the caller's context).
    let user_buf = unsafe { core::slice::from_raw_parts_mut(buf_va as *mut u8, len as usize) };

    vmo::read(vmo_id, offset, user_buf).ok_or(Error::InvalidArgument)
}
fn sys_vmo_restore(handle_nr: u64, generation: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // Restore requires WRITE right (replaces VMO's page list).
    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process
            .handles
            .get(Handle(handle_nr as u16), Rights::WRITE)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    let (freed, mappings) = vmo::restore(vmo_id, generation).ok_or(Error::InvalidArgument)?;

    // Free physical pages that are no longer referenced.
    for pa in freed {
        super::page_allocator::free_frame(pa);
    }

    // Invalidate PTEs for all active mappings so they re-fault to restored data.
    for mapping in &mappings {
        scheduler::with_process(mapping.process_id, |process| {
            process
                .address_space
                .invalidate_vmo_pages(mapping.va_base, mapping.page_count);
        });
    }

    Ok(0)
}
fn sys_vmo_seal(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // Seal requires the SEAL right.
    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process
            .handles
            .get(Handle(handle_nr as u16), Rights::SEAL)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;
    let mappings = vmo::seal(vmo_id).ok_or(Error::InvalidArgument)?;

    // Invalidate writable PTEs for all active mappings. The sealed-aware
    // fault handler will re-map them as RO. Permission faults (write to
    // the now-RO page) go straight to process termination — the exception
    // handler only dispatches translation faults to handle_fault.
    for mapping in &mappings {
        scheduler::with_process(mapping.process_id, |process| {
            process
                .address_space
                .invalidate_vmo_pages(mapping.va_base, mapping.page_count);
        });
    }

    Ok(0)
}
fn sys_vmo_snapshot(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    // Snapshot requires WRITE right (mutates VMO state: generation counter,
    // refcounts, snapshot ring).
    let vmo_id = scheduler::current_process_do(|process| {
        let obj = process
            .handles
            .get(Handle(handle_nr as u16), Rights::WRITE)?;

        match obj {
            HandleObject::Vmo(id) => Ok(id),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    vmo::snapshot(vmo_id).ok_or(Error::PermissionDenied) // Sealed or contiguous
}
fn sys_vmo_unmap(va: u64, size_pages: u64) -> Result<u64, Error> {
    if size_pages == 0 || va >= USER_VA_END {
        return Err(Error::InvalidArgument);
    }

    // Look up the VmoId from the VMA before unmapping (needed for mapping tracker).
    let vmo_id = scheduler::current_process_do(|process| process.address_space.vmo_id_at(va));
    let ok =
        scheduler::current_process_do(|process| process.address_space.unmap_vmo(va, size_pages));

    if ok {
        // Remove the mapping record from the VMO.
        if let Some(id) = vmo_id {
            let pid = scheduler::current_process_with_pid_do(|pid, _| pid);

            vmo::remove_mapping(id, pid, va);
        }

        Ok(0)
    } else {
        Err(Error::InvalidArgument)
    }
}
fn sys_vmo_write(handle_nr: u64, offset: u64, buf_va: u64, len: u64) -> Result<u64, Error> {
    if handle_nr > u16::MAX as u64 || len == 0 {
        return Err(Error::InvalidArgument);
    }

    // Validate user buffer (source — must be readable).
    let end = buf_va.checked_add(len).ok_or(Error::BadAddress)?;

    if end > USER_VA_END {
        return Err(Error::BadAddress);
    }
    if !is_user_range_readable(buf_va, len) {
        return Err(Error::BadAddress);
    }

    // Check handle rights: WRITE or APPEND.
    let (vmo_id, append_only) = scheduler::current_process_do(|process| {
        let (obj, rights) = process
            .handles
            .get_entry(Handle(handle_nr as u16), Rights::NONE)?;
        // Must have either WRITE or APPEND.
        let has_write = rights.contains(Rights::WRITE);
        let has_append = rights.contains(Rights::APPEND);

        if !has_write && !has_append {
            return Err(HandleError::InsufficientRights);
        }

        match obj {
            HandleObject::Vmo(id) => Ok((id, has_append && !has_write)),
            _ => Err(HandleError::InvalidHandle),
        }
    })?;

    // Read user data and write to VMO.
    // SAFETY: buf_va was validated as a readable user buffer above.
    let user_buf = unsafe { core::slice::from_raw_parts(buf_va as *const u8, len as usize) };

    vmo::write(vmo_id, offset, user_buf, append_only).ok_or(Error::InvalidArgument)
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

    // Clean up stale waiter registrations from a previous blocked wait.
    // When sys_wait takes the BlockResult::Blocked path, unfired handles
    // still have this thread registered as a waiter. Those registrations
    // can cause spurious wakeups with incorrect results on subsequent waits.
    let stale = scheduler::take_stale_waiters();

    for entry in &stale {
        match entry.object {
            HandleObject::Channel(id) => channel::unregister_waiter(id),
            HandleObject::Timer(id) => timer::unregister_waiter(id),
            HandleObject::Interrupt(id) => interrupt::unregister_waiter(id),
            HandleObject::Thread(id) => thread_exit::unregister_waiter(id),
            HandleObject::Process(id) => process_exit::unregister_waiter(id),
            HandleObject::SchedulingContext(_) | HandleObject::Vmo(_) => {}
        }
    }

    drop(stale);

    // Validate count.
    if count == 0 || count > MAX_WAIT_HANDLES {
        return dispatch_ok(ctx, Error::InvalidArgument as i64 as u64);
    }

    // Validate user buffer (u16 per handle = 2 bytes each).
    let byte_len = count * 2;

    if handles_ptr >= USER_VA_END {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }
    if let Some(end) = handles_ptr.checked_add(byte_len) {
        if end > USER_VA_END {
            return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
        }
    } else {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }
    if !is_user_range_readable(handles_ptr, byte_len) {
        return dispatch_ok(ctx, Error::BadAddress as i64 as u64);
    }

    // Read handle indices from user memory (u16 per handle).
    // SAFETY: TTBR0 is still loaded. Address and length validated above.
    let handle_indices =
        unsafe { core::slice::from_raw_parts(handles_ptr as *const u16, count as usize) };
    // Resolve handles and populate thread.wait_set in-place (reuses the Vec's
    // backing allocation from previous calls — no heap alloc in steady state).
    // A stack-allocated copy is returned for use outside the scheduler lock.
    let resolve_result = scheduler::current_thread_and_process_do(|thread, process| {
        thread.wait_set.clear();

        let mut entries: [Option<WaitEntry>; MAX_WAIT_HANDLES as usize + 1] =
            [None; MAX_WAIT_HANDLES as usize + 1];
        let mut count = 0usize;

        for (i, &h) in handle_indices.iter().enumerate() {
            let obj = process.handles.get(Handle(h), Rights::WAIT)?;

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
            HandleObject::SchedulingContext(_) | HandleObject::Vmo(_) => {}
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

    serial::write_bytes(slice);

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
    let par = super::arch::mmu::translate_user_read(va);

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

    // Syscall filtering: check caller's per-process mask.
    // EXIT is always allowed (can't trap a process with no way out).
    // Syscall numbers >= 32 skip the filter (future-proofing — they fall
    // through to the UnknownSyscall arm naturally).
    if syscall_nr != nr::EXIT && syscall_nr < 32 {
        let allowed = scheduler::current_process_do(|p| p.syscall_mask & (1u64 << syscall_nr) != 0);

        if !allowed {
            return dispatch_ok(
                ctx,
                result_to_u64!(Err::<u64, Error>(Error::SyscallBlocked)),
            );
        }
    }

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
        // 17 (DMA_ALLOC), 18 (DMA_FREE) removed — VMO syscalls replace them.
        nr::THREAD_CREATE => dispatch_ok(ctx, result_to_u64!(sys_thread_create(x0, x1))),
        nr::PROCESS_CREATE => dispatch_ok(ctx, result_to_u64!(sys_process_create(x0, x1))),
        nr::PROCESS_START => dispatch_ok(ctx, result_to_u64!(sys_process_start(x0))),
        nr::HANDLE_SEND => {
            // SAFETY: ctx is valid, x[2] is within [u64; 31] bounds. addr_of!
            // avoids creating a reference (same aliasing UB prevention as above).
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };

            dispatch_ok(ctx, result_to_u64!(sys_handle_send(x0, x1, x2)))
        }
        nr::PROCESS_KILL => dispatch_ok(ctx, result_to_u64!(sys_process_kill(x0))),
        // 24 (MEMORY_SHARE) removed — VMO cross-process mapping replaces it.
        nr::MEMORY_ALLOC => dispatch_ok(ctx, result_to_u64!(sys_memory_alloc(x0))),
        nr::MEMORY_FREE => dispatch_ok(ctx, result_to_u64!(sys_memory_free(x0, x1))),
        nr::PROCESS_SET_SYSCALL_FILTER => {
            dispatch_ok(ctx, result_to_u64!(sys_process_set_syscall_filter(x0, x1)))
        }
        nr::HANDLE_SET_BADGE => dispatch_ok(ctx, result_to_u64!(sys_handle_set_badge(x0, x1))),
        nr::HANDLE_GET_BADGE => dispatch_ok(ctx, result_to_u64!(sys_handle_get_badge(x0))),
        nr::VMO_CREATE => {
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            dispatch_ok(ctx, result_to_u64!(sys_vmo_create(x0, x1, x2)))
        }
        nr::VMO_MAP => {
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            dispatch_ok(ctx, result_to_u64!(sys_vmo_map(x0, x1, x2)))
        }
        nr::VMO_UNMAP => dispatch_ok(ctx, result_to_u64!(sys_vmo_unmap(x0, x1))),
        nr::VMO_READ => {
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            let x3 = unsafe { xbase.add(3).read() };
            dispatch_ok(ctx, result_to_u64!(sys_vmo_read(x0, x1, x2, x3)))
        }
        nr::VMO_WRITE => {
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            let x3 = unsafe { xbase.add(3).read() };
            dispatch_ok(ctx, result_to_u64!(sys_vmo_write(x0, x1, x2, x3)))
        }
        nr::VMO_GET_INFO => dispatch_ok(ctx, result_to_u64!(sys_vmo_get_info(x0, x1))),
        nr::VMO_SNAPSHOT => dispatch_ok(ctx, result_to_u64!(sys_vmo_snapshot(x0))),
        nr::VMO_RESTORE => dispatch_ok(ctx, result_to_u64!(sys_vmo_restore(x0, x1))),
        nr::VMO_SEAL => dispatch_ok(ctx, result_to_u64!(sys_vmo_seal(x0))),
        nr::VMO_OP_RANGE => {
            let xbase = unsafe { core::ptr::addr_of!((*ctx).x) as *const u64 };
            let x2 = unsafe { xbase.add(2).read() };
            let x3 = unsafe { xbase.add(3).read() };
            dispatch_ok(ctx, result_to_u64!(sys_vmo_op_range(x0, x1, x2, x3)))
        }

        _ => dispatch_ok(
            ctx,
            result_to_u64!(Err::<u64, Error>(Error::UnknownSyscall)),
        ),
    }
}
