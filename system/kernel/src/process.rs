//! Process creation from ELF binaries.

use super::addr_space::{AddressSpace, PageAttrs};
use super::asid;
use super::elf;
use super::memory;
use super::page_alloc;
use super::paging::{PAGE_SIZE, USER_STACK_TOP, USER_STACK_VA};
use super::scheduler;
use super::thread::ThreadId;
use alloc::boxed::Box;

/// Parse an ELF binary and spawn a user process.
///
/// Loads each PT_LOAD segment into a fresh address space, maps a user
/// stack page, and hands the thread to the scheduler.
pub fn spawn_from_elf(elf_bytes: &[u8]) -> ThreadId {
    let header = elf::parse_header(elf_bytes).expect("bad ELF header");
    let asid = asid::alloc();
    let mut addr_space = Box::new(AddressSpace::new(asid));

    // Map each PT_LOAD segment from the ELF into the new address space.
    // Assumes page-aligned vaddr (enforced by the user linker script).
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

                // SAFETY: `pa` was just allocated and converted to kernel VA.
                // `src` is bounded by the ELF data slice. No overlap possible.
                unsafe { core::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            }

            addr_space.map_page(va, pa as u64, &attrs);
        }
    }

    // Map one user stack page.
    let stack_pa = page_alloc::alloc_frame().expect("out of frames for user stack");

    addr_space.map_page(USER_STACK_VA, stack_pa as u64, &PageAttrs::user_rw());

    scheduler::spawn_user(addr_space, header.entry, USER_STACK_TOP)
}
