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
//! | -10  | InvalidHandle       | `HandleError` |
//! | -12  | InsufficientRights  | `HandleError` |
//! | -13  | TableFull           | `HandleError` |

use super::channel;
use super::futex;
use super::handle::{Handle, HandleError, HandleObject, Rights};
use super::interrupt;
use super::interrupt::InterruptId;
use super::paging;
use super::paging::USER_VA_END;
use super::scheduler;
use super::serial;
use super::thread::WaitEntry;
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
}

/// Maximum number of handles in a single `wait` call.
const MAX_WAIT_HANDLES: u64 = 16;
const MAX_WRITE_LEN: u64 = 4096;

impl From<HandleError> for u64 {
    fn from(e: HandleError) -> u64 {
        (e as i64) as u64
    }
}

/// Check if a user virtual address is readable by EL0 using the hardware
/// address translation instruction. Returns false if the page is unmapped
/// or inaccessible.
fn is_user_page_readable(va: u64) -> bool {
    let par: u64;

    unsafe {
        // AT S1E0R: translate va as a Stage 1 EL0 Read.
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            va = in(reg) va,
            options(nostack)
        );
        core::arch::asm!(
            "mrs {par}, par_el1",
            par = out(reg) par,
            options(nostack, nomem)
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
fn sys_device_map(pa: u64, size: u64) -> Result<u64, Error> {
    if size == 0 {
        return Err(Error::InvalidArgument);
    }

    // Validate: PA must be outside RAM (device MMIO space only).
    let end = pa.checked_add(size).ok_or(Error::InvalidArgument)?;

    if !(end <= paging::RAM_START || pa >= paging::RAM_END) {
        return Err(Error::InvalidArgument); // Overlaps RAM — not a device
    }

    scheduler::current_thread_do(|thread| {
        if let Some(ref mut addr_space) = thread.address_space {
            addr_space
                .map_device_mmio(pa, size)
                .map(|va| va)
                .ok_or(Error::InvalidArgument)
        } else {
            Err(Error::InvalidArgument) // Kernel threads have no address space
        }
    })
}
fn sys_channel_signal(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    // Extract handle info under scheduler lock, then release before channel ops.
    let (channel_id, caller_id) = scheduler::current_thread_do(|thread| {
        let channel_id = match thread.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Channel(id)) => id,
            Ok(_) => return Err(HandleError::InvalidHandle),
            Err(e) => return Err(e),
        };

        Ok((channel_id, thread.id()))
    })?;

    channel::signal(channel_id, caller_id);

    Ok(0)
}
fn sys_exit(ctx: *mut Context) -> *const Context {
    scheduler::exit_current_from_syscall(ctx)
}
fn sys_futex_wait(ctx: *mut Context) -> *const Context {
    let c = unsafe { &mut *ctx };
    let addr = c.x[0];
    let expected = c.x[1] as u32;

    // Validate: must be in user VA space and word-aligned.
    if addr >= USER_VA_END || addr & 3 != 0 {
        c.x[0] = Error::BadAddress as i64 as u64;

        return ctx as *const Context;
    }

    // Translate VA → PA for the futex key.
    let pa = match user_va_to_pa(addr) {
        Some(pa) => pa,
        None => {
            c.x[0] = Error::BadAddress as i64 as u64;

            return ctx as *const Context;
        }
    };
    // Read the current value at the user address.
    // SAFETY: TTBR0 is still loaded, address validated via AT S1E0R.
    let current_value = unsafe { core::ptr::read_volatile(addr as *const u32) };

    if current_value != expected {
        // Value changed — don't block (spurious, not a lost wakeup).
        c.x[0] = Error::WouldBlock as i64 as u64;

        return ctx as *const Context;
    }

    // Record this thread in the futex wait table.
    let thread_id = scheduler::current_thread_do(|thread| thread.id());

    futex::wait(pa, thread_id);

    // Pre-set success return value before blocking.
    c.x[0] = 0;

    // Block (or return immediately if a wake arrived in the gap).
    scheduler::block_current_unless_woken(ctx)
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

    let int_id = scheduler::current_thread_do(|thread| {
        match thread.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
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

    match scheduler::current_thread_do(|thread| {
        thread
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

    let obj = scheduler::current_thread_do(|thread| thread.handles.close(Handle(handle_nr as u8)))?;

    // Release kernel resources associated with the closed handle.
    match obj {
        HandleObject::Interrupt(id) => interrupt::destroy(id),
        HandleObject::SchedulingContext(id) => scheduler::release_scheduling_context(id),
        HandleObject::Timer(id) => timer::destroy(id),
        _ => {}
    }

    Ok(0)
}
fn sys_scheduling_context_bind(handle_nr: u64) -> Result<u64, Error> {
    if handle_nr > u8::MAX as u64 {
        return Err(Error::InvalidArgument);
    }

    let ctx_id = scheduler::current_thread_do(|thread| {
        match thread.handles.get(Handle(handle_nr as u8), Rights::READ) {
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

    let ctx_id = scheduler::current_thread_do(|thread| {
        match thread.handles.get(Handle(handle_nr as u8), Rights::READ) {
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
    let handle = scheduler::current_thread_do(|thread| {
        thread
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
fn sys_timer_create(timeout_ns: u64) -> Result<u64, HandleError> {
    let timer_id = timer::create(timeout_ns).ok_or(HandleError::TableFull)?;

    match scheduler::current_thread_do(|thread| {
        thread
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
fn sys_wait(ctx: *mut Context) -> *const Context {
    let c = unsafe { &mut *ctx };
    let handles_ptr = c.x[0];
    let count = c.x[1];
    let timeout = c.x[2];

    // Validate count.
    if count == 0 || count > MAX_WAIT_HANDLES {
        c.x[0] = Error::InvalidArgument as i64 as u64;

        return ctx as *const Context;
    }

    // Validate user buffer.
    if handles_ptr >= USER_VA_END {
        c.x[0] = Error::BadAddress as i64 as u64;

        return ctx as *const Context;
    }

    if let Some(end) = handles_ptr.checked_add(count) {
        if end > USER_VA_END {
            c.x[0] = Error::BadAddress as i64 as u64;

            return ctx as *const Context;
        }
    } else {
        c.x[0] = Error::BadAddress as i64 as u64;

        return ctx as *const Context;
    }

    if !is_user_range_readable(handles_ptr, count) {
        c.x[0] = Error::BadAddress as i64 as u64;

        return ctx as *const Context;
    }

    // Read handle indices from user memory.
    // SAFETY: TTBR0 is still loaded. Address and length validated above.
    let handle_bytes =
        unsafe { core::slice::from_raw_parts(handles_ptr as *const u8, count as usize) };
    // Resolve all handles and validate they are waitable.
    let resolve_result = scheduler::current_thread_do(|thread| {
        let mut entries = Vec::new();

        for (i, &h) in handle_bytes.iter().enumerate() {
            let obj = thread.handles.get(Handle(h), Rights::READ)?;

            match obj {
                HandleObject::Channel(_) | HandleObject::Interrupt(_) | HandleObject::Timer(_) => {
                    entries.push(WaitEntry {
                        object: obj,
                        user_index: i as u8,
                    });
                }
                _ => return Err(HandleError::InvalidHandle), // Not waitable
            }
        }

        Ok((entries, thread.id()))
    });
    let (wait_entries, caller_id) = match resolve_result {
        Ok(pair) => pair,
        Err(e) => {
            c.x[0] = e.into();

            return ctx as *const Context;
        }
    };
    // Collect timer and interrupt IDs for waiter registration and cleanup.
    let mut timer_ids: [Option<TimerId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];
    let mut interrupt_ids: [Option<InterruptId>; MAX_WAIT_HANDLES as usize] =
        [None; MAX_WAIT_HANDLES as usize];

    for entry in &wait_entries {
        match entry.object {
            HandleObject::Timer(id) => timer_ids[entry.user_index as usize] = Some(id),
            HandleObject::Interrupt(id) => interrupt_ids[entry.user_index as usize] = Some(id),
            _ => {}
        }
    }
    // Register as waiter on each timer and interrupt BEFORE storing wait set
    // and checking readiness. If an event fires in the gap,
    // set_wake_pending_for_handle can target this thread.
    for &id in timer_ids.iter().flatten() {
        timer::register_waiter(id, caller_id);
    }
    for &id in interrupt_ids.iter().flatten() {
        interrupt::register_waiter(id, caller_id);
    }

    // Store wait set BEFORE checking readiness. This ensures that if a signal
    // arrives during the readiness check, set_wake_pending_for_handle can find
    // the wait set and prevent a lost wakeup.
    let entries_for_check = wait_entries.clone();

    scheduler::store_wait_set(wait_entries);

    // Check each handle for readiness.
    for entry in &entries_for_check {
        let ready = match entry.object {
            HandleObject::Channel(ch_id) => channel::check_pending(ch_id, caller_id),
            HandleObject::Interrupt(int_id) => interrupt::check_pending(int_id),
            HandleObject::Timer(t_id) => timer::check_fired(t_id),
            _ => false,
        };

        if ready {
            // Ready — clear wait state, unregister from timers/interrupts, return index.
            scheduler::clear_wait_state();

            unregister_timers(&timer_ids);
            unregister_interrupts(&interrupt_ids);

            c.x[0] = entry.user_index as u64;

            return ctx as *const Context;
        }
    }

    // None ready. Poll mode: return immediately.
    if timeout == 0 {
        scheduler::clear_wait_state();

        unregister_timers(&timer_ids);
        unregister_interrupts(&interrupt_ids);

        c.x[0] = Error::WouldBlock as i64 as u64;

        return ctx as *const Context;
    }

    // Block until woken. wake_pending catches signals that arrived in the gap
    // between store_wait_set and here.
    let result = scheduler::block_current_unless_woken(ctx);

    // Woken — unregister from any timers/interrupts that didn't fire.
    unregister_timers(&timer_ids);
    unregister_interrupts(&interrupt_ids);

    result
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
        if byte == b'\n' {
            serial::putc(b'\r');
        }

        serial::putc(byte);
    }

    Ok(len)
}
fn sys_yield(ctx: *mut Context) -> *const Context {
    scheduler::schedule(ctx)
}
/// Unregister interrupt waiters after `sys_wait` returns (any path).
///
/// Safe to call even if the waiter was already cleared by the fire path.
fn unregister_interrupts(ids: &[Option<InterruptId>]) {
    for &id in ids.iter().flatten() {
        interrupt::unregister_waiter(id);
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

    unsafe {
        // AT S1E0R: translate va as a Stage 1 EL0 Read.
        core::arch::asm!(
            "at s1e0r, {va}",
            "isb",
            va = in(reg) va,
            options(nostack)
        );
        core::arch::asm!(
            "mrs {par}, par_el1",
            par = out(reg) par,
            options(nostack, nomem)
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

pub fn dispatch(ctx: *mut Context) -> *const Context {
    let c = unsafe { &mut *ctx };
    let syscall_nr = c.x[8];

    match syscall_nr {
        nr::EXIT => sys_exit(ctx),
        nr::WRITE => {
            c.x[0] = match sys_write(c.x[0], c.x[1]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::HANDLE_CLOSE => {
            c.x[0] = match sys_handle_close(c.x[0]) {
                Ok(n) => n,
                Err(e) => e.into(),
            };

            ctx as *const Context
        }
        nr::CHANNEL_SIGNAL => {
            c.x[0] = match sys_channel_signal(c.x[0]) {
                Ok(n) => n,
                Err(e) => e.into(),
            };

            ctx as *const Context
        }
        nr::WAIT => sys_wait(ctx),
        nr::SCHEDULING_CONTEXT_CREATE => {
            c.x[0] = match sys_scheduling_context_create(c.x[0], c.x[1]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::SCHEDULING_CONTEXT_BORROW => {
            c.x[0] = match sys_scheduling_context_borrow(c.x[0]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::SCHEDULING_CONTEXT_RETURN => {
            c.x[0] = match sys_scheduling_context_return() {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::SCHEDULING_CONTEXT_BIND => {
            c.x[0] = match sys_scheduling_context_bind(c.x[0]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::FUTEX_WAIT => sys_futex_wait(ctx),
        nr::FUTEX_WAKE => {
            c.x[0] = match sys_futex_wake(c.x[0], c.x[1]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::TIMER_CREATE => {
            c.x[0] = match sys_timer_create(c.x[0]) {
                Ok(n) => n,
                Err(e) => e.into(),
            };

            ctx as *const Context
        }
        nr::INTERRUPT_REGISTER => {
            c.x[0] = match sys_interrupt_register(c.x[0]) {
                Ok(n) => n,
                Err(e) => e.into(),
            };

            ctx as *const Context
        }
        nr::INTERRUPT_ACK => {
            c.x[0] = match sys_interrupt_ack(c.x[0]) {
                Ok(n) => n,
                Err(e) => e.into(),
            };

            ctx as *const Context
        }
        nr::DEVICE_MAP => {
            c.x[0] = match sys_device_map(c.x[0], c.x[1]) {
                Ok(n) => n,
                Err(e) => e as i64 as u64,
            };

            ctx as *const Context
        }
        nr::YIELD => sys_yield(ctx),
        _ => {
            c.x[0] = Error::UnknownSyscall as i64 as u64;

            ctx as *const Context
        }
    }
}
