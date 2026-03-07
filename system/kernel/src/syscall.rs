//! Syscall dispatcher and handlers.

use super::scheduler;
use super::uart;
use super::Context;

pub mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
}

#[repr(i64)]
pub enum Error {
    UnknownSyscall = -1,
    BadAddress = -2,
    BadLength = -3,
}

const RAM_START: u64 = 0x4000_0000;
const RAM_END: u64 = 0x5000_0000;
const MAX_WRITE_LEN: u64 = 4096;

fn sys_exit(ctx: *mut Context) -> *const Context {
    scheduler::exit_current_from_syscall(ctx)
}

fn sys_write(buf_ptr: u64, len: u64) -> Result<u64, Error> {
    if len > MAX_WRITE_LEN {
        return Err(Error::BadLength);
    }

    if !(RAM_START..RAM_END).contains(&buf_ptr) {
        return Err(Error::BadAddress);
    }

    let end = buf_ptr.checked_add(len).ok_or(Error::BadAddress)?;

    if end > RAM_END {
        return Err(Error::BadAddress);
    }

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

/// Dispatch a syscall. Called from `svc_handler` with the current Context.
///
/// Returns a pointer to the next thread's Context (may differ from input
/// if the syscall triggers a context switch).
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
        nr::YIELD => sys_yield(ctx),
        _ => {
            c.x[0] = Error::UnknownSyscall as i64 as u64;

            ctx as *const Context
        }
    }
}
