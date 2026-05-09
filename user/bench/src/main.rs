//! EL0 benchmarks — measures real SVC fast-path round-trip from userspace.
//!
//! Reports results via thread_exit args: the kernel reads a0-a5 and prints
//! cycle estimates. Each result is the total timer ticks for BATCH_N iterations.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use abi::raw;

const BATCH_N: u64 = 500;

fn bench_svc_null() -> u64 {
    for _ in 0..BATCH_N {
        raw::syscall(255, 0, 0, 0, 0, 0, 0);
    }

    abi::system::isb();

    let start = abi::system::raw_counter();

    for _ in 0..BATCH_N {
        raw::syscall(255, 0, 0, 0, 0, 0, 0);
    }

    abi::system::isb();
    abi::system::raw_counter() - start
}

fn bench_clock_read() -> u64 {
    for _ in 0..BATCH_N {
        let _ = abi::system::clock_read();
    }

    abi::system::isb();

    let start = abi::system::raw_counter();

    for _ in 0..BATCH_N {
        let _ = abi::system::clock_read();
    }

    abi::system::isb();
    abi::system::raw_counter() - start
}

fn bench_handle_info(handle: u64) -> u64 {
    for _ in 0..BATCH_N {
        raw::syscall(raw::num::HANDLE_INFO, handle, 0, 0, 0, 0, 0);
    }

    abi::system::isb();

    let start = abi::system::raw_counter();

    for _ in 0..BATCH_N {
        raw::syscall(raw::num::HANDLE_INFO, handle, 0, 0, 0, 0, 0);
    }

    abi::system::isb();
    abi::system::raw_counter() - start
}

fn bench_event_signal(handle: u64) -> u64 {
    for _ in 0..BATCH_N {
        raw::syscall(raw::num::EVENT_SIGNAL, handle, 0x1, 0, 0, 0, 0);
    }

    abi::system::isb();

    let start = abi::system::raw_counter();

    for _ in 0..BATCH_N {
        raw::syscall(raw::num::EVENT_SIGNAL, handle, 0x1, 0, 0, 0, 0);
    }

    abi::system::isb();
    abi::system::raw_counter() - start
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.boot")]
extern "C" fn _start() -> ! {
    let svc_ticks = bench_svc_null();
    let clock_ticks = bench_clock_read();
    // Bootstrap handle 0 = AddressSpace, handle 1 = code VMO.
    // Use handle 1 for handle_info bench.
    let handle_ticks = bench_handle_info(1);
    let (err, evt) = raw::syscall(raw::num::EVENT_CREATE, 0, 0, 0, 0, 0, 0);
    let event_ticks = if err == 0 {
        let t = bench_event_signal(evt);

        raw::syscall(raw::num::HANDLE_CLOSE, evt, 0, 0, 0, 0, 0);

        t
    } else {
        0
    };

    // Exit with all results in syscall args.
    // a0 = 0 (success), a1-a4 = benchmark ticks, a5 = batch size.
    loop {
        raw::syscall(
            raw::num::THREAD_EXIT,
            0,
            svc_ticks,
            clock_ticks,
            handle_ticks,
            event_ticks,
            BATCH_N,
        );
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    abi::thread::exit(0xDEAD);
}
