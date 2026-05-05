//! Bare-metal integration tests — exercises real syscalls from EL0.
//!
//! Runs under the hypervisor via `make integration-test`. Each test
//! function returns on success. Failures exit with a unique code,
//! which the kernel prints before PSCI SYSTEM_OFF.
//!
//! Exit codes: 0 = all pass. Non-zero identifies the failing assertion.
//! The test script maps codes to test names.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::types::{Handle, ObjectType, Rights, SyscallError};

const PAGE_SIZE: usize = 16384;
const MSG_SIZE: usize = 128;

fn fail(code: u32) -> ! {
    abi::thread::exit(code);
}

fn assert_ok<T>(result: Result<T, SyscallError>, code: u32) -> T {
    match result {
        Ok(v) => v,
        Err(_) => fail(code),
    }
}

fn assert_eq_u64(actual: u64, expected: u64, code: u32) {
    if actual != expected {
        fail(code);
    }
}

fn assert_true(cond: bool, code: u32) {
    if !cond {
        fail(code);
    }
}

fn assert_err(result: Result<u64, SyscallError>, expected: SyscallError, code: u32) {
    match result {
        Err(e) if e == expected => {}
        _ => fail(code),
    }
}

// ── System info tests ─────────────────────────────────────────────

fn test_system_info_page_size() {
    let val = assert_ok(abi::system::info(abi::system::INFO_PAGE_SIZE), 10);

    assert_eq_u64(val, PAGE_SIZE as u64, 11);
}

fn test_system_info_msg_size() {
    let val = assert_ok(abi::system::info(abi::system::INFO_MSG_SIZE), 12);

    assert_eq_u64(val, MSG_SIZE as u64, 13);
}

fn test_system_info_num_cores() {
    let val = assert_ok(abi::system::info(abi::system::INFO_NUM_CORES), 14);

    assert_true(val >= 1, 15);
}

fn test_clock_read() {
    let t1 = assert_ok(abi::system::clock_read(), 20);
    let t2 = assert_ok(abi::system::clock_read(), 21);

    assert_true(t2 >= t1, 22);
}

fn test_clock_monotonic() {
    let mut prev = assert_ok(abi::system::clock_read(), 23);

    for _ in 0..100 {
        let now = assert_ok(abi::system::clock_read(), 24);

        assert_true(now >= prev, 25);
        prev = now;
    }
}

// ── VMO tests ─────────────────────────────────────────────────────

fn test_vmo_create() {
    let h = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 30);

    assert_true(h.0 >= 2, 31);
}

fn test_vmo_create_and_info() {
    let h = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 40);
    let info = assert_ok(abi::handle::info(h), 41);

    assert_true(info.object_type == ObjectType::Vmo, 42);
    assert_true(info.rights == Rights::ALL, 43);
}

fn test_vmo_map_and_access() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 150);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = assert_ok(abi::vmo::map(vmo, 0, rw), 151);

    assert_true(va != 0, 152);

    let ptr = va as *mut u64;

    // SAFETY: The kernel mapped this page RW at `va`. We write within bounds.
    unsafe { core::ptr::write_volatile(ptr, 0xCAFE_BABE) };

    let read_back = unsafe { core::ptr::read_volatile(ptr) };

    assert_eq_u64(read_back, 0xCAFE_BABE, 153);
}

fn test_vmo_map_write_pattern() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 154);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let va = assert_ok(abi::vmo::map(vmo, 0, rw), 155);
    let ptr = va as *mut u64;

    // Write a pattern across the full page.
    for i in 0..(PAGE_SIZE / 8) {
        unsafe { core::ptr::write_volatile(ptr.add(i), i as u64 * 0x0101_0101) };
    }

    // Verify every word.
    for i in 0..(PAGE_SIZE / 8) {
        let val = unsafe { core::ptr::read_volatile(ptr.add(i)) };

        assert_eq_u64(val, i as u64 * 0x0101_0101, 156);
    }
}

fn test_vmo_snapshot() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 120);
    let snap = assert_ok(abi::vmo::snapshot(vmo), 121);
    let info = assert_ok(abi::handle::info(snap), 122);

    assert_true(info.object_type == ObjectType::Vmo, 123);
}

fn test_vmo_seal() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 130);

    assert_ok(abi::vmo::seal(vmo), 131);

    let result = abi::vmo::resize(vmo, PAGE_SIZE * 2);

    assert_err(result.map(|_| 0), SyscallError::AlreadySealed, 132);
}

fn test_vmo_resize() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 133);

    assert_ok(abi::vmo::resize(vmo, PAGE_SIZE * 2), 134);

    // Resize back down.
    assert_ok(abi::vmo::resize(vmo, PAGE_SIZE), 135);
}

// ── Event tests ───────────────────────────────────────────────────

fn test_event_create() {
    let h = assert_ok(abi::event::create(), 50);
    let info = assert_ok(abi::handle::info(h), 51);

    assert_true(info.object_type == ObjectType::Event, 52);
}

fn test_event_signal_and_wait() {
    let ev = assert_ok(abi::event::create(), 60);

    assert_ok(abi::event::signal(ev, 0x5), 61);

    let fired = assert_ok(abi::event::wait(&[(ev, 0x5)]), 62);

    assert_true(fired.0 == ev.0, 63);
}

fn test_event_clear() {
    let ev = assert_ok(abi::event::create(), 70);

    assert_ok(abi::event::signal(ev, 0x3), 71);
    assert_ok(abi::event::clear(ev, 0x1), 72);

    // Bit 1 cleared, bit 2 still set — wait on bit 2 should succeed.
    let fired = assert_ok(abi::event::wait(&[(ev, 0x2)]), 73);

    assert_true(fired.0 == ev.0, 74);
}

fn test_event_multi_signal() {
    let ev = assert_ok(abi::event::create(), 75);

    // Signal bits incrementally and verify accumulation.
    assert_ok(abi::event::signal(ev, 0x01), 76);
    assert_ok(abi::event::signal(ev, 0x10), 77);

    // Both bits should be set — wait on combined mask.
    let fired = assert_ok(abi::event::wait(&[(ev, 0x11)]), 78);

    assert_true(fired.0 == ev.0, 79);
}

// ── Endpoint tests ────────────────────────────────────────────────

fn test_endpoint_create() {
    let h = assert_ok(abi::ipc::endpoint_create(), 80);
    let info = assert_ok(abi::handle::info(h), 81);

    assert_true(info.object_type == ObjectType::Endpoint, 82);
}

fn test_endpoint_bind_event() {
    let ep = assert_ok(abi::ipc::endpoint_create(), 140);
    let ev = assert_ok(abi::event::create(), 141);

    assert_ok(abi::ipc::endpoint_bind_event(ep, ev), 142);
}

// ── Handle tests ──────────────────────────────────────────────────

fn test_handle_dup() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 90);
    let dup = assert_ok(abi::handle::dup(vmo, Rights::READ), 91);
    let info = assert_ok(abi::handle::info(dup), 92);

    assert_true(info.object_type == ObjectType::Vmo, 93);
    assert_true(info.rights == Rights::READ, 94);
}

fn test_handle_close() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 100);

    assert_ok(abi::handle::close(vmo), 101);

    let result = abi::handle::info(vmo);

    assert_err(
        result.map(|i| i.rights.0 as u64),
        SyscallError::InvalidHandle,
        102,
    );
}

fn test_handle_dup_rights_attenuation() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 103);
    let read_only = assert_ok(abi::handle::dup(vmo, Rights::READ), 104);

    // Cannot escalate rights via dup.
    let result = abi::handle::dup(read_only, Rights::ALL);

    assert_err(
        result.map(|h| h.0 as u64),
        SyscallError::InsufficientRights,
        105,
    );
}

// ── Space tests ───────────────────────────────────────────────────

fn test_space_create() {
    let h = assert_ok(abi::space::create(), 110);
    let info = assert_ok(abi::handle::info(h), 111);

    assert_true(info.object_type == ObjectType::AddressSpace, 112);
}

// ── IPC round-trip ────────────────────────────────────────────────

fn test_ipc_call_recv_reply() {
    let ep = assert_ok(abi::ipc::endpoint_create(), 160);
    let stack_vmo = assert_ok(abi::vmo::create(PAGE_SIZE * 4, 0), 161);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let stack_va = assert_ok(abi::vmo::map(stack_vmo, 0, rw), 162);

    assert_true(stack_va != 0, 163);

    let stack_top = stack_va + PAGE_SIZE * 4;
    let _thread = assert_ok(
        abi::thread::create(
            ipc_caller_entry as *const () as usize,
            stack_top,
            ep.0 as usize,
        ),
        164,
    );

    let mut msg_buf = [0u8; MSG_SIZE];
    let mut handles_buf = [0u32; 4];
    let recv = assert_ok(abi::ipc::recv(ep, &mut msg_buf, &mut handles_buf), 165);

    assert_true(recv.msg_len == 8, 166);

    let payload = u64::from_le_bytes(msg_buf[..8].try_into().unwrap());

    assert_eq_u64(payload, 0xDEAD_BEEF, 167);

    let reply_data = 0xC0FFEEu64.to_le_bytes();

    assert_ok(abi::ipc::reply(ep, recv.reply_cap, &reply_data, &[]), 168);
}

extern "C" fn ipc_caller_entry(endpoint_handle: usize) -> ! {
    let ep = Handle(endpoint_handle as u32);
    let mut buf = [0u8; MSG_SIZE];
    let payload = 0xDEAD_BEEFu64.to_le_bytes();

    buf[..8].copy_from_slice(&payload);

    let result = abi::ipc::call(ep, &mut buf, 8, &[]);

    if result.is_err() {
        abi::thread::exit(200);
    }

    let reply = u64::from_le_bytes(buf[..8].try_into().unwrap());

    if reply != 0xC0FFEE {
        abi::thread::exit(201);
    }

    abi::thread::exit(0);
}

// ── Event notification between threads ────────────────────────────

fn test_event_cross_thread() {
    let ev = assert_ok(abi::event::create(), 170);
    let stack_vmo = assert_ok(abi::vmo::create(PAGE_SIZE * 2, 0), 171);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let stack_va = assert_ok(abi::vmo::map(stack_vmo, 0, rw), 172);
    let stack_top = stack_va + PAGE_SIZE * 2;

    let _thread = assert_ok(
        abi::thread::create(
            event_signaler_entry as *const () as usize,
            stack_top,
            ev.0 as usize,
        ),
        173,
    );

    // Wait for the signaler thread to fire bit 0x42.
    let fired = assert_ok(abi::event::wait(&[(ev, 0x42)]), 174);

    assert_true(fired.0 == ev.0, 175);
}

extern "C" fn event_signaler_entry(event_handle: usize) -> ! {
    let ev = Handle(event_handle as u32);

    // Signal the event so the main thread wakes up.
    let _ = abi::event::signal(ev, 0x42);

    abi::thread::exit(0);
}

// ── Capacity limit + recovery ─────────────────────────────────────

fn test_capacity_recovery() {
    // Create VMOs until we get OutOfMemory or a reasonable cap.
    let mut handles = [Handle(0); 64];
    let mut count = 0;

    for h in &mut handles {
        match abi::vmo::create(PAGE_SIZE, 0) {
            Ok(handle) => {
                *h = handle;
                count += 1;
            }
            Err(SyscallError::OutOfMemory) => break,
            Err(_) => fail(180),
        }
    }

    // Must have created at least some.
    assert_true(count > 0, 181);

    // Close all of them.
    for h in &handles[..count] {
        assert_ok(abi::handle::close(*h), 182);
    }

    // Should be able to create again after closing.
    let recovered = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 183);

    assert_ok(abi::handle::close(recovered), 184);
}

// ── Differential tests (host vs bare-metal) ─────────────────────
// These mirror the scenarios in kernel/src/differential.rs.

fn diff_object_lifecycle() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 300);

    let info = assert_ok(abi::handle::info(vmo), 301);
    assert_true(info.object_type == ObjectType::Vmo, 302);
    assert_true(info.rights == Rights::ALL, 303);

    let dup = assert_ok(abi::handle::dup(vmo, Rights::READ), 304);
    assert_true(dup.0 != vmo.0, 305);

    let dup_info = assert_ok(abi::handle::info(dup), 306);
    assert_true(dup_info.rights == Rights::READ, 307);

    assert_ok(abi::handle::close(dup), 308);
    assert_err(
        abi::handle::info(dup).map(|i| i.rights.0 as u64),
        SyscallError::InvalidHandle,
        309,
    );

    assert_ok(abi::handle::info(vmo).map(|_| 0), 310);
    assert_ok(abi::handle::close(vmo), 311);
}

fn diff_event_signal_clear() {
    let ev = assert_ok(abi::event::create(), 312);

    assert_ok(abi::event::signal(ev, 0x5), 313);

    let fired = assert_ok(abi::event::wait(&[(ev, 0x4)]), 314);
    assert_true(fired.0 == ev.0, 315);

    assert_ok(abi::event::clear(ev, 0x1), 316);

    // Bit 0x4 still set — wait should succeed immediately.
    let fired = assert_ok(abi::event::wait(&[(ev, 0x4)]), 317);
    assert_true(fired.0 == ev.0, 318);

    assert_ok(abi::handle::close(ev), 319);
}

fn diff_endpoint_bind_close() {
    let ep = assert_ok(abi::ipc::endpoint_create(), 320);

    let info = assert_ok(abi::handle::info(ep), 321);
    assert_true(info.object_type == ObjectType::Endpoint, 322);

    let ev = assert_ok(abi::event::create(), 323);
    assert_ok(abi::ipc::endpoint_bind_event(ep, ev), 324);

    assert_ok(abi::handle::close(ep), 325);
    assert_ok(abi::handle::close(ev), 326);
}

fn diff_error_codes() {
    // Invalid handle
    assert_err(
        abi::handle::info(Handle(999)).map(|i| i.rights.0 as u64),
        SyscallError::InvalidHandle,
        330,
    );

    // Wrong handle type — VMO handle for event operation
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 331);
    assert_err(
        abi::event::signal(vmo, 0x1).map(|_| 0),
        SyscallError::WrongHandleType,
        332,
    );

    // VMO create with zero size
    assert_err(
        abi::vmo::create(0, 0).map(|h| h.0 as u64),
        SyscallError::InvalidArgument,
        333,
    );

    // Seal then resize
    assert_ok(abi::vmo::seal(vmo), 334);
    assert_err(
        abi::vmo::resize(vmo, PAGE_SIZE * 2).map(|_| 0),
        SyscallError::AlreadySealed,
        335,
    );

    // Rights escalation
    let read_only = assert_ok(abi::handle::dup(vmo, Rights::READ), 336);
    assert_err(
        abi::handle::dup(read_only, Rights::ALL).map(|h| h.0 as u64),
        SyscallError::InsufficientRights,
        337,
    );

    assert_ok(abi::handle::close(vmo), 338);
    assert_ok(abi::handle::close(read_only), 339);
}

fn diff_vmo_snapshot_seal_resize() {
    let vmo = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 340);
    let snap = assert_ok(abi::vmo::snapshot(vmo), 341);

    let info = assert_ok(abi::handle::info(snap), 342);
    assert_true(info.object_type == ObjectType::Vmo, 343);

    assert_ok(abi::vmo::resize(vmo, PAGE_SIZE * 2), 344);

    assert_ok(abi::vmo::seal(snap), 345);
    assert_err(
        abi::vmo::resize(snap, PAGE_SIZE).map(|_| 0),
        SyscallError::AlreadySealed,
        346,
    );

    assert_ok(abi::handle::close(vmo), 347);
    assert_ok(abi::handle::close(snap), 348);
}

fn diff_handle_slot_reuse() {
    let h1 = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 350);
    let h2 = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 351);
    assert_true(h1.0 != h2.0, 352);

    assert_ok(abi::handle::close(h1), 353);

    let h3 = assert_ok(abi::vmo::create(PAGE_SIZE, 0), 354);

    // h2 is still valid regardless of h1's reuse.
    assert_ok(abi::handle::info(h2).map(|_| 0), 355);
    assert_ok(abi::handle::info(h3).map(|_| 0), 356);

    assert_ok(abi::handle::close(h2), 357);
    assert_ok(abi::handle::close(h3), 358);
}

// ── SMP IPC stress test ─────────────────────────────────────────

const IPC_SMP_ROUNDS: usize = 50;

fn test_smp_ipc_stress() {
    let num_cores = assert_ok(abi::system::info(abi::system::INFO_NUM_CORES), 420) as usize;

    if num_cores < 2 {
        return;
    }

    let ep = assert_ok(abi::ipc::endpoint_create(), 421);

    let stack_vmo = assert_ok(abi::vmo::create(PAGE_SIZE * 4, 0), 422);
    let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
    let stack_va = assert_ok(abi::vmo::map(stack_vmo, 0, rw), 423);
    let stack_top = stack_va + PAGE_SIZE * 4;

    let arg = (ep.0 as usize) | (IPC_SMP_ROUNDS << 16);
    let server = assert_ok(
        abi::thread::create(ipc_smp_server_entry as *const () as usize, stack_top, arg),
        424,
    );

    let _ = abi::thread::set_affinity(server, 1);

    for round in 0..IPC_SMP_ROUNDS {
        let mut buf = [0u8; MSG_SIZE];
        let payload = (round as u64).to_le_bytes();
        buf[..8].copy_from_slice(&payload);

        let result = abi::ipc::call(ep, &mut buf, 8, &[]);
        if result.is_err() {
            fail(425);
        }

        let reply_val = u64::from_le_bytes(buf[..8].try_into().unwrap());
        if reply_val != round as u64 + 1000 {
            fail(426);
        }
    }

    assert_ok(abi::handle::close(ep), 427);
}

extern "C" fn ipc_smp_server_entry(arg: usize) -> ! {
    let ep = Handle((arg & 0xFFFF) as u32);
    let rounds = arg >> 16;

    for _ in 0..rounds {
        let mut msg_buf = [0u8; MSG_SIZE];
        let mut handles_buf = [0u32; 4];

        let recv = match abi::ipc::recv(ep, &mut msg_buf, &mut handles_buf) {
            Ok(r) => r,
            Err(_) => abi::thread::exit(430),
        };

        let val = u64::from_le_bytes(msg_buf[..8].try_into().unwrap());
        let reply_val = (val + 1000).to_le_bytes();

        if abi::ipc::reply(ep, recv.reply_cap, &reply_val, &[]).is_err() {
            abi::thread::exit(431);
        }
    }

    abi::thread::exit(0);
}

// ── SMP stress test ──────────────────────────────────────────────

const SMP_ITERATIONS: usize = 200;

fn test_smp_stress() {
    let num_cores = assert_ok(abi::system::info(abi::system::INFO_NUM_CORES), 400) as usize;

    if num_cores < 2 {
        return;
    }

    let worker_count = if num_cores > 4 { 4 } else { num_cores };

    let sync_event = assert_ok(abi::event::create(), 401);

    for i in 0..worker_count {
        let stack_vmo = assert_ok(abi::vmo::create(PAGE_SIZE * 4, 0), 402);
        let rw = Rights(Rights::READ.0 | Rights::WRITE.0 | Rights::MAP.0);
        let stack_va = assert_ok(abi::vmo::map(stack_vmo, 0, rw), 403);
        let stack_top = stack_va + PAGE_SIZE * 4;

        let arg = (sync_event.0 as usize) | (i << 16) | (SMP_ITERATIONS << 32);

        let thread = assert_ok(
            abi::thread::create(smp_worker_entry as *const () as usize, stack_top, arg),
            404,
        );

        let _ = abi::thread::set_affinity(thread, i as u64);
    }

    let done_mask: u64 = (1u64 << worker_count) - 1;

    let fired = assert_ok(abi::event::wait(&[(sync_event, done_mask)]), 405);
    assert_true(fired.0 == sync_event.0, 406);

    assert_ok(abi::handle::close(sync_event), 407);
}

extern "C" fn smp_worker_entry(arg: usize) -> ! {
    let event_handle = Handle((arg & 0xFFFF) as u32);
    let worker_id = (arg >> 16) & 0xFFFF;
    let iterations = arg >> 32;

    for _ in 0..iterations {
        let vmo = match abi::vmo::create(PAGE_SIZE, 0) {
            Ok(h) => h,
            Err(_) => abi::thread::exit(410),
        };
        let ev = match abi::event::create() {
            Ok(h) => h,
            Err(_) => abi::thread::exit(411),
        };

        if abi::event::signal(ev, 0xFF).is_err() {
            abi::thread::exit(412);
        }
        if abi::event::clear(ev, 0x0F).is_err() {
            abi::thread::exit(413);
        }

        let snap = match abi::vmo::snapshot(vmo) {
            Ok(h) => h,
            Err(_) => abi::thread::exit(414),
        };

        if abi::handle::close(snap).is_err() {
            abi::thread::exit(415);
        }
        if abi::handle::close(ev).is_err() {
            abi::thread::exit(416);
        }
        if abi::handle::close(vmo).is_err() {
            abi::thread::exit(417);
        }
    }

    let done_bit = 1u64 << worker_id;
    let _ = abi::event::signal(event_handle, done_bit);

    abi::thread::exit(0);
}

// ── Entry point ───────────────────────────────────────────────────

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    // System info
    test_system_info_page_size();
    test_system_info_msg_size();
    test_system_info_num_cores();
    test_clock_read();
    test_clock_monotonic();

    // VMO
    test_vmo_create();
    test_vmo_create_and_info();
    test_vmo_snapshot();
    test_vmo_seal();
    test_vmo_resize();

    // Event
    test_event_create();
    test_event_signal_and_wait();
    test_event_clear();
    test_event_multi_signal();

    // Endpoint
    test_endpoint_create();
    test_endpoint_bind_event();

    // Handle
    test_handle_dup();
    test_handle_dup_rights_attenuation();
    test_handle_close();

    // Space
    test_space_create();

    // Memory mapping + fault resolution
    test_vmo_map_and_access();
    test_vmo_map_write_pattern();

    // Cross-thread event notification
    test_event_cross_thread();

    // IPC round-trip
    test_ipc_call_recv_reply();

    // Capacity limit and recovery
    test_capacity_recovery();

    // Differential tests (host vs bare-metal)
    diff_object_lifecycle();
    diff_event_signal_clear();
    diff_endpoint_bind_close();
    diff_error_codes();
    diff_vmo_snapshot_seal_resize();
    diff_handle_slot_reuse();

    // SMP IPC stress (cross-core IPC round-trips)
    test_smp_ipc_stress();

    // SMP stress (multiple cores, concurrent object ops)
    test_smp_stress();

    // All tests passed.
    abi::thread::exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
