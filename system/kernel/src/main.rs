#![no_std]
#![no_main]
#![feature(sync_unsafe_cell)]

extern crate alloc;

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("boot.S"));

mod gic;
mod heap;
mod memory;
mod mmio;
mod scheduler;
mod syscall;
mod thread;
mod timer;
mod uart;
mod user_test;

/// Thread context saved/restored across exception boundaries.
///
/// Layout must match the CTX_* offsets in boot.S exactly.
/// The const assertions below enforce this at compile time.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31], // x0..x30
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,    // user stack pointer (EL0 threads)
    pub tpidr_el0: u64, // user thread-local base (EL0 threads)
    pub q: [u128; 32],  // FP/SIMD q0..q31
    pub fpcr: u64,      // FP control register (low 32 bits)
    pub fpsr: u64,      // FP status register (low 32 bits)
}

// Verify Context layout matches boot.S CTX_* constants.
const _: () = {
    assert!(core::mem::offset_of!(Context, x) == 0x000);
    assert!(core::mem::offset_of!(Context, sp) == 0x0F8);
    assert!(core::mem::offset_of!(Context, elr) == 0x100);
    assert!(core::mem::offset_of!(Context, spsr) == 0x108);
    assert!(core::mem::offset_of!(Context, sp_el0) == 0x110);
    assert!(core::mem::offset_of!(Context, tpidr_el0) == 0x118);
    assert!(core::mem::offset_of!(Context, q) == 0x120);
    assert!(core::mem::offset_of!(Context, fpcr) == 0x320);
    assert!(core::mem::offset_of!(Context, fpsr) == 0x328);
    assert!(core::mem::size_of::<Context>() == 0x330);
};

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    uart::puts("🥾 booting…\n");

    memory::init();
    heap::init();
    gic::init();
    scheduler::init();

    // Spawn EL0 test thread.
    let user_stack_layout = core::alloc::Layout::from_size_align(16 * 1024, 16).unwrap();
    let user_stack_bottom = unsafe { alloc::alloc::alloc_zeroed(user_stack_layout) };

    assert!(!user_stack_bottom.is_null());

    let user_stack_top = unsafe { user_stack_bottom.add(16 * 1024) } as u64;

    scheduler::spawn_user(
        user_test::user_test_entry as *const () as usize,
        user_stack_top,
    );

    timer::init(); // Unmasks IRQs — all data structures must be ready above this line.

    uart::puts("🥾 booted.\n");

    // Boot thread becomes the idle thread.
    loop {
        unsafe { core::arch::asm!("wfe", options(nostack, nomem)) };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_handler(ctx: *mut Context) -> *const Context {
    let mut next: *const Context = ctx;

    if let Some(iar) = gic::acknowledge() {
        let id = iar & 0x3FF;

        if id == timer::IRQ_ID {
            timer::handle_irq();

            next = scheduler::schedule(ctx);
        }

        gic::end_of_interrupt(iar);
    }

    next
}

#[unsafe(no_mangle)]
pub extern "C" fn svc_handler(ctx: *mut Context) -> *const Context {
    syscall::dispatch(ctx)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    uart::puts("\n😱 panicking…\n");

    if let Some(location) = info.location() {
        uart::puts(location.file());
        uart::puts(":");
        uart::put_u32(location.line());
        uart::puts("\n");
    }

    if let Some(msg) = info.message().as_str() {
        uart::puts(msg);
        uart::puts("\n");
    }

    loop {
        core::hint::spin_loop();
    }
}
