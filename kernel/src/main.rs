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

    if let Some((root, asid)) = arch::page_table::create_page_table() {
        let test_page = arch::page_alloc::alloc_page().expect("OOM");
        let test_va = arch::page_table::VirtAddr(0x1000_0000);
        arch::page_table::map_page(root, test_va, test_page, arch::page_table::Perms::RW);
        arch::page_table::destroy_page_table(root, asid);
        println!("page_table: create/map/destroy ok");
    }

    println!("alive");

    arch::cpu::activate_secondaries();

    loop {
        arch::halt();
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
