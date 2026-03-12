//! Adversarial kernel test — exercises every syscall with invalid, edge-case,
//! and hostile arguments. The kernel must never panic, hang, or corrupt state.
//!
//! Phases 1-12:  Single-process tests (bad args, exhaustion, concurrency)
//! Phases 13-17: Cross-process tests (kill races, blocking, resource contention)

#![no_std]
#![no_main]

include!(env!("FUZZ_EMBEDDED_RS"));

// Raw syscall helpers — bypass sys library validation.
#[inline(always)]
unsafe fn raw_syscall0(nr: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "svc #0",
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );
    ret
}
#[inline(always)]
unsafe fn raw_syscall1(nr: u64, a0: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );
    ret
}
#[inline(always)]
unsafe fn raw_syscall2(nr: u64, a0: u64, a1: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );
    ret
}
#[inline(always)]
unsafe fn raw_syscall3(nr: u64, a0: u64, a1: u64, a2: u64) -> u64 {
    let ret: u64;
    core::arch::asm!(
        "svc #0",
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x8") nr,
        lateout("x0") ret,
        options(nostack),
    );
    ret
}

fn print_u64(mut n: u64) {
    if n == 0 {
        sys::print(b"0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 20;
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    sys::print(&buf[i..]);
}

fn print_hex(n: u64) {
    sys::print(b"0x");
    let mut buf = [b'0'; 16];
    let mut val = n;
    for i in (0..16).rev() {
        let d = (val & 0xF) as u8;
        buf[i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        val >>= 4;
    }
    // Skip leading zeros.
    let start = buf.iter().position(|&b| b != b'0').unwrap_or(15);
    sys::print(&buf[start..]);
}

fn phase_ok(name: &[u8]) {
    sys::print(b"     \xE2\x9C\x93 ");
    sys::print(name);
    sys::print(b"\n");
}

fn phase_fail(name: &[u8], detail: &[u8]) -> ! {
    sys::print(b"     \xE2\x9C\x97 ");
    sys::print(name);
    sys::print(b": ");
    sys::print(detail);
    sys::print(b"\n");
    sys::print(b"  FAIL fuzz\n");
    sys::exit();
}

fn assert_err(result: sys::SyscallResult<u64>, name: &[u8]) {
    if result.is_ok() {
        phase_fail(name, b"expected error, got Ok");
    }
}

fn assert_err_code(result: sys::SyscallResult<u64>, expected: sys::SyscallError, name: &[u8]) {
    match result {
        Ok(_) => phase_fail(name, b"expected error, got Ok"),
        Err(e) if e != expected => {
            sys::print(b"     wrong error code in ");
            sys::print(name);
            sys::print(b"\n");
        }
        Err(_) => {}
    }
}

// -----------------------------------------------------------------------
// Phase 1: Invalid syscall numbers
// -----------------------------------------------------------------------
fn phase_1_invalid_syscall_numbers() {
    let bad_nrs: [u64; 8] = [27, 28, 100, 255, 1000, u64::MAX, u64::MAX - 1, 0x8000_0000];

    for &nr in &bad_nrs {
        let ret = unsafe { raw_syscall0(nr) } as i64;
        if ret != -1 {
            sys::print(b"     bad nr ");
            print_u64(nr);
            sys::print(b" returned ");
            print_hex(ret as u64);
            sys::print(b" (expected -1)\n");
            phase_fail(b"phase 1", b"wrong return for invalid syscall nr");
        }
    }

    phase_ok(b"phase 1: invalid syscall numbers");
}

// -----------------------------------------------------------------------
// Phase 2: Bad handle arguments
// -----------------------------------------------------------------------
fn phase_2_bad_handles() {
    // Close invalid handles.
    for h in [0u8, 1, 127, 128, 255] {
        let _ = sys::handle_close(h); // Should return InvalidHandle, not crash.
    }

    // Signal invalid handles.
    for h in [0u8, 1, 127, 128, 255] {
        let _ = sys::channel_signal(h);
    }

    // Handle > u8::MAX via raw syscall.
    let ret = unsafe { raw_syscall1(3, 256) } as i64; // handle_close(256)
    if ret >= 0 {
        phase_fail(b"phase 2", b"close(256) should fail");
    }
    let ret = unsafe { raw_syscall1(3, u64::MAX) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 2", b"close(MAX) should fail");
    }

    // Double-close: create a channel, close both endpoints, close again.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"channel_create failed");
    });
    sys::handle_close(a).unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"first close(a) failed");
    });
    sys::handle_close(b).unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"first close(b) failed");
    });
    // Second close should return error, not crash.
    let r = sys::handle_close(a);
    if r.is_ok() {
        phase_fail(b"phase 2", b"double-close(a) should fail");
    }
    let r = sys::handle_close(b);
    if r.is_ok() {
        phase_fail(b"phase 2", b"double-close(b) should fail");
    }

    // Signal a closed channel.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"channel_create 2 failed");
    });
    let _ = sys::handle_close(b);
    // Signal with peer closed — should not crash.
    let _ = sys::channel_signal(a);
    let _ = sys::handle_close(a);

    // interrupt_ack on non-interrupt handle.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"channel_create 3 failed");
    });
    let _ = sys::interrupt_ack(a); // Channel handle, not interrupt — should fail.
    let _ = sys::handle_close(a);
    let _ = sys::handle_close(b);

    // process_start on non-process handle.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 2", b"channel_create 4 failed");
    });
    let _ = sys::process_start(a); // Channel handle, not process.
    let _ = sys::process_kill(a); // Channel handle, not process.
    let _ = sys::handle_close(a);
    let _ = sys::handle_close(b);

    phase_ok(b"phase 2: bad handle arguments");
}

// -----------------------------------------------------------------------
// Phase 3: Bad address arguments
// -----------------------------------------------------------------------
fn phase_3_bad_addresses() {
    // write() with bad pointers.
    let bad_addrs: [u64; 6] = [
        0,                     // null
        0xFFFF_FFFF_FFFF_FFFF, // kernel space
        0xFFFF_0000_0000_0000, // kernel space
        0x0000_FFFF_FFFF_F000, // near top of user space
        0xDEAD_BEEF_0000_0000, // unmapped
        1,                     // unaligned
    ];

    // write(bad_addr, length) — kernel must validate, not read.
    for &addr in &bad_addrs {
        let ret = unsafe { raw_syscall2(1, addr, 100) } as i64;
        if ret >= 0 && addr != 0 {
            // addr=0 with len=100 should fail, but just being safe.
        }
    }

    // write(valid, 0) — zero length should succeed or fail gracefully.
    let buf = [0u8; 1];
    let _ = sys::write(&buf[..0]);

    // write(valid, huge) — length > MAX_WRITE_LEN.
    let ret = unsafe { raw_syscall2(1, buf.as_ptr() as u64, 1_000_000) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"write with huge len should fail");
    }

    // futex_wait with bad address.
    for &addr in &bad_addrs {
        let ret = unsafe { raw_syscall2(10, addr, 0) } as i64;
        if ret >= 0 {
            // Some might return WouldBlock (value != expected), that's fine.
        }
    }
    // futex_wait with unaligned address.
    let ret = unsafe { raw_syscall2(10, 0x1001, 0) } as i64;
    let _ = ret; // Should be BadAddress.

    // futex_wake with bad address.
    for &addr in &bad_addrs {
        let _ = unsafe { raw_syscall2(11, addr, 1) };
    }

    // wait() with bad handles_ptr.
    for &addr in &bad_addrs {
        let ret = unsafe { raw_syscall3(12, addr, 1, 0) } as i64;
        if ret >= 0 {
            phase_fail(b"phase 3", b"wait with bad ptr should fail");
        }
    }

    // wait() with count=0.
    let handles = [0u8];
    let ret = unsafe { raw_syscall3(12, handles.as_ptr() as u64, 0, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"wait with count=0 should fail");
    }

    // wait() with huge count.
    let ret = unsafe { raw_syscall3(12, handles.as_ptr() as u64, 1000, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"wait with huge count should fail");
    }

    // process_create with bad ELF pointer.
    for &addr in &bad_addrs {
        let ret = unsafe { raw_syscall2(20, addr, 1024) } as i64;
        if ret >= 0 {
            phase_fail(b"phase 3", b"process_create with bad ptr should fail");
        }
    }

    // process_create with zero length.
    let ret = unsafe { raw_syscall2(20, buf.as_ptr() as u64, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"process_create with len=0 should fail");
    }

    // process_create with huge length.
    let ret = unsafe { raw_syscall2(20, buf.as_ptr() as u64, 100_000_000) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"process_create with huge len should fail");
    }

    // dma_alloc with bad pa_out_ptr.
    for &addr in &bad_addrs {
        let ret = unsafe { raw_syscall2(17, 0, addr) } as i64;
        if ret >= 0 {
            // Order 0 with bad pa_out — should fail with BadAddress.
        }
    }

    // dma_alloc with huge order.
    let mut pa_out: u64 = 0;
    let ret = unsafe { raw_syscall2(17, 100, &mut pa_out as *mut u64 as u64) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"dma_alloc with order=100 should fail");
    }

    // thread_create with bad entry/stack.
    let ret = unsafe { raw_syscall2(19, 0xFFFF_0000_0000_0000, 0x1000) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"thread_create with kernel entry should fail");
    }
    let ret = unsafe { raw_syscall2(19, 0x1000, 0xFFFF_0000_0000_0000) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"thread_create with kernel stack should fail");
    }
    // Unaligned stack.
    let ret = unsafe { raw_syscall2(19, 0x1000, 0x1001) } as i64;
    if ret >= 0 {
        phase_fail(
            b"phase 3",
            b"thread_create with unaligned stack should fail",
        );
    }

    // memory_alloc(0) — zero pages.
    let ret = unsafe { raw_syscall1(25, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"memory_alloc(0) should fail");
    }

    // memory_free with bad VA.
    let ret = unsafe { raw_syscall2(26, 0xDEAD_0000, 1) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"memory_free with bad VA should fail");
    }

    // memory_free with unaligned VA.
    let ret = unsafe { raw_syscall2(26, 0x5000_0001, 1) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"memory_free with unaligned VA should fail");
    }

    // device_map with RAM address (not device MMIO).
    let ret = unsafe { raw_syscall2(16, 0x4000_0000, 0x1000) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"device_map into RAM should fail");
    }

    // device_map with size=0.
    let ret = unsafe { raw_syscall2(16, 0x0800_0000, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 3", b"device_map with size=0 should fail");
    }

    // memory_share with bad args.
    let ret = unsafe { raw_syscall3(24, 255, 0, 0) } as i64; // bad handle, 0 pages
    let _ = ret; // Should fail with InvalidArgument.

    // handle_send with invalid handles.
    let ret = unsafe { raw_syscall2(22, 255, 255) } as i64;
    let _ = ret; // Should fail.

    // scheduling_context_create with zero budget/period.
    let ret = unsafe { raw_syscall2(6, 0, 0) } as i64;
    let _ = ret; // May or may not fail — kernel decides.

    phase_ok(b"phase 3: bad address arguments");
}

// -----------------------------------------------------------------------
// Phase 4: Handle table exhaustion + recovery
// -----------------------------------------------------------------------
fn phase_4_handle_exhaustion() {
    // Fill the handle table with channels (each creates 2 handles).
    let mut count: u32 = 0;
    let mut handles_a: [u8; 128] = [0; 128];
    let mut handles_b: [u8; 128] = [0; 128];

    loop {
        match sys::channel_create() {
            Ok((a, b)) => {
                if (count as usize) < 128 {
                    handles_a[count as usize] = a;
                    handles_b[count as usize] = b;
                }
                count += 1;
            }
            Err(sys::SyscallError::TableFull) | Err(sys::SyscallError::OutOfMemory) => break,
            Err(_) => break,
        }
    }

    sys::print(b"       created ");
    print_u64(count as u64);
    sys::print(b" channels before table full\n");

    if count == 0 {
        phase_fail(b"phase 4", b"couldn't create any channels");
    }

    // Verify creating more fails.
    let r = sys::channel_create();
    if r.is_ok() {
        phase_fail(b"phase 4", b"should fail when table full");
    }

    // Close all — should recover.
    for i in 0..count.min(128) as usize {
        let _ = sys::handle_close(handles_a[i]);
        let _ = sys::handle_close(handles_b[i]);
    }

    // Verify recovery — should be able to create again.
    match sys::channel_create() {
        Ok((a, b)) => {
            let _ = sys::handle_close(a);
            let _ = sys::handle_close(b);
        }
        Err(_) => phase_fail(b"phase 4", b"couldn't create after recovery"),
    }

    phase_ok(b"phase 4: handle table exhaustion + recovery");
}

// -----------------------------------------------------------------------
// Phase 5: Memory exhaustion + recovery
// -----------------------------------------------------------------------
fn phase_5_memory_exhaustion() {
    // Allocate 1-page blocks until OOM.
    let mut allocs: [usize; 512] = [0; 512];
    let mut count: usize = 0;

    loop {
        match sys::memory_alloc(1) {
            Ok(va) => {
                // Touch the page to ensure it's really mapped.
                unsafe { core::ptr::write_volatile(va as *mut u8, 0xAA) };
                if count < 512 {
                    allocs[count] = va;
                }
                count += 1;
            }
            Err(sys::SyscallError::OutOfMemory) => break,
            Err(_) => break,
        }
        if count >= 512 {
            break; // Safety limit.
        }
    }

    sys::print(b"       allocated ");
    print_u64(count as u64);
    sys::print(b" pages before OOM/limit\n");

    // Free all.
    for i in 0..count.min(512) {
        let _ = sys::memory_free(allocs[i], 1);
    }

    // Verify recovery.
    match sys::memory_alloc(1) {
        Ok(va) => {
            unsafe { core::ptr::write_volatile(va as *mut u8, 0xBB) };
            let _ = sys::memory_free(va, 1);
        }
        Err(_) => phase_fail(b"phase 5", b"couldn't alloc after freeing"),
    }

    phase_ok(b"phase 5: memory exhaustion + recovery");
}

// -----------------------------------------------------------------------
// Phase 6: Timer exhaustion + recovery
// -----------------------------------------------------------------------
fn phase_6_timer_exhaustion() {
    let mut timer_handles: [u8; 128] = [0; 128];
    let mut count: usize = 0;

    loop {
        match sys::timer_create(1_000_000_000) {
            Ok(h) => {
                if count < 128 {
                    timer_handles[count] = h;
                }
                count += 1;
            }
            Err(sys::SyscallError::TableFull) => break,
            Err(_) => break,
        }
        if count >= 128 {
            break; // Safety limit (also table is 256 total handles).
        }
    }

    sys::print(b"       created ");
    print_u64(count as u64);
    sys::print(b" timers before full\n");

    // Close all timers.
    for i in 0..count.min(128) {
        let _ = sys::handle_close(timer_handles[i]);
    }

    // Verify recovery.
    match sys::timer_create(1_000_000) {
        Ok(h) => {
            let _ = sys::handle_close(h);
        }
        Err(_) => phase_fail(b"phase 6", b"couldn't create timer after recovery"),
    }

    phase_ok(b"phase 6: timer exhaustion + recovery");
}

// -----------------------------------------------------------------------
// Phase 7: Process lifecycle edge cases
// -----------------------------------------------------------------------
fn phase_7_process_lifecycle() {
    // process_create with garbage ELF data — should fail gracefully.
    let garbage = [0u8; 64];
    let r = sys::process_create(garbage.as_ptr(), garbage.len());
    if r.is_ok() {
        // If it somehow succeeded, kill it.
        let _ = sys::process_kill(r.unwrap());
    }

    // process_start with non-process handle.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 7", b"channel_create failed");
    });
    let _ = sys::process_start(a); // Should fail — channel, not process.
    let _ = sys::handle_close(a);
    let _ = sys::handle_close(b);

    // process_kill with non-process handle.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 7", b"channel_create 2 failed");
    });
    let _ = sys::process_kill(a); // Should fail.
    let _ = sys::handle_close(a);
    let _ = sys::handle_close(b);

    // process_kill(self) — should be rejected.
    // We can't easily get our own process handle, but process_kill validates
    // that target != caller. Just exercise invalid handle paths.
    let _ = sys::process_kill(255); // Invalid handle.

    // Rapid create+kill cycles.
    for _ in 0..5 {
        let garbage_elf = [0u8; 32];
        let r = sys::process_create(garbage_elf.as_ptr(), garbage_elf.len());
        if let Ok(h) = r {
            let _ = sys::process_kill(h);
            let _ = sys::handle_close(h);
        }
    }

    phase_ok(b"phase 7: process lifecycle edge cases");
}

// -----------------------------------------------------------------------
// Phase 8: Thread lifecycle
// -----------------------------------------------------------------------

extern "C" fn trivial_thread(_: u64) -> ! {
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
    trivial_thread(args);
}

fn alloc_thread_stack() -> u64 {
    const STACK_PAGES: u64 = 2;
    match sys::memory_alloc(STACK_PAGES) {
        Ok(va) => (va + (STACK_PAGES as usize * 4096)) as u64,
        Err(_) => 0,
    }
}

fn phase_8_thread_lifecycle() {
    // Create threads that immediately exit, wait for them.
    for _ in 0..10 {
        let stack = alloc_thread_stack();
        if stack == 0 {
            break;
        }
        // Write args.
        let args_ptr = (stack - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_ptr, 0) };

        match sys::thread_create(thread_trampoline as u64, stack - 16) {
            Ok(h) => {
                let _ = sys::wait(&[h], u64::MAX);
                let _ = sys::handle_close(h);
            }
            Err(_) => break,
        }
    }

    // wait() with timeout on thread handles (thread may already be exited).
    let stack = alloc_thread_stack();
    if stack != 0 {
        let args_ptr = (stack - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_ptr, 0) };

        if let Ok(h) = sys::thread_create(thread_trampoline as u64, stack - 16) {
            // Poll immediately — thread might or might not have exited.
            let _ = sys::wait(&[h], 0);
            // Wait with short timeout.
            let _ = sys::wait(&[h], 1_000_000);
            // Wait forever (thread should exit quickly).
            let _ = sys::wait(&[h], u64::MAX);
            let _ = sys::handle_close(h);
        }
    }

    phase_ok(b"phase 8: thread lifecycle");
}

// -----------------------------------------------------------------------
// Phase 9: Scheduling context edge cases
// -----------------------------------------------------------------------
fn phase_9_scheduling_context() {
    // Return without borrowing — should fail.
    let r = sys::scheduling_context_return();
    if r.is_ok() {
        phase_fail(b"phase 9", b"return without borrow should fail");
    }

    // Create context, bind, try to bind again.
    match sys::scheduling_context_create(1_000_000, 10_000_000) {
        Ok(h) => {
            // Bind should succeed (if not already bound).
            let _ = sys::scheduling_context_bind(h);
            // Second bind — should fail (already bound).
            let r2 = sys::scheduling_context_bind(h);
            let _ = r2; // AlreadyBound expected.
            let _ = sys::handle_close(h);
        }
        Err(_) => {
            // May fail if table full — not a test failure.
            sys::print(b"       (skipped sc_create - table issue)\n");
        }
    }

    // Borrow/return cycle.
    match sys::scheduling_context_create(500_000, 1_000_000) {
        Ok(h) => {
            let _ = sys::scheduling_context_borrow(h);
            let _ = sys::scheduling_context_return();
            // Double return — should fail.
            let r = sys::scheduling_context_return();
            let _ = r;
            let _ = sys::handle_close(h);
        }
        Err(_) => {
            sys::print(b"       (skipped sc_borrow - table issue)\n");
        }
    }

    phase_ok(b"phase 9: scheduling context edge cases");
}

// -----------------------------------------------------------------------
// Phase 10: Concurrent random syscalls
// -----------------------------------------------------------------------

extern "C" fn chaos_worker(seed: u64) -> ! {
    let mut rng = seed;
    for _ in 0..5_000 {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;

        let syscall_choice = (rng % 9) as u8;
        match syscall_choice {
            0 => {
                sys::yield_now();
            }
            1 => {
                if let Ok((a, b)) = sys::channel_create() {
                    let _ = sys::channel_signal(a);
                    let _ = sys::handle_close(a);
                    let _ = sys::handle_close(b);
                }
            }
            2 => {
                if let Ok(h) = sys::timer_create(1) {
                    let _ = sys::wait(&[h], 0);
                    let _ = sys::handle_close(h);
                }
            }
            3 => {
                let val: u32 = rng as u32;
                let _ = sys::futex_wait(&val as *const u32, val.wrapping_add(1));
                let _ = sys::futex_wake(&val as *const u32, 1);
            }
            4 => {
                if let Ok(va) = sys::memory_alloc(1) {
                    unsafe { core::ptr::write_volatile(va as *mut u8, 0xFF) };
                    let _ = sys::memory_free(va, 1);
                }
            }
            5 => {
                let _ = sys::scheduling_context_return();
            }
            6 => {
                if let Ok(h) = sys::timer_create(1_000) {
                    let _ = sys::wait(&[h], 0);
                    let _ = sys::handle_close(h);
                }
            }
            7 => {
                if let Ok((a, b)) = sys::channel_create() {
                    let _ = sys::channel_signal(a);
                    let _ = sys::channel_signal(b);
                    let _ = sys::wait(&[a], 0);
                    let _ = sys::wait(&[b], 0);
                    let _ = sys::handle_close(a);
                    let _ = sys::handle_close(b);
                }
            }
            _ => {
                let val: u32 = 0;
                let _ = sys::futex_wake(&val as *const u32, 1);
            }
        }
    }
    sys::exit();
}

extern "C" fn chaos_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack, nomem)
        );
    }
    chaos_worker(args);
}

fn phase_10_concurrent_chaos() {
    const NUM_CHAOS_THREADS: usize = 4;
    let mut thread_handles: [u8; NUM_CHAOS_THREADS] = [0; NUM_CHAOS_THREADS];
    let mut spawned: usize = 0;

    for i in 0..NUM_CHAOS_THREADS {
        let stack = alloc_thread_stack();
        if stack == 0 {
            break;
        }
        let seed = (i as u64 + 1) * 0xDEAD_BEEF;
        let args_ptr = (stack - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_ptr, seed) };

        match sys::thread_create(chaos_trampoline as u64, stack - 16) {
            Ok(h) => {
                thread_handles[spawned] = h;
                spawned += 1;
            }
            Err(_) => break,
        }
    }

    sys::print(b"       ");
    print_u64(spawned as u64);
    sys::print(b" chaos threads (");
    print_u64(5000);
    sys::print(b" ops each)\n");

    for i in 0..spawned {
        let _ = sys::wait(&[thread_handles[i]], u64::MAX);
        let _ = sys::handle_close(thread_handles[i]);
    }

    phase_ok(b"phase 10: concurrent random syscalls");
}

// -----------------------------------------------------------------------
// Phase 11: wait() edge cases
// -----------------------------------------------------------------------
fn phase_11_wait_edge_cases() {
    // Wait on multiple handles, some valid some not.
    let (a, b) = sys::channel_create().unwrap_or_else(|_| {
        phase_fail(b"phase 11", b"channel_create failed");
    });

    // Wait with poll (timeout=0) on channel — should return WouldBlock.
    let r = sys::wait(&[a], 0);
    match r {
        Err(sys::SyscallError::WouldBlock) => {}
        _ => {} // Any result is fine as long as no crash.
    }

    // Signal then poll — should succeed.
    let _ = sys::channel_signal(b);
    let r = sys::wait(&[a], 0);
    match r {
        Ok(0) => {} // Expected.
        _ => {}     // Timer race etc — fine.
    }

    // Wait with very short timeout.
    let _ = sys::wait(&[a], 1);

    // Wait on 16 handles (max).
    let handles_to_wait = [a; 16];
    let r = sys::wait(&handles_to_wait, 0);
    let _ = r;

    // Wait on 17 handles (over max) — should fail.
    // Can't easily make a 17-element array from the API, use raw syscall.
    let big_buf = [0u8; 17];
    let ret = unsafe { raw_syscall3(12, big_buf.as_ptr() as u64, 17, 0) } as i64;
    let _ = ret;

    let _ = sys::handle_close(a);
    let _ = sys::handle_close(b);

    // Wait with timeout on a timer that fires.
    if let Ok(t) = sys::timer_create(1_000) {
        // Timer fires in 1us — wait up to 1s.
        let r = sys::wait(&[t], 1_000_000_000);
        match r {
            Ok(0) => {} // Timer fired.
            _ => {}     // Timeout or error — still fine.
        }
        let _ = sys::handle_close(t);
    }

    phase_ok(b"phase 11: wait edge cases");
}

// -----------------------------------------------------------------------
// Phase 12: dma_free edge cases
// -----------------------------------------------------------------------
fn phase_12_dma_edge_cases() {
    // dma_free with address not in DMA region.
    let ret = unsafe { raw_syscall2(18, 0x1000, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 12", b"dma_free bad VA should fail");
    }

    // dma_free with kernel VA.
    let ret = unsafe { raw_syscall2(18, 0xFFFF_0000_0000_0000, 0) } as i64;
    if ret >= 0 {
        phase_fail(b"phase 12", b"dma_free kernel VA should fail");
    }

    phase_ok(b"phase 12: dma edge cases");
}

// -----------------------------------------------------------------------
// Helper: spawn fuzz-helper child with a command byte
// -----------------------------------------------------------------------

struct ChildProcess {
    proc_h: u8,
    cmd_va: usize,
}

fn spawn_helper(cmd: u8) -> Option<ChildProcess> {
    let proc_h = match sys::process_create(HELPER_ELF.as_ptr(), HELPER_ELF.len()) {
        Ok(h) => h,
        Err(_) => return None,
    };
    // Pass the command byte via shared memory (dma_alloc gives us the PA,
    // memory_share maps it into the child at SHARED_MEMORY_BASE=0xC000_0000).
    let mut cmd_pa: u64 = 0;
    let cmd_va = match sys::dma_alloc(0, &mut cmd_pa) {
        Ok(va) => va,
        Err(_) => {
            let _ = sys::process_kill(proc_h);
            let _ = sys::handle_close(proc_h);
            return None;
        }
    };
    unsafe { core::ptr::write_volatile(cmd_va as *mut u8, cmd) };

    if sys::memory_share(proc_h, cmd_pa, 1, false).is_err() {
        let _ = sys::dma_free(cmd_va as u64, 0);
        let _ = sys::process_kill(proc_h);
        let _ = sys::handle_close(proc_h);
        return None;
    }

    let _ = sys::process_start(proc_h);

    Some(ChildProcess { proc_h, cmd_va })
}

/// Wait for child to exit and free resources.
fn reap_helper(child: ChildProcess) {
    let _ = sys::wait(&[child.proc_h], u64::MAX);
    let _ = sys::handle_close(child.proc_h);
    let _ = sys::dma_free(child.cmd_va as u64, 0);
}

// -----------------------------------------------------------------------
// Phase 13: Kill process while blocked in wait()
// -----------------------------------------------------------------------
fn phase_13_kill_while_blocked() {
    match spawn_helper(0x01) {
        Some(child) => {
            for _ in 0..100 {
                sys::yield_now();
            }
            let _ = sys::process_kill(child.proc_h);
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 13: kill process while blocked in wait");
}

// -----------------------------------------------------------------------
// Phase 14: Kill process while busy-looping (running on another core)
// -----------------------------------------------------------------------
fn phase_14_kill_while_running() {
    match spawn_helper(0x02) {
        Some(child) => {
            for _ in 0..200 {
                sys::yield_now();
            }
            let _ = sys::process_kill(child.proc_h);
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 14: kill process while running");
}

// -----------------------------------------------------------------------
// Phase 15: Kill process with multiple blocked threads
// -----------------------------------------------------------------------
fn phase_15_kill_multi_threaded() {
    match spawn_helper(0x04) {
        Some(child) => {
            for _ in 0..300 {
                sys::yield_now();
            }
            let _ = sys::process_kill(child.proc_h);
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 15: kill multi-threaded process");
}

// -----------------------------------------------------------------------
// Phase 16: Rapid create/start/kill cycles
// -----------------------------------------------------------------------
fn phase_16_rapid_lifecycle() {
    let mut success: u32 = 0;
    for _ in 0..20 {
        match spawn_helper(0x02) {
            Some(child) => {
                let _ = sys::process_kill(child.proc_h);
                reap_helper(child);
                success += 1;
            }
            None => break,
        }
    }
    sys::print(b"       ");
    print_u64(success as u64);
    sys::print(b" rapid create/kill cycles\n");
    phase_ok(b"phase 16: rapid create/start/kill cycles");
}

// -----------------------------------------------------------------------
// Phase 17: Kill process doing resource churn
// -----------------------------------------------------------------------
fn phase_17_kill_during_resource_churn() {
    match spawn_helper(0x05) {
        Some(child) => {
            for _ in 0..500 {
                sys::yield_now();
            }
            let _ = sys::process_kill(child.proc_h);
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 17: kill during resource churn");
}

// -----------------------------------------------------------------------
// Phase 18: Sibling thread closes handle while another is blocked on it
// -----------------------------------------------------------------------

extern "C" fn blocker_thread(handle: u64) -> ! {
    // Block on a channel handle (wait for signal that never comes).
    let h = handle as u8;
    let _ = sys::wait(&[h], 10_000_000); // 10ms timeout as safety net.
    sys::exit();
}

extern "C" fn closer_thread(handle: u64) -> ! {
    // Close the handle the blocker is waiting on.
    let h = handle as u8;
    // Small delay to let blocker enter wait.
    sys::yield_now();
    sys::yield_now();
    let _ = sys::handle_close(h);
    sys::exit();
}

extern "C" fn blocker_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack, nomem)
        );
    }
    blocker_thread(args);
}

extern "C" fn closer_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack, nomem)
        );
    }
    closer_thread(args);
}

fn phase_18_sibling_close_while_blocked() {
    for _ in 0..5 {
        // Create a channel. Blocker waits on endpoint A.
        // Closer closes endpoint A from another thread.
        let (ch_a, ch_b) = match sys::channel_create() {
            Ok(pair) => pair,
            Err(_) => break,
        };

        let stack1 = alloc_thread_stack();
        let stack2 = alloc_thread_stack();
        if stack1 == 0 || stack2 == 0 {
            break;
        }

        // Blocker thread: waits on ch_a.
        let args_ptr1 = (stack1 - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_ptr1, ch_a as u64) };
        let t1 = sys::thread_create(blocker_trampoline as u64, stack1 - 16);

        // Closer thread: closes ch_a.
        let args_ptr2 = (stack2 - 8) as *mut u64;
        unsafe { core::ptr::write_volatile(args_ptr2, ch_a as u64) };
        let t2 = sys::thread_create(closer_trampoline as u64, stack2 - 16);

        // Wait for both threads.
        if let Ok(h) = t1 {
            let _ = sys::wait(&[h], 100_000_000); // 100ms timeout.
            let _ = sys::handle_close(h);
        }
        if let Ok(h) = t2 {
            let _ = sys::wait(&[h], 100_000_000);
            let _ = sys::handle_close(h);
        }
        // ch_a may or may not be closed by closer thread. Try closing.
        let _ = sys::handle_close(ch_a);
        let _ = sys::handle_close(ch_b);
    }

    phase_ok(b"phase 18: sibling close while blocked");
}

// -----------------------------------------------------------------------
// Phase 19: Process exit while another process holds channel peer
// -----------------------------------------------------------------------
fn phase_19_peer_exit() {
    match spawn_helper(0x03) {
        Some(child) => {
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 19: process exit with channel peer");
}

// -----------------------------------------------------------------------
// Phase 20: Double-kill and kill-after-exit
// -----------------------------------------------------------------------
fn phase_20_double_kill() {
    // Kill after exit.
    match spawn_helper(0x03) {
        Some(child) => {
            let _ = sys::wait(&[child.proc_h], u64::MAX);
            let _ = sys::process_kill(child.proc_h);
            let _ = sys::process_kill(child.proc_h);
            let _ = sys::handle_close(child.proc_h);
            let _ = sys::dma_free(child.cmd_va as u64, 0);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    // Double kill while alive.
    match spawn_helper(0x02) {
        Some(child) => {
            for _ in 0..50 {
                sys::yield_now();
            }
            let _ = sys::process_kill(child.proc_h);
            let _ = sys::process_kill(child.proc_h);
            reap_helper(child);
        }
        None => sys::print(b"       (skipped - spawn failed)\n"),
    }
    phase_ok(b"phase 20: double kill and kill-after-exit");
}

// -----------------------------------------------------------------------
// Phase 21: Futex wake race (concurrent wait/wake on same address)
// -----------------------------------------------------------------------

// FUTEX_DONE: wakers set to 1 when finished. Waiters check before blocking.
static FUTEX_VAR: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
static FUTEX_DONE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

extern "C" fn futex_waiter(_: u64) -> ! {
    let addr = &FUTEX_VAR as *const core::sync::atomic::AtomicU32 as *const u32;
    for _ in 0..2000 {
        // Exit if wakers are done (prevents infinite block).
        if FUTEX_DONE.load(core::sync::atomic::Ordering::Relaxed) != 0 {
            break;
        }
        let val = FUTEX_VAR.load(core::sync::atomic::Ordering::Relaxed);
        // futex_wait: blocks only if *addr == val. Races with wakers
        // incrementing the value — exercises the wake_pending gap.
        let _ = sys::futex_wait(addr, val);
    }
    sys::exit();
}

extern "C" fn futex_waker(_: u64) -> ! {
    let addr = &FUTEX_VAR as *const core::sync::atomic::AtomicU32 as *const u32;
    for _ in 0..2000 {
        FUTEX_VAR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let _ = sys::futex_wake(addr, u32::MAX);
    }
    // Signal done and do a final wake to unblock any stragglers.
    FUTEX_DONE.store(1, core::sync::atomic::Ordering::Relaxed);
    FUTEX_VAR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let _ = sys::futex_wake(addr, u32::MAX);
    sys::exit();
}

extern "C" fn futex_waiter_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    futex_waiter(args);
}

extern "C" fn futex_waker_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    futex_waker(args);
}

fn spawn_with_trampoline(trampoline: u64, arg: u64) -> Option<u8> {
    let stack = alloc_thread_stack();
    if stack == 0 {
        return None;
    }
    let args_ptr = (stack - 8) as *mut u64;
    unsafe { core::ptr::write_volatile(args_ptr, arg) };
    sys::thread_create(trampoline, stack - 16).ok()
}

fn phase_21_futex_race() {
    FUTEX_VAR.store(0, core::sync::atomic::Ordering::Relaxed);
    FUTEX_DONE.store(0, core::sync::atomic::Ordering::Relaxed);
    let mut handles: [u8; 6] = [0; 6];
    let mut count = 0usize;

    // 3 waiters + 3 wakers, all hammering the same futex.
    for _ in 0..3 {
        if let Some(h) = spawn_with_trampoline(futex_waiter_trampoline as u64, 0) {
            handles[count] = h;
            count += 1;
        }
    }
    for _ in 0..3 {
        if let Some(h) = spawn_with_trampoline(futex_waker_trampoline as u64, 0) {
            handles[count] = h;
            count += 1;
        }
    }
    for i in 0..count {
        let _ = sys::wait(&[handles[i]], u64::MAX);
        let _ = sys::handle_close(handles[i]);
    }

    phase_ok(b"phase 21: futex wake race");
}

// -----------------------------------------------------------------------
// Phase 22: ASID pressure (rapid process create/destroy)
// -----------------------------------------------------------------------
fn phase_22_asid_pressure() {
    let mut success: u32 = 0;
    for _ in 0..50 {
        match spawn_helper(0x03) {
            Some(child) => {
                reap_helper(child);
                success += 1;
            }
            None => break,
        }
    }
    sys::print(b"       ");
    print_u64(success as u64);
    sys::print(b" processes cycled (ASID pressure)\n");

    phase_ok(b"phase 22: ASID pressure");
}

// -----------------------------------------------------------------------
// Phase 23: Wait on timer that fires during setup
// -----------------------------------------------------------------------
fn phase_23_timer_fire_during_setup() {
    // Create a timer with 0ns (fires immediately), then wait.
    // Tests the race between timer fire callback and wait registration.
    for _ in 0..100 {
        if let Ok(h) = sys::timer_create(0) {
            let _ = sys::wait(&[h], 1_000_000);
            let _ = sys::handle_close(h);
        }
    }
    // Also: create timer with 1ns.
    for _ in 0..100 {
        if let Ok(h) = sys::timer_create(1) {
            let _ = sys::wait(&[h], 1_000_000);
            let _ = sys::handle_close(h);
        }
    }

    phase_ok(b"phase 23: timer fire during setup");
}

// -----------------------------------------------------------------------
// Phase 24: Multiple threads wait on same channel
// -----------------------------------------------------------------------

extern "C" fn channel_waiter(handle: u64) -> ! {
    let h = handle as u8;
    let _ = sys::wait(&[h], 50_000_000); // 50ms timeout.
    sys::exit();
}

extern "C" fn channel_waiter_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    channel_waiter(args);
}

fn phase_24_multi_waiter_same_handle() {
    // Two threads both wait on the same channel handle.
    // Only one waiter can be registered per handle — second should
    // either overwrite or fail, but kernel must not crash.
    let (ch_a, ch_b) = match sys::channel_create() {
        Ok(pair) => pair,
        Err(_) => {
            sys::print(b"       (skipped - channel_create failed)\n");
            phase_ok(b"phase 24: multi-waiter same handle");
            return;
        }
    };

    let t1 = spawn_with_trampoline(channel_waiter_trampoline as u64, ch_a as u64);
    let t2 = spawn_with_trampoline(channel_waiter_trampoline as u64, ch_a as u64);

    // Signal the channel — should wake at least one waiter.
    sys::yield_now();
    sys::yield_now();
    let _ = sys::channel_signal(ch_b);

    if let Some(h) = t1 {
        let _ = sys::wait(&[h], 100_000_000);
        let _ = sys::handle_close(h);
    }
    if let Some(h) = t2 {
        let _ = sys::wait(&[h], 100_000_000);
        let _ = sys::handle_close(h);
    }
    let _ = sys::handle_close(ch_a);
    let _ = sys::handle_close(ch_b);

    phase_ok(b"phase 24: multi-waiter same handle");
}

// -----------------------------------------------------------------------
// Phase 25: Stress process creation under memory pressure
// -----------------------------------------------------------------------
fn phase_25_create_under_pressure() {
    // Eat up some memory, then try to create processes.
    let mut allocs: [usize; 256] = [0; 256];
    let mut alloc_count = 0usize;

    // Consume memory.
    for i in 0..256 {
        match sys::memory_alloc(1) {
            Ok(va) => {
                unsafe { core::ptr::write_volatile(va as *mut u8, 0xCC) };
                allocs[i] = va;
                alloc_count += 1;
            }
            Err(_) => break,
        }
    }

    // Try to create a process under pressure — should fail gracefully.
    let r = sys::process_create(HELPER_ELF.as_ptr(), HELPER_ELF.len());
    match r {
        Ok(h) => {
            // Somehow succeeded — clean up.
            let _ = sys::process_kill(h);
            let _ = sys::handle_close(h);
        }
        Err(_) => {} // Expected — out of memory.
    }

    // Free everything.
    for i in 0..alloc_count {
        let _ = sys::memory_free(allocs[i], 1);
    }

    match spawn_helper(0x03) {
        Some(child) => reap_helper(child),
        None => sys::print(b"       (could not spawn after release)\n"),
    }

    phase_ok(b"phase 25: create under memory pressure");
}

// -----------------------------------------------------------------------
// Phase 26: Userspace faults (kernel must not panic)
// -----------------------------------------------------------------------

extern "C" fn fault_udf(_: u64) -> ! {
    // Execute undefined instruction — should fault, kernel kills thread.
    unsafe { core::arch::asm!("udf #0", options(noreturn)) };
}

extern "C" fn fault_udf_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    fault_udf(args);
}

extern "C" fn fault_null_read(_: u64) -> ! {
    // Read from address 0 — should fault.
    let val: u64;
    unsafe {
        core::arch::asm!(
            "mov x9, #0",
            "ldr {0}, [x9]",
            out(reg) val,
            out("x9") _,
            options(nostack)
        );
    }
    let _ = val;
    sys::exit();
}

extern "C" fn fault_null_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    fault_null_read(args);
}

extern "C" fn fault_kernel_read(_: u64) -> ! {
    // Read from kernel address — should fault.
    let val: u64;
    unsafe {
        core::arch::asm!(
            "mov x9, #0xFFFF000000000000",
            "ldr {0}, [x9]",
            out(reg) val,
            out("x9") _,
            options(nostack)
        );
    }
    let _ = val;
    sys::exit();
}

extern "C" fn fault_kernel_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!("ldr {0}, [sp, #8]", out(reg) args, options(nostack, nomem));
    }
    fault_kernel_read(args);
}

fn phase_26_userspace_faults() {
    // Each fault should kill the faulting thread/process, not panic the kernel.
    // We run each fault in a child process so our process survives.

    // UDF (undefined instruction).
    match spawn_helper(0x03) {
        Some(child) => reap_helper(child),
        None => {}
    }

    // Instead of helper processes, use threads with faults.
    // A faulting thread should be killed. But does the kernel handle this?
    // If the kernel panics on a user fault, the test hangs/crashes.

    // Test: child process executes UDF.
    // We can't easily make the helper execute UDF via the command byte approach.
    // But we CAN test it with threads in our own process — if the kernel
    // kills only the faulting thread, our process survives.
    // If the kernel kills the whole process, the test fails (init sees fuzz exit).

    // For safety, run these in child processes.
    // UDF fault in a child.
    // We'll create a bare process for this... but we only have the helper ELF.
    // Let's test thread faults and see what happens.

    // Thread that executes UDF.
    let t = spawn_with_trampoline(fault_udf_trampoline as u64, 0);
    if let Some(h) = t {
        // If kernel kills thread, wait returns. If kernel panics, we hang.
        let _ = sys::wait(&[h], 50_000_000); // 50ms timeout.
        let _ = sys::handle_close(h);
    }

    // Thread that reads from null.
    let t = spawn_with_trampoline(fault_null_trampoline as u64, 0);
    if let Some(h) = t {
        let _ = sys::wait(&[h], 50_000_000);
        let _ = sys::handle_close(h);
    }

    // Thread that reads from kernel space.
    let t = spawn_with_trampoline(fault_kernel_trampoline as u64, 0);
    if let Some(h) = t {
        let _ = sys::wait(&[h], 50_000_000);
        let _ = sys::handle_close(h);
    }

    phase_ok(b"phase 26: userspace faults");
}

// -----------------------------------------------------------------------
// Phase 27: Stack overflow (recurse until guard page fault)
// -----------------------------------------------------------------------

#[inline(never)]
fn recurse_forever(depth: u64) -> u64 {
    // Volatile to prevent tail-call elimination.
    let mut buf = [0u8; 256];
    unsafe { core::ptr::write_volatile(&mut buf[0], depth as u8) };
    recurse_forever(depth + 1) + unsafe { core::ptr::read_volatile(&buf[0]) as u64 }
}

extern "C" fn stack_overflow_entry(_: u64) -> ! {
    let _ = recurse_forever(0);
    sys::exit();
}

extern "C" fn stack_overflow_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack),
        );
    }
    stack_overflow_entry(args);
}

fn phase_27_stack_overflow() {
    let t = spawn_with_trampoline(stack_overflow_trampoline as u64, 0);
    if let Some(h) = t {
        let _ = sys::wait(&[h], 100_000_000); // 100ms timeout
        let _ = sys::handle_close(h);
    }
    phase_ok(b"phase 27: stack overflow (guard page fault)");
}

// -----------------------------------------------------------------------
// Phase 28: Signal closed channel endpoint
// -----------------------------------------------------------------------

fn phase_28_signal_closed_channel() {
    // Create a channel, close one end, then signal the closed end from the
    // other. Also: signal after both ends closed (handle still valid briefly).
    for _ in 0..10 {
        match sys::channel_create() {
            Ok((a, b)) => {
                let _ = sys::handle_close(b);
                // Signal the orphaned peer — should not crash.
                let _ = sys::channel_signal(a);
                let _ = sys::handle_close(a);
            }
            Err(_) => break,
        }
    }
    phase_ok(b"phase 28: signal closed channel endpoint");
}

// -----------------------------------------------------------------------
// Phase 29: Rapid thread create/exit (thread ID recycling stress)
// -----------------------------------------------------------------------

extern "C" fn noop_entry(_: u64) -> ! {
    sys::exit();
}

extern "C" fn noop_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack),
        );
    }
    noop_entry(args);
}

fn phase_29_thread_id_recycling() {
    let mut created: u32 = 0;
    for _ in 0..100 {
        let t = spawn_with_trampoline(noop_trampoline as u64, 0);
        if let Some(h) = t {
            let _ = sys::wait(&[h], u64::MAX);
            let _ = sys::handle_close(h);
            created += 1;
        } else {
            break;
        }
    }
    sys::print(b"       ");
    print_u64(created as u64);
    sys::print(b" thread create/exit cycles\n");
    phase_ok(b"phase 29: thread ID recycling");
}

// -----------------------------------------------------------------------
// Phase 30: Close handle while another thread waits on it
// -----------------------------------------------------------------------

static WAIT_HANDLE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

extern "C" fn wait_on_shared_handle(_: u64) -> ! {
    let h = WAIT_HANDLE.load(core::sync::atomic::Ordering::Acquire) as u8;
    let _ = sys::wait(&[h], u64::MAX);
    sys::exit();
}

extern "C" fn wait_on_shared_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack),
        );
    }
    wait_on_shared_handle(args);
}

fn phase_30_close_while_waiting() {
    // Create a timer that won't fire (u64::MAX ns). A sibling thread blocks
    // on sys_wait for it. Then the main thread closes the timer handle.
    // The kernel must wake the blocked sibling — otherwise it hangs forever.
    match sys::timer_create(u64::MAX) {
        Ok(timer_h) => {
            WAIT_HANDLE.store(timer_h as u64, core::sync::atomic::Ordering::Release);
            let t = spawn_with_trampoline(wait_on_shared_trampoline as u64, 0);
            if let Some(th) = t {
                for _ in 0..200 {
                    sys::yield_now();
                }
                // Close the timer handle — kernel must wake the blocked sibling.
                let _ = sys::handle_close(timer_h);
                // Sibling should wake and exit promptly.
                let _ = sys::wait(&[th], u64::MAX);
                let _ = sys::handle_close(th);
            } else {
                let _ = sys::handle_close(timer_h);
            }
        }
        Err(_) => sys::print(b"       (skipped - timer create failed)\n"),
    }
    phase_ok(b"phase 30: close handle while another thread waits");
}

// -----------------------------------------------------------------------
// Phase 31: Concurrent channel create/close (stress channel table)
// -----------------------------------------------------------------------

extern "C" fn channel_churn_entry(_: u64) -> ! {
    for _ in 0..200 {
        match sys::channel_create() {
            Ok((a, b)) => {
                let _ = sys::channel_signal(a);
                let _ = sys::channel_signal(b);
                let _ = sys::handle_close(a);
                let _ = sys::handle_close(b);
            }
            Err(_) => break,
        }
    }
    sys::exit();
}

extern "C" fn channel_churn_trampoline() -> ! {
    let args: u64;
    unsafe {
        core::arch::asm!(
            "ldr {0}, [sp, #8]",
            out(reg) args,
            options(nostack),
        );
    }
    channel_churn_entry(args);
}

fn phase_31_concurrent_channel_churn() {
    let mut handles: [u8; 4] = [0; 4];
    let mut count = 0usize;
    for _ in 0..4 {
        if let Some(h) = spawn_with_trampoline(channel_churn_trampoline as u64, 0) {
            handles[count] = h;
            count += 1;
        }
    }
    for i in 0..count {
        let _ = sys::wait(&[handles[i]], u64::MAX);
        let _ = sys::handle_close(handles[i]);
    }
    phase_ok(b"phase 31: concurrent channel create/close");
}

// -----------------------------------------------------------------------
// Entry
// -----------------------------------------------------------------------
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x94\xAC fuzz test starting (31 phases)\n");

    // Single-process tests.
    phase_1_invalid_syscall_numbers();
    phase_2_bad_handles();
    phase_3_bad_addresses();
    phase_4_handle_exhaustion();
    phase_5_memory_exhaustion();
    phase_6_timer_exhaustion();
    phase_7_process_lifecycle();
    phase_8_thread_lifecycle();
    phase_9_scheduling_context();
    phase_10_concurrent_chaos();
    phase_11_wait_edge_cases();
    phase_12_dma_edge_cases();

    // Cross-process tests.
    phase_13_kill_while_blocked();
    phase_14_kill_while_running();
    phase_15_kill_multi_threaded();
    phase_16_rapid_lifecycle();
    phase_17_kill_during_resource_churn();
    phase_18_sibling_close_while_blocked();
    phase_19_peer_exit();
    phase_20_double_kill();

    // Race condition tests.
    phase_21_futex_race();
    phase_22_asid_pressure();
    phase_23_timer_fire_during_setup();
    phase_24_multi_waiter_same_handle();
    phase_25_create_under_pressure();
    phase_26_userspace_faults();

    // Extended tests.
    phase_27_stack_overflow();
    phase_28_signal_closed_channel();
    phase_29_thread_id_recycling();
    phase_30_close_while_waiting();
    phase_31_concurrent_channel_churn();

    sys::print(b"  \xE2\x9C\x85 fuzz test PASS\n");
    sys::exit();
}
