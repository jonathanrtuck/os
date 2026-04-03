//! Minimal userspace runtime for kernel examples.
//!
//! Provides syscall wrappers, a panic handler, and print utilities.
//! Self-contained — no external dependencies beyond `core`.
//!
//! # Syscall ABI (aarch64)
//!
//! | Register | Role            |
//! |----------|-----------------|
//! | x8       | Syscall number  |
//! | x0..x5   | Arguments       |
//! | x0       | Return value    |
//!
//! Invoke via `svc #0`. All registers except x0 are preserved.
//! Negative return values are errors.

#![no_std]

use core::arch::asm;

// ---------------------------------------------------------------------------
// Syscall numbers (must match kernel SYSCALLS.md)
// ---------------------------------------------------------------------------

pub mod nr {
    pub const EXIT: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const YIELD: u64 = 2;
    pub const HANDLE_CLOSE: u64 = 3;
    pub const CHANNEL_CREATE: u64 = 7;
    pub const CHANNEL_SIGNAL: u64 = 8;
    pub const WAIT: u64 = 9;
    pub const FUTEX_WAIT: u64 = 10;
    pub const FUTEX_WAKE: u64 = 11;
    pub const TIMER_CREATE: u64 = 12;
    pub const CLOCK_GET: u64 = 13;
    pub const MEMORY_ALLOC: u64 = 14;
    pub const MEMORY_FREE: u64 = 15;
    pub const VMO_CREATE: u64 = 16;
    pub const VMO_MAP: u64 = 17;
    pub const VMO_READ: u64 = 19;
    pub const VMO_WRITE: u64 = 20;
    pub const EVENT_CREATE: u64 = 28;
    pub const EVENT_SIGNAL: u64 = 29;
    pub const THREAD_CREATE: u64 = 35;
}

// ---------------------------------------------------------------------------
// Raw syscall glue
// ---------------------------------------------------------------------------

#[inline(always)]
pub unsafe fn syscall0(nr: u64) -> u64 {
    let ret: u64;

    // SAFETY: svc #0 traps to the kernel. x8 = syscall number, result in x0.
    // `nostack` is correct: SVC doesn't touch the user stack.
    asm!("svc #0", in("x8") nr, lateout("x0") ret, options(nostack));

    ret
}

#[inline(always)]
pub unsafe fn syscall1(nr: u64, a0: u64) -> u64 {
    let ret: u64;

    // SAFETY: Same as syscall0, with one argument in x0.
    asm!("svc #0", in("x0") a0, in("x8") nr, lateout("x0") ret, options(nostack));

    ret
}

#[inline(always)]
pub unsafe fn syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;

    // SAFETY: Same as syscall0, with arguments in x0, x1.
    asm!("svc #0", in("x0") a0, in("x1") a1, in("x8") nr, lateout("x0") ret, options(nostack));

    ret
}

#[inline(always)]
pub unsafe fn syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;

    // SAFETY: Same as syscall0, with arguments in x0, x1, x2.
    asm!(
        "svc #0",
        in("x0") a0, in("x1") a1, in("x2") a2, in("x8") nr,
        lateout("x0") ret, options(nostack),
    );

    ret
}

#[inline(always)]
pub unsafe fn syscall4(nr: u64, a0: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;

    // SAFETY: Same as syscall0, with arguments in x0, x1, x2, x3.
    asm!(
        "svc #0",
        in("x0") a0, in("x1") a1, in("x2") a2, in("x3") a3, in("x8") nr,
        lateout("x0") ret, options(nostack),
    );

    ret
}

// ---------------------------------------------------------------------------
// High-level syscall wrappers
// ---------------------------------------------------------------------------

/// Terminate the calling thread. Does not return.
pub fn exit() -> ! {
    // SAFETY: EXIT (0) is always valid and never returns.
    unsafe { syscall0(nr::EXIT) };

    loop {}
}

/// Write bytes to the kernel serial console.
/// Returns the number of bytes written, or a negative error.
pub fn write(buf: &[u8]) -> i64 {
    // SAFETY: buf points to valid readable memory within our address space.
    unsafe { syscall2(nr::WRITE, buf.as_ptr() as u64, buf.len() as u64) as i64 }
}

/// Print a string to the serial console.
pub fn print(s: &[u8]) {
    let _ = write(s);
}

/// Print a u64 as decimal.
pub fn print_u64(mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();

    if n == 0 {
        print(b"0");

        return;
    }

    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }

    print(&buf[i..]);
}

/// Print a u64 as hexadecimal with 0x prefix.
pub fn print_hex(n: u64) {
    const HEX: &[u8] = b"0123456789abcdef";

    print(b"0x");

    let mut buf = [0u8; 16];

    for i in 0..16 {
        buf[i] = HEX[((n >> (60 - i * 4)) & 0xf) as usize];
    }

    // Skip leading zeros.
    let start = buf.iter().position(|&b| b != b'0').unwrap_or(15);

    print(&buf[start..]);
}

/// Voluntarily yield the CPU.
pub fn yield_now() {
    // SAFETY: YIELD (2) is always valid, returns 0.
    unsafe { syscall0(nr::YIELD) };
}

/// Read the monotonic clock (nanoseconds since boot).
pub fn clock_get() -> u64 {
    // SAFETY: CLOCK_GET (13) is always valid, returns nanoseconds.
    unsafe { syscall0(nr::CLOCK_GET) }
}

/// Allocate heap pages. Returns the mapped VA or a negative error.
pub fn memory_alloc(page_count: u64) -> Result<*mut u8, i64> {
    // SAFETY: MEMORY_ALLOC maps pages in the heap region.
    let ret = unsafe { syscall1(nr::MEMORY_ALLOC, page_count) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as *mut u8)
    }
}

/// Free heap pages.
pub fn memory_free(va: *mut u8, page_count: u64) -> Result<(), i64> {
    // SAFETY: va must be a page-aligned address previously returned by memory_alloc.
    let ret = unsafe { syscall2(nr::MEMORY_FREE, va as u64, page_count) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

/// Create a bidirectional IPC channel.
/// Returns (handle_a, handle_b) packed as handle_a | (handle_b << 16).
pub fn channel_create() -> Result<(u16, u16), i64> {
    // SAFETY: CHANNEL_CREATE allocates kernel objects, no preconditions.
    let ret = unsafe { syscall0(nr::CHANNEL_CREATE) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        let packed = ret as u64;
        Ok((packed as u16, (packed >> 16) as u16))
    }
}

/// Signal the far endpoint of a channel.
pub fn channel_signal(handle: u16) -> Result<(), i64> {
    // SAFETY: handle must be a valid channel handle with SIGNAL right.
    let ret = unsafe { syscall1(nr::CHANNEL_SIGNAL, handle as u64) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

/// Wait for any handle in the array to become ready.
/// Returns the index of the first ready handle.
pub fn wait(handles: &[u16], timeout_ns: u64) -> Result<u64, i64> {
    // SAFETY: handles must point to valid readable memory containing valid handle indices.
    let ret = unsafe {
        syscall3(
            nr::WAIT,
            handles.as_ptr() as u64,
            handles.len() as u64,
            timeout_ns,
        )
    } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u64)
    }
}

/// Close a handle.
pub fn handle_close(handle: u16) -> Result<(), i64> {
    // SAFETY: handle must be a valid handle index.
    let ret = unsafe { syscall1(nr::HANDLE_CLOSE, handle as u64) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

/// Create a new thread. Returns the thread handle.
pub fn thread_create(entry: extern "C" fn() -> !, stack_top: u64) -> Result<u16, i64> {
    // SAFETY: entry must be a valid code address; stack_top must be 16-byte aligned.
    let ret = unsafe { syscall2(nr::THREAD_CREATE, entry as u64, stack_top) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u16)
    }
}

/// Compare-and-sleep on a futex word. Blocks if *addr == expected.
pub fn futex_wait(addr: &core::sync::atomic::AtomicU32, expected: u32) -> Result<(), i64> {
    // SAFETY: addr is a valid, aligned reference to an AtomicU32 in user memory.
    let ret = unsafe { syscall2(nr::FUTEX_WAIT, addr as *const _ as u64, expected as u64) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

/// Wake threads sleeping on a futex.
pub fn futex_wake(addr: &core::sync::atomic::AtomicU32, count: u32) -> Result<u64, i64> {
    // SAFETY: addr is a valid, aligned reference to an AtomicU32 in user memory.
    let ret = unsafe { syscall2(nr::FUTEX_WAKE, addr as *const _ as u64, count as u64) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u64)
    }
}

/// Create a VMO. Returns the VMO handle.
pub fn vmo_create(size_pages: u64, flags: u64, type_tag: u64) -> Result<u16, i64> {
    // SAFETY: vmo_create allocates kernel objects, no memory preconditions.
    let ret = unsafe { syscall3(nr::VMO_CREATE, size_pages, flags, type_tag) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u16)
    }
}

/// Map a VMO into the calling process. Returns the mapped VA.
pub fn vmo_map(handle: u16, flags: u64) -> Result<*mut u8, i64> {
    // SAFETY: handle must be a valid VMO handle with MAP right.
    let ret = unsafe { syscall3(nr::VMO_MAP, handle as u64, flags, 0) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as *mut u8)
    }
}

/// Read from a VMO into a buffer.
pub fn vmo_read(handle: u16, offset: u64, buf: &mut [u8]) -> Result<u64, i64> {
    // SAFETY: handle must be a valid VMO handle with READ right; buf is writable.
    let ret = unsafe {
        syscall4(
            nr::VMO_READ,
            handle as u64,
            offset,
            buf.as_mut_ptr() as u64,
            buf.len() as u64,
        )
    } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u64)
    }
}

/// Write from a buffer into a VMO.
pub fn vmo_write(handle: u16, offset: u64, buf: &[u8]) -> Result<u64, i64> {
    // SAFETY: handle must be a valid VMO handle with WRITE right; buf is readable.
    let ret = unsafe {
        syscall4(
            nr::VMO_WRITE,
            handle as u64,
            offset,
            buf.as_ptr() as u64,
            buf.len() as u64,
        )
    } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u64)
    }
}

/// Create a one-shot timer. Returns the timer handle.
pub fn timer_create(timeout_ns: u64) -> Result<u16, i64> {
    // SAFETY: timer_create allocates a kernel timer object.
    let ret = unsafe { syscall1(nr::TIMER_CREATE, timeout_ns) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u16)
    }
}

/// Create an event object. Returns the event handle.
pub fn event_create() -> Result<u16, i64> {
    // SAFETY: event_create allocates a kernel event object.
    let ret = unsafe { syscall0(nr::EVENT_CREATE) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(ret as u16)
    }
}

/// Signal an event.
pub fn event_signal(handle: u16) -> Result<(), i64> {
    // SAFETY: handle must be a valid event handle with SIGNAL right.
    let ret = unsafe { syscall1(nr::EVENT_SIGNAL, handle as u64) } as i64;

    if ret < 0 {
        Err(ret)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

/// Unwrap a syscall result, printing the error code and exiting on failure.
pub fn unwrap_or_exit<T>(result: Result<T, i64>, name: &[u8]) -> T {
    match result {
        Ok(v) => v,
        Err(code) => {
            print(b"FATAL: ");
            print(name);
            print(b" failed (error ");
            print_u64((-code) as u64);
            print(b")\n");
            exit()
        }
    }
}

// ---------------------------------------------------------------------------
// Panic handler
// ---------------------------------------------------------------------------

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    print(b"\nPANIC: ");

    if let Some(loc) = info.location() {
        print(loc.file().as_bytes());
        print(b":");
        print_u64(loc.line() as u64);
    }

    print(b"\n");

    exit()
}
