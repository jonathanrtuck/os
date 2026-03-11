//! Fuzz helper — a minimal child process for cross-process tests.
//!
//! Behavior is controlled by the first byte of the config channel message:
//!   0x01 = block forever on channel (wait handle 0, infinite timeout)
//!   0x02 = busy-loop calling yield
//!   0x03 = exit immediately
//!   0x04 = create threads that block, then exit main thread
//!   0x05 = rapid channel create/close loop (resource churn)
//!   otherwise = exit immediately
//!
//! Handle 0 is always the config channel endpoint (received via handle_send).

#![no_std]
#![no_main]

const SHARED_MEMORY_BASE: usize = 0xC000_0000;

extern "C" fn blocking_thread(_: u64) -> ! {
    // Block on a timer that won't fire for a very long time.
    if let Ok(h) = sys::timer_create(999_999_999_999) {
        let _ = sys::wait(&[h], u64::MAX);
    }
    sys::exit();
}

extern "C" fn thread_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack, nomem)
        );
    }
    blocking_thread(args);
}

fn alloc_stack() -> u64 {
    match sys::memory_alloc(2) {
        Ok(va) => (va + 2 * 4096) as u64,
        Err(_) => 0,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Read command from shared memory region (parent wrote via dma_alloc + memory_share).
    let shm = SHARED_MEMORY_BASE as *const u8;
    let cmd = unsafe { core::ptr::read_volatile(shm) };

    match cmd {
        0x01 => {
            sys::print(b"H:01\n");
            // Create a timer to block on (we may not have any handle at slot 0).
            if let Ok(h) = sys::timer_create(999_999_999_999) {
                let _ = sys::wait(&[h], u64::MAX);
            }
            sys::exit();
        }
        0x02 => {
            sys::print(b"H:02\n");
            loop {
                sys::yield_now();
            }
        }
        0x03 => {
            sys::print(b"H:03\n");
            sys::exit();
        }
        0x04 => {
            sys::print(b"H:04\n");
            for _ in 0..3 {
                let stack = alloc_stack();
                if stack == 0 { break; }
                let args_ptr = (stack - 8) as *mut u64;
                unsafe { core::ptr::write_volatile(args_ptr, 0) };
                let _ = sys::thread_create(thread_trampoline as u64, stack - 16);
            }
            sys::exit();
        }
        0x05 => {
            sys::print(b"H:05\n");
            loop {
                if let Ok((a, b)) = sys::channel_create() {
                    let _ = sys::channel_signal(a);
                    let _ = sys::handle_close(a);
                    let _ = sys::handle_close(b);
                }
                sys::yield_now();
            }
        }
        _ => {
            sys::print(b"H:??\n");
            sys::exit();
        }
    }
}
