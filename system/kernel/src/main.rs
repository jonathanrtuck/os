//! Bare-metal aarch64 kernel for QEMU `virt`.
//!
//! # Memory Map
//!
//! ## Physical (QEMU virt, 256 MiB RAM at 0x4000_0000)
//!
//! ```text
//! 0x0800_0000  GICv2 (distributor + CPU interface)
//! 0x0900_0000  PL011 UART
//! 0x4000_0000  RAM_START ─── kernel image (.text/.rodata/.data/.bss)
//!              __kernel_end ─ heap (16 MiB, linked-list allocator)
//!              heap_end ───── page frame pool (rest of RAM, 4 KiB frames)
//! 0x5000_0000  RAM_END
//! ```
//!
//! ## Virtual — TTBR1 (kernel, shared by all threads)
//!
//! ```text
//! 0xFFFF_0000_4000_0000   VA = PA + 0xFFFF_0000_0000_0000
//!                         W^X enforced: .text RX, .rodata RO, .data/.bss RW
//!                         Refined from 2 MiB blocks → 4 KiB L3 pages at boot
//! ```
//!
//! ## Virtual — TTBR0 (per-process, swapped on context switch)
//!
//! ```text
//! 0x0000_0000_0040_0000   User code (ELF segments, matches link.ld)
//! 0x0000_0000_4000_0000   Channel shared memory (one 4 KiB page per channel)
//! 0x0000_0000_7FFF_C000   User stack (4 pages = 16 KiB, guard page below)
//! 0x0000_0000_8000_0000   USER_STACK_TOP
//! ```
//!
//! ## Boot Sequence
//!
//! boot.S: coarse 2 MiB identity map (TTBR0) + kernel VA map (TTBR1),
//! enable MMU, drop EL2→EL1 → `kernel_main` → refine TTBR1 (W^X) →
//! init heap → init frame allocator → init GIC → init scheduler →
//! spawn user processes + IPC channels → start timer (10 Hz) → WFE idle.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exception.S"));

mod addr_space;
mod asid;
mod channel;
mod context;
mod elf;
mod gic;
mod handle;
mod heap;
mod memory;
mod mmio;
mod page_alloc;
mod paging;
mod process;
mod scheduler;
mod sync;
mod syscall;
mod thread;
mod timer;
mod uart;

use context::Context;

/// User process ELF binaries, compiled by build.rs and embedded in .rodata.
/// Avoids needing a filesystem or bootloader protocol for the first processes.
static INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.elf"));
static ECHO_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/echo.elf"));

extern "C" {
    static __kernel_end: u8;
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    uart::puts("🥾 booting…\n");

    memory::init();
    heap::init();

    // Initialize page frame allocator with memory above kernel heap.
    let kernel_end_pa = memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize });
    let heap_end = kernel_end_pa + memory::HEAP_SIZE;
    let ram_end = paging::RAM_END as usize;

    assert!(heap_end < ram_end, "heap extends beyond physical RAM");

    page_alloc::init(heap_end, ram_end);
    gic::init();
    scheduler::init();

    // Spawn user processes and create an IPC channel between them.
    let init_id = process::spawn_from_elf(INIT_ELF);
    let echo_id = process::spawn_from_elf(ECHO_ELF);

    channel::create(init_id, echo_id);

    timer::init();

    uart::puts("🥾 booted.\n");

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
/// Handle non-SVC synchronous exceptions from EL0 (user faults).
///
/// Data aborts, instruction aborts, alignment faults, undefined instructions, etc.
/// Logs the fault, then terminates the faulting process and reschedules.
#[unsafe(no_mangle)]
pub extern "C" fn user_fault_handler(ctx: *mut Context) -> *const Context {
    let esr: u64;
    let elr: u64;
    let far: u64;

    // SAFETY: Reading system registers to diagnose the fault. These are
    // read-only queries with no side effects.
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) esr, options(nostack, nomem));
        core::arch::asm!("mrs {}, elr_el1", out(reg) elr, options(nostack, nomem));
        core::arch::asm!("mrs {}, far_el1", out(reg) far, options(nostack, nomem));
    }

    let ec = (esr >> 26) & 0x3F;

    uart::puts("user fault: EC=0x");
    uart::put_hex(ec);
    uart::puts(" ELR=0x");
    uart::put_hex(elr);
    uart::puts(" FAR=0x");
    uart::put_hex(far);
    uart::puts("\n");

    scheduler::exit_current_from_syscall(ctx)
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
