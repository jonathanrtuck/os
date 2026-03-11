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
//! probe virtio → spawn init (proto-OS-service) with device manifest →
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
mod interrupt;
mod interrupt_controller;
mod memory;
mod memory_mapped_io;
mod memory_region;
mod metrics;
mod page_allocator;
mod paging;
mod per_core;
mod power;
mod process;
mod process_exit;
mod scheduler;
mod scheduling_algorithm;
mod scheduling_context;
mod serial;
mod slab;
mod sync;
mod syscall;
mod thread;
mod thread_exit;
mod timer;
mod waitable;

/// Virtio MMIO constants for device probe.
const VIRTIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_BASE_PA: u64 = 0x0A00_0000;
const VIRTIO_MMIO_STRIDE: u64 = 0x200;
const VIRTIO_MMIO_COUNT: usize = 32;
const VIRTIO_IRQ_BASE: u32 = 48; // SPI 16 = GIC IRQ 48

/// Info discovered about a virtio-mmio device.
struct VirtioDeviceInfo {
    pa: u64,
    irq: u32,
    device_id: u32,
}

extern "C" {
    static __kernel_end: u8;
}

/// Init ELF — the only process the kernel spawns directly.
/// Init is the proto-OS-service that spawns all other processes.
static INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.elf"));

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
/// Probe virtio-mmio devices from the DTB.
fn probe_from_dtb(
    dt: &device_tree::DeviceTable,
    out: &mut [Option<VirtioDeviceInfo>; 8],
    count: &mut usize,
) {
    for dtb_dev in dt.find_all("virtio,mmio") {
        if *count >= out.len() {
            break;
        }

        let pa = dtb_dev.base_address();
        let base = pa as usize + memory::KERNEL_VA_OFFSET;

        if memory_mapped_io::read32(base) != VIRTIO_MAGIC {
            continue;
        }

        let version = memory_mapped_io::read32(base + 4);

        if version != 1 && version != 2 {
            continue;
        }

        let device_id = memory_mapped_io::read32(base + 8);

        if device_id == 0 {
            continue;
        }

        out[*count] = Some(VirtioDeviceInfo {
            pa,
            irq: dtb_dev.irq.unwrap_or(0),
            device_id,
        });
        *count += 1;
    }
}
/// Fallback probe: scan all 32 hardcoded QEMU `virt` virtio-mmio slots.
fn probe_hardcoded(out: &mut [Option<VirtioDeviceInfo>; 8], count: &mut usize) {
    for i in 0..VIRTIO_MMIO_COUNT {
        if *count >= out.len() {
            break;
        }

        let pa = VIRTIO_MMIO_BASE_PA + i as u64 * VIRTIO_MMIO_STRIDE;
        let base = pa as usize + memory::KERNEL_VA_OFFSET;

        if memory_mapped_io::read32(base) != VIRTIO_MAGIC {
            continue;
        }

        let version = memory_mapped_io::read32(base + 4);

        if version != 1 && version != 2 {
            continue;
        }

        let device_id = memory_mapped_io::read32(base + 8);

        if device_id == 0 {
            continue;
        }

        out[*count] = Some(VirtioDeviceInfo {
            pa,
            irq: VIRTIO_IRQ_BASE + i as u32,
            device_id,
        });
        *count += 1;
    }
}
/// Probe virtio-mmio devices and return the results.
fn probe_virtio_devices(
    device_table: Option<&device_tree::DeviceTable>,
    devices: &mut [Option<VirtioDeviceInfo>; 8],
) -> usize {
    let mut count = 0;

    if let Some(dt) = device_table {
        probe_from_dtb(dt, devices, &mut count);
    } else {
        probe_hardcoded(devices, &mut count);
    }

    count
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
/// Write a device manifest to a channel shared page.
///
/// The manifest lists all discovered virtio devices so init can spawn
/// the appropriate drivers. Format:
///
/// ```text
/// offset 0:  device_count (u32)
/// offset 4:  device[0]: { pa: u64, irq: u32, device_id: u32 }  (16 bytes)
/// offset 20: device[1]: ...
/// ```
fn write_device_manifest(
    shared_pa: memory::Pa,
    devices: &[Option<VirtioDeviceInfo>; 8],
    count: usize,
) {
    let shared_va = memory::phys_to_virt(shared_pa) as *mut u8;

    unsafe {
        // Write device count at offset 0 (u32). Offset 4 is padding.
        core::ptr::write_volatile(shared_va as *mut u32, count as u32);

        // Write each 16-byte device entry starting at offset 8 (8-byte aligned).
        for i in 0..count {
            if let Some(ref dev) = devices[i] {
                let base = shared_va.add(8 + i * 16);

                core::ptr::write_volatile(base as *mut u64, dev.pa);
                core::ptr::write_volatile(base.add(8) as *mut u32, dev.irq);
                core::ptr::write_volatile(base.add(12) as *mut u32, dev.device_id);
            }
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_handler(ctx: *mut Context) -> *const Context {
    let mut next: *const Context = ctx;

    if let Some(iar) = interrupt_controller::acknowledge() {
        let id = iar & 0x3FF;

        if id == timer::IRQ_ID {
            metrics::inc_timer_ticks();
            timer::handle_irq();
        } else {
            // Forward to registered userspace driver (if any).
            interrupt::handle_irq(id);
        }

        // Reschedule after any IRQ — timer tick or woken driver thread.
        next = scheduler::schedule(ctx);

        interrupt_controller::end_of_interrupt(iar);
    }

    next
}
/// Handle fatal exceptions from EL1 (kernel faults).
///
/// Called from exception.S on a per-core emergency stack (the original SP
/// may be corrupted, e.g. by a kernel stack overflow hitting a guard page).
/// Diagnoses the fault, prints diagnostic info, and panics.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_fault_handler(esr: u64, elr: u64, far: u64, exc_type: u64) -> ! {
    let ec = (esr >> 26) & 0x3F;
    let type_name = match exc_type {
        0 => "sync",
        1 => "FIQ",
        _ => "SError",
    };

    serial::panic_puts("\n💥 kernel ");
    serial::panic_puts(type_name);
    serial::panic_puts(": EC=0x");
    serial::panic_put_hex(ec);
    serial::panic_puts(" ESR=0x");
    serial::panic_put_hex(esr);
    serial::panic_puts(" ELR=0x");
    serial::panic_put_hex(elr);
    serial::panic_puts(" FAR=0x");
    serial::panic_put_hex(far);

    if ec == 0x25 {
        // Data Abort from current EL — most likely a guard page hit.
        serial::panic_puts("\ndata abort at EL1 — likely kernel stack overflow");
    } else if ec == 0x21 {
        serial::panic_puts("\ninstruction abort at EL1");
    }

    serial::panic_puts("\n");

    panic!("unrecoverable kernel fault");
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

    // Wire DTB into device initialization.
    let gic_from_dtb = if let Some(ref dt) = device_table {
        // GIC: look for "arm,cortex-a15-gic" (QEMU virt GICv2).
        // The reg property has two entries: [distributor, CPU interface].
        if let Some(gic) = dt.find_first("arm,cortex-a15-gic") {
            if gic.regs.len() >= 2 {
                interrupt_controller::set_base_addresses(gic.regs[0].0, gic.regs[1].0);

                true
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    interrupt_controller::init();

    if gic_from_dtb {
        serial::puts("  ⚡ interrupts - gic v2 (dtb)\n");
    } else {
        serial::puts("  ⚡ interrupts - gic v2 (hardcoded)\n");
    }

    scheduler::init();
    serial::puts("  📋 scheduler - eevdf + scheduling contexts\n");

    // Probe virtio devices (before spawning init so the manifest is ready).
    let mut devices = [const { None }; 8];
    let device_count = probe_virtio_devices(device_table.as_ref(), &mut devices);

    if device_count == 0 {
        serial::puts("  🔌 virtio - no devices\n");
    } else {
        serial::puts("  🔌 virtio - ");
        serial::put_u32(device_count as u32);
        serial::puts(" devices found\n");
    }

    // Spawn init (suspended) — the only process the kernel creates directly.
    // Microkernel pattern: kernel provides mechanism, init provides policy.
    let (init_pid, _) = process::create_from_user_elf(INIT_ELF).expect("failed to create init");
    let (ch_a, ch_b) = channel::create().expect("failed to create init channel");
    // Write device manifest to channel page 0 (kernel→init direction).
    let pages = channel::shared_pages(ch_a).expect("channel shared pages");

    write_device_manifest(pages[0], &devices, device_count);

    // Give init the channel endpoint.
    channel::setup_endpoint(ch_b, init_pid).expect("failed to setup init channel");
    // Close kernel's endpoint — init reads the manifest from the shared page.
    channel::close_endpoint(ch_a);
    // Start init now that everything is set up.
    scheduler::start_suspended_threads(init_pid);
    serial::puts("  🔀 processes - init started with device manifest\n");

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
/// For translation faults (missing pages) from data aborts (EC=0x24) and
/// instruction aborts (EC=0x20), attempts demand paging via the process's
/// VMA map. Only translation faults (DFSC/IFSC 0b0001xx, levels 0-3) are
/// demand-paging candidates. All other fault types (permission, alignment,
/// access flag, etc.) skip straight to diagnostic + terminate.
///
/// Without the DFSC check, a non-translation fault on a VMA-backed address
/// would cause handle_fault to remap an already-present page, return true,
/// and create an infinite fault loop with a one-page-per-iteration leak.
#[unsafe(no_mangle)]
pub extern "C" fn user_fault_handler(ctx: *mut Context) -> *const Context {
    let esr: u64;
    let far: u64;
    let elr: u64;

    // SAFETY: Reading system registers to diagnose the fault. These are
    // read-only queries with no side effects.
    unsafe {
        core::arch::asm!("mrs {}, esr_el1", out(reg) esr, options(nostack, nomem));
        core::arch::asm!("mrs {}, far_el1", out(reg) far, options(nostack, nomem));
        core::arch::asm!("mrs {}, elr_el1", out(reg) elr, options(nostack, nomem));
    }

    let ec = (esr >> 26) & 0x3F;
    let fsc = esr & 0x3F; // DFSC (data abort) or IFSC (instruction abort)

    // Translation faults: DFSC/IFSC 0b0001xx (levels 0-3).
    // Only these can be resolved by demand paging. Permission faults,
    // alignment faults, access flag faults, etc. must NOT attempt paging —
    // the page is already mapped; remapping would loop and leak memory.
    let is_translation_fault = (fsc & 0b111100) == 0b000100;

    if (ec == 0x24 || ec == 0x20) && is_translation_fault {
        metrics::inc_page_faults();

        let handled =
            scheduler::current_process_do(|process| process.address_space.handle_fault(far));

        if handled {
            // Page mapped successfully — return to the faulting instruction.
            // The CPU will re-execute it and find the page present.
            return ctx;
        }
    }

    // Unresolvable fault — log and terminate.
    serial::panic_puts("user fault: EC=0x");
    serial::panic_put_hex(ec);
    serial::panic_puts(" ISS=0x");
    serial::panic_put_hex(esr & 0x1FFFFFF);
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

    metrics::panic_dump();

    loop {
        core::hint::spin_loop();
    }
}
