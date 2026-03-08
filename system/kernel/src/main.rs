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
mod percpu;
mod process;
mod psci;
mod scheduler;
mod slab;
mod sync;
mod syscall;
mod thread;
mod timer;
mod uart;
mod virtio;
mod vma;

use context::Context;

/// User process ELF binaries, compiled by build.rs and embedded in .rodata.
/// Avoids needing a filesystem or bootloader protocol for the first processes.
static INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.elf"));
static ECHO_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/echo.elf"));

extern "C" {
    static __kernel_end: u8;
}

/// Boot secondary cores via PSCI CPU_ON.
///
/// Called after all kernel data structures are initialized. Secondary cores
/// jump to `secondary_entry` (boot.S), which enables MMU and calls
/// `secondary_main` below.
fn boot_secondaries() {
    extern "C" {
        // Physical address of secondary_entry, stored in .rodata by boot.S.
        // Reading this avoids an ADRP relocation across VMA regions.
        static SECONDARY_ENTRY_PA: u64;
    }

    // SAFETY: SECONDARY_ENTRY_PA is a .quad in .rodata set by boot.S.
    let entry_pa = unsafe { core::ptr::read_volatile(&SECONDARY_ENTRY_PA) };

    percpu::init_core(0);

    // Ensure page tables and stacks are visible to secondary cores before
    // they start executing.
    unsafe {
        core::arch::asm!("dsb ish", options(nostack));
    }

    let mut expected_online = 1u32; // Core 0 is already online.

    for core_id in 1..percpu::MAX_CORES as u64 {
        if psci::cpu_on(core_id, entry_pa, core_id).is_ok() {
            expected_online += 1;
        }
    }

    // Wait for all secondaries to finish their boot trampoline (MMU setup
    // in secondary_entry). After this, the boot TTBR0 pages are safe to free.
    while percpu::online_count() < expected_online {
        core::hint::spin_loop();
    }

    // Reclaim the 4 boot TTBR0 page table pages. TTBR1 tables are still
    // live (shared kernel mappings) — do NOT free those.
    reclaim_boot_ttbr0();
}
/// Free the boot identity-map pages (TTBR0) now that all cores have
/// transitioned to upper VA via TTBR1.
fn reclaim_boot_ttbr0() {
    extern "C" {
        static boot_tt0_l0: u8;
        static boot_tt0_l1: u8;
        static boot_tt0_l2_0: u8;
        static boot_tt0_l2_1: u8;
    }

    let pages = unsafe {
        [
            &boot_tt0_l0 as *const u8 as usize,
            &boot_tt0_l1 as *const u8 as usize,
            &boot_tt0_l2_0 as *const u8 as usize,
            &boot_tt0_l2_1 as *const u8 as usize,
        ]
    };

    for &va in &pages {
        let pa = memory::virt_to_phys(va);
        page_alloc::free_frame(pa);
    }
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
    virtio::init();

    // Spawn user processes and create an IPC channel between them.
    let init_id = process::spawn_from_elf(INIT_ELF);
    let echo_id = process::spawn_from_elf(ECHO_ELF);

    channel::create(init_id, echo_id);

    boot_secondaries();

    timer::init();

    uart::puts("🥾 booted.\n");

    loop {
        unsafe { core::arch::asm!("wfe", options(nostack, nomem)) };
    }
}
/// Entry point for secondary cores (called from boot.S secondary_entry).
///
/// `core_id` is the MPIDR affinity (1..7), passed as context_id by PSCI.
/// Initializes per-core GIC, scheduler state, and timer, then enters idle.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(core_id: u64) -> ! {
    gic::init_cpu_interface();
    percpu::init_core(core_id as u32);
    scheduler::init_secondary(core_id as u32);

    // Format as a single string so it prints atomically (one lock acquire).
    let digit = b'0' + core_id as u8;
    let msg = [
        b'c', b'o', b'r', b'e', b' ', digit, b' ', b'o', b'n', b'l', b'i', b'n', b'e', b'\n',
    ];

    // SAFETY: All bytes are ASCII, which is valid UTF-8.
    uart::puts(unsafe { core::str::from_utf8_unchecked(&msg) });
    // Enable timer last — once IRQs are unmasked, this core participates
    // in scheduling and may immediately switch to a user thread.
    timer::init();

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
/// For data aborts (EC=0x24) and instruction aborts (EC=0x20) from EL0,
/// attempts demand paging via the process's VMA map. If the fault address
/// is covered by a VMA, a page is allocated and mapped, and we return to
/// the faulting instruction. Otherwise (or for other exception classes),
/// the process is terminated.
#[unsafe(no_mangle)]
pub extern "C" fn user_fault_handler(ctx: *mut Context) -> *const Context {
    let esr: u64;
    let far: u64;

    // SAFETY: Reading system registers to diagnose the fault. These are
    // read-only queries with no side effects.
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) esr, options(nostack, nomem));
        core::arch::asm!("mrs {}, far_el1", out(reg) far, options(nostack, nomem));
    }

    let ec = (esr >> 26) & 0x3F;

    // EC 0x24 = Data Abort from EL0, EC 0x20 = Instruction Abort from EL0.
    // These are the only exception classes that can be resolved by demand paging.
    if ec == 0x24 || ec == 0x20 {
        let handled = scheduler::current_thread_do(|thread| {
            if let Some(ref mut addr_space) = thread.address_space {
                addr_space.handle_fault(far)
            } else {
                false
            }
        });

        if handled {
            // Page mapped successfully — return to the faulting instruction.
            // The CPU will re-execute it and find the page present.
            return ctx;
        }
    }

    // Unresolvable fault — log and terminate.
    let elr: u64;

    unsafe {
        core::arch::asm!("mrs {}, elr_el1", out(reg) elr, options(nostack, nomem));
    }

    uart::panic_puts("user fault: EC=0x");
    uart::panic_put_hex(ec);
    uart::panic_puts(" ELR=0x");
    uart::panic_put_hex(elr);
    uart::panic_puts(" FAR=0x");
    uart::panic_put_hex(far);
    uart::panic_puts("\n");

    scheduler::exit_current_from_syscall(ctx)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Use panic_ variants to bypass the UART lock (may already be held).
    uart::panic_puts("\n😱 panicking…\n");

    if let Some(location) = info.location() {
        uart::panic_puts(location.file());
        uart::panic_puts(":");
        uart::panic_put_u32(location.line());
        uart::panic_puts("\n");
    }
    if let Some(msg) = info.message().as_str() {
        uart::panic_puts(msg);
        uart::panic_puts("\n");
    }

    loop {
        core::hint::spin_loop();
    }
}
