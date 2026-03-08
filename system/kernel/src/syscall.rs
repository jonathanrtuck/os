//! Syscall dispatcher and handlers.

use super::channel;
use super::handle::{Handle, HandleError, HandleObject, Rights};
use super::paging;
use super::paging::USER_VA_END;
use super::scheduler;
use super::uart;
use super::Context;

pub mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const HANDLE_CLOSE: u64 = 3;
    pub const CHANNEL_SIGNAL: u64 = 4;
    pub const CHANNEL_WAIT: u64 = 5;
}

#[repr(i64)]
pub enum Error {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
}

impl From<HandleError> for u64 {
    fn from(e: HandleError) -> u64 {
        (e as i64) as u64
    }
}

const MAX_WRITE_LEN: u64 = 4096;

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
fn sys_channel_signal(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    // Extract handle info under scheduler lock, then release before channel ops.
    let (channel_id, caller_id) = scheduler::current_thread_do(|thread| {
        let channel_id = match thread.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
            Ok(HandleObject::Channel(id)) => id,
            Err(e) => return Err(e),
        };

        Ok((channel_id, thread.id))
    })?;

    channel::signal(channel_id, caller_id);

    Ok(0)
}
fn sys_channel_wait(ctx: *mut Context) -> *const Context {
    let c = unsafe { &mut *ctx };
    let handle_nr = c.x[0];

    if handle_nr > u8::MAX as u64 {
        c.x[0] = HandleError::InvalidHandle as i64 as u64;

        return ctx as *const Context;
    }

    // Extract handle info under scheduler lock, then release.
    let result = scheduler::current_thread_do(|thread| {
        let channel_id = match thread.handles.get(Handle(handle_nr as u8), Rights::READ) {
            Ok(HandleObject::Channel(id)) => id,
            Err(e) => return Err(e),
        };

        Ok((channel_id, thread.id))
    });
    let (channel_id, caller_id) = match result {
        Ok(pair) => pair,
        Err(e) => {
            c.x[0] = e as i64 as u64;

            return ctx as *const Context;
        }
    };

    if channel::check_pending(channel_id, caller_id) {
        // Signal was pending — consumed. Return immediately.
        c.x[0] = 0;

        ctx as *const Context
    } else {
        // No signal pending. Pre-set return value, block, reschedule.
        c.x[0] = 0;

        scheduler::block_current_and_schedule(ctx)
    }
}
fn sys_exit(ctx: *mut Context) -> *const Context {
    scheduler::exit_current_from_syscall(ctx)
}
fn sys_handle_close(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    scheduler::current_thread_do(|thread| thread.handles.close(Handle(handle_nr as u8)))?;

    Ok(0)
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
            uart::putc(b'\r');
        }

        uart::putc(byte);
    }

    Ok(len)
}
fn sys_yield(ctx: *mut Context) -> *const Context {
    scheduler::schedule(ctx)
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
        nr::CHANNEL_WAIT => sys_channel_wait(ctx),
        nr::YIELD => sys_yield(ctx),
        _ => {
            c.x[0] = Error::UnknownSyscall as i64 as u64;

            ctx as *const Context
        }
    }
}
