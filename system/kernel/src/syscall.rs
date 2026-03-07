//! Syscall dispatcher and handlers.

use super::channel;
use super::handle::{Handle, HandleError, HandleObject, Rights};
use super::scheduler;
use super::thread::ThreadState;
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

/// User VA range: 0 .. 2^48 (T0SZ=16).
const USER_VA_END: u64 = 0x0001_0000_0000_0000;
const MAX_WRITE_LEN: u64 = 4096;

fn sys_channel_signal(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let thread = scheduler::current_thread();
    let channel_id = match thread.handles.get(Handle(handle_nr as u8), Rights::WRITE) {
        Ok(HandleObject::Channel(id)) => id,
        Err(e) => return Err(e),
    };
    let caller_id = thread.id;

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

    let thread = scheduler::current_thread();
    let channel_id = match thread.handles.get(Handle(handle_nr as u8), Rights::READ) {
        Ok(HandleObject::Channel(id)) => id,
        Err(e) => {
            c.x[0] = e as i64 as u64;

            return ctx as *const Context;
        }
    };
    let caller_id = thread.id;

    if channel::check_pending(channel_id, caller_id) {
        // Signal was pending — consumed. Return immediately.
        c.x[0] = 0;

        ctx as *const Context
    } else {
        // No signal pending. Pre-set return value, block, reschedule.
        c.x[0] = 0;
        thread.state = ThreadState::Blocked;

        scheduler::schedule(ctx)
    }
}
fn sys_exit(ctx: *mut Context) -> *const Context {
    scheduler::exit_current_from_syscall(ctx)
}
fn sys_handle_close(handle_nr: u64) -> Result<u64, HandleError> {
    if handle_nr > u8::MAX as u64 {
        return Err(HandleError::InvalidHandle);
    }

    let thread = scheduler::current_thread();

    thread.handles.close(Handle(handle_nr as u8))?;

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

    // TTBR0 is still loaded during syscall, so kernel can read user pages.
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
