#![no_std]
#![no_main]
#![feature(sync_unsafe_cell)]

extern crate alloc;

use core::panic::PanicInfo;

core::arch::global_asm!(include_str!("boot.S"));
core::arch::global_asm!(include_str!("exception.S"));

mod addr_space;
mod asid;
mod elf;
mod gic;
mod handle;
mod heap;
mod memory;
mod mmio;
mod page_alloc;
mod scheduler;
mod syscall;
mod thread;
mod timer;
mod uart;

/// Thread context saved/restored across exception boundaries.
///
/// Layout must match the CTX_* offsets in exception.S exactly.
/// The const assertions below enforce this at compile time.
#[repr(C)]
pub struct Context {
    pub x: [u64; 31], // x0..x30
    pub sp: u64,
    pub elr: u64,
    pub spsr: u64,
    pub sp_el0: u64,
    pub tpidr_el0: u64,
    pub q: [u128; 32],
    pub fpcr: u64,
    pub fpsr: u64,
}

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

const PAGE_SIZE: u64 = 4096;
const USER_STACK_TOP: u64 = 0x0000_0000_8000_0000; // 2 GB
const USER_STACK_VA: u64 = USER_STACK_TOP - PAGE_SIZE;

/// User process ELF binaries, compiled by build.rs and embedded in .rodata.
/// Avoids needing a filesystem or bootloader protocol for the first processes.
static INIT_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/init.elf"));
static ECHO_ELF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/echo.elf"));

extern "C" {
    static __kernel_end: u8;
}

fn spawn_user_from_elf(elf_bytes: &[u8]) {
    let header = elf::parse_header(elf_bytes).expect("bad ELF header");
    let asid = asid::alloc();
    let mut addr_space = alloc::boxed::Box::new(addr_space::AddressSpace::new(asid));

    // Map each PT_LOAD segment from the ELF into the new address space.
    // Assumes page-aligned vaddr (enforced by the user linker script).
    // A general-purpose loader would handle within-page offsets for unaligned segments.
    for i in 0..header.ph_count {
        let seg = match elf::load_segment(elf_bytes, &header, i).expect("bad program header") {
            Some(seg) => seg,
            None => continue,
        };
        let file_data = elf::segment_data(elf_bytes, &seg).expect("segment data out of bounds");
        let attrs = elf::segment_attrs(seg.flags);
        let page_count = (seg.mem_size + PAGE_SIZE - 1) / PAGE_SIZE;

        for page in 0..page_count {
            let pa = page_alloc::alloc_frame().expect("out of frames for user segment");
            let va = (seg.vaddr & !(PAGE_SIZE - 1)) + page * PAGE_SIZE;
            // Copy file-backed data into this page. Pages beyond file_size are
            // pure BSS — alloc_frame returns zeroed pages, so they're already correct.
            let seg_offset = page * PAGE_SIZE;

            if seg_offset < seg.file_size {
                let src_start = seg_offset as usize;
                let src_end =
                    core::cmp::min((seg_offset + PAGE_SIZE) as usize, seg.file_size as usize);
                let src = &file_data[src_start..src_end];
                let dst = memory::phys_to_virt(pa) as *mut u8;

                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            }

            addr_space.map_page(va, pa as u64, &attrs);
        }
    }

    // Map one user stack page.
    let stack_pa = page_alloc::alloc_frame().expect("out of frames for user stack");

    addr_space.map_page(
        USER_STACK_VA,
        stack_pa as u64,
        &addr_space::PageAttrs::user_rw(),
    );

    scheduler::spawn_user(addr_space, header.entry, USER_STACK_TOP);
}

#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    uart::puts("🥾 booting…\n");

    memory::init();
    heap::init();

    // Initialize page frame allocator with memory above kernel heap.
    let kernel_end_pa = memory::virt_to_phys(unsafe { &__kernel_end as *const u8 as usize });
    let heap_end = kernel_end_pa + memory::HEAP_SIZE;
    let ram_end = 0x4000_0000 + 256 * 1024 * 1024;

    page_alloc::init(heap_end, ram_end);
    gic::init();
    scheduler::init();

    // Spawn user processes from embedded ELFs.
    spawn_user_from_elf(INIT_ELF);
    spawn_user_from_elf(ECHO_ELF);

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
