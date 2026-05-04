//! Kernel entry point.
//!
//! Boot sequence: exception vectors, platform discovery (DTB), MMU,
//! serial lock (SMP-safe), entropy, interrupts, timer, secondary cores.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

use kernel::{frame::arch, println};

#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_ptr: usize) -> ! {
    arch::exception::init();
    arch::init_percpu_bsp();
    arch::platform::init(dtb_ptr);
    arch::mmu::init();
    arch::serial::enable_lock();
    arch::entropy::init();
    arch::interrupts::init();
    arch::timer::init();
    arch::enable_interrupts();
    arch::page_alloc::init(
        arch::platform::ram_base(),
        arch::platform::ram_size(),
        kernel_end_addr(),
    );

    println!(
        "pages: {} total, {} free",
        arch::page_alloc::total_pages(),
        arch::page_alloc::free_pages(),
    );

    // Bootstrap the init service.
    let init_binary = include_bytes!(concat!(env!("OUT_DIR"), "/init.bin"));
    let mut kern = kernel::syscall::Kernel::new(arch::platform::core_count());

    arch::set_kernel_ptr(&mut kern as *mut _ as *mut u8);

    kernel::bench::run();

    match kernel::bootstrap::create_init(&mut kern, init_binary) {
        Ok(tid) => {
            println!("init: bootstrapped as thread {}", tid.0);

            // Set init as current thread on core 0.
            arch::cpu::set_current_thread(tid.0);

            // Initialize RegisterState for EL0 entry.
            let thread = kern.threads.get(tid.0).unwrap();
            let entry = thread.entry_point() as u64;
            let stack = thread.stack_top() as u64;
            let arg = thread.arg() as u64;
            let rs = kern.threads.get_mut(tid.0).unwrap().init_register_state();

            rs.pc = entry;
            rs.sp = stack;
            rs.gprs[0] = arg;
            rs.pstate = 0; // EL0t

            println!("alive");

            arch::cpu::activate_secondaries();

            // Switch to init's page table before entering userspace.
            let space = kern.threads.get(tid.0).unwrap().address_space().unwrap();
            let space_obj = kern.spaces.get(space.0).unwrap();

            arch::page_table::switch_table(
                arch::page_alloc::PhysAddr(space_obj.page_table_root()),
                arch::page_table::Asid(space_obj.asid()),
            );

            // Enter userspace — never returns.
            let rs = kern.threads.get(tid.0).unwrap().register_state().unwrap();

            arch::context::enter_userspace(rs);
        }
        Err(e) => {
            println!("init: bootstrap failed: {:?}", e);
            println!("alive");

            arch::cpu::activate_secondaries();

            loop {
                arch::halt();
            }
        }
    }
}

fn kernel_end_addr() -> usize {
    unsafe extern "C" {
        static __kernel_end: u8;
    }

    (&raw const __kernel_end) as usize
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    arch::disable_interrupts();
    arch::serial::break_lock();

    println!();
    println!("panic: {info}");
    println!();

    arch::dump_panic_registers();
    arch::signal_panic();

    loop {
        arch::halt();
    }
}
