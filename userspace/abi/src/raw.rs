//! Raw syscall invocation — SVC #0 with 6 argument registers.

use crate::types::SyscallError;

pub mod num {
    pub const VMO_CREATE: u64 = 0;
    pub const VMO_MAP: u64 = 1;
    pub const VMO_MAP_INTO: u64 = 2;
    pub const VMO_UNMAP: u64 = 3;
    pub const VMO_SNAPSHOT: u64 = 4;
    pub const VMO_SEAL: u64 = 5;
    pub const VMO_RESIZE: u64 = 6;
    pub const VMO_SET_PAGER: u64 = 7;
    pub const ENDPOINT_CREATE: u64 = 8;
    pub const CALL: u64 = 9;
    pub const RECV: u64 = 10;
    pub const REPLY: u64 = 11;
    pub const EVENT_CREATE: u64 = 12;
    pub const EVENT_SIGNAL: u64 = 13;
    pub const EVENT_WAIT: u64 = 14;
    pub const EVENT_CLEAR: u64 = 15;
    pub const THREAD_CREATE: u64 = 16;
    pub const THREAD_CREATE_IN: u64 = 17;
    pub const THREAD_EXIT: u64 = 18;
    pub const THREAD_SET_PRIORITY: u64 = 19;
    pub const THREAD_SET_AFFINITY: u64 = 20;
    pub const SPACE_CREATE: u64 = 21;
    pub const SPACE_DESTROY: u64 = 22;
    pub const HANDLE_DUP: u64 = 23;
    pub const HANDLE_CLOSE: u64 = 24;
    pub const HANDLE_INFO: u64 = 25;
    pub const CLOCK_READ: u64 = 26;
    pub const SYSTEM_INFO: u64 = 27;
    pub const EVENT_BIND_IRQ: u64 = 28;
    pub const ENDPOINT_BIND_EVENT: u64 = 29;
}

#[inline(always)]
pub fn syscall(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> (u64, u64) {
    let error: u64;
    let value: u64;

    // SAFETY: SVC #0 is the kernel entry point. The kernel validates all
    // arguments and returns a well-defined (error, value) pair.
    unsafe {
        core::arch::asm!(
            "svc #0",
            inout("x8") num => _,
            inout("x0") a0 => error,
            inout("x1") a1 => value,
            inout("x2") a2 => _,
            inout("x3") a3 => _,
            inout("x4") a4 => _,
            inout("x5") a5 => _,
            options(nostack),
        );
    }
    (error, value)
}

#[inline(always)]
pub fn check(result: (u64, u64)) -> Result<u64, SyscallError> {
    let (err, val) = result;

    if err == 0 {
        Ok(val)
    } else {
        Err(SyscallError::from_code(err).unwrap_or(SyscallError::InvalidArgument))
    }
}
