//! Adversarial kernel test — exercises every syscall with invalid, edge-case,
//! and hostile arguments. The kernel must never panic, hang, or corrupt state.
//!
//! Phases:
//!   1. Invalid syscall numbers
//!   2. Bad handle arguments (invalid index, wrong type, double-close)
//!   3. Bad address arguments (null, kernel VA, unmapped, unaligned)
//!   4. Handle table exhaustion + recovery
//!   5. Memory exhaustion + recovery
//!   6. Timer exhaustion + recovery
//!   7. Process lifecycle (create/kill/start edge cases)
//!   8. Thread lifecycle (bad entry/stack, rapid create/exit)
//!   9. Scheduling context edge cases
//!  10. Concurrent random syscalls from multiple threads

#![no_std]
#![no_main]

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
    let _ = sys::process_kill(a);  // Channel handle, not process.
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
        0,                        // null
        0xFFFF_FFFF_FFFF_FFFF,    // kernel space
        0xFFFF_0000_0000_0000,    // kernel space
        0x0000_FFFF_FFFF_F000,    // near top of user space
        0xDEAD_BEEF_0000_0000,    // unmapped
        1,                        // unaligned
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
        phase_fail(b"phase 3", b"thread_create with unaligned stack should fail");
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
            0 => { sys::yield_now(); }
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
        _ => {} // Timer race etc — fine.
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
// Entry
// -----------------------------------------------------------------------
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    sys::print(b"  \xF0\x9F\x94\xAC fuzz test starting\n");

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

    sys::print(b"  \xE2\x9C\x85 fuzz test PASS\n");
    sys::exit();
}
