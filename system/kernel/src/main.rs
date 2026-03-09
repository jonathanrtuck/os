//! Bare-metal aarch64 kernel for QEMU `virt`.
//!
//! # Memory Map
//!
//! ## Physical (QEMU virt, 256 MiB RAM at 0x4000_0000)
//!
//! ```text
//! 0x0800_0000  GICv2 (distributor + CPU interface)
//! 0x0900_0000  PL011 UART
//! 0x0A00_0000  Virtio MMIO (32 slots, 0x200 stride)
//! 0x4000_0000  RAM_START ─── kernel image (.text/.rodata/.data/.bss)
//!              __kernel_end ─ heap (16 MiB, linked-list + slab allocator)
//!              heap_end ───── page frame pool (buddy allocator, 4 KiB – 4 MiB)
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
//! 0x0000_0000_0040_0000   User code (ELF segments, demand-paged via VMAs)
//! 0x0000_0000_4000_0000   Channel shared memory (one 4 KiB page per channel)
//! 0x0000_0000_7FFF_C000   User stack (4 pages = 16 KiB, guard page below)
//! 0x0000_0000_8000_0000   USER_STACK_TOP
//! ```
//!
//! ## Boot Sequence
//!
//! boot.S: coarse 2 MiB identity map (TTBR0) + kernel VA map (TTBR1),
//! enable MMU, drop EL2→EL1 → `kernel_main` → refine TTBR1 (W^X) →
//! init heap → init buddy allocator → init GIC → init scheduler →
//! probe virtio devices → spawn user processes + IPC channels →
//! boot secondary cores via PSCI → start timer (250 Hz) → WFE idle.

#![no_std]
#![no_main]

extern crate alloc;

use context::Context;
use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exception.S"));

mod address_space;
mod address_space_id;
mod channel;
mod context;
mod device_tree;
mod executable;
mod futex;
mod handle;
mod heap;
mod interrupt_controller;
mod memory;
mod memory_mapped_io;
mod memory_region;
mod page_allocator;
mod paging;
mod per_core;
mod power;
mod process;
mod scheduler;
mod scheduling_algorithm;
mod scheduling_context;
mod serial;
mod slab;
mod sync;
mod syscall;
mod thread;
mod timer;
mod virtio;

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

    per_core::init_core(0);

    // Ensure page tables and stacks are visible to secondary cores before
    // they start executing.
    unsafe {
        core::arch::asm!("dsb ish", options(nostack));
    }

    serial::puts("  🧵 smp - booting secondaries via psci\n");

    let mut expected_online = 1u32; // Core 0 is already online.

    for core_id in 1..per_core::MAX_CORES as u64 {
        if power::cpu_on(core_id, entry_pa, core_id).is_ok() {
            expected_online += 1;
        }
    }

    // Wait for all secondaries to finish their boot trampoline (MMU setup
    // in secondary_entry). After this, the boot TTBR0 pages are safe to free.
    while per_core::online_count() < expected_online {
        core::hint::spin_loop();
    }

    // Reclaim the 4 boot TTBR0 page table pages. TTBR1 tables are still
    // live (shared kernel mappings) — do NOT free those.
    reclaim_boot_ttbr0();
}
/// Find and parse the DTB blob.
///
/// Strategy: try the firmware-provided address first (x0 per aarch64 boot
/// protocol). If that fails (address is 0 or outside RAM), scan RAM for
/// the FDT magic. QEMU on macOS/Apple Silicon (HVF) doesn't pass the DTB
/// address in x0 but does load it into RAM — typically right after the
/// kernel image.
fn find_and_parse_dtb(firmware_pa: u64) -> Option<device_tree::DeviceTable> {
    const FDT_MAGIC: u32 = 0xD00D_FEED;

    // Try firmware-provided address.
    if let Some(dt) = try_parse_dtb_at(firmware_pa) {
        return Some(dt);
    }

    // Scan RAM for FDT magic. Check two regions:
    // 1. Pre-kernel area (0x40000000..0x40080000) — some platforms put DTB here.
    // 2. Post-kernel area (__kernel_end..+2MB) — QEMU typically places DTB after the image.
    // Skip the kernel image itself to avoid false positives.
    let regions = [
        (paging::RAM_START as u64, paging::RAM_START as u64 + 0x80000),
        (
            memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize }).as_u64(),
            memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize }).as_u64()
                + 2 * 1024 * 1024,
        ),
    ];

    for (start, end) in regions {
        let end = end.min(paging::RAM_END as u64);
        let mut addr = start;

        while addr + 4 <= end {
            let va = memory::phys_to_virt(memory::Pa(addr as usize));
            // SAFETY: Address is within mapped RAM range.
            let magic = unsafe { core::ptr::read_volatile(va as *const u32) };

            if u32::from_be(magic) == FDT_MAGIC {
                if let Some(dt) = try_parse_dtb_at(addr) {
                    return Some(dt);
                }
            }

            addr += 4;
        }
    }

    None
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
        page_allocator::free_frame(memory::virt_to_phys(va));
    }
}
/// Try to parse a DTB at the given physical address. Returns None if the
/// address is outside RAM or the blob is invalid.
fn try_parse_dtb_at(pa: u64) -> Option<device_tree::DeviceTable> {
    if pa < paging::RAM_START as u64 || pa >= paging::RAM_END as u64 {
        return None;
    }

    let va = memory::phys_to_virt(memory::Pa(pa as usize));
    let max_len = (paging::RAM_END as u64 - pa) as usize;
    let len = max_len.min(64 * 1024);
    // SAFETY: Address validated within mapped RAM range.
    let blob = unsafe { core::slice::from_raw_parts(va as *const u8, len) };

    device_tree::parse(blob)
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_handler(ctx: *mut Context) -> *const Context {
    let mut next: *const Context = ctx;

    if let Some(iar) = interrupt_controller::acknowledge() {
        let id = iar & 0x3FF;

        if id == timer::IRQ_ID {
            timer::handle_irq();

            next = scheduler::schedule(ctx);
        }

        interrupt_controller::end_of_interrupt(iar);
    }

    next
}
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb_pa: u64) -> ! {
    serial::puts("🥾 booting…\n");
    memory::init();
    serial::puts("  💾 memory - 256mib ram, w^x page tables\n");
    heap::init();
    serial::puts("  📦 heap - 16mib (linked-list + slab)\n");

    // Parse the device tree blob before the page allocator reclaims its memory.
    // Try firmware-provided address first (x0 per aarch64 boot protocol), then
    // scan RAM for the FDT magic if firmware didn't deliver it (e.g. QEMU/macOS).
    let device_table = find_and_parse_dtb(dtb_pa);

    if let Some(ref dt) = device_table {
        serial::puts("  🌳 dtb - ");
        serial::put_u32(dt.device_count() as u32);
        serial::puts(" devices discovered\n");
    } else {
        serial::puts("  🌳 dtb - not found\n");
    }

    // Initialize page frame allocator with memory above kernel heap.
    let kernel_end_pa = memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize });
    let heap_end = kernel_end_pa.0 + memory::HEAP_SIZE;
    let ram_end = paging::RAM_END as usize;

    assert!(heap_end < ram_end, "heap extends beyond physical ram");

    page_allocator::init(heap_end, ram_end);
    serial::puts("  🧩 frames - ");
    serial::put_u32(page_allocator::free_count() as u32);
    serial::puts(" free (buddy allocator, 4k–4m)\n");
    interrupt_controller::init();
    serial::puts("  ⚡ interrupts - gic v2\n");
    scheduler::init();
    serial::puts("  📋 scheduler - eevdf + scheduling contexts\n");
    virtio::init();

    // Spawn user processes and create an IPC channel between them.
    let init_id = process::spawn_from_elf(INIT_ELF).expect("failed to spawn init");
    let echo_id = process::spawn_from_elf(ECHO_ELF).expect("failed to spawn echo");

    channel::create(init_id, echo_id).expect("failed to create ipc channel");
    serial::puts("  🔀 processes - init + echo, ipc channel\n");

    boot_secondaries();

    timer::init();
    serial::puts("  ⏱️  timer - 250hz\n");
    serial::puts("🥾 booted.\n");

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
    interrupt_controller::init_cpu_interface();
    scheduler::init_secondary(core_id as u32);

    // Print before marking online — core 0 waits for online flags, so this
    // guarantees all "core N online" messages appear before core 0 proceeds.
    // Format as a single string so it prints atomically (one lock acquire).
    let digit = b'0' + core_id as u8;
    let msg = [
        b' ', b' ', 0xE2, 0x9C, 0x93, // ✓ (U+2713)
        b' ', b'c', b'o', b'r', b'e', b' ', digit, b' ', b'o', b'n', b'l', b'i', b'n', b'e', b'\n',
    ];

    // SAFETY: All bytes are valid UTF-8 (ASCII + 3-byte U+2713).
    serial::puts(unsafe { core::str::from_utf8_unchecked(&msg) });
    per_core::init_core(core_id as u32);
    // Enable timer last — once IRQs are unmasked, this core participates
    // in scheduling and may immediately switch to a user thread.
    timer::init();

    loop {
        unsafe { core::arch::asm!("wfe", options(nostack, nomem)) };
    }
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

    serial::panic_puts("user fault: EC=0x");
    serial::panic_put_hex(ec);
    serial::panic_puts(" ELR=0x");
    serial::panic_put_hex(elr);
    serial::panic_puts(" FAR=0x");
    serial::panic_put_hex(far);
    serial::panic_puts("\n");
    scheduler::exit_current_from_syscall(ctx)
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Use panic_ variants to bypass the UART lock (may already be held).
    serial::panic_puts("\n😱 panicking…\n");

    if let Some(location) = info.location() {
        serial::panic_puts(location.file());
        serial::panic_puts(":");
        serial::panic_put_u32(location.line());
        serial::panic_puts("\n");
    }
    if let Some(msg) = info.message().as_str() {
        serial::panic_puts(msg);
        serial::panic_puts("\n");
    }

    loop {
        core::hint::spin_loop();
    }
}
