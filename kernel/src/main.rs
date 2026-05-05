//! Kernel entry point.
//!
//! Boot sequence is split into two phases:
//! - `kernel_main`: runs at physical address (TTBR0 identity map). Sets up
//!   exception vectors, per-CPU data, DTB scan, and enables the MMU.
//! - `kernel_main_upper`: runs at upper-half VA (TTBR1). All code from
//!   this point forward executes from TTBR1, which persists across TTBR0
//!   page table switches for user address spaces.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

use kernel::{frame::arch, println};

/// Phase 1: physical-address boot. Called from boot.S at the physical address.
///
/// Only PC-relative code (no vtables, no function pointers, no trait objects)
/// can run here — absolute addresses resolve to upper-half VAs which are not
/// yet usable.
// SAFETY: no_mangle is required so boot.S can call this symbol via `bl`.
// x0 = dtb_ptr, passed by the hypervisor/firmware in the boot protocol.
#[unsafe(no_mangle)]
extern "C" fn kernel_main(dtb_ptr: usize) -> ! {
    arch::exception::init();
    arch::init_percpu_bsp();
    arch::platform::init(dtb_ptr);
    // mmu::init() enables the MMU and branches to kernel_main_upper at the
    // upper-half VA. It never returns.
    arch::mmu::init();

    unreachable!();
}

/// Phase 2: upper-half VA boot. Called from the MMU trampoline at the
/// TTBR1 upper-half address. From this point on, the kernel runs from
/// TTBR1 and TTBR0 is free for per-process user page tables.
// SAFETY: no_mangle is required so mmu.rs can take its address via
// `unsafe extern "C" { fn kernel_main_upper(); }` for the upper-half branch.
#[unsafe(no_mangle)]
extern "C" fn kernel_main_upper() -> ! {
    arch::exception::reinit_vbar();
    arch::cpu::reinit_percpu_bsp();
    arch::exception::register_handlers();
    arch::serial::enable_lock();
    arch::platform::print_info();
    arch::entropy::init();
    arch::interrupts::init();
    arch::timer::init();
    arch::enable_interrupts();
    arch::page_alloc::init(
        arch::platform::ram_base(),
        arch::platform::ram_size(),
        kernel_end_phys(),
    );

    println!(
        "pages: {} total, {} free",
        arch::page_alloc::total_pages(),
        arch::page_alloc::free_pages(),
    );

    // Bootstrap the init service.
    let init_binary = include_bytes!(concat!(env!("OUT_DIR"), "/init.bin"));
    let mut kern =
        alloc::boxed::Box::new(kernel::syscall::Kernel::new(arch::platform::core_count()));

    arch::set_kernel_ptr(&mut *kern as *mut _ as *mut u8);

    #[cfg(debug_assertions)]
    kernel::post::run(&mut kern);
    kernel::bench::run(&mut kern);

    match kernel::bootstrap::create_init(&mut kern, init_binary) {
        Ok(tid) => {
            println!("init: bootstrapped as thread {}", tid.0);

            arch::cpu::set_current_thread(tid.0);

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

            let space = kern.threads.get(tid.0).unwrap().address_space().unwrap();
            let space_obj = kern.spaces.get(space.0).unwrap();

            arch::page_table::switch_table(
                arch::page_alloc::PhysAddr(space_obj.page_table_root()),
                arch::page_table::Asid(space_obj.asid()),
            );

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

fn kernel_end_phys() -> usize {
    // SAFETY: __kernel_end is a linker-provided symbol at the end of the
    // kernel image. We take its address and convert to PA for the page
    // allocator.
    unsafe extern "C" {
        static __kernel_end: u8;
    }

    arch::platform::virt_to_phys((&raw const __kernel_end) as usize)
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
