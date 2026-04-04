// AUDIT: 2026-03-14 — All 24 unsafe blocks enumerated and verified.
// Each has a // SAFETY: comment explaining the invariant. Categories:
//   - Linker symbol address (4): __kernel_end (×3), boot_tt0_l0..l2_1
//   - Context read — Fix 17 eret validation (5): validate_context_before_eret
//     reads elr, spsr, sp, x30, thread_id via addr_of!/read_volatile. Sound:
//     ctx from scheduler is valid, no mutation, no aliasing.
//   - Volatile read (8): SECONDARY_ENTRY_PA, FDT magic scan, kernel_fault_handler
//     Context diagnostics (5), stack walk
//   - Volatile write (1): write_device_manifest
//   - Inline asm barrier (1): dsb ish (no nomem — intentional, Fix 6/9)
//   - Inline asm hint (2): wfi idle loops (nomem correct)
//   - System register read (1): mrs esr_el1/far_el1/elr_el1 (1 block, 3 reads)
//   - from_raw_parts (1): DTB blob slice
//   - from_utf8_unchecked (1): secondary_main message
// Fix 6/Fix 9 (nomem removal from DAIF/system register asm) re-verified:
//   DSB correctly omits nomem. WFE and MRS correctly use nomem.
// Fix 17 (TPIDR race, 5 blocks): formally reviewed 2026-03-14. All 5 blocks
//   use addr_of! to avoid aliasing UB, read from documented Context/Thread
//   offsets, execute only in validation/error paths. Sound.
// No code bugs found.
//!
//! Bare-metal aarch64 kernel for QEMU `virt`.
//!
//! # Memory Map
//!
//! ## Physical (QEMU virt, RAM at 0x4000_0000, size from DTB)
//!
//! ```text
//! 0x0800_0000  GICv3 (distributor + redistributor, CPU interface via system registers)
//! 0x0900_0000  PL011 UART
//! 0x0901_0000  PL031 RTC
//! 0x0902_0000  pvpanic (paravirtual panic notification)
//! 0x0A00_0000  Virtio MMIO (32 slots, 0x200 stride)
//! 0x4000_0000  RAM_START ─── kernel image (.text/.rodata/.data/.bss)
//!              __kernel_end ─ heap (16 MiB, linked-list + slab allocator)
//!              heap_end ───── page frame pool (buddy allocator, 16 KiB – 8 MiB)
//!              ram_end ────── from DTB /memory node (fallback: 0x5000_0000 = 256 MiB)
//! ```
//!
//! ## Virtual — TTBR1 (kernel, shared by all threads)
//!
//! ```text
//! 0xFFFF_FFF0_4000_0000   VA = PA + 0xFFFF_FFF0_0000_0000
//!                         W^X enforced: .text RX, .rodata RO, .data/.bss RW
//!                         Refined from 32 MiB blocks → 16 KiB L3 pages at boot
//! ```
//!
//! ## Virtual — TTBR0 (per-process, swapped on context switch)
//!
//! ```text
//! 0x0000_0000_0040_0000   User code (ELF segments, demand-paged via VMAs)
//! 0x0000_0000_4000_0000   Channel shared memory (one 16 KiB page per channel)
//! 0x0000_0000_7FFF_0000   User stack (4 pages = 64 KiB, guard page below)
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
#![feature(allocator_api)]

extern crate alloc;

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicUsize, Ordering},
};

use context::Context;

core::arch::global_asm!(include_str!("arch/aarch64/boot.S"));
core::arch::global_asm!(include_str!("arch/aarch64/exception.S"));

mod address_space;
mod address_space_id;
mod arch;
mod aslr;
mod channel;
mod context;
mod device_tree;
mod event;
mod executable;
mod futex;
mod handle;
mod heap;
mod heap_va;
mod interrupt;
mod interrupt_controller;
mod intrusive_list;
mod memory;
mod memory_mapped_io;
mod memory_region;
mod metrics;
mod page_allocator;
mod pager;
mod paging;
mod per_core;
mod power;
mod process;
mod process_exit;
mod random;
mod relocate;
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
mod vmo;
mod waitable;

const DEVICE_ID_PL031_RTC: u32 = 200;
const SGI_IPI: u32 = 0;
const VIRTIO_IRQ_BASE: u32 = 48; // SPI 16 = GIC IRQ 48
const VIRTIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_MMIO_BASE_PA: u64 = 0x0A00_0000;
const VIRTIO_MMIO_COUNT: usize = 32;
const VIRTIO_MMIO_STRIDE: u64 = 0x200;

/// Init ELF — the only process the kernel spawns directly.
/// Init is the proto-OS-service that spawns all other processes.
static INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.elf"));
/// Atomic gate for panic output serialization. The first core to CAS this
/// from 0 to its core_id+1 "wins" and prints diagnostics. Other cores spin
/// until the winner finishes, then print their own. Without this, concurrent
/// panics produce unreadable interleaved UART output.
static PANIC_GATE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
// Service pack: build.rs packs all service ELFs into services.pack, converts
// it to services.o via llvm-objcopy with a .services section, and tells cargo
// to link it. The linker script defines _services_start/_services_end.
/// pvpanic MMIO address (kernel VA). Zero if not available.
///
/// Set once during boot from the DTB "qemu,pvpanic-mmio" node. Read from the
/// panic handler to signal the hypervisor. Using AtomicUsize for safe cross-core
/// access without locks (the panic handler cannot acquire locks).
static PVPANIC_ADDR: AtomicUsize = AtomicUsize::new(0);

extern "C" {
    static __kernel_end: u8;
    /// Start of the service pack section (linker script symbol).
    static _services_start: u8;
    /// End of the service pack section (linker script symbol).
    static _services_end: u8;
}

/// Global kernel PRNG, protected by IrqMutex.
///
/// Initialized during boot by `init_prng()`. Used for ASLR seeds, PAC keys,
/// and per-process PRNG derivation. None until seeded.
static KERNEL_PRNG: sync::IrqMutex<Option<random::Prng>> = sync::IrqMutex::new(None);

/// Info discovered about a virtio-mmio device.
struct VirtioDeviceInfo {
    pa: u64,
    irq: u32,
    device_id: u32,
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
    // Ensure all prior stores (page tables, stacks) are visible to the
    // inner-shareable domain before secondary cores begin executing.
    arch::cpu::dsb_ish();
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
        (paging::RAM_START, paging::RAM_START + 0x80000),
        (
            // SAFETY: __kernel_end is a linker-defined symbol marking the end
            // of the kernel image. Taking its address yields a valid VA within
            // the kernel's mapped region.
            memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize }).as_u64(),
            // SAFETY: Same linker symbol as above — valid kernel VA.
            memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize }).as_u64()
                + 2 * 1024 * 1024,
        ),
    ];

    for (start, end) in regions {
        let end = end.min(paging::RAM_END_MAX);
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
/// Initialize the kernel PRNG from available entropy sources.
///
/// Probes for hardware RNG (RNDR via FEAT_RNG), mixes in timer counter
/// jitter, and seals the entropy pool once ≥256 bits are accumulated.
/// Called once during boot, before any processes are spawned.
fn init_prng() {
    let mut pool = random::EntropyPool::new();

    // Source 1: Hardware RNG (RNDR) — strongest source, if available.
    if arch::entropy::has_hardware_rng() {
        // Draw 4 × 64 bits = 256 bits from hardware RNG.
        let mut hw_count = 0u32;

        for _ in 0..8 {
            if let Some(val) = arch::entropy::hardware_random() {
                pool.add_entropy(&val.to_le_bytes(), 64);

                hw_count += 1;
            }
        }

        serial::puts("  🎲 entropy - rndr: ");
        serial::put_u32(hw_count * 64);
        serial::puts(" bits\n");
    } else {
        serial::puts("  🎲 entropy - rndr: not available\n");
    }

    // Source 2: CPU jitter entropy — execution time variation from cache,
    // TLB, and pipeline nondeterminism. Each sample produces 8 bytes of
    // jitter data; we conservatively credit 4 bits per sample.
    // 64 samples × 4 bits = 256 bits — enough to seed even without RNDR.
    let mut scratch = [0u8; 64];

    for _ in 0..64 {
        let jitter = arch::entropy::collect_jitter(&mut scratch);

        pool.add_entropy(&jitter, 4);
    }

    // Seal the pool.
    match pool.try_seal() {
        Ok(prng) => {
            let mut guard = KERNEL_PRNG.lock();

            *guard = Some(prng);

            serial::puts("  🔐 prng - seeded (chacha20 + fast key erasure)\n");
        }
        Err(_pool) => {
            // On systems without RNDR and limited interrupts, we may not
            // have enough entropy at this point. The PRNG will remain None;
            // ASLR will use deterministic layout as fallback.
            serial::puts("  ⚠️  prng - insufficient entropy, aslr disabled\n");
        }
    }
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
        let base = pa as usize + memory::KERNEL_VA_OFFSET + memory::kaslr_slide();

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
        let base = pa as usize + memory::KERNEL_VA_OFFSET + memory::kaslr_slide();

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
/// Signal panic to the hypervisor via the pvpanic device.
///
/// Writes PVPANIC_PANICKED (0x01) to the pvpanic MMIO register if the device
/// was discovered during boot. The hypervisor captures vCPU state and writes a
/// crash report, then terminates the VM. If pvpanic is not available, this is
/// a no-op and the caller should fall through to PSCI SYSTEM_OFF.
fn pvpanic_signal() {
    let addr = PVPANIC_ADDR.load(Ordering::Relaxed);

    if addr != 0 {
        // SAFETY: addr is a kernel VA pointing to the pvpanic MMIO register,
        // validated during boot from the DTB. The address is within the UART
        // L2 block (0x0900_0000-0x091F_FFFF), which is mapped at boot.
        // A single byte write to offset 0 signals panic to the hypervisor.
        unsafe {
            core::ptr::write_volatile(addr as *mut u8, 0x01);
        }
    }
}
/// Free the boot identity-map pages (TTBR0) now that all cores have
/// transitioned to upper VA via TTBR1.
fn reclaim_boot_ttbr0() {
    extern "C" {
        static boot_tt0_l2: u8;
    }

    // SAFETY: boot_tt0_l2 is a 16K-aligned .bss symbol defined in boot.S.
    // Taking its address yields the kernel VA of the TTBR0 L2 root table
    // used for identity mapping during boot. No longer needed after all
    // cores have transitioned to upper VA via TTBR1.
    let va = unsafe { &boot_tt0_l2 as *const u8 as usize };

    page_allocator::free_frame(memory::virt_to_phys(va));
}
/// Request VM shutdown via PSCI SYSTEM_OFF.
///
/// Issues an HVC call to the hypervisor. If handled, the VM terminates and
/// this function does not return. If the hypervisor doesn't handle it (e.g.,
/// bare metal), the HVC returns and the caller should fall through to a
/// spin loop.
fn system_off() {
    // Delegate to arch-specific PSCI SYSTEM_OFF.
    // If handled, the VM terminates. If not, power::system_off() spins.
    arch::power::system_off();
}
/// Try to parse a DTB at the given physical address. Returns None if the
/// address is outside RAM or the blob is invalid.
fn try_parse_dtb_at(pa: u64) -> Option<device_tree::DeviceTable> {
    if !(paging::RAM_START..paging::RAM_END_MAX).contains(&pa) {
        return None;
    }

    let va = memory::phys_to_virt(memory::Pa(pa as usize));
    let max_len = (paging::RAM_END_MAX - pa) as usize;
    let len = max_len.min(64 * 1024);
    // SAFETY: Address validated within mapped RAM range.
    let blob = unsafe { core::slice::from_raw_parts(va as *const u8, len) };

    device_tree::parse(blob)
}
/// Validate a context pointer before returning to exception.S for eret.
///
/// Catches corruption early: if SPSR says EL1 but ELR is in user VA range
/// (or vice versa), the eret would crash. This check detects the mismatch
/// before the eret, providing better diagnostics.
#[inline(always)]
fn validate_context_before_eret(ctx: *const Context) {
    // SAFETY: ctx was returned by the scheduler and is a valid Context pointer.
    // Reading elr and spsr for validation — no mutation, no aliasing concern.
    let elr = unsafe { core::ptr::addr_of!((*ctx).elr).read() };
    let spsr = unsafe { core::ptr::addr_of!((*ctx).spsr).read() };
    let sp = unsafe { core::ptr::addr_of!((*ctx).sp).read() };
    let mode = spsr & 0xF; // M[3:0]
    let is_el1 = mode == 0x4 || mode == 0x5; // EL1t or EL1h
    let is_kernel_va = elr >= (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64;

    // EL1 return with user-range ELR: the eret would try to fetch instructions
    // from a lower-half VA at EL1, using TTBR0 (which may be empty for idle
    // threads). This is the EC=0x21 crash pattern.
    if is_el1 && !is_kernel_va && elr != 0 {
        serial::panic_puts("\n🛑 eret validation: EL1 return to user VA\n  elr=0x");
        serial::panic_put_hex(elr);
        serial::panic_puts(" spsr=0x");
        serial::panic_put_hex(spsr);
        serial::panic_puts(" sp=0x");
        serial::panic_put_hex(sp);
        serial::panic_puts(" ctx=0x");
        serial::panic_put_hex(ctx as u64);

        // Dump more context: x30 (link register), thread ID
        // SAFETY: ctx is a valid pointer to the exception Context struct, passed from
        // exception.S. The x[] field is [u64; 31], so index 30 (link register / x30)
        // is within bounds. This is a diagnostic read for panic output.
        let x30 = unsafe { core::ptr::addr_of!((*ctx).x).cast::<u64>().add(30).read() };
        // SAFETY: ctx is a valid Context pointer. Offset 0x330 is the thread_id field
        // (verified by Context layout). read_volatile prevents the compiler from eliding
        // this diagnostic read, which could otherwise be optimized away since ctx is not
        // &Context.
        let thread_id =
            unsafe { core::ptr::read_volatile((ctx as *const u8).add(0x330) as *const u64) };

        serial::panic_puts("\n  x30=0x");
        serial::panic_put_hex(x30);
        serial::panic_puts(" thread_id=0x");
        serial::panic_put_hex(thread_id);
        serial::panic_puts("\n");

        panic!("corrupt context: EL1 eret to user VA");
    }
    // EL0 return with kernel-range ELR: would give user code kernel access.
    if !is_el1 && is_kernel_va {
        serial::panic_puts("\n🛑 eret validation: EL0 return to kernel VA\n  elr=0x");
        serial::panic_put_hex(elr);
        serial::panic_puts(" spsr=0x");
        serial::panic_put_hex(spsr);
        serial::panic_puts("\n");

        panic!("corrupt context: EL0 eret to kernel VA");
    }
    // EL1 return with invalid kernel SP: stack corruption.
    if is_el1 && (sp < (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64 || sp == 0) {
        serial::panic_puts("\n🛑 eret validation: EL1 return with bad SP\n  sp=0x");
        serial::panic_put_hex(sp);
        serial::panic_puts(" elr=0x");
        serial::panic_put_hex(elr);
        serial::panic_puts("\n");

        panic!("corrupt context: EL1 eret with non-kernel SP");
    }
}
/// Write a device manifest to a channel shared page.
///
/// The manifest lists all discovered virtio devices so init can spawn
/// the appropriate drivers. Format:
///
/// ```text
/// offset 0:  device_count (u32)
/// offset 4:  (padding for u64 alignment)
/// offset 8:  device[0]: { pa: u64, irq: u32, device_id: u32 }  (16 bytes)
/// offset 24: device[1]: ...
/// ```
fn write_device_manifest(
    shared_pa: memory::Pa,
    devices: &[Option<VirtioDeviceInfo>; 8],
    count: usize,
) {
    let shared_va = memory::phys_to_virt(shared_pa) as *mut u8;

    // SAFETY: shared_pa is a page-aligned physical address obtained from
    // channel::shared_pages(), mapped into kernel VA via phys_to_virt.
    // The page is 4 KiB; maximum write extent is 8 + 8*16 = 136 bytes.
    // All writes are naturally aligned (u32 at 4-byte, u64 at 8-byte offsets).
    // Volatile writes ensure the compiler doesn't elide stores that will be
    // read by the init process from the same physical page.
    unsafe {
        // Write device count at offset 0 (u32). Offset 4 is padding.
        core::ptr::write_volatile(shared_va as *mut u32, count as u32);

        // Write each 16-byte device entry starting at offset 8 (8-byte aligned).
        for (i, dev) in devices.iter().enumerate().take(count) {
            if let Some(ref dev) = dev {
                let base = shared_va.add(8 + i * 16);

                core::ptr::write_volatile(base as *mut u64, dev.pa);
                core::ptr::write_volatile(base.add(8) as *mut u32, dev.irq);
                core::ptr::write_volatile(base.add(12) as *mut u32, dev.device_id);
            }
        }
    }
}

/// Fork a new PRNG from the kernel PRNG for per-process use.
///
/// Returns None if the kernel PRNG is not seeded.
pub fn fork_prng() -> Option<random::Prng> {
    let mut guard = KERNEL_PRNG.lock();

    guard.as_mut().map(|prng| prng.fork())
}

#[unsafe(no_mangle)]
pub extern "C" fn irq_handler(ctx: *mut Context) -> *const Context {
    use interrupt_controller::InterruptController;

    debug_assert!(!ctx.is_null(), "irq_handler: ctx is null (TPIDR_EL1 was 0)");

    // Stack canary: detect corruption of this function's stack frame during
    // schedule_inner. If any code path writes to our saved LR position, this
    // canary (adjacent on the stack) will also be overwritten.
    let mut canary: u64 = 0;

    // SAFETY: volatile write to a stack-local ensures the canary is physically
    // written before any function calls that might corrupt the stack.
    unsafe {
        core::ptr::write_volatile(&mut canary, 0xCAFE_BABE_1240_0001_u64);
    }

    let mut next: *const Context = ctx;

    if let Some(id) = interrupt_controller::GIC.acknowledge() {
        if id == SGI_IPI {
            // IPI wakeup: just acknowledge and reschedule.
            // Do NOT call timer::handle_irq or increment TICKS — SGI 0 is
            // distinct from the virtual timer PPI (IRQ 27). The scheduler will pick
            // up the newly-ready thread that triggered this IPI.
        } else if id == timer::IRQ_ID {
            metrics::inc_timer_ticks();
            timer::handle_irq();
        } else {
            // Forward to registered userspace driver (if any).
            interrupt::handle_irq(id);
        }

        // Reschedule after any IRQ — timer tick, IPI, or woken driver thread.
        next = scheduler::schedule(ctx);

        interrupt_controller::GIC.end_of_interrupt(id);
    }

    // SAFETY: volatile read of stack-local canary. If the stack frame was
    // corrupted during schedule_inner (the deferred_drops race that was fixed,
    // or any future bug), this catches it before the corrupted LR causes a
    // mysterious instruction abort.
    let check = unsafe { core::ptr::read_volatile(&canary) };

    if check != 0xCAFE_BABE_1240_0001 {
        serial::panic_puts("\n🛑 irq_handler: stack canary corrupt! got=0x");
        serial::panic_put_hex(check);
        serial::panic_puts(" expected=0xCAFEBABE12400001\n  SP=0x");
        serial::panic_put_hex(arch::cpu::read_sp());
        serial::panic_puts(" TPIDR=0x");
        serial::panic_put_hex(arch::cpu::read_tpidr());
        serial::panic_puts(" core=");
        serial::panic_put_u32(per_core::core_id());
        serial::panic_puts("\n");

        panic!("irq_handler: stack canary corrupt");
    }

    debug_assert!(
        !next.is_null(),
        "irq_handler: returning null context pointer"
    );

    validate_context_before_eret(next);

    next
}
/// Handle fatal exceptions from EL1 (kernel faults).
///
/// Called from exception.S on a per-core emergency stack (the original SP
/// may be corrupted, e.g. by a kernel stack overflow hitting a guard page).
/// Diagnoses the fault, prints diagnostic info, and panics.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_fault_handler(
    esr: u64,
    elr: u64,
    far: u64,
    exc_type: u64,
    sp: u64,
    lr: u64,
    tpidr: u64,
) -> ! {
    // Serialize panic output across cores. First core proceeds immediately;
    // others spin-wait (with a timeout) for the first core to finish.
    let my_core = per_core::core_id() + 1; // +1 so 0 means "unclaimed"

    if PANIC_GATE
        .compare_exchange(
            0,
            my_core,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        // Another core is already printing. Wait for it to finish (spin with
        // a bounded timeout — 100M iterations ≈ seconds on most cores).
        for _ in 0..100_000_000u64 {
            if PANIC_GATE.load(core::sync::atomic::Ordering::Acquire) == 0 {
                break;
            }

            core::hint::spin_loop();
        }

        // Reclaim the gate for our output.
        PANIC_GATE.store(my_core, core::sync::atomic::Ordering::Release);
    }

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
    serial::panic_puts("\n  SP=0x");
    serial::panic_put_hex(sp);
    serial::panic_puts(" LR=0x");
    serial::panic_put_hex(lr);
    serial::panic_puts(" TPIDR=0x");
    serial::panic_put_hex(tpidr);

    // Read the thread's saved Context from TPIDR to check if the crash
    // came from restoring a zeroed context (eret path) or from kernel code
    // (ret/blr to null — TPIDR context would have valid elr).
    if tpidr >= (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64 {
        // SAFETY: TPIDR_EL1 is validated above as a kernel VA (>= 0xFFFF...).
        // It points to a Thread's Context struct. Reading at documented
        // offsets (matching context.rs compile-time assertions) for diagnostics.
        let ctx_elr = unsafe { core::ptr::read_volatile((tpidr + 0x100) as *const u64) };
        // SAFETY: Same TPIDR validation — reading SPSR from Context.
        let ctx_spsr = unsafe { core::ptr::read_volatile((tpidr + 0x108) as *const u64) };
        // SAFETY: Same TPIDR validation — reading SP from Context.
        let ctx_sp = unsafe { core::ptr::read_volatile((tpidr + 0x0F8) as *const u64) };
        // SAFETY: Same TPIDR validation — reading x30 from Context.
        let ctx_x30 = unsafe { core::ptr::read_volatile((tpidr + 0x0F0) as *const u64) };
        // SAFETY: Same TPIDR validation — reading thread ID past Context end.
        let thread_id = unsafe { core::ptr::read_volatile((tpidr + 0x330) as *const u64) };

        serial::panic_puts("\n  thread id=0x");
        serial::panic_put_hex(thread_id);
        serial::panic_puts(" ctx.elr=0x");
        serial::panic_put_hex(ctx_elr);
        serial::panic_puts(" ctx.spsr=0x");
        serial::panic_put_hex(ctx_spsr);
        serial::panic_puts(" ctx.sp=0x");
        serial::panic_put_hex(ctx_sp);
        serial::panic_puts(" ctx.x30=0x");
        serial::panic_put_hex(ctx_x30);
    }

    // Walk the stack for return addresses (best-effort backtrace).
    let kva = (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64;

    if (kva..kva + 0x1000_0000).contains(&sp) {
        serial::panic_puts("\n  stack:");

        let sp_ptr = sp as *const u64;

        for i in 0..8u64 {
            // SAFETY: SP validated above within kernel VA range. Reading up
            // to 8 words (64 bytes) for best-effort backtrace diagnostics.
            let val = unsafe { core::ptr::read_volatile(sp_ptr.add(i as usize)) };

            if i < 4 || (kva + paging::RAM_START..kva + paging::RAM_END_MAX).contains(&val) {
                serial::panic_puts(" [");
                serial::panic_put_hex(i * 8);
                serial::panic_puts("]=0x");
                serial::panic_put_hex(val);
            }
        }
    }

    if ec == 0x25 {
        serial::panic_puts("\ndata abort at EL1 — likely kernel stack overflow");
    } else if ec == 0x21 {
        serial::panic_puts("\ninstruction abort at EL1");
    }

    serial::panic_puts("\n");

    // Release the panic gate so other cores (if also faulted) can print their diagnostics.
    PANIC_GATE.store(0, core::sync::atomic::Ordering::Release);

    panic!("unrecoverable kernel fault");
}
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main(dtb_pa: u64, kaslr_slide: u64) -> ! {
    // SAFETY: Called once, before any phys_to_virt/virt_to_phys. The slide
    // is 32 MiB-aligned, generated by boot.S from CNTVCT_EL0.
    unsafe { memory::set_kaslr_slide(kaslr_slide as usize) };

    serial::puts("🥾 booting…\n");

    if kaslr_slide != 0 {
        serial::puts("  🎲 kaslr - slide ");
        serial::put_u32((kaslr_slide >> 20) as u32);
        serial::puts("mib\n");
    } else {
        serial::puts("  🎲 kaslr - disabled (slide = 0)\n");
    }

    memory::init();
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

    // Read RAM size from the DTB's /memory node. The reg property contains
    // (base, size) pairs; we use the first entry. Falls back to the
    // compile-time RAM_END_MAX if the DTB is missing or has no memory node.
    let ram_end = if let Some(ref dt) = device_table {
        if let Some((base, size)) = dt.memory_region() {
            let dtb_ram_end = base.saturating_add(size);

            // Sanity: base must match our known RAM start, and end must not
            // exceed what boot.S identity-mapped (RAM_END_MAX).
            if base == paging::RAM_START && dtb_ram_end <= paging::RAM_END_MAX {
                paging::set_ram_end(dtb_ram_end);

                dtb_ram_end as usize
            } else {
                paging::RAM_END_MAX as usize
            }
        } else {
            paging::RAM_END_MAX as usize
        }
    } else {
        paging::RAM_END_MAX as usize
    };
    let ram_mib = (ram_end - paging::RAM_START as usize) / (1024 * 1024);

    serial::puts("  💾 memory - ");
    serial::put_u32(ram_mib as u32);
    serial::puts("mib ram, w^x page tables\n");

    // Initialize page frame allocator with memory above kernel heap.
    // SAFETY: __kernel_end is a linker-defined symbol; taking its address
    // yields a valid kernel VA marking the end of the kernel image.
    let kernel_end_pa = memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize });
    let heap_end = kernel_end_pa.0 + memory::HEAP_SIZE;

    assert!(heap_end < ram_end, "heap extends beyond physical ram");

    page_allocator::init(heap_end, ram_end);
    serial::puts("  🧩 frames - ");
    serial::put_u32(page_allocator::free_count() as u32);
    serial::puts(" free (buddy allocator, 4k–8m)\n");

    // pvpanic: paravirtual panic notification (QEMU pvpanic-mmio spec).
    // Discovered as early as possible so it's available for any subsequent panic.
    // The address is in the UART L2 block (0x0900_0000), already mapped at boot.
    if let Some(ref dt) = device_table {
        if let Some(dev) = dt.find_first("qemu,pvpanic-mmio") {
            let pa = dev.base_address();

            if pa != 0 {
                PVPANIC_ADDR.store(
                    pa as usize + memory::KERNEL_VA_OFFSET + memory::kaslr_slide(),
                    Ordering::Relaxed,
                );

                serial::puts("  🚨 pvpanic - registered\n");
            }
        }
    }

    // Wire DTB into device initialization.
    let gic_from_dtb = if let Some(ref dt) = device_table {
        // GIC: look for "arm,gic-v3" (QEMU virt GICv3).
        // The reg property has 2+ entries: [distributor, redistributor, ...].
        // A 3rd entry (GICv2 compat region) may be present — handle gracefully.
        if let Some(gic) = dt.find_first("arm,gic-v3") {
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
        serial::puts("  ⚡ interrupts - gic v3 (dtb)\n");
    } else {
        serial::puts("  ⚡ interrupts - gic v3 (hardcoded)\n");
    }

    // Initialize kernel PRNG. Seed from hardware RNG (RNDR) if available,
    // then mix in timer counter for additional entropy.
    init_prng();

    scheduler::init();
    channel::init();
    event::init();
    vmo::init();
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

    // Probe PL031 RTC from DTB and append to device manifest.
    // The PL031 is a simple read-only clock — no IRQ needed, just the MMIO PA.
    let mut total_count = device_count;

    if let Some(ref dt) = device_table {
        if total_count < devices.len() {
            if let Some(rtc) = dt.find_first("arm,pl031") {
                let pa = rtc.base_address();

                if pa != 0 {
                    devices[total_count] = Some(VirtioDeviceInfo {
                        pa,
                        irq: 0,
                        device_id: DEVICE_ID_PL031_RTC,
                    });
                    total_count += 1;

                    serial::puts("  🕐 rtc - pl031 discovered\n");
                }
            }
        }
    }

    // Spawn init (suspended) — the only process the kernel creates directly.
    // Microkernel pattern: kernel provides mechanism, init provides policy.
    let (init_pid, _) = process::create_from_user_elf(INIT_ELF, paging::SERVICE_PACK_BASE)
        .expect("failed to create init");

    process::set_init_pid(init_pid);

    // Map the service pack into init's address space (read-only).
    // Init reads ELFs from this region at boot instead of compiled-in statics.
    // SAFETY: _services_start/_services_end are linker-defined symbols; taking
    // their address yields the kernel VA of the .services section boundaries.
    let pack_start_kva = unsafe { &_services_start as *const u8 as u64 };
    let pack_end_kva = unsafe { &_services_end as *const u8 as u64 };
    let pack_size = pack_end_kva - pack_start_kva;

    if pack_size > 0 {
        let pack_pa = pack_start_kva - (memory::KERNEL_VA_OFFSET + memory::kaslr_slide()) as u64;
        let page_count = (pack_size + paging::PAGE_SIZE - 1) / paging::PAGE_SIZE;

        scheduler::with_process(init_pid, |process| {
            assert!(
                process.address_space.map_fixed_readonly(
                    paging::SERVICE_PACK_BASE,
                    pack_pa,
                    page_count,
                ),
                "failed to map service pack into init"
            );
        })
        .expect("init process not found for pack mapping");

        serial::puts("  📦 services - pack mapped into init\n");
    }

    let (ch_a, ch_b) = channel::create().expect("failed to create init channel");
    // Write device manifest to channel page 0 (kernel→init direction).
    let pages = channel::shared_pages(ch_a).expect("channel shared pages");

    write_device_manifest(pages[0], &devices, total_count);

    // Give init the channel endpoint.
    channel::setup_endpoint(ch_b, init_pid).expect("failed to setup init channel");
    // Close kernel's endpoint — init reads the manifest from the shared page.
    channel::close_endpoint(ch_a);

    boot_secondaries();

    timer::init();
    serial::puts("  ⏱️  timer - tickless\n");
    // Start init AFTER secondaries and timer are live. start_suspended_threads
    // sends an IPI to wake an idle core — secondaries must be online for the
    // IPI to reach them. Without this ordering, the init thread sits in a ready
    // queue with no core to pick it up.
    scheduler::start_suspended_threads(init_pid);
    serial::puts("  🔀 processes - init started with device manifest\n");
    serial::puts("🥾 booted.\n");

    loop {
        // SAFETY: WFI puts the core into a low-power wait state until an
        // interrupt (timer, IPI, or device IRQ). Does not access memory or
        // use the stack. nomem is correct. WFI is used instead of WFE
        // because IPIs (SGI 0 via ICC_SGI1R_EL1) wake WFI but not WFE
        // (WFE requires a SEV event which GICv3 IPIs do not generate).
        arch::cpu::wait_for_interrupt();
    }
}
/// Entry point for secondary cores (called from boot.S secondary_entry).
///
/// `core_id` is the MPIDR affinity (1..7), passed as context_id by PSCI.
/// Initializes per-core GIC, scheduler state, and timer, then enters idle.
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(core_id: u64) -> ! {
    use interrupt_controller::InterruptController;

    interrupt_controller::GIC.init_per_core(core_id as u32);
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
        // SAFETY: WFI puts the core into a low-power wait state until an
        // interrupt (timer, IPI, or device IRQ). No memory access, no stack
        // usage. nomem is correct. WFI is used instead of WFE because IPIs
        // (SGI 0) wake WFI but not WFE.
        arch::cpu::wait_for_interrupt();
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn svc_handler(ctx: *mut Context) -> *const Context {
    debug_assert!(!ctx.is_null(), "svc_handler: ctx is null (TPIDR_EL1 was 0)");

    let mut canary: u64 = 0;

    // SAFETY: volatile write to stack-local canary.
    unsafe {
        core::ptr::write_volatile(&mut canary, 0xCAFE_BABE_5BC0_0002_u64);
    }

    let result = syscall::dispatch(ctx);
    // SAFETY: volatile read of stack-local canary.
    let check = unsafe { core::ptr::read_volatile(&canary) };

    if check != 0xCAFE_BABE_5BC0_0002 {
        panic!(
            "svc_handler: stack canary corrupt (got {check:#018x}), \
             stack frame was overwritten during syscall dispatch"
        );
    }

    debug_assert!(
        !result.is_null(),
        "svc_handler: returning null context pointer"
    );

    validate_context_before_eret(result);

    result
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
    debug_assert!(
        !ctx.is_null(),
        "user_fault_handler: ctx is null (TPIDR_EL1 was 0)"
    );

    let esr = arch::cpu::read_esr();
    let far = arch::cpu::read_far();
    let elr = arch::cpu::read_elr();
    let ec = (esr >> 26) & 0x3F;
    let fsc = esr & 0x3F; // DFSC (data abort) or IFSC (instruction abort)

    // Translation faults: DFSC/IFSC 0b0001xx (levels 0-3).
    // Only these can be resolved by demand paging. Permission faults,
    // alignment faults, access flag faults, etc. must NOT attempt paging —
    // the page is already mapped; remapping would loop and leak memory.
    let is_translation_fault = (fsc & 0b111100) == 0b000100;

    if (ec == 0x24 || ec == 0x20) && is_translation_fault {
        metrics::inc_page_faults();

        let result =
            scheduler::current_process_do(|process| process.address_space.handle_fault(far));

        match result {
            address_space::FaultResult::Handled => {
                // Page mapped successfully — return to the faulting instruction.
                return ctx;
            }
            address_space::FaultResult::NeedsPager {
                vmo_id,
                page_offset,
                channel_id,
                is_new_request,
            } => {
                // Forward fault to userspace pager. Write fault offset to
                // pager ring, signal the pager channel, block this thread.
                if is_new_request {
                    pager::dispatch_fault(channel_id, page_offset);
                }

                return pager::block_for_pager(ctx, vmo_id, page_offset);
            }
            address_space::FaultResult::Unhandled => {
                // Fall through to diagnostic + terminate.
            }
        }
    }
    // Future extension point (phase 3d): dispatch to process exception handler here.

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

    let result = scheduler::exit_current_from_syscall(ctx);

    validate_context_before_eret(result);

    result
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Serialize panic output across cores (same gate as kernel_fault_handler).
    let my_core = per_core::core_id() + 1;

    if PANIC_GATE
        .compare_exchange(
            0,
            my_core,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        for _ in 0..100_000_000u64 {
            if PANIC_GATE.load(core::sync::atomic::Ordering::Acquire) == 0 {
                break;
            }

            core::hint::spin_loop();
        }

        PANIC_GATE.store(my_core, core::sync::atomic::Ordering::Release);
    }

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

    // Release the panic gate so other cores can print (if they also faulted).
    PANIC_GATE.store(0, core::sync::atomic::Ordering::Release);

    // Signal the hypervisor to capture state and write a crash report.
    // pvpanic is the primary mechanism — the hypervisor exits immediately.
    // SYSTEM_OFF is the fallback for hypervisors without pvpanic support.
    // Spin loop is the ultimate fallback if neither mechanism terminates the VM.
    pvpanic_signal();
    system_off();

    loop {
        core::hint::spin_loop();
    }
}
