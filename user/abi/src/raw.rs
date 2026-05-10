//! Raw syscall invocation — SVC #0 with 6 argument registers.

use crate::types::SyscallError;

pub mod num {
    // VMO (0–8)
    pub const VMO_CREATE: u64 = 0;
    pub const VMO_MAP: u64 = 1;
    pub const VMO_MAP_INTO: u64 = 2;
    pub const VMO_UNMAP: u64 = 3;
    pub const VMO_SNAPSHOT: u64 = 4;
    pub const VMO_SEAL: u64 = 5;
    pub const VMO_RESIZE: u64 = 6;
    pub const VMO_SET_PAGER: u64 = 7;
    pub const VMO_INFO: u64 = 8;
    // Endpoint (9–14)
    pub const ENDPOINT_CREATE: u64 = 9;
    pub const CALL: u64 = 10;
    pub const RECV: u64 = 11;
    pub const REPLY: u64 = 12;
    pub const ENDPOINT_BIND_EVENT: u64 = 13;
    pub const RECV_TIMED: u64 = 14;
    // Event (15–20)
    pub const EVENT_CREATE: u64 = 15;
    pub const EVENT_SIGNAL: u64 = 16;
    pub const EVENT_WAIT: u64 = 17;
    pub const EVENT_CLEAR: u64 = 18;
    pub const EVENT_BIND_IRQ: u64 = 19;
    pub const EVENT_WAIT_DEADLINE: u64 = 20;
    // Thread (21–26)
    pub const THREAD_CREATE: u64 = 21;
    pub const THREAD_CREATE_IN: u64 = 22;
    pub const THREAD_EXIT: u64 = 23;
    pub const THREAD_SET_PRIORITY: u64 = 24;
    pub const THREAD_SET_AFFINITY: u64 = 25;
    pub const THREAD_YIELD: u64 = 26;
    // Space (27–28)
    pub const SPACE_CREATE: u64 = 27;
    pub const SPACE_DESTROY: u64 = 28;
    // Handle (29–31)
    pub const HANDLE_DUP: u64 = 29;
    pub const HANDLE_CLOSE: u64 = 30;
    pub const HANDLE_INFO: u64 = 31;
    // System (32–33)
    pub const CLOCK_READ: u64 = 32;
    pub const SYSTEM_INFO: u64 = 33;
    pub const EVENT_BIND_THREAD: u64 = 34;
}

#[inline(always)]
pub fn syscall(num: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> (u64, u64) {
    let error: u64;
    let value: u64;

    // SAFETY: SVC #0 is the kernel entry point. The SVC fast path
    // rearranges registers (moves x8 to x6) and calls into Rust, which
    // may clobber any caller-saved register. Only x0 (error) and x1
    // (value) are defined on return. Callee-saved registers (x19-x28)
    // are preserved by the kernel's Rust calling convention.
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
            out("x6") _,
            out("x7") _,
            out("x9") _,
            out("x10") _,
            out("x11") _,
            out("x12") _,
            out("x13") _,
            out("x14") _,
            out("x15") _,
            out("x16") _,
            out("x17") _,
            out("x18") _,
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
